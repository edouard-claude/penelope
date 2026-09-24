//! Jobs d'outils natifs : un appel long sort du tour (issue #204).
//!
//! Avant : `run_effect` planifiait l'effet puis attendait `execute_cancellable` du début à
//! la fin. Un `shell_exec` à `timeout_ms` d'une heure immobilisait le tour, et le message
//! du propriétaire attendait derrière lui.
//!
//! ```text
//!  appel `background: true`
//!     ledger : effet planifié puis `dispatching` (§4.2, rien ne le contourne)
//!     ligne `tool_jobs` `working`, événement `tool.job.started`
//!     l'outil rend tout de suite {job, state:"working"} ─► le tour continue
//!
//!  ailleurs, hors du tour
//!     l'outil tourne, `/stop` le coupe par son jeton
//!     fin : ledger `completed`/`failed`, job terminal, `tool.job.completed`
//!
//!  livraison
//!     boucle de fond ─► tour `Nudge` dans la session d'origine, avec le résultat
//!     session fermée : rien n'est perdu, la livraison attend sa réouverture
//! ```
//!
//! Le modèle de durée est celui des tâches MCP (`penelope_mcp::tasks`) : mêmes états —
//! `TaskState` est importé, pas redéfini —, mêmes colonnes `request`/`result`, mêmes
//! règles de purge et de rétention. La table est distincte parce qu'un job natif ne se
//! sonde pas chez un serveur : il tourne ici, porte un outil, ses arguments et l'effet du
//! ledger. Sans `poll_at`, donc : rien ne le sonde (décision 0012).

use crate::agent::ToolExecutor;
use crate::bus::Origin;
use crate::runtime::{Daemon, Services};
use penelope_kernel::clock::SharedClock;
use penelope_kernel::config::Config;
use penelope_kernel::event::EventDraft;
use penelope_kernel::ids::EffectId;
use penelope_kernel::turn::TurnKind;
use penelope_llm::CancelToken;
use penelope_llm::types::ToolCall;
use penelope_mcp::tasks::TaskState;
use penelope_store::{Store, rusqlite::params};
use penelope_tools::ToolOutcome;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Un job d'outil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolJob {
    pub id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    /// Tour d'origine, qui peut être clos depuis longtemps.
    pub turn_id: Option<String>,
    /// Appel d'outil qui l'a lancé, tel que le transcript le nomme.
    pub call_id: Option<String>,
    pub tool: String,
    pub request: Value,
    pub state: TaskState,
    pub result: Option<Value>,
    /// Effet du ledger resté `dispatching` tant que le job tourne (§4.2).
    pub effect_id: Option<String>,
    pub delivered_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ToolJob {
    /// Âge en secondes, pour `self_status`, `doctor` et `penelope jobs`.
    pub fn age_s(&self, now_ms: i64) -> i64 {
        chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map(|t| (now_ms - t.timestamp_millis()).max(0) / 1000)
            .unwrap_or(0)
    }

    /// Ce que le modèle lit quand il interroge le job.
    pub fn view(&self, now_ms: i64) -> Value {
        json!({
            "job": self.id,
            "tool": self.tool,
            "state": self.state.as_str(),
            "session": self.session_id,
            "age_s": self.age_s(now_ms),
            "result": self.result.clone().unwrap_or(Value::Null),
        })
    }
}

/// Ce qu'une demande de job crée.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub session_id: String,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub call_id: Option<String>,
    pub tool: String,
    pub request: Value,
    pub effect_id: Option<String>,
}

/// Table `tool_jobs`, lue et écrite comme `mcp_tasks`.
#[derive(Clone)]
pub struct JobStore {
    store: Store,
    clock: SharedClock,
}

impl JobStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        JobStore { store, clock }
    }

    pub async fn create(&self, new: NewJob) -> anyhow::Result<ToolJob> {
        let now = self.clock.now_rfc3339();
        let job = ToolJob {
            id: format!("tj_{}", penelope_kernel::ids::Ulid::new()),
            session_id: new.session_id,
            run_id: new.run_id,
            turn_id: new.turn_id,
            call_id: new.call_id,
            tool: new.tool,
            request: new.request,
            state: TaskState::Working,
            result: None,
            effect_id: new.effect_id,
            delivered_at: None,
            created_at: now.clone(),
            updated_at: now,
        };
        let row = job.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO tool_jobs(id, session_id, run_id, turn_id, call_id, tool,
                        request, state, effect_id, created_at, updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,'working',?8,?9,?9)",
                    params![
                        row.id,
                        row.session_id,
                        row.run_id,
                        row.turn_id,
                        row.call_id,
                        row.tool,
                        row.request.to_string(),
                        row.effect_id,
                        row.created_at
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(job)
    }

    pub async fn get(&self, id: &str) -> anyhow::Result<Option<ToolJob>> {
        let id = id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(&format!("{SELECT} WHERE id = ?1"))?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_job(r)?)),
                    None => Ok(None),
                }
            })
            .await?)
    }

    /// Jobs d'une session, du plus récent au plus ancien. `live` : seulement ceux qui
    /// tournent encore.
    pub async fn of_session(&self, session_id: &str, live: bool) -> anyhow::Result<Vec<ToolJob>> {
        let sid = session_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let filter = if live { LIVE } else { "1=1" };
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE session_id = ?1 AND {filter} ORDER BY created_at DESC LIMIT 50"
                ))?;
                let rows = st.query_map([&sid], row_to_job)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?)
    }

    /// Tous les jobs vivants du processus, toutes sessions confondues.
    pub async fn live(&self) -> anyhow::Result<Vec<ToolJob>> {
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE {LIVE} ORDER BY created_at LIMIT 200"
                ))?;
                let rows = st.query_map([], row_to_job)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?)
    }

    /// Compte les jobs vivants d'une session et du daemon entier, en une lecture.
    pub async fn counts(&self, session_id: &str) -> anyhow::Result<(usize, usize)> {
        let sid = session_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let total: i64 = c.query_row(
                    &format!("SELECT count(*) FROM tool_jobs WHERE {LIVE}"),
                    [],
                    |r| r.get(0),
                )?;
                let mine: i64 = c.query_row(
                    &format!("SELECT count(*) FROM tool_jobs WHERE session_id = ?1 AND {LIVE}"),
                    [&sid],
                    |r| r.get(0),
                )?;
                Ok((mine as usize, total as usize))
            })
            .await?)
    }

    /// Pose l'état terminal d'un job. Un job déjà terminal ne bouge plus : le premier qui
    /// conclut gagne, une annulation n'est pas écrasée par la fin de la commande tuée.
    pub async fn finish(
        &self,
        id: &str,
        state: TaskState,
        result: Option<Value>,
    ) -> anyhow::Result<bool> {
        let (id, ts) = (id.to_string(), self.clock.now_rfc3339());
        Ok(self
            .store
            .write(move |tx| {
                let n = tx.execute(
                    &format!(
                        "UPDATE tool_jobs SET state = ?2, result = ?3, updated_at = ?4
                         WHERE id = ?1 AND {LIVE}"
                    ),
                    params![id, state.as_str(), result.map(|r| r.to_string()), ts],
                )?;
                Ok(n > 0)
            })
            .await?)
    }

    /// Jobs terminés dont le résultat n'a pas encore rejoint sa session.
    pub async fn undelivered(&self, limit: i64) -> anyhow::Result<Vec<ToolJob>> {
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE delivered_at IS NULL AND NOT ({LIVE})
                     ORDER BY updated_at LIMIT ?1"
                ))?;
                let rows = st.query_map([limit], row_to_job)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?)
    }

    /// Marque la livraison faite. Faux si un autre l'a déjà faite : la relance ne part
    /// qu'une fois.
    pub async fn mark_delivered(&self, id: &str) -> anyhow::Result<bool> {
        let (id, ts) = (id.to_string(), self.clock.now_rfc3339());
        Ok(self
            .store
            .write(move |tx| {
                let n = tx.execute(
                    "UPDATE tool_jobs SET delivered_at = ?2 WHERE id = ?1 AND delivered_at IS NULL",
                    params![id, ts],
                )?;
                Ok(n > 0)
            })
            .await?)
    }

    /// Au démarrage : le processus qui portait le job est mort avec le daemon.
    ///
    /// Aucun retry d'office, même pour un outil déclaré idempotent (décision 0012) : le
    /// job devient `failed`, et son effet reste sur le chemin `dispatching` → `unknown`
    /// du ledger, qui pose **une** question au propriétaire (#83). La livraison dira au
    /// modèle que le job est mort, pour qu'il cesse de l'attendre.
    pub async fn recover_on_boot(&self) -> anyhow::Result<Vec<ToolJob>> {
        let ts = self.clock.now_rfc3339();
        Ok(self
            .store
            .write(move |tx| {
                let lost: Vec<ToolJob> = {
                    let mut st = tx.prepare(&format!("{SELECT} WHERE {LIVE}"))?;
                    let rows = st.query_map([], row_to_job)?;
                    let mut v = Vec::new();
                    for r in rows {
                        v.push(r?);
                    }
                    v
                };
                tx.execute(
                    &format!(
                        "UPDATE tool_jobs SET state = 'failed', result = ?1, updated_at = ?2
                         WHERE {LIVE}"
                    ),
                    params![json!({"error": LOST_ON_BOOT}).to_string(), ts],
                )?;
                Ok(lost)
            })
            .await?)
    }
}

/// Ce qu'un job perdu au redémarrage porte comme résultat.
pub const LOST_ON_BOOT: &str =
    "le daemon a redémarré pendant ce job : son processus est mort, rien n'a été relancé";

// ---------------------------------------------------------------- registre du processus

/// Jobs qui tournent **dans ce processus**, avec leur jeton d'annulation.
///
/// La table dit ce qui existe, ce registre dit ce qu'on peut encore interrompre : après
/// un redémarrage, la table garde des jobs `working` que plus aucun jeton ne couvre, et
/// c'est exactement ce que [`JobStore::recover_on_boot`] tranche.
#[derive(Default)]
pub struct Running {
    inner: Mutex<HashMap<String, (String, CancelToken)>>,
    /// Réveille la boucle de livraison : un job qui vient de finir n'attend pas le
    /// prochain battement pour revenir dans sa conversation.
    wake: tokio::sync::Notify,
}

impl Running {
    fn with<T>(&self, f: impl FnOnce(&mut HashMap<String, (String, CancelToken)>) -> T) -> T {
        match self.inner.lock() {
            Ok(mut g) => f(&mut g),
            Err(p) => f(&mut p.into_inner()),
        }
    }

    fn insert(&self, job: &str, session: &str, cancel: CancelToken) {
        self.with(|m| m.insert(job.to_string(), (session.to_string(), cancel)));
    }

    fn forget(&self, job: &str) {
        self.with(|m| m.remove(job));
    }

    /// Interrompt un job nommé. Faux : il ne tourne pas ici.
    pub fn cancel(&self, job: &str) -> bool {
        self.with(|m| match m.remove(job) {
            Some((_, c)) => {
                c.cancel();
                true
            }
            None => false,
        })
    }

    /// Interrompt les jobs d'une session (`/stop`, issue #57).
    pub fn cancel_session(&self, session: &str) -> usize {
        self.with(|m| {
            let mine: Vec<String> = m
                .iter()
                .filter(|(_, (s, _))| s == session)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &mine {
                if let Some((_, c)) = m.remove(id) {
                    c.cancel();
                }
            }
            mine.len()
        })
    }

    /// Interrompt tous les jobs du daemon (`/stop tout`, issue #155).
    pub fn cancel_all(&self) -> usize {
        self.with(|m| {
            let n = m.len();
            for (_, (_, c)) in m.drain() {
                c.cancel();
            }
            n
        })
    }

    /// Jobs de cette session qui tournent ici.
    pub fn of_session(&self, session: &str) -> usize {
        self.with(|m| m.values().filter(|(s, _)| s == session).count())
    }

    pub fn len(&self) -> usize {
        self.with(|m| m.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Un job vient de conclure : la livraison peut partir.
    pub fn wake(&self) {
        self.wake.notify_one();
    }
}

// ---------------------------------------------------------------- lancement

/// Le magasin de jobs des services.
pub fn store(s: &Services) -> JobStore {
    JobStore::new(s.store.clone(), s.clock.clone())
}

/// Outils qui savent rendre la main : les deux que le ticket nomme, et eux seuls. Un
/// `background: true` ailleurs reste un argument inconnu, refusé par le schéma.
pub const BACKGROUNDABLE: &[&str] = &["shell_exec", "sub_agent_spawn"];

/// Outils de suivi, exposés à la session dès qu'un job y naît (#104).
pub const FOLLOW_UP: &[&str] = &["job_status", "job_wait", "job_cancel", "job_list"];

/// Vrai quand l'appel demande explicitement l'arrière-plan.
pub fn wants_background(tool: &str, args: &Value) -> bool {
    BACKGROUNDABLE.contains(&tool) && args.get("background").and_then(|v| v.as_bool()) == Some(true)
}

/// Ce qu'un appel long se voit **proposer** dans le texte de son résultat : rien n'est
/// détourné d'office, c'est le modèle qui décide au prochain appel (issue #204).
pub fn background_hint(cfg: &Config, tool: &str, args: &Value) -> Option<String> {
    if !BACKGROUNDABLE.contains(&tool) || wants_background(tool, args) {
        return None;
    }
    let asked = args.get("timeout_ms").and_then(|v| v.as_i64())?;
    let seuil = penelope_kernel::config::parse_duration(&cfg.tools.background_after)
        .ok()?
        .as_millis() as i64;
    (asked > seuil).then(|| {
        format!(
            "\n\n[Harnais : ce `{tool}` a demandé {} s de délai, au-delà des {} s de \
             `tools.background_after`. Pendant ce temps le tour est bloqué et le \
             propriétaire attend, même pour dire « laisse tomber ». La prochaine fois, \
             `background: true` : l'appel rend la main tout de suite et son résultat \
             revient seul dans la conversation.]",
            asked / 1000,
            seuil / 1000
        )
    })
}

/// Vrai quand une session sait recevoir une relance : une conversation, ou une session
/// planifiée, qui est une conversation ouverte par un déclencheur (#39).
async fn delivers_to_a_conversation(s: &Services, session_id: &str) -> bool {
    use penelope_kernel::session::SessionKind;
    matches!(
        s.sessions.get(session_id).await,
        Ok(Some(ref sess)) if matches!(sess.kind, SessionKind::Chat | SessionKind::Scheduled)
    )
}

/// Ce qu'il faut savoir d'un appel pour en faire un job.
pub struct JobRequest<'a> {
    pub session_id: &'a str,
    pub run_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub call: &'a ToolCall,
    /// Nom effectif de l'outil, après normalisation (`tool_call` compris).
    pub tool: &'a str,
    /// Effet déjà planifié, encore `planned` : c'est le job qui le passera
    /// `dispatching` puis à son état final (§4.2).
    pub effect: &'a EffectId,
}

/// Transforme l'appel en job s'il le demande. `None` : rien à faire, l'appel suit le
/// chemin ordinaire et le ledger reste sur sa trajectoire habituelle.
///
/// Sur refus (plafond atteint), l'effet est clos `failed` : il ne reste pas `planned`
/// dans le ledger pour un appel qui n'aura jamais lieu.
pub async fn maybe_spawn(
    services: &Arc<Services>,
    execute: &(dyn ToolExecutor + Send + Sync),
    req: JobRequest<'_>,
) -> anyhow::Result<Option<ToolOutcome>> {
    // `tool_call` enveloppe l'appel dans `{name, args}` : la demande d'arrière-plan est
    // dans l'enveloppe, pas au premier niveau.
    let asked = crate::executor::effective_arguments(&req.call.name, &req.call.arguments);
    if !wants_background(req.tool, &asked) {
        return Ok(None);
    }
    // Un exécuteur qui ne sait pas se détacher exécute comme avant : mieux vaut un tour
    // occupé qu'un job qui n'aboutit jamais.
    let Some(detached) = execute.detached() else {
        return Ok(None);
    };
    let s = services.clone();
    // Un job n'existe que pour une conversation. La session d'un sous-agent meurt avec sa
    // conclusion et celle d'un run de workflow est pilotée, pas par des tours : la relance
    // n'y trouverait personne, et un run a déjà son attente d'étape (`mcp_tasks`). Là,
    // l'appel suit le chemin ordinaire — c'est le comportement d'avant #204, qui rend son
    // vrai résultat.
    if !delivers_to_a_conversation(&s, req.session_id).await {
        return Ok(None);
    }
    let cfg = s.config.config();
    let jobs = store(&s);
    let (mine, total) = jobs.counts(req.session_id).await?;
    if mine >= cfg.tools.jobs_per_session || total >= cfg.tools.jobs_total {
        let reason = if mine >= cfg.tools.jobs_per_session {
            format!(
                "{mine} job(s) déjà en cours dans cette conversation, plafond \
                 `tools.jobs_per_session` = {}",
                cfg.tools.jobs_per_session
            )
        } else {
            format!(
                "{total} job(s) déjà en cours sur tout le daemon, plafond \
                 `tools.jobs_total` = {}",
                cfg.tools.jobs_total
            )
        };
        s.effects.fail(req.effect, &reason).await?;
        return Ok(Some(ToolOutcome {
            value: json!({"state": "refused", "reason": reason}),
            is_error: true,
            text: format!(
                "Job refusé : {reason}. Attends les précédents — `job_list` les montre, \
                 `job_wait` en attend un — ou lance cet appel sans `background`.",
            ),
            eager: false,
        }));
    }

    s.effects.dispatching(req.effect).await?;
    let job = jobs
        .create(NewJob {
            session_id: req.session_id.to_string(),
            run_id: req.run_id.map(String::from),
            turn_id: req.turn_id.map(String::from),
            call_id: Some(req.call.id.clone()),
            tool: req.tool.to_string(),
            request: penelope_observe::redact_json(&asked),
            effect_id: Some(req.effect.as_str().to_string()),
        })
        .await?;

    // Le résultat renvoie vers `job_status` et consorts : ils doivent être sous la main
    // du tour suivant, pas seulement trouvables par `tool_search` (#104).
    for t in FOLLOW_UP {
        crate::tools_on_demand::touch(&s, req.session_id, t).await;
    }

    let cancel = CancelToken::new();
    s.jobs.insert(&job.id, req.session_id, cancel.clone());
    s.events
        .append(
            EventDraft::new(
                "tool.job.started",
                json!({"job": job.id, "tool": job.tool, "effect": req.effect.as_str()}),
            )
            .session(req.session_id),
        )
        .await?;

    let name = req.call.name.clone();
    let args = req.call.arguments.clone();
    let effect = req.effect.clone();
    let id = job.id.clone();
    let session = req.session_id.to_string();
    tokio::spawn(async move {
        // Une tâche qui panique s'arrête en silence (#84). Ici, le silence laisserait le
        // job `working` et son effet `dispatching` pour toujours : la panique devient un
        // échec ordinaire, que le ledger et la livraison savent traiter.
        let run = std::panic::AssertUnwindSafe(detached.execute_cancellable(&name, &args, &cancel));
        let outcome = match futures::FutureExt::catch_unwind(run).await {
            Ok(o) => o,
            Err(payload) => {
                let message = crate::tasks::panic_text(payload.as_ref());
                tracing::error!(job = %id, tool = %name, %message, "job en panique");
                Err(penelope_tools::ToolError::Other(format!(
                    "l'outil a paniqué : {message}"
                )))
            }
        };
        if let Err(e) = conclude(&s, &id, &session, &effect, outcome, cancel.is_cancelled()).await {
            tracing::error!(job = %id, error = %e, "fin de job non enregistrée");
        }
    });

    Ok(Some(ToolOutcome {
        value: json!({"job": job.id, "state": "working"}),
        is_error: false,
        text: format!(
            "Job `{}` lancé en arrière-plan ({}). Le tour continue : réponds sans \
             l'attendre. `job_status` donne son état, `job_wait` l'attend un moment, \
             `job_cancel` l'arrête. Son résultat reviendra seul dans cette conversation, \
             même si ce tour est clos depuis longtemps.",
            job.id, job.tool
        ),
        eager: false,
    }))
}

/// Clôt un job : ledger d'abord, ligne ensuite, événement enfin.
async fn conclude(
    s: &Arc<Services>,
    job_id: &str,
    session_id: &str,
    effect: &EffectId,
    outcome: Result<ToolOutcome, penelope_tools::ToolError>,
    cancelled: bool,
) -> anyhow::Result<()> {
    s.jobs.forget(job_id);
    let (state, result, error) = match &outcome {
        _ if cancelled => (
            TaskState::Cancelled,
            json!({"cancelled": true}),
            Some("job annulé".to_string()),
        ),
        Ok(o) if !o.is_error => (TaskState::Completed, o.value.clone(), None),
        Ok(o) => (
            TaskState::Failed,
            json!({"error": o.text}),
            Some(o.text.clone()),
        ),
        Err(e) => (
            TaskState::Failed,
            json!({"error": e.to_string()}),
            Some(e.to_string()),
        ),
    };
    // Le ledger d'abord : un job conclu dont l'effet serait resté `dispatching`
    // poserait une question au propriétaire au prochain démarrage pour rien (#83).
    match &error {
        None => s.effects.complete(effect, result.clone()).await?,
        Some(e) => s.effects.fail(effect, e.clone()).await?,
    }
    let text = match &outcome {
        Ok(o) => o.text.clone(),
        Err(e) => e.to_string(),
    };
    let stored = json!({"value": result, "text": text});
    store(s).finish(job_id, state, Some(stored)).await?;
    s.events
        .append(
            EventDraft::new(
                "tool.job.completed",
                json!({"job": job_id, "state": state.as_str(), "effect": effect.as_str()}),
            )
            .session(session_id),
        )
        .await?;
    s.jobs.wake();
    Ok(())
}

// ---------------------------------------------------------------- livraison

/// Ce que le tour de relance lit. Il nomme l'outil et la demande : le job revient dans
/// une conversation qui a pu changer de sujet entre-temps.
pub fn delivery_text(job: &ToolJob, now_ms: i64) -> String {
    let demande = job.request["command"]
        .as_str()
        .or_else(|| job.request["prompt"].as_str())
        .map(|c| c.chars().take(300).collect::<String>())
        .unwrap_or_else(|| job.tool.clone());
    let corps = job
        .result
        .as_ref()
        .map(|r| {
            let text = r["text"].as_str().unwrap_or_default();
            if text.is_empty() {
                r["error"].as_str().unwrap_or(&r.to_string()).to_string()
            } else {
                text.to_string()
            }
        })
        .unwrap_or_else(|| "sans résultat".into());
    format!(
        "Résultat du job `{}` lancé plus tôt dans cette conversation : `{}` — {}, \
         après {} s.\n\nDemande d'origine : {demande}\n\nRésultat :\n{}\n\n\
         Reprends la suite si elle a encore un sens, ou dis-le si le sujet a changé.",
        job.id,
        job.tool,
        job.state.as_str(),
        job.age_s(now_ms),
        corps.chars().take(6_000).collect::<String>()
    )
}

/// Origine de livraison d'une session : le chat Telegram auquel elle est liée, sinon un
/// canal interne. Elle se relit après une purge, contrairement à ce qu'on aurait rangé
/// dans les arguments du job.
async fn origin_of(s: &Services, session_id: &str) -> Origin {
    match s.sessions.get(session_id).await {
        Ok(Some(sess)) => match sess.tg_chat_id {
            Some(chat_id) => Origin::Telegram {
                chat_id,
                topic_id: sess.tg_topic_id,
                message_id: None,
            },
            None => Origin::Internal {
                source: "tool_job".into(),
            },
        },
        _ => Origin::Internal {
            source: "tool_job".into(),
        },
    }
}

/// Livre les résultats prêts. Un job dont la session est fermée attend sa réouverture :
/// `TurnQueue::claim` annule les tours d'une session fermée, une relance enfilée là
/// serait perdue.
pub async fn deliver_due(d: &Arc<Daemon>) -> anyhow::Result<usize> {
    let s = &d.services;
    let now = s.clock.now_ms();
    let mut sent = 0;
    for job in store(s).undelivered(20).await? {
        let open = matches!(
            s.sessions.get(&job.session_id).await,
            Ok(Some(ref sess)) if sess.state == "active"
        );
        if !open {
            continue;
        }
        if !store(s).mark_delivered(&job.id).await? {
            continue;
        }
        let origin = origin_of(s, &job.session_id).await;
        s.turns
            .enqueue(
                &job.session_id,
                TurnKind::Nudge,
                json!({"text": delivery_text(&job, now), "origin": origin.to_value(), "job": job.id}),
                Some(format!("job:{}", job.id)),
                5,
            )
            .await?;
        d.bus.notify_enqueued();
        sent += 1;
    }
    Ok(sent)
}

/// Battement de la boucle de livraison quand rien ne la réveille : une session rouverte
/// ou un job repris au démarrage n'émet pas de réveil, il faut donc repasser.
const DELIVERY_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Boucle de livraison, surveillée comme les autres (issue #84).
pub async fn deliver_loop(d: Arc<Daemon>) {
    while !d.handle.is_shutting_down() {
        if let Err(e) = deliver_due(&d).await {
            tracing::warn!(error = %e, "livraison des jobs d'outils");
        }
        tokio::select! {
            _ = d.services.jobs.wake.notified() => {}
            _ = tokio::time::sleep(DELIVERY_POLL) => {}
        }
    }
}

// ---------------------------------------------------------------- outils du modèle

/// Attente bornée : au-delà du délai, l'état courant, jamais un tour perdu.
pub async fn wait(s: &Services, id: &str, timeout_ms: i64) -> anyhow::Result<Option<ToolJob>> {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(timeout_ms.clamp(1_000, 120_000) as u64);
    let jobs = store(s);
    loop {
        let Some(job) = jobs.get(id).await? else {
            return Ok(None);
        };
        if job.state.is_terminal() || std::time::Instant::now() >= deadline {
            return Ok(Some(job));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// `job_status`, `job_wait`, `job_cancel`, `job_list` (#104 : à la demande).
pub async fn tool(
    s: &Arc<Services>,
    session_id: &str,
    name: &str,
    args: &Value,
) -> Result<Value, penelope_tools::ToolError> {
    let now = s.clock.now_ms();
    let jobs = store(s);
    let id_of = |args: &Value| {
        args.get("job")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| penelope_tools::ToolError::Invalid("`job` manquant".into()))
    };
    let missing = |id: &str| {
        penelope_tools::ToolError::Invalid(format!(
            "job `{id}` inconnu : `job_list` donne ceux de cette conversation"
        ))
    };
    let fail = |e: anyhow::Error| penelope_tools::ToolError::Other(e.to_string());
    match name {
        "job_list" => {
            let all = args.get("all").and_then(|v| v.as_bool()) == Some(true);
            let rows = if all {
                jobs.live().await.map_err(fail)?
            } else {
                jobs.of_session(session_id, false).await.map_err(fail)?
            };
            Ok(json!({
                "jobs": rows.iter().map(|j| j.view(now)).collect::<Vec<_>>(),
                "running": rows.iter().filter(|j| !j.state.is_terminal()).count(),
            }))
        }
        "job_status" => {
            let id = id_of(args)?;
            let job = jobs
                .get(&id)
                .await
                .map_err(fail)?
                .ok_or_else(|| missing(&id))?;
            Ok(job.view(now))
        }
        "job_wait" => {
            let id = id_of(args)?;
            let ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(30_000);
            let job = wait(s, &id, ms)
                .await
                .map_err(fail)?
                .ok_or_else(|| missing(&id))?;
            let mut v = job.view(s.clock.now_ms());
            v["waited"] = json!(!job.state.is_terminal());
            Ok(v)
        }
        "job_cancel" => {
            let id = id_of(args)?;
            let job = jobs
                .get(&id)
                .await
                .map_err(fail)?
                .ok_or_else(|| missing(&id))?;
            if job.state.is_terminal() {
                return Ok(json!({"job": id, "state": job.state.as_str(), "cancelled": false}));
            }
            // Le jeton tue le groupe de processus ; la conclusion du job se charge du
            // ledger et de l'état final.
            let here = s.jobs.cancel(&id);
            if !here {
                // Job d'une vie antérieure du daemon : rien à tuer, la ligne est close.
                jobs.finish(&id, TaskState::Cancelled, Some(json!({"cancelled": true})))
                    .await
                    .map_err(fail)?;
            }
            Ok(json!({"job": id, "state": "cancelled", "cancelled": true}))
        }
        other => Err(penelope_tools::ToolError::Invalid(format!(
            "outil de job inconnu : {other}"
        ))),
    }
}

const SELECT: &str = "SELECT id, session_id, run_id, turn_id, call_id, tool, request, state,
     result, effect_id, delivered_at, created_at, updated_at FROM tool_jobs";

/// Un job qui tourne encore, au sens des états MCP.
const LIVE: &str = "state IN ('working','input_required')";

fn row_to_job(r: &penelope_store::rusqlite::Row<'_>) -> penelope_store::rusqlite::Result<ToolJob> {
    let request: String = r.get(6)?;
    let state: String = r.get(7)?;
    let result: Option<String> = r.get(8)?;
    Ok(ToolJob {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        turn_id: r.get(3)?,
        call_id: r.get(4)?,
        tool: r.get(5)?,
        request: serde_json::from_str(&request).unwrap_or(Value::Null),
        state: TaskState::parse(&state).unwrap_or(TaskState::Working),
        result: result.and_then(|s| serde_json::from_str(&s).ok()),
        effect_id: r.get(9)?,
        delivered_at: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_kernel::turn::Turn;
    use penelope_llm::mock::{MockProvider, Scripted};
    use std::sync::Arc;

    fn jobs() -> (Store, JobStore, TestClock) {
        let store = Store::open_memory().unwrap();
        let clock = TestClock::default();
        let j = JobStore::new(store.clone(), Arc::new(clock.clone()));
        (store, j, clock)
    }

    fn spec(session: &str, tool: &str) -> NewJob {
        NewJob {
            session_id: session.into(),
            run_id: None,
            turn_id: Some("t1".into()),
            call_id: Some("call_1".into()),
            tool: tool.into(),
            request: json!({"command": "sleep 30"}),
            effect_id: Some("ef_1".into()),
        }
    }

    /// #204 : un job naît `working`, se retrouve par son identifiant, et son résultat
    /// arrive à la fin — le modèle durable des tâches MCP, appliqué aux outils natifs.
    #[tokio::test]
    async fn a_job_lives_until_its_result_lands() {
        let (_s, j, _clock) = jobs();
        let job = j.create(spec("s1", "shell_exec")).await.unwrap();
        assert_eq!(job.state, TaskState::Working);
        assert_eq!(j.of_session("s1", true).await.unwrap().len(), 1);
        assert_eq!(j.counts("s1").await.unwrap(), (1, 1));

        assert!(
            j.finish(&job.id, TaskState::Completed, Some(json!({"exit_code": 0})))
                .await
                .unwrap()
        );
        let got = j.get(&job.id).await.unwrap().unwrap();
        assert_eq!(got.state, TaskState::Completed);
        assert_eq!(got.result.unwrap()["exit_code"], 0);
        assert!(j.of_session("s1", true).await.unwrap().is_empty());
        assert_eq!(j.of_session("s1", false).await.unwrap().len(), 1);
        assert_eq!(j.counts("s1").await.unwrap(), (0, 0));
    }

    /// #204 : le premier état terminal gagne. `/stop` annule, la commande tuée rend son
    /// échec juste après : le job reste `cancelled`.
    #[tokio::test]
    async fn the_first_terminal_state_wins() {
        let (_s, j, _clock) = jobs();
        let job = j.create(spec("s1", "shell_exec")).await.unwrap();
        assert!(j.finish(&job.id, TaskState::Cancelled, None).await.unwrap());
        assert!(
            !j.finish(&job.id, TaskState::Failed, Some(json!({"error": "tué"})))
                .await
                .unwrap()
        );
        assert_eq!(
            j.get(&job.id).await.unwrap().unwrap().state,
            TaskState::Cancelled
        );
    }

    /// #204 : un job terminé attend sa livraison, et ne part qu'une fois — même si deux
    /// boucles le voient ensemble.
    #[tokio::test]
    async fn a_finished_job_is_delivered_exactly_once() {
        let (_s, j, _clock) = jobs();
        let job = j.create(spec("s1", "shell_exec")).await.unwrap();
        assert!(j.undelivered(10).await.unwrap().is_empty(), "pas fini");
        j.finish(&job.id, TaskState::Completed, None).await.unwrap();

        let due = j.undelivered(10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, job.id);
        assert!(j.mark_delivered(&job.id).await.unwrap());
        assert!(!j.mark_delivered(&job.id).await.unwrap(), "une seule fois");
        assert!(j.undelivered(10).await.unwrap().is_empty());
    }

    /// #204 : au redémarrage, un job en cours est perdu avec son processus. Il devient
    /// `failed` sans être relancé (décision 0012), et reste à livrer pour que le modèle
    /// cesse de l'attendre.
    #[tokio::test]
    async fn a_running_job_is_lost_on_restart_and_never_replayed() {
        let (store, j, clock) = jobs();
        let job = j.create(spec("s1", "shell_exec")).await.unwrap();
        let done = j.create(spec("s1", "shell_exec")).await.unwrap();
        j.finish(&done.id, TaskState::Completed, None)
            .await
            .unwrap();
        j.mark_delivered(&done.id).await.unwrap();

        let after = JobStore::new(store, Arc::new(clock));
        let lost = after.recover_on_boot().await.unwrap();
        assert_eq!(lost.len(), 1, "seul le job vivant est perdu");
        assert_eq!(lost[0].id, job.id);
        let got = after.get(&job.id).await.unwrap().unwrap();
        assert_eq!(got.state, TaskState::Failed);
        assert!(
            got.result.unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("redémarré")
        );
        assert_eq!(
            after.undelivered(10).await.unwrap().len(),
            1,
            "le job perdu se dit, il ne disparaît pas"
        );
    }

    /// #204 : les plafonds se comptent par session **et** pour tout le daemon.
    #[tokio::test]
    async fn caps_count_per_session_and_globally() {
        let (_s, j, _clock) = jobs();
        for _ in 0..2 {
            j.create(spec("s1", "shell_exec")).await.unwrap();
        }
        j.create(spec("s2", "sub_agent_spawn")).await.unwrap();
        assert_eq!(j.counts("s1").await.unwrap(), (2, 3));
        assert_eq!(j.counts("s2").await.unwrap(), (1, 3));
        assert_eq!(j.live().await.unwrap().len(), 3);
    }

    #[test]
    fn a_job_reports_its_age() {
        let job = ToolJob {
            id: "tj_1".into(),
            session_id: "s1".into(),
            run_id: None,
            turn_id: None,
            call_id: None,
            tool: "shell_exec".into(),
            request: json!({}),
            state: TaskState::Working,
            result: None,
            effect_id: None,
            delivered_at: None,
            created_at: "2026-09-23T10:00:00Z".into(),
            updated_at: "2026-09-23T10:00:00Z".into(),
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-23T10:02:30Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(job.age_s(now), 150);
        assert_eq!(job.view(now)["state"], "working");
    }

    // ------------------------------------------------------------ de bout en bout

    async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        (dir, d, p)
    }

    /// Une session de chat qui n'exige pas d'approbation : la carte n'est pas le sujet.
    async fn session(d: &Arc<Daemon>) -> String {
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.pin_model(&sid, Some("main")).await.unwrap();
        crate::approval_mode::set(
            &d.services,
            &sid,
            crate::approval_mode::ApprovalMode::parse("auto"),
        )
        .await
        .unwrap();
        sid
    }

    async fn claim(d: &Daemon) -> Turn {
        d.services.turns.claim("test").await.unwrap().unwrap()
    }

    /// Un tour qui lance `command` en arrière-plan puis répond.
    async fn background_turn(d: &Arc<Daemon>, p: &Arc<MockProvider>, sid: &str, command: &str) {
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command": command, "background": true}),
            }],
        ));
        p.reply("C'est parti, je te dis quand c'est fini.");
        d.enqueue_message(sid, "lance les tests", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(d).await;
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "le tour doit répondre sans attendre l'outil : {out:?}"
        );
        d.services.turns.complete(&turn).await.unwrap();
    }

    /// Attend qu'un job soit terminal, ou échoue au bout de dix secondes.
    async fn settled(d: &Arc<Daemon>, id: &str) -> ToolJob {
        for _ in 0..200 {
            let job = store(&d.services).get(id).await.unwrap().unwrap();
            if job.state.is_terminal() {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("job {id} toujours en cours");
    }

    /// #204 : `shell_exec background=true` sur un `sleep` — le tour répond tout de suite,
    /// et le résultat arrive plus tard dans la session par un tour `Nudge`.
    #[tokio::test]
    async fn a_background_shell_frees_the_turn_and_comes_back_as_a_nudge() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "sleep 0.3").await;

        let jobs = store(&d.services).of_session(&sid, false).await.unwrap();
        assert_eq!(jobs.len(), 1, "un job créé");
        let job = &jobs[0];
        assert_eq!(job.tool, "shell_exec");
        assert!(job.effect_id.is_some(), "le job porte son effet du ledger");

        // Ce que le modèle a lu : l'appel a rendu la main, pas la sortie de la commande.
        let results = tool_results(&d, &sid).await;
        assert!(
            results.iter().any(|r| r.contains("lancé en arrière-plan")),
            "{results:?}"
        );

        // Pendant ce temps, l'effet reste `dispatching` : rien ne contourne le ledger.
        let done = settled(&d, &job.id).await;
        assert_eq!(done.state, TaskState::Completed, "{done:?}");

        assert_eq!(deliver_due(&d).await.unwrap(), 1);
        assert_eq!(deliver_due(&d).await.unwrap(), 0, "livré une seule fois");
        let next = claim(&d).await;
        assert_eq!(next.kind, TurnKind::Nudge);
        let text = next.payload["text"].as_str().unwrap();
        assert!(
            text.contains(&job.id) && text.contains("sleep 0.3"),
            "{text}"
        );
    }

    /// #204 : la livraison ne dépend de personne. La boucle de fond rend le résultat
    /// dans la conversation sans qu'un tour ni une commande aient à la réclamer.
    #[tokio::test]
    async fn the_delivery_loop_brings_the_result_back_on_its_own() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        let loop_handle = tokio::spawn(deliver_loop(d.clone()));
        background_turn(&d, &p, &sid, "true").await;

        let mut delivered = None;
        for _ in 0..200 {
            if let Some(turn) = d.services.turns.claim("livraison").await.unwrap() {
                delivered = Some(turn);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        d.handle.shutdown();
        loop_handle.abort();
        let turn = delivered.expect("aucune relance livrée par la boucle");
        assert_eq!(turn.kind, TurnKind::Nudge);
        assert!(
            turn.payload["text"]
                .as_str()
                .unwrap()
                .contains("Résultat du job")
        );
    }

    /// #204 : un job n'existe que pour une conversation. Un sous-agent ou une étape de
    /// workflow qui demande l'arrière-plan exécute comme avant : sa session meurt avec son
    /// travail, la relance n'y trouverait personne, et le workflow a déjà son attente
    /// d'étape. Le flag est ignoré, l'appel rend son vrai résultat.
    #[tokio::test]
    async fn a_sub_agent_session_runs_the_call_instead_of_making_a_job() {
        let (_dir, d, _p) = daemon().await;
        let s = d.services.clone();
        let sub = s
            .sessions
            .create(penelope_kernel::session::SessionKind::SubAgent, None)
            .await
            .unwrap()
            .id
            .to_string();
        let effect = match s
            .effects
            .plan(
                penelope_kernel::effects::EffectSpec::new(
                    penelope_kernel::effects::EffectKind::Tool,
                    "shell_exec",
                    json!({"command": "true"}),
                )
                .session(&sub),
            )
            .await
            .unwrap()
        {
            penelope_kernel::effects::Planned::Fresh(id) => id,
            o => panic!("{o:?}"),
        };
        let call = ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": "true", "background": true}),
        };
        let out = maybe_spawn(
            &s,
            &Paniquant,
            JobRequest {
                session_id: &sub,
                run_id: None,
                turn_id: None,
                call: &call,
                tool: "shell_exec",
                effect: &effect,
            },
        )
        .await
        .unwrap();
        assert!(out.is_none(), "l'appel doit suivre le chemin ordinaire");
        assert!(store(&s).of_session(&sub, false).await.unwrap().is_empty());
    }

    /// #204 : un message du propriétaire pendant un job est traité sans attendre sa fin.
    #[tokio::test]
    async fn an_owner_message_is_answered_while_a_job_runs() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "sleep 30").await;
        let job = store(&d.services).of_session(&sid, true).await.unwrap()[0].clone();

        p.reply("Oui, j'écoute.");
        d.enqueue_message(&sid, "laisse tomber en fait", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        let out = tokio::time::timeout(std::time::Duration::from_secs(20), d.run_turn(&turn))
            .await
            .expect("le message ne doit pas attendre le job");
        d.services.turns.complete(&turn).await.unwrap();
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "{out:?}"
        );
        assert_eq!(
            store(&d.services)
                .get(&job.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            TaskState::Working,
            "le job tourne toujours"
        );
        d.services.jobs.cancel_all();
    }

    /// #204 : `/stop` annule les jobs de la session, le processus est tué, et le
    /// ramassage des orphelins ne trouve rien (issues #57 et #65).
    #[tokio::test]
    async fn stop_cancels_the_jobs_of_a_session_and_leaves_no_orphan() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "sleep 300").await;
        let job = store(&d.services).of_session(&sid, true).await.unwrap()[0].clone();

        assert_eq!(d.services.jobs.cancel_session(&sid), 1);
        let done = settled(&d, &job.id).await;
        assert_eq!(done.state, TaskState::Cancelled);

        // Le processus du job était bien déclaré au ramasse-miettes (#65) : c'est ce qui
        // fait que `reap_orphans` les connaît. Et il est mort : rien à tuer.
        use penelope_platform::ProcessHost;
        let pid_dir = d.services.platform.dirs.pid_dir();
        let pids = |dir: &std::path::Path| {
            std::fs::read_dir(dir)
                .map(|e| {
                    e.flatten()
                        .filter(|f| f.path().extension().is_some_and(|x| x == "pid"))
                        .count()
                })
                .unwrap_or(0)
        };
        assert!(pids(&pid_dir) >= 1, "le job n'a pas déclaré son processus");
        let orphans = d
            .services
            .platform
            .processes
            .reap_orphans(&pid_dir)
            .unwrap_or_default();
        assert!(
            orphans.is_empty(),
            "processus orphelins vivants : {orphans:?}"
        );
        assert_eq!(pids(&pid_dir), 0, "étiquettes PID non ramassées");
    }

    /// #204 : au plafond, le job suivant est refusé avec ce qu'il faut pour s'en sortir,
    /// et son effet est clos — il ne reste pas en attente d'un appel qui n'aura pas lieu.
    #[tokio::test]
    async fn the_job_over_the_cap_is_refused_with_what_to_do() {
        let (_dir, d, p) = daemon().await;
        d.publish_config("test", |c| {
            c.tools.jobs_per_session = 1;
            Ok(vec!["tools.jobs_per_session".into()])
        })
        .unwrap();
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "sleep 30").await;
        background_turn(&d, &p, &sid, "sleep 31").await;

        let results = tool_results(&d, &sid).await;
        let refus = results
            .iter()
            .find(|r| r.contains("Job refusé"))
            .unwrap_or_else(|| panic!("aucun refus : {results:?}"));
        assert!(refus.contains("jobs_per_session"), "{refus}");
        assert!(refus.contains("job_wait"), "il dit quoi faire : {refus}");
        assert_eq!(
            store(&d.services)
                .of_session(&sid, false)
                .await
                .unwrap()
                .len(),
            1,
            "le refus ne crée pas de ligne"
        );
        d.services.jobs.cancel_all();
    }

    /// #204 : un job terminé alors que la session est fermée n'est pas perdu ; il part à
    /// la réouverture.
    #[tokio::test]
    async fn a_job_finished_in_a_closed_session_is_delivered_when_it_reopens() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "true").await;
        let job = store(&d.services).of_session(&sid, false).await.unwrap()[0].clone();
        settled(&d, &job.id).await;
        let (s, pr, bus) = (&d.services, d.providers.clone(), &d.bus);
        crate::session_ops::close(s, pr, bus, &sid).await.unwrap();
        assert_eq!(
            deliver_due(&d).await.unwrap(),
            0,
            "rien dans une session fermée"
        );
        assert_eq!(store(&d.services).undelivered(10).await.unwrap().len(), 1);

        d.services.sessions.set_state(&sid, "active").await.unwrap();
        assert_eq!(deliver_due(&d).await.unwrap(), 1);
    }

    /// Exécuteur de test qui panique : #84 a montré qu'une tâche qui panique s'arrête en
    /// silence. Un job qui disparaîtrait ainsi laisserait son effet `dispatching` pour
    /// toujours, et une question au propriétaire au prochain démarrage.
    struct Paniquant;

    #[async_trait::async_trait]
    impl ToolExecutor for Paniquant {
        async fn execute(
            &self,
            _name: &str,
            _args: &Value,
        ) -> Result<ToolOutcome, penelope_tools::ToolError> {
            panic!("outil en panique");
        }
        fn detached(&self) -> Option<Arc<dyn ToolExecutor + Send + Sync>> {
            Some(Arc::new(Paniquant))
        }
    }

    /// #204 et #84 : un job dont la tâche panique est conclu `failed`, et son effet est
    /// clos — pas laissé `dispatching`.
    #[tokio::test]
    async fn a_panicking_job_is_closed_not_left_dispatching() {
        let (_dir, d, _p) = daemon().await;
        let s = d.services.clone();
        let sid = session(&d).await;
        let effect = match s
            .effects
            .plan(
                penelope_kernel::effects::EffectSpec::new(
                    penelope_kernel::effects::EffectKind::Tool,
                    "shell_exec",
                    json!({"command": "boum"}),
                )
                .session(&sid),
            )
            .await
            .unwrap()
        {
            penelope_kernel::effects::Planned::Fresh(id) => id,
            o => panic!("{o:?}"),
        };
        let call = ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": "boum", "background": true}),
        };
        let out = maybe_spawn(
            &s,
            &Paniquant,
            JobRequest {
                session_id: &sid,
                run_id: None,
                turn_id: None,
                call: &call,
                tool: "shell_exec",
                effect: &effect,
            },
        )
        .await
        .unwrap()
        .expect("un job");
        let id = out.value["job"].as_str().unwrap().to_string();

        for _ in 0..100 {
            if store(&s)
                .get(&id)
                .await
                .unwrap()
                .unwrap()
                .state
                .is_terminal()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let job = store(&s).get(&id).await.unwrap().unwrap();
        assert_eq!(job.state, TaskState::Failed, "{job:?}");
        assert!(s.jobs.is_empty(), "le registre est purgé");
        let state: String = s
            .store
            .read({
                let e = effect.as_str().to_string();
                move |c| {
                    Ok(
                        c.query_row("SELECT state FROM effects WHERE id = ?1", [&e], |r| {
                            r.get(0)
                        })?,
                    )
                }
            })
            .await
            .unwrap();
        assert_eq!(state, "failed", "l'effet ne reste pas `dispatching`");
    }

    /// #204 et #104 : lancer un job expose les outils de suivi à la session. Sans ça, le
    /// résultat dit « `job_status` donne son état » en désignant un outil que le modèle
    /// n'a pas sous la main.
    #[tokio::test]
    async fn starting_a_job_puts_the_follow_up_tools_in_reach() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        assert!(
            crate::tools_on_demand::exposed_for_turn(&d.services, &sid)
                .await
                .is_empty()
        );
        background_turn(&d, &p, &sid, "sleep 30").await;
        let exposed = crate::tools_on_demand::exposed_for_turn(&d.services, &sid).await;
        for t in ["job_status", "job_wait", "job_cancel", "job_list"] {
            assert!(exposed.contains(&t.to_string()), "{t} absent : {exposed:?}");
        }
        d.services.jobs.cancel_all();
    }

    /// #204 et §6.6 : le résultat d'un job revient dans l'épisode en cours, il n'en ouvre
    /// pas un nouveau. Une relance n'est pas un message du propriétaire : elle ne doit ni
    /// paraître un changement de sujet, ni clore l'épisode que le propriétaire poursuit.
    #[tokio::test]
    async fn a_delivered_result_joins_the_episode_it_does_not_open_one() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "true").await;
        let job = store(&d.services).of_session(&sid, false).await.unwrap()[0].clone();
        settled(&d, &job.id).await;
        let before = d.services.sessions.require(&sid).await.unwrap().episode_seq;

        assert_eq!(deliver_due(&d).await.unwrap(), 1);
        p.reply("Les tests passent.");
        let nudge = claim(&d).await;
        assert_eq!(nudge.kind, TurnKind::Nudge);
        d.run_turn(&nudge).await;
        d.services.turns.complete(&nudge).await.unwrap();

        assert_eq!(
            d.services.sessions.require(&sid).await.unwrap().episode_seq,
            before,
            "une relance ne déplace pas la frontière d'épisode"
        );
    }

    /// #204 et #83 : un redémarrage pendant un job laisse son effet `dispatching`. Le job
    /// devient `failed` sans être relancé (décision 0012), l'effet devient `unknown`, et
    /// la carte « C'est fait / Relancer / Ignorer » part **une** seule fois.
    #[tokio::test]
    async fn a_restart_during_a_job_fails_it_and_asks_the_owner_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Arc::new(
            crate::runtime::Services::for_tests(root.clone(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "sleep 300").await;
        let job = store(&d.services).of_session(&sid, true).await.unwrap()[0].clone();
        let effect = job.effect_id.clone().unwrap();

        // Redémarrage : un autre processus reprend la même base.
        let after = Arc::new(Daemon::from_services(Arc::new(
            crate::runtime::Services::for_tests(root, clock)
                .await
                .unwrap(),
        )));
        let report = after.recover().await.unwrap();
        assert_eq!(report.tool_jobs_lost, 1, "{report:?}");

        let lost = store(&after.services).get(&job.id).await.unwrap().unwrap();
        assert_eq!(lost.state, TaskState::Failed);
        assert!(
            lost.result.unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("redémarré")
        );

        let cards = after.services.approvals.pending(10).await.unwrap();
        let mine: Vec<_> = cards
            .iter()
            .filter(|a| a.payload["effect_id"] == effect)
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "une carte pour l'effet incertain : {cards:?}"
        );
        assert!(mine[0].payload["request"].to_string().contains("sleep 300"));

        // Un second démarrage ne redemande pas (#83).
        after.recover().await.unwrap();
        assert_eq!(
            after
                .services
                .approvals
                .pending(10)
                .await
                .unwrap()
                .iter()
                .filter(|a| a.payload["effect_id"] == effect)
                .count(),
            1
        );
        d.services.jobs.cancel_all();
    }

    /// #204 : `job_status`, `job_wait` et `job_cancel` rendent ce que le modèle attend, et
    /// `job_wait` est borné — il rend l'état courant plutôt que de bloquer le tour.
    #[tokio::test]
    async fn the_model_can_follow_and_cancel_a_job() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        background_turn(&d, &p, &sid, "sleep 300").await;
        let job = store(&d.services).of_session(&sid, true).await.unwrap()[0].clone();
        let s = &d.services;

        let listed = tool(s, &sid, "job_list", &json!({})).await.unwrap();
        assert_eq!(listed["running"], 1);

        let status = tool(s, &sid, "job_status", &json!({"job": job.id}))
            .await
            .unwrap();
        assert_eq!(status["state"], "working");

        // Borné : l'attente rend la main sans que le job soit fini.
        let waited = tool(
            s,
            &sid,
            "job_wait",
            &json!({"job": job.id, "timeout_ms": 1000}),
        )
        .await
        .unwrap();
        assert_eq!(waited["state"], "working");
        assert_eq!(waited["waited"], true);

        let cancelled = tool(s, &sid, "job_cancel", &json!({"job": job.id}))
            .await
            .unwrap();
        assert_eq!(cancelled["cancelled"], true);
        assert_eq!(settled(&d, &job.id).await.state, TaskState::Cancelled);

        let unknown = tool(s, &sid, "job_status", &json!({"job": "tj_inconnu"})).await;
        assert!(unknown.is_err());
    }

    /// #204 et #19 : `sub_agent_spawn` est précisément l'outil que le harnais recommande
    /// pour les travaux longs (le nudge de #19) — et il était attendu comme les autres.
    /// En job, le tour répond tout de suite et la conclusion du sous-agent revient seule.
    #[tokio::test]
    async fn a_sub_agent_can_run_as_a_job_and_report_back() {
        let (_dir, d, p) = daemon().await;
        d.hooks
            .set_orchestrator(Arc::new(crate::workflow::WorkflowOrchestrator {
                daemon: d.clone(),
            }));
        let sid = session(&d).await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "sub_agent_spawn".into(),
                arguments: json!({
                    "kind": "general",
                    "prompt": "relis le dépôt et conclus",
                    "background": true
                }),
            }],
        ));
        p.reply("Je lui ai confié ça.");
        // Ce que le sous-agent répondra, plus tard, hors du tour.
        p.reply("Conclusion du sous-agent : rien à signaler.");
        d.enqueue_message(&sid, "confie ça à un sous-agent", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "{out:?}"
        );
        d.services.turns.complete(&turn).await.unwrap();

        let job = store(&d.services).of_session(&sid, false).await.unwrap()[0].clone();
        assert_eq!(job.tool, "sub_agent_spawn");
        let done = settled(&d, &job.id).await;
        assert_eq!(done.state, TaskState::Completed, "{done:?}");

        assert_eq!(deliver_due(&d).await.unwrap(), 1);
        let nudge = claim(&d).await;
        assert_eq!(nudge.kind, TurnKind::Nudge);
        let text = nudge.payload["text"].as_str().unwrap();
        assert!(text.contains("sub_agent_spawn"), "{text}");
        assert!(
            text.contains("relis le dépôt"),
            "de quoi il est le résultat : {text}"
        );
    }

    /// #204 et #104 : `tool_call` enveloppe l'appel dans `{name, args}`. La demande
    /// d'arrière-plan est dans l'enveloppe : sans la lire là, un `background: true` passé
    /// par ce chemin partait en silence dans le tour bloquant.
    #[tokio::test]
    async fn the_background_flag_is_read_through_tool_call() {
        let (_dir, d, p) = daemon().await;
        let sid = session(&d).await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "tool_call".into(),
                arguments: json!({
                    "name": "shell_exec",
                    "args": {"command": "sleep 30", "background": true}
                }),
            }],
        ));
        p.reply("C'est lancé.");
        d.enqueue_message(&sid, "lance", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "{out:?}"
        );
        let jobs = store(&d.services).of_session(&sid, true).await.unwrap();
        assert_eq!(jobs.len(), 1, "le drapeau doit traverser `tool_call`");
        assert_eq!(jobs[0].tool, "shell_exec");
        d.services.jobs.cancel_all();
    }

    /// #204 : un `timeout_ms` au-delà de `tools.background_after` propose l'arrière-plan
    /// au lieu de le prendre : c'est le modèle qui décide.
    #[test]
    fn a_long_timeout_is_offered_the_background_not_forced_into_it() {
        let cfg = penelope_kernel::config::Config::sample(1);
        let hint = background_hint(
            &cfg,
            "shell_exec",
            &json!({"command": "x", "timeout_ms": 900_000}),
        )
        .expect("au-delà du seuil");
        assert!(
            hint.contains("background: true") && hint.contains("900"),
            "{hint}"
        );
        assert!(
            background_hint(
                &cfg,
                "shell_exec",
                &json!({"command": "x", "timeout_ms": 5_000})
            )
            .is_none()
        );
        assert!(
            background_hint(
                &cfg,
                "shell_exec",
                &json!({"command": "x", "timeout_ms": 900_000, "background": true})
            )
            .is_none(),
            "déjà en arrière-plan"
        );
        assert!(background_hint(&cfg, "fs_read", &json!({"timeout_ms": 900_000})).is_none());
    }

    async fn tool_results(d: &Daemon, sid: &str) -> Vec<String> {
        d.services
            .context
            .history
            .load(sid, 0)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.message.role == penelope_llm::types::Role::Tool)
            .map(|e| e.message.text())
            .collect()
    }
}
