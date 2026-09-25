//! Compaction niveau 3 (§5.4) : résumés LCM écrits par le rôle `compaction`.
//!
//! Quatre déclencheurs : la fin d'un tour dont la projection estimée **ou le prompt
//! réellement facturé** atteint le seuil moins la marge (tâche de fond), la reprise d'une
//! session froide au-delà de ce seuil (avant l'appel au modèle), `/compact` (lève le
//! cooldown), et le dépassement de fenêtre prouvé par le provider (une tentative, publiée
//! aussitôt). Le résumé de fond se calcule sans bloquer la conversation et se publie à une
//! frontière de tour. Chaque décision laisse un événement : `context.compaction_requested`,
//! `context.compaction_skipped` (avec sa raison), `context.compacted` (issue #40).

use crate::helpers::last_model_key;
use crate::runtime::{Daemon, Services};
use penelope_context::{CompactionParams, Cooldown, SummaryJob};
use penelope_kernel::event::EventDraft;
use penelope_llm::Provider;
use penelope_llm::catalog::strip_provider;
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Lots résumés au plus par compaction ; la suivante reprend le reste.
const MAX_PASSES: usize = 12;
/// Échecs consécutifs du résumeur au-delà desquels la compaction se fait sans modèle
/// (issue #131).
const MECHANICAL_AFTER: u32 = 3;

/// Délai du résumeur, proportionné au lot : 120 s, plus une seconde par millier de tokens
/// de source, 420 s au plus. Au-delà, le lot est en échec (issue #131).
fn summary_timeout(tokens_src: u64) -> Duration {
    Duration::from_secs((120 + tokens_src / 1_000).min(420))
}
/// Attente maximale d'une compaction concurrente, sur dépassement de fenêtre.
const OVERFLOW_WAIT: Duration = Duration::from_secs(200);

/// Ce qui déclenche une compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Fin d'un tour au-delà du seuil moins la marge.
    Background,
    /// `/compact`, `penelope session compact` : force un lot et lève le cooldown.
    Manual,
    /// Dépassement de fenêtre prouvé par le provider, en plein tour.
    Overflow,
    /// Reprise d'une session froide au-delà du seuil de fond : avant l'appel au modèle,
    /// publiée aussitôt (issue #40).
    Resume,
}

impl Trigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Trigger::Background => "background",
            Trigger::Manual => "manual",
            Trigger::Overflow => "overflow",
            Trigger::Resume => "resume",
        }
    }

    /// Publié aussitôt, sans attendre la fin du tour.
    fn publishes_now(&self) -> bool {
        matches!(self, Trigger::Overflow | Trigger::Resume)
    }
}

/// État partagé du daemon : compactions en cours, sessions à compacter après leur tour.
#[derive(Default)]
pub struct State {
    /// Canal du propriétaire, reçu de la composition : une compaction sans modèle se dit.
    pub messenger: crate::ports::Slot<dyn crate::executor::Messenger>,
    running: Mutex<HashSet<String>>,
    /// Session → tour qui a demandé la compaction (le coût lui est attribué).
    wanted: Mutex<HashMap<String, Option<String>>>,
}

impl State {
    pub fn with_messenger(messenger: crate::ports::Slot<dyn crate::executor::Messenger>) -> State {
        State {
            messenger,
            ..State::default()
        }
    }

    /// Demande une compaction de fond à la fin du tour en cours.
    pub fn request(&self, session_id: &str, turn_id: Option<String>) {
        if let Ok(mut g) = self.wanted.lock() {
            g.insert(session_id.to_string(), turn_id);
        }
    }

    fn take_request(&self, session_id: &str) -> Option<Option<String>> {
        self.wanted
            .lock()
            .ok()
            .and_then(|mut g| g.remove(session_id))
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.running
            .lock()
            .map(|g| g.contains(session_id))
            .unwrap_or(false)
    }

    fn claim(&self, session_id: &str) -> Option<Claim<'_>> {
        let mut g = self.running.lock().ok()?;
        g.insert(session_id.to_string()).then(|| Claim {
            state: self,
            session_id: session_id.to_string(),
        })
    }
}

/// Une compaction à la fois par session : libérée en fin de portée, même sur erreur.
struct Claim<'a> {
    state: &'a State,
    session_id: String,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if let Ok(mut g) = self.state.running.lock() {
            g.remove(&self.session_id);
        }
    }
}

/// Bilan d'une compaction.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub session: String,
    pub trigger: String,
    /// Lots résumés et publiés.
    pub published: usize,
    /// Un résumé est prêt et attend la fin du tour en cours.
    pub deferred: bool,
    /// Messages couverts par les lots publiés.
    pub messages: i64,
    pub node: Option<String>,
    /// Dernier message couvert par le résumé actif.
    pub covered_to: i64,
    /// Tokens des messages résumés.
    pub tokens_src: u64,
    /// Tokens du résumé qui les remplace.
    pub tokens_summary: u64,
    /// Lots restant à résumer : la prochaine compaction les reprend.
    pub remaining_batches: usize,
    pub model: Option<String>,
    pub cost_usd: f64,
    /// Pourquoi rien n'a été fait.
    pub skipped: Option<String>,
    /// Reprises faites pour obtenir le résumé (demande plus courte, alias de repli,
    /// compaction sans modèle), dans l'ordre (issue #131).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
}

/// Résumé calculé, en attente d'une frontière de tour. Persisté : un redémarrage ne
/// perd pas un appel déjà payé.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    job: SummaryJob,
    summary: Value,
    model: String,
}

fn cooldown_key(session_id: &str) -> String {
    format!("compaction.cooldown.{session_id}")
}

fn pending_key(session_id: &str) -> String {
    format!("compaction.pending.{session_id}")
}

/// Fin de tour : publie le résumé en attente, puis lance la compaction de fond si le
/// tour l'a demandée.
pub async fn after_turn(d: &Arc<Daemon>, session_id: &str) {
    if let Err(e) = publish_pending(d, session_id).await {
        tracing::warn!(session = %session_id, error = %e, "résumé en attente non publié");
    }
    if let Some(turn_id) = d.compaction.take_request(session_id) {
        spawn(
            d.clone(),
            session_id.to_string(),
            Trigger::Background,
            turn_id,
        );
    }
}

/// Lance une compaction sans l'attendre.
pub fn spawn(d: Arc<Daemon>, session_id: String, trigger: Trigger, turn_id: Option<String>) {
    tokio::spawn(async move {
        match compact(&d, &session_id, trigger, turn_id.as_deref()).await {
            Ok(r) if r.published > 0 || r.deferred => tracing::info!(
                session = %session_id,
                trigger = trigger.as_str(),
                messages = r.messages,
                tokens_src = r.tokens_src,
                tokens_summary = r.tokens_summary,
                deferred = r.deferred,
                "contexte compacté"
            ),
            Ok(r) => tracing::info!(
                session = %session_id,
                trigger = trigger.as_str(),
                skipped = ?r.skipped,
                "compaction non faite"
            ),
            Err(e) => tracing::warn!(session = %session_id, error = %e, "compaction en échec"),
        }
    });
}

/// Publie le résumé qui attendait une frontière de tour, s'il y en a un.
pub async fn publish_pending(d: &Arc<Daemon>, session_id: &str) -> anyhow::Result<Option<Report>> {
    // Une compaction en cours publie elle-même ce qu'elle a préparé.
    let Some(_claim) = d.compaction.claim(session_id) else {
        return Ok(None);
    };
    let Some(pending) = load_pending(&d.services, session_id).await? else {
        return Ok(None);
    };
    let mut report = Report {
        session: session_id.to_string(),
        trigger: Trigger::Background.as_str().into(),
        ..Default::default()
    };
    publish_saved(d, session_id, pending, Trigger::Background, &mut report).await;
    Ok(Some(report))
}

/// Compacte une session : prépare les lots, les fait résumer, les publie. Un saut laisse
/// l'événement `context.compaction_skipped` avec sa raison (issue #40).
pub async fn compact(
    d: &Arc<Daemon>,
    session_id: &str,
    trigger: Trigger,
    turn_id: Option<&str>,
) -> anyhow::Result<Report> {
    let report = compact_inner(d, session_id, trigger, turn_id).await?;
    if let Some(reason) = report
        .skipped
        .as_ref()
        .filter(|_| report.published == 0 && !report.deferred)
    {
        tracing::info!(
            session = %session_id,
            trigger = trigger.as_str(),
            reason = %reason,
            "compaction sautée"
        );
        let _ = d
            .services
            .events
            .append(
                EventDraft::new(
                    "context.compaction_skipped",
                    json!({"trigger": trigger.as_str(), "reason": reason}),
                )
                .session(session_id),
            )
            .await;
    }
    Ok(report)
}

/// Demande une compaction de fond pour la fin du tour, en le disant (issue #40).
pub async fn request(
    d: &Arc<Daemon>,
    session_id: &str,
    turn_id: Option<String>,
    trigger: Trigger,
    details: Value,
) {
    tracing::info!(
        session = %session_id,
        trigger = trigger.as_str(),
        details = %details,
        "compaction demandée"
    );
    let mut payload = json!({"trigger": trigger.as_str()});
    if let (Some(p), Some(extra)) = (payload.as_object_mut(), details.as_object()) {
        p.extend(extra.clone());
    }
    let _ = d
        .services
        .events
        .append(EventDraft::new("context.compaction_requested", payload).session(session_id))
        .await;
    if trigger == Trigger::Background {
        d.compaction.request(session_id, turn_id);
    }
}

/// Prompt réellement facturé au dernier appel de conversation d'une session, et son
/// instant (millisecondes).
pub async fn last_prompt(s: &Services, session_id: &str) -> Option<(u64, i64)> {
    use penelope_store::rusqlite::OptionalExtension;
    let sid = session_id.to_string();
    let row: Option<(i64, String)> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT prompt, ts FROM usage
                 WHERE session_id = ?1 AND COALESCE(role, 'chat') = 'chat'
                 ORDER BY ts DESC, rowid DESC LIMIT 1",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
        .await
        .ok()
        .flatten();
    row.map(|(prompt, ts)| {
        let ms = chrono::DateTime::parse_from_rfc3339(&ts)
            .map(|t| t.timestamp_millis())
            .unwrap_or(0);
        (prompt.max(0) as u64, ms)
    })
}

/// Seuil de la compaction de fond pour le modèle de conversation d'une session.
pub fn background_threshold(s: &Services, model_id: &str) -> u64 {
    let cfg = s.config.config();
    let params = CompactionParams::from_config(
        &cfg,
        s.catalog.window_of(strip_provider(model_id)),
        model_id,
    );
    params.background_threshold_tokens(params.background_margin)
}

/// Fin d'un tour : la compaction de fond est demandée si la projection estimée l'a
/// réclamée, ou si le prompt réellement facturé dépasse le seuil (issue #40).
pub async fn after_answer(
    d: &Arc<Daemon>,
    session_id: &str,
    model_id: &str,
    turn_id: Option<String>,
    estimated: bool,
) {
    let threshold = background_threshold(&d.services, model_id);
    let real = last_prompt(&d.services, session_id).await.map(|(p, _)| p);
    let over_real = real.is_some_and(|p| p >= threshold);
    if !estimated && !over_real {
        return;
    }
    request(
        d,
        session_id,
        turn_id,
        Trigger::Background,
        json!({
            "reason": if over_real { "taille réelle" } else { "estimation" },
            "prompt_tokens": real,
            "threshold": threshold,
        }),
    )
    .await;
}

/// Début d'un tour : une session froide (cache perdu) dont le dernier prompt dépasse le
/// seuil de fond est compactée **avant** l'appel au modèle (issue #40).
pub async fn before_turn(d: &Arc<Daemon>, session_id: &str, model_id: &str, turn_id: Option<&str>) {
    let s = &d.services;
    let threshold = background_threshold(s, model_id);
    let now = s.clock.now_ms();
    let last = last_prompt(s, session_id).await;
    let (size, reason) = match last {
        Some((prompt, ts)) if now - ts > crate::cache_audit::CACHE_TTL_MS => {
            (prompt, "reprise après une pause")
        }
        Some(_) => return,
        // Premier tour d'un fork : il hérite du contexte de sa session mère, sans cache.
        None => {
            let Ok(Some(parent)) = s
                .sessions
                .get(session_id)
                .await
                .map(|x| x.and_then(|x| x.parent_id))
            else {
                return;
            };
            let size = match last_prompt(s, &parent).await {
                Some((prompt, _)) => prompt,
                None => match s.context.history.load(session_id, 0).await {
                    Ok(entries) => entries
                        .iter()
                        .filter(|e| !e.compacted)
                        .map(|e| e.tokens)
                        .sum::<u64>(),
                    Err(_) => return,
                },
            };
            (size, "premier tour d'un fork")
        }
    };
    if size < threshold {
        return;
    }
    request(
        d,
        session_id,
        turn_id.map(String::from),
        Trigger::Resume,
        json!({"reason": reason, "prompt_tokens": size, "threshold": threshold}),
    )
    .await;
    match compact(d, session_id, Trigger::Resume, turn_id).await {
        Ok(r) if r.published > 0 => tracing::info!(
            session = %session_id,
            messages = r.messages,
            tokens_src = r.tokens_src,
            tokens_summary = r.tokens_summary,
            "contexte compacté avant la reprise"
        ),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(session = %session_id, error = %e, "compaction de reprise en échec")
        }
    }
}

/// Dépense du jour en résumés.
async fn compaction_spent_today(s: &Services) -> f64 {
    let day = s.budget.today();
    s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(cost_usd), 0) FROM usage WHERE day = ?1 AND role = 'compaction'",
                [day],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap_or(0.0)
}

async fn compact_inner(
    d: &Arc<Daemon>,
    session_id: &str,
    trigger: Trigger,
    turn_id: Option<&str>,
) -> anyhow::Result<Report> {
    let s = &d.services;
    let mut report = Report {
        session: session_id.to_string(),
        trigger: trigger.as_str().into(),
        ..Default::default()
    };

    let _claim = match d.compaction.claim(session_id) {
        Some(c) => c,
        // Sur dépassement ou reprise, le résumé de fond déjà en route est la meilleure
        // chance.
        None if trigger.publishes_now() => match wait_claim(d, session_id).await {
            Some(c) => c,
            None => {
                report.skipped = Some("une autre compaction ne se termine pas".into());
                return Ok(report);
            }
        },
        None => {
            report.skipped = Some("une compaction est déjà en cours pour cette session".into());
            return Ok(report);
        }
    };

    // Un résumé prêt passe d'abord : le lot suivant se prépare sur l'état publié.
    if let Some(pending) = load_pending(&d.services, session_id).await? {
        if !trigger.publishes_now() && d.bus.is_active(session_id) {
            report.deferred = true;
            report.skipped = Some("un résumé attend déjà la fin du tour en cours".into());
            return Ok(report);
        }
        publish_saved(d, session_id, pending, trigger, &mut report).await;
    }

    let cfg = s.config.config();
    let mut cooldown = load_cooldown(&d.services, session_id).await;
    let now = s.clock.now_ms();
    match trigger {
        Trigger::Background | Trigger::Resume if cooldown.is_active(now) => {
            report.skipped = Some(format!(
                "échec récent, nouvel essai possible dans {} s",
                (cooldown.until_ms - now) / 1000
            ));
            return Ok(report);
        }
        Trigger::Manual | Trigger::Overflow if cooldown.failures > 0 => {
            cooldown.clear();
            save_cooldown(&d.services, session_id, &cooldown).await;
        }
        _ => {}
    }
    // Un résumé coûte peu et allège chaque appel suivant : les plafonds de session et de
    // run ne l'arrêtent pas. Le plafond du jour reste un garde-fou, au-delà d'une réserve
    // propre aux résumés (issue #40).
    if matches!(trigger, Trigger::Background | Trigger::Resume) {
        let statuses = s.budget.status(&cfg.budget, None, None).await?;
        let daily_exceeded = statuses
            .iter()
            .any(|b| b.exceeded && b.scope == penelope_kernel::budget::BudgetScope::Daily);
        if daily_exceeded {
            let spent = compaction_spent_today(s).await;
            if spent >= cfg.budget.compaction_reserve_usd {
                report.skipped = Some(format!(
                    "budget du jour atteint et réserve des résumés épuisée ({spent:.2} $ sur \
                     {:.2} $, `budget.compaction_reserve_usd`)",
                    cfg.budget.compaction_reserve_usd
                ));
                return Ok(report);
            }
        }
    }

    let alias = cfg.role_alias("compaction");
    let model = cfg
        .alias_model(&alias)
        .ok_or_else(|| anyhow::anyhow!("aucun modèle pour l'alias `{alias}` du rôle `compaction`"))?
        .to_string();
    let model = crate::codex_scope::background(&d.services, &model, "compaction").await;
    let provider = d.provider_for(&model).await.map_err(anyhow::Error::msg)?;
    let conversation = conversation_model(d, session_id).await;
    let params = CompactionParams::from_config(
        &cfg,
        s.catalog.window_of(strip_provider(&conversation)),
        &conversation,
    );
    let window = s.catalog.window_of(strip_provider(&model));

    for pass in 0..MAX_PASSES {
        let force = pass == 0 && matches!(trigger, Trigger::Manual | Trigger::Overflow);
        let Some(job) = s
            .context
            .prepare_summary(session_id, &params, window, &model, force)
            .await?
        else {
            break;
        };
        report.remaining_batches = job.remaining_batches();

        let attempt = Attempt {
            session_id,
            params: &params,
            window,
            force,
            turn_id,
        };
        let (job, summary, used) =
            match summarise_or_recover(d, provider.as_ref(), &model, job, &attempt, &mut report)
                .await
            {
                Ok(done) => done,
                Err(failed) => {
                    let (job, failure) = *failed;
                    let error = failure.message;
                    cooldown.record_failure(s.clock.now_ms(), &cfg.context.cooldown_ms);
                    // Trois échecs de suite : une compaction franche sans modèle plutôt qu'un
                    // quatrième refroidissement, et le propriétaire le sait (issue #131).
                    if cooldown.failures >= MECHANICAL_AFTER {
                        cooldown.clear();
                        save_cooldown(&d.services, session_id, &cooldown).await;
                        let pending = Pending {
                            summary: mechanical_summary(&job),
                            job,
                            model: MECHANICAL_MODEL.into(),
                        };
                        report.recovered.push(format!(
                            "compaction sans modèle après {MECHANICAL_AFTER} échecs ({error})"
                        ));
                        publish(d, session_id, &pending, trigger, &mut report).await?;
                        tell_mechanical(d, session_id, &model, &error, &report).await;
                        break;
                    }
                    save_cooldown(&d.services, session_id, &cooldown).await;
                    let retry_in_s = (cooldown.until_ms - s.clock.now_ms()).max(0) / 1000;
                    let _ = s
                        .events
                        .append(
                            EventDraft::new(
                                "context.compaction_failed",
                                json!({
                                    "error": error,
                                    "trigger": trigger.as_str(),
                                    "failures": cooldown.failures,
                                    "retry_in_s": retry_in_s,
                                    "model": model,
                                }),
                            )
                            .session(session_id),
                        )
                        .await;
                    penelope_observe::metrics::counter_inc(
                        "penelope_compactions_total",
                        &[("trigger", trigger.as_str()), ("outcome", "failed")],
                        1.0,
                    );
                    if report.published == 0 {
                        anyhow::bail!(
                            "le résumé a échoué ({error}), nouvel essai dans {retry_in_s} s"
                        );
                    }
                    break;
                }
            };

        let pending = Pending {
            job,
            summary,
            model: used.clone(),
        };
        if !trigger.publishes_now() && d.bus.is_active(session_id) {
            save_pending(&d.services, session_id, &pending).await?;
            report.deferred = true;
            report.model = Some(used.clone());
            // Le tour a pu se terminer pendant l'écriture : sa frontière est passée.
            if !d.bus.is_active(session_id)
                && let Some(p) = load_pending(&d.services, session_id).await?
            {
                report.deferred = false;
                publish_saved(d, session_id, p, trigger, &mut report).await;
            }
            break;
        }
        let more = pending.job.remaining_batches() > 0;
        publish(d, session_id, &pending, trigger, &mut report).await?;
        if !more {
            break;
        }
    }

    if report.published > 0 && cooldown.failures > 0 {
        cooldown.clear();
        save_cooldown(&d.services, session_id, &cooldown).await;
    }
    if report.published == 0 && !report.deferred && report.skipped.is_none() {
        report.skipped =
            Some("rien à compacter : la conversation tient dans la queue verbatim".into());
    }
    Ok(report)
}

async fn wait_claim<'a>(d: &'a Arc<Daemon>, session_id: &str) -> Option<Claim<'a>> {
    let deadline = tokio::time::Instant::now() + OVERFLOW_WAIT;
    loop {
        if let Some(c) = d.compaction.claim(session_id) {
            return Some(c);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Fait résumer un lot par le modèle du rôle `compaction` et valide sa sortie.
/// Échec d'un résumé : passager (délai, 5xx, 429), ou réponse inutilisable.
struct SummaryFailure {
    message: String,
    passing: bool,
}

impl SummaryFailure {
    fn passing(message: String) -> Self {
        SummaryFailure {
            message,
            passing: true,
        }
    }
}

/// Ce qu'il faut pour préparer un lot plus court.
struct Attempt<'a> {
    session_id: &'a str,
    params: &'a CompactionParams,
    window: u64,
    force: bool,
    turn_id: Option<&'a str>,
}

/// Modèle inscrit sur un résumé fait sans modèle.
const MECHANICAL_MODEL: &str = "sans modèle";

/// Un lot résumé, avec ses reprises (issue #131) : le lot tel quel ; sur un échec
/// passager, le début du même lot en trois fois plus court ; puis, s'il est déclaré,
/// l'alias de repli du résumeur (`models.routing.fallback`), si la réserve des résumés
/// couvre son coût estimé. Rend le lot effectivement résumé et le modèle qui l'a fait,
/// ou le dernier lot essayé et la raison de l'échec.
async fn summarise_or_recover(
    d: &Arc<Daemon>,
    provider: &dyn Provider,
    model: &str,
    job: SummaryJob,
    at: &Attempt<'_>,
    report: &mut Report,
) -> Result<(SummaryJob, Value, String), Box<(SummaryJob, SummaryFailure)>> {
    let s = &d.services;
    let first = match summarise(d, provider, model, &job, at.turn_id).await {
        Ok((summary, cost)) => {
            report.cost_usd += cost;
            return Ok((job, summary, model.to_string()));
        }
        Err(f) => f,
    };
    if !first.passing {
        return Err(Box::new((job, first)));
    }
    let mut job = job;
    let mut failure = first;
    if let Ok(Some(short)) = s
        .context
        .prepare_summary_capped(
            at.session_id,
            at.params,
            at.window,
            model,
            at.force,
            Some((job.tokens_src / 3).max(1)),
        )
        .await
        && short.to_seq < job.to_seq
    {
        report.recovered.push(format!(
            "demande plus courte ({} tokens au lieu de {}) après : {}",
            short.tokens_src, job.tokens_src, failure.message
        ));
        job = short;
        match summarise(d, provider, model, &job, at.turn_id).await {
            Ok((summary, cost)) => {
                report.cost_usd += cost;
                return Ok((job, summary, model.to_string()));
            }
            Err(f) => failure = f,
        }
    }
    if !failure.passing {
        return Err(Box::new((job, failure)));
    }
    let cfg = s.config.config();
    let Some(fallback) = cfg
        .models
        .routing
        .fallback
        .get(&cfg.role_alias("compaction"))
        .and_then(|v| v.first())
        .and_then(|a| cfg.alias_model(a))
        .map(String::from)
        .filter(|m| m != model)
    else {
        return Err(Box::new((job, failure)));
    };
    // Un repli coûte plus cher que le résumeur : il reste dans la réserve des résumés.
    let estimated = s
        .catalog
        .get(strip_provider(&fallback))
        .map(|i| (job.tokens_src + 2_000) as f64 * i.price_prompt + 8_000.0 * i.price_completion)
        .unwrap_or(f64::INFINITY);
    let spent = compaction_spent_today(s).await;
    if spent + estimated > cfg.budget.compaction_reserve_usd {
        report.recovered.push(format!(
            "repli `{fallback}` écarté : ~{estimated:.2} $ estimés, {spent:.2} $ déjà dépensés \
             sur {:.2} $ de réserve (`budget.compaction_reserve_usd`)",
            cfg.budget.compaction_reserve_usd
        ));
        return Err(Box::new((job, failure)));
    }
    let fallback = crate::codex_scope::background(&d.services, &fallback, "compaction").await;
    let Ok(fb) = d.provider_for(&fallback).await else {
        return Err(Box::new((job, failure)));
    };
    report.recovered.push(format!(
        "repli sur `{fallback}` après : {}",
        failure.message
    ));
    match summarise(d, fb.as_ref(), &fallback, &job, at.turn_id).await {
        Ok((summary, cost)) => {
            report.cost_usd += cost;
            Ok((job, summary, fallback))
        }
        Err(f) => Err(Box::new((job, f))),
    }
}

/// Résumé fait sans modèle, quand le résumeur a échoué trop souvent : il ne prétend rien
/// comprendre. Les derniers messages du propriétaire du lot (dans le budget habituel) et
/// les ancres sont gardés tels quels par le rendu du nœud ; le résumé précédent est
/// repris.
fn mechanical_summary(job: &SummaryJob) -> Value {
    let previous = job
        .previous_summary
        .as_deref()
        .map(penelope_context::compaction::summary_sections_only)
        .unwrap_or_default();
    json!({
        "objectif": "Compaction sans modèle : le résumeur n'a pas répondu à temps plusieurs \
                     fois de suite. Ce qui suit n'est pas un résumé : les derniers messages du \
                     propriétaire du passage compacté et ses ancres (chemins, identifiants) \
                     sont gardés tels quels, le reste se relit avec `history_expand`.",
        "fait": previous.chars().take(3_500).collect::<String>(),
        "en_cours": format!(
            "{} messages compactés sans être relus (séquences {} à {}) : les relire au besoin \
             avec `history_expand`.",
            job.messages(),
            job.chunk_from_seq,
            job.to_seq
        ),
        "prochaines_etapes": "",
    })
}

/// Dit au propriétaire qu'une session s'est compactée sans modèle, avec son coût par tour.
async fn tell_mechanical(
    d: &Arc<Daemon>,
    session_id: &str,
    model: &str,
    error: &str,
    report: &Report,
) {
    let s = &d.services;
    let _ = s
        .events
        .append(
            EventDraft::new(
                "context.compaction_mechanical",
                json!({"model": model, "error": error, "messages": report.messages}),
            )
            .session(session_id),
        )
        .await;
    let title = s
        .sessions
        .get(session_id)
        .await
        .ok()
        .flatten()
        .and_then(|x| x.title)
        .unwrap_or_else(|| session_id.to_string());
    let per_turn = cost_per_turn(s, session_id).await;
    if let Some(m) = d.compaction.messenger.get() {
        let _ = m
            .send_text(
                &crate::helpers::owner_origin_of(&d.services),
                &format!(
                    "⚠️ La session « {title} » ne se résumait plus : le résumeur (`{model}`) a \
                     échoué {MECHANICAL_AFTER} fois ({error}). {} messages ont été compactés \
                     sans modèle : tes derniers messages de ce passage et les ancres sont \
                     gardés tels quels, le reste se relit à la demande.{}",
                    report.messages,
                    per_turn
                        .map(|c| format!(" Coût moyen des derniers tours : {c:.3} $."))
                        .unwrap_or_default()
                ),
            )
            .await;
    }
}

/// Sessions dont la compaction a échoué ces dernières 24 h : titre, échecs, coût moyen
/// des derniers tours ; pour le digest (issue #131).
pub async fn struggling_sessions(s: &Services) -> Vec<(String, u32, Option<f64>)> {
    let since = chrono::DateTime::from_timestamp_millis(s.clock.now_ms() - 86_400_000)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let rows: Vec<(String, u32)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT session_id, COUNT(*) FROM events
                 WHERE kind = 'context.compaction_failed' AND session_id IS NOT NULL
                   AND ts >= ?1
                 GROUP BY session_id ORDER BY COUNT(*) DESC LIMIT 5",
            )?;
            let rows = st.query_map([&since], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap_or_default();
    let mut out = Vec::new();
    for (sid, n) in rows {
        let title = s
            .sessions
            .get(&sid)
            .await
            .ok()
            .flatten()
            .and_then(|x| x.title)
            .unwrap_or_else(|| sid.clone());
        out.push((title, n, cost_per_turn(s, &sid).await));
    }
    out
}

/// Coût moyen des cinq derniers tours d'une session.
async fn cost_per_turn(s: &Services, session_id: &str) -> Option<f64> {
    let sid = session_id.to_string();
    s.store
        .read(move |c| {
            let v: Option<f64> = c
                .query_row(
                    "SELECT AVG(cost) FROM (SELECT SUM(cost_usd) AS cost FROM usage
                     WHERE session_id = ?1 AND turn_id IS NOT NULL
                     GROUP BY turn_id ORDER BY MAX(ts) DESC LIMIT 5)",
                    [&sid],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            Ok(v)
        })
        .await
        .ok()
        .flatten()
}

async fn summarise(
    d: &Arc<Daemon>,
    provider: &dyn Provider,
    model: &str,
    job: &SummaryJob,
    turn_id: Option<&str>,
) -> Result<(Value, f64), SummaryFailure> {
    let s = &d.services;
    let info = s.catalog.get(strip_provider(model));
    let effort = info.as_ref().and_then(|i| i.lightest_effort());
    let structured = info
        .as_ref()
        .map(|i| i.supports_structured_output())
        .unwrap_or(false);
    let request = ChatRequest {
        model: model.to_string(),
        messages: job.summarizer_messages(),
        stream: true,
        // Neuf sections de 4 000 caractères au plus, plus le raisonnement éventuel.
        max_tokens: Some(if effort.as_deref() == Some("none") {
            8_000
        } else {
            16_000
        }),
        reasoning_effort: effort,
        response_format: structured.then(penelope_context::compaction::summary_response_format),
        session_id: Some(job.session_id.clone()),
        ..Default::default()
    };
    let failed = |e: penelope_llm::types::LlmError| SummaryFailure {
        passing: e.kind.is_retryable(),
        message: e.to_string(),
    };
    let call = async {
        let rx = provider
            .chat_stream(request, CancelToken::new())
            .await
            .map_err(failed)?;
        collect_stream(rx, model, provider.name(), &s.catalog)
            .await
            .map_err(failed)
    };
    let timeout = summary_timeout(job.tokens_src);
    let response = tokio::time::timeout(timeout, call).await.map_err(|_| {
        SummaryFailure::passing(format!(
            "le résumeur n'a pas répondu en {} s",
            timeout.as_secs()
        ))
    })??;
    let _ = s
        .budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(job.session_id.clone()),
            turn_id: turn_id.map(String::from),
            model: response.model.clone(),
            provider: response.provider.clone(),
            role: Some("compaction".into()),
            generation_id: (!response.id.is_empty()).then(|| response.id.clone()),
            upstream: response.upstream.clone(),
            finish: Some(format!("{:?}", response.finish).to_lowercase()),
            prompt: response.usage.prompt,
            completion: response.usage.completion,
            cached: response.usage.cached,
            cache_write: response.usage.cache_write,
            reasoning: response.usage.reasoning,
            cost_usd: response.cost_usd,
            estimated: response.cost_estimated,
            ..Default::default()
        })
        .await;
    let summary = penelope_context::compaction::validate_summary(&response.message.text())
        .map_err(|message| SummaryFailure {
            message,
            passing: false,
        })?;
    Ok((summary, response.cost_usd))
}

/// Publie un résumé validé : nœud LCM, canonique marqué, événement.
async fn publish(
    d: &Arc<Daemon>,
    session_id: &str,
    pending: &Pending,
    trigger: Trigger,
    report: &mut Report,
) -> anyhow::Result<()> {
    let s = &d.services;
    let job = &pending.job;
    let node_id = s
        .context
        .apply_summary_as(job, &pending.summary, &pending.model, trigger.as_str())
        .await?;
    let tokens_summary = s
        .context
        .lcm
        .get(&node_id)
        .await?
        .map(|n| n.tokens_self)
        .unwrap_or(0);

    // Frontière sûre pour le cache : le préfixe change de toute façon, les instantanés
    // mémoire T2 se rafraîchissent au tour suivant (§6.6).
    crate::episodes::refresh_snapshot(s, session_id).await;

    report.published += 1;
    report.messages += job.messages();
    report.node = Some(node_id.clone());
    report.covered_to = job.to_seq;
    report.tokens_src += job.tokens_src;
    report.tokens_summary = tokens_summary;
    report.model = Some(pending.model.clone());

    // Observation en shadow du contexte réellement rendu : ni le résumé accepté
    // ni le lot source ne sont retouchés. Les métriques ne portent aucun texte.
    let fidelity = observe_fidelity(job, &pending.summary);
    if fidelity.actions_missing > 0 || fidelity.identifiers_missing > 0 {
        tracing::warn!(
            session = %session_id,
            actions_missing = fidelity.actions_missing,
            identifiers_missing = fidelity.identifiers_missing,
            "indices explicites absents du contexte compacté"
        );
    }
    for (kind, count) in [
        ("action", fidelity.actions_missing),
        ("identifier", fidelity.identifiers_missing),
    ] {
        if count > 0 {
            penelope_observe::metrics::counter_inc(
                "penelope_compaction_missing_evidence_total",
                &[("kind", kind)],
                count as f64,
            );
        }
    }

    s.events
        .append(
            EventDraft::new(
                "context.compacted",
                json!({
                    "node": node_id,
                    "from_seq": job.from_seq,
                    "to_seq": job.to_seq,
                    "chunk_from_seq": job.chunk_from_seq,
                    "messages": job.messages(),
                    "tokens_src": job.tokens_src,
                    "tokens_summary": tokens_summary,
                    "updated": job.previous_node_id,
                    "model": pending.model,
                    "trigger": trigger.as_str(),
                    "evidence": fidelity,
                }),
            )
            .session(session_id),
        )
        .await?;
    penelope_observe::metrics::counter_inc(
        "penelope_compactions_total",
        &[("trigger", trigger.as_str()), ("outcome", "published")],
        1.0,
    );
    Ok(())
}

/// Publie un résumé mis de côté. Il est retiré d'abord : un résumé périmé ne revient
/// pas en boucle.
async fn publish_saved(
    d: &Arc<Daemon>,
    session_id: &str,
    pending: Pending,
    trigger: Trigger,
    report: &mut Report,
) {
    if let Err(e) = d.services.kv_delete(&pending_key(session_id)).await {
        tracing::warn!(session = %session_id, error = %e, "résumé en attente non retiré");
        return;
    }
    if let Err(e) = publish(d, session_id, &pending, trigger, report).await {
        tracing::warn!(session = %session_id, error = %e, "résumé en attente abandonné");
    }
}

async fn load_pending(s: &Services, session_id: &str) -> anyhow::Result<Option<Pending>> {
    let Some(raw) = s.kv_get(&pending_key(session_id)).await? else {
        return Ok(None);
    };
    match serde_json::from_str(&raw) {
        Ok(p) => Ok(Some(p)),
        Err(e) => {
            tracing::warn!(session = %session_id, error = %e, "résumé en attente illisible, écarté");
            s.kv_delete(&pending_key(session_id)).await?;
            Ok(None)
        }
    }
}

async fn save_pending(s: &Services, session_id: &str, pending: &Pending) -> anyhow::Result<()> {
    s.kv_set(&pending_key(session_id), &serde_json::to_string(pending)?)
        .await
}

async fn load_cooldown(s: &Services, session_id: &str) -> Cooldown {
    s.kv_get(&cooldown_key(session_id))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

async fn save_cooldown(s: &Services, session_id: &str, cooldown: &Cooldown) {
    let raw = serde_json::to_string(cooldown).unwrap_or_default();
    if let Err(e) = s.kv_set(&cooldown_key(session_id), &raw).await {
        tracing::warn!(session = %session_id, error = %e, "cooldown de compaction non enregistré");
    }
}

/// Modèle de la conversation : sa fenêtre fixe la queue et le seuil.
async fn conversation_model(d: &Arc<Daemon>, session_id: &str) -> String {
    if let Some(pin) = d.pinned_model(session_id).await {
        return pin.model_id;
    }
    let cfg = d.services.config.config();
    let last = d.services.kv_get(&last_model_key(session_id)).await;
    let alias = last
        .ok()
        .flatten()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| cfg.role_alias("chat_default"));
    cfg.alias_model(&alias).unwrap_or_default().to_string()
}

/// Compaction sur dépassement de fenêtre, offerte à la conversation d'un tour.
pub struct OverflowCompactor {
    pub daemon: Arc<Daemon>,
    pub turn_id: Option<String>,
}

#[async_trait::async_trait]
impl crate::agent::Compactor for OverflowCompactor {
    async fn compact_now(&self, session_id: &str) -> anyhow::Result<bool> {
        let r = compact(
            &self.daemon,
            session_id,
            Trigger::Overflow,
            self.turn_id.as_deref(),
        )
        .await?;
        Ok(r.published > 0)
    }
}

/// Bilan lisible, pour Telegram et la CLI.
pub fn report_text(r: &Report) -> String {
    if r.published > 0 {
        let mut s = format!(
            "🗜 {} messages résumés : {} tokens remplacés par un résumé de {}.",
            r.messages, r.tokens_src, r.tokens_summary
        );
        if r.remaining_batches > 0 {
            s.push_str(&format!(
                " Il reste {} lot(s), repris à la prochaine compaction.",
                r.remaining_batches
            ));
        }
        return s;
    }
    if r.deferred {
        return "🗜 Résumé prêt : publié à la fin du tour en cours.".into();
    }
    format!(
        "Rien à compacter : {}.",
        r.skipped
            .as_deref()
            .unwrap_or("la conversation tient dans la queue verbatim")
            .trim_start_matches("rien à compacter : ")
    )
}

mod fidelity;
mod view;
use fidelity::*;
pub use view::context_view;

#[cfg(test)]
mod tests;
