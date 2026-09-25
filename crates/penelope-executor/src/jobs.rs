//! Jobs d'outils natifs (issue #204) : le magasin (`tool_jobs`) et les outils de suivi
//! `job_*`. Le lancement hors du tour et la livraison vivent dans le daemon
//! (`tool_jobs.rs`), qui compose la boucle ; ce qu'en lisent l'exécuteur et `self_status`
//! est ici (épopée #208, T24).

use penelope_app::services::Services;
use penelope_kernel::clock::SharedClock;
use penelope_kernel::config::Config;
use penelope_mcp::tasks::TaskState;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

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
    /// job devient `failed`, **et son effet aussi**, dans la même transaction et avant
    /// que le ledger ne passe ses `dispatching` en `unknown`. Un job est observable (son
    /// processus est mort avec le daemon) : sa complétion n'est pas inconnaissable, la
    /// carte `effect_unknown` de #83 n'a pas à la poser. La livraison dira au modèle que
    /// le job est mort, pour qu'il cesse de l'attendre et propose la reprise lui-même.
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
                        "UPDATE effects SET state = 'failed', error = ?1, updated_at = ?2
                         WHERE state = 'dispatching'
                           AND id IN (SELECT effect_id FROM tool_jobs WHERE {LIVE})"
                    ),
                    params![LOST_ON_BOOT, ts],
                )?;
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
