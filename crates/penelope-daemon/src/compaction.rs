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
use crate::runtime::Services;
use penelope_context::{CompactionParams, Cooldown, SummaryJob};
use penelope_kernel::event::EventDraft;
use penelope_llm::Provider;
use penelope_llm::catalog::strip_provider;
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

/// Lots résumés au plus par compaction ; la suivante reprend le reste.
const MAX_PASSES: usize = 12;
/// Échecs consécutifs du résumeur au-delà desquels la compaction se fait sans modèle
/// (issue #131).
const MECHANICAL_AFTER: u32 = 3;

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

/// Ce que la compaction lit du daemon (T23 : `State` et le daemon ne se tiennent plus).
pub fn context_of(d: &crate::runtime::Daemon) -> Context {
    Context {
        services: d.services.clone(),
        providers: d.providers.clone(),
        bus: d.bus.clone(),
        compaction: d.compaction.clone(),
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
pub async fn after_turn(d: &Context, session_id: &str) {
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
pub fn spawn(d: Context, session_id: String, trigger: Trigger, turn_id: Option<String>) {
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
pub async fn publish_pending(d: &Context, session_id: &str) -> anyhow::Result<Option<Report>> {
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
    d: &Context,
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
    d: &Context,
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
    d: &Context,
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
pub async fn before_turn(d: &Context, session_id: &str, model_id: &str, turn_id: Option<&str>) {
    let s = &d.services;
    let threshold = background_threshold(s, model_id);
    let now = s.clock.now_ms();
    let last = last_prompt(s, session_id).await;
    let (size, reason) = match last {
        Some((prompt, ts)) if now - ts > crate::helpers::CACHE_TTL_MS => {
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
    d: &Context,
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

async fn wait_claim<'a>(d: &'a Context, session_id: &str) -> Option<Claim<'a>> {
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

/// Modèle de la conversation : sa fenêtre fixe la queue et le seuil.
async fn conversation_model(d: &Context, session_id: &str) -> String {
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

mod context;
mod fidelity;
mod health;
mod publish;
mod summarise;
mod view;
pub use context::{Context, OverflowCompactor};
use fidelity::*;
pub use health::struggling_sessions;
use health::{cost_per_turn, tell_mechanical};
use publish::*;
use summarise::*;
pub use view::context_view;

#[cfg(test)]
mod tests;
