//! Compaction niveau 3 (§5.4) : résumés LCM écrits par le rôle `compaction`.
//!
//! Quatre déclencheurs : la fin d'un tour dont la projection estimée **ou le prompt
//! réellement facturé** atteint le seuil moins la marge (tâche de fond), la reprise d'une
//! session froide au-delà de ce seuil (avant l'appel au modèle), `/compact` (lève le
//! cooldown), et le dépassement de fenêtre prouvé par le provider (une tentative, publiée
//! aussitôt). Le résumé de fond se calcule sans bloquer la conversation et se publie à une
//! frontière de tour. Chaque décision laisse un événement : `context.compaction_requested`,
//! `context.compaction_skipped` (avec sa raison), `context.compacted` (issue #40).

use crate::runtime::Daemon;
use penelope_context::{CompactionParams, Cooldown, SummaryJob};
use penelope_kernel::event::EventDraft;
use penelope_llm::Provider;
use penelope_llm::catalog::strip_provider;
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Lots résumés au plus par compaction ; la suivante reprend le reste.
const MAX_PASSES: usize = 12;
/// Au-delà, le résumeur est en échec et le cooldown s'applique.
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(180);
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
    running: Mutex<HashSet<String>>,
    /// Session → tour qui a demandé la compaction (le coût lui est attribué).
    wanted: Mutex<HashMap<String, Option<String>>>,
}

impl State {
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
    let Some(pending) = load_pending(d, session_id).await? else {
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
pub async fn last_prompt(s: &crate::runtime::Services, session_id: &str) -> Option<(u64, i64)> {
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
pub fn background_threshold(s: &crate::runtime::Services, model_id: &str) -> u64 {
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
async fn compaction_spent_today(s: &crate::runtime::Services) -> f64 {
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
    if let Some(pending) = load_pending(d, session_id).await? {
        if !trigger.publishes_now() && d.bus.is_active(session_id) {
            report.deferred = true;
            report.skipped = Some("un résumé attend déjà la fin du tour en cours".into());
            return Ok(report);
        }
        publish_saved(d, session_id, pending, trigger, &mut report).await;
    }

    let cfg = s.config.config();
    let mut cooldown = load_cooldown(d, session_id).await;
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
            save_cooldown(d, session_id, &cooldown).await;
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

        let summary = match summarise(d, provider.as_ref(), &model, &job, turn_id).await {
            Ok((summary, cost)) => {
                report.cost_usd += cost;
                summary
            }
            Err(error) => {
                cooldown.record_failure(s.clock.now_ms(), &cfg.context.cooldown_ms);
                save_cooldown(d, session_id, &cooldown).await;
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
                    anyhow::bail!("le résumé a échoué ({error}), nouvel essai dans {retry_in_s} s");
                }
                break;
            }
        };

        let pending = Pending {
            job,
            summary,
            model: model.clone(),
        };
        if !trigger.publishes_now() && d.bus.is_active(session_id) {
            save_pending(d, session_id, &pending).await?;
            report.deferred = true;
            report.model = Some(model.clone());
            // Le tour a pu se terminer pendant l'écriture : sa frontière est passée.
            if !d.bus.is_active(session_id)
                && let Some(p) = load_pending(d, session_id).await?
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
        save_cooldown(d, session_id, &cooldown).await;
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
async fn summarise(
    d: &Arc<Daemon>,
    provider: &dyn Provider,
    model: &str,
    job: &SummaryJob,
    turn_id: Option<&str>,
) -> Result<(Value, f64), String> {
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
    let call = async {
        let rx = provider
            .chat_stream(request, CancelToken::new())
            .await
            .map_err(|e| e.to_string())?;
        collect_stream(rx, model, provider.name(), &s.catalog)
            .await
            .map_err(|e| e.to_string())
    };
    let response = tokio::time::timeout(SUMMARY_TIMEOUT, call)
        .await
        .map_err(|_| {
            format!(
                "le résumeur n'a pas répondu en {} s",
                SUMMARY_TIMEOUT.as_secs()
            )
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
    let summary = penelope_context::compaction::validate_summary(&response.message.text())?;
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
        .apply_summary(job, &pending.summary, &pending.model)
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
    crate::episodes::refresh_snapshot(d, s, session_id).await;

    report.published += 1;
    report.messages += job.messages();
    report.node = Some(node_id.clone());
    report.covered_to = job.to_seq;
    report.tokens_src += job.tokens_src;
    report.tokens_summary = tokens_summary;
    report.model = Some(pending.model.clone());

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
    if let Err(e) = d.kv_delete(&pending_key(session_id)).await {
        tracing::warn!(session = %session_id, error = %e, "résumé en attente non retiré");
        return;
    }
    if let Err(e) = publish(d, session_id, &pending, trigger, report).await {
        tracing::warn!(session = %session_id, error = %e, "résumé en attente abandonné");
    }
}

async fn load_pending(d: &Arc<Daemon>, session_id: &str) -> anyhow::Result<Option<Pending>> {
    let Some(raw) = d.kv_get(&pending_key(session_id)).await? else {
        return Ok(None);
    };
    match serde_json::from_str(&raw) {
        Ok(p) => Ok(Some(p)),
        Err(e) => {
            tracing::warn!(session = %session_id, error = %e, "résumé en attente illisible, écarté");
            d.kv_delete(&pending_key(session_id)).await?;
            Ok(None)
        }
    }
}

async fn save_pending(d: &Arc<Daemon>, session_id: &str, pending: &Pending) -> anyhow::Result<()> {
    d.kv_set(&pending_key(session_id), &serde_json::to_string(pending)?)
        .await
}

async fn load_cooldown(d: &Arc<Daemon>, session_id: &str) -> Cooldown {
    d.kv_get(&cooldown_key(session_id))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

async fn save_cooldown(d: &Arc<Daemon>, session_id: &str, cooldown: &Cooldown) {
    let raw = serde_json::to_string(cooldown).unwrap_or_default();
    if let Err(e) = d.kv_set(&cooldown_key(session_id), &raw).await {
        tracing::warn!(session = %session_id, error = %e, "cooldown de compaction non enregistré");
    }
}

/// Modèle de la conversation : sa fenêtre fixe la queue et le seuil.
async fn conversation_model(d: &Arc<Daemon>, session_id: &str) -> String {
    if let Some(pin) = d.pinned_model(session_id).await {
        return pin.model_id;
    }
    let cfg = d.services.config.config();
    let alias = d
        .kv_get(&crate::engine::last_model_key(session_id))
        .await
        .ok()
        .flatten()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| cfg.role_alias("chat_default"));
    cfg.alias_model(&alias).unwrap_or_default().to_string()
}

/// Taille du contexte d'une session (issue #18) : prompt du dernier appel de conversation,
/// part en cache, seuils de compaction et fenêtre du modèle.
pub async fn context_view(
    s: &crate::runtime::Services,
    session_id: &str,
    model_id: Option<&str>,
) -> anyhow::Result<Value> {
    use penelope_store::rusqlite::OptionalExtension;
    let sid = session_id.to_string();
    /// Prompt, cache, modèle, cause du raté, fournisseur amont.
    type LastCall = (i64, i64, String, Option<String>, Option<String>);
    let last: Option<LastCall> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT prompt, cached, model, miss_cause, upstream FROM usage
                 WHERE session_id = ?1 AND COALESCE(role, 'chat') = 'chat'
                 ORDER BY ts DESC, rowid DESC LIMIT 1",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?)
        })
        .await?;
    let model = model_id
        .map(String::from)
        .or_else(|| last.as_ref().map(|l| l.2.clone()))
        .unwrap_or_default();
    let cfg = s.config.config();
    let params =
        CompactionParams::from_config(&cfg, s.catalog.window_of(strip_provider(&model)), &model);
    let sid = session_id.to_string();
    let last_compaction: Option<String> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT ts FROM events WHERE session_id = ?1 AND kind = 'context.compacted'
                 ORDER BY id DESC LIMIT 1",
                [sid],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
        .ok()
        .flatten();
    Ok(json!({
        "last_compaction": last_compaction,
        "last_prompt_tokens": last.as_ref().map(|l| l.0),
        "last_cached_tokens": last.as_ref().map(|l| l.1),
        "last_upstream": last.as_ref().and_then(|l| l.4.clone()),
        "last_cache_miss": last.as_ref().and_then(|l| l.3.as_deref()).map(|m| {
            json!({"cause": m, "label": penelope_kernel::budget::miss_label(m)})
        }),
        "model": model,
        "window": params.window,
        "max_prompt_tokens": params.max_prompt_tokens,
        "background_compaction_at": params.background_threshold_tokens(params.background_margin),
        "compaction_at": params.threshold_tokens(),
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Conversation;
    use crate::bus::Origin;
    use crate::conversation::SessionConversation;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::catalog::ModelInfo;
    use penelope_llm::mock::{MockProvider, Scripted};
    use penelope_llm::types::ChatMessage;

    const SUMMARY: &str = r#"{"objectif": "préparer la migration PROJ-7",
        "contraintes_et_preferences": "réponses courtes", "fait": "schéma exporté",
        "en_cours": "vérification", "bloque": "", "decisions_cles": "PostgreSQL 17",
        "fichiers_et_ressources": "db/schema.sql", "prochaines_etapes": "migrer ce soir",
        "contexte_critique": ""}"#;

    async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
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

    /// Une longue conversation : 40 échanges d'environ 600 tokens chacun.
    async fn long_session(d: &Arc<Daemon>) -> String {
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        let h = &d.services.context.history;
        for i in 0..20 {
            let q = format!("question {i} sur PROJ-7 : {}", "détail ".repeat(340));
            let a = format!("réponse {i} : {}", "analyse ".repeat(300));
            h.append(&sid, &ChatMessage::user(q), 600, 0, false, None)
                .await
                .unwrap();
            h.append(&sid, &ChatMessage::assistant(a), 600, 0, false, None)
                .await
                .unwrap();
        }
        sid
    }

    fn summarizer_requests(p: &MockProvider) -> Vec<penelope_llm::types::ChatRequest> {
        p.requests()
            .into_iter()
            .filter(|r| {
                r.messages
                    .first()
                    .is_some_and(|m| m.text().contains("module de compaction"))
            })
            .collect()
    }

    #[tokio::test]
    async fn manual_compaction_replaces_old_turns_with_a_summary() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        p.reply(SUMMARY);

        let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
        assert_eq!(r.published, 1, "{r:?}");
        assert!(r.messages > 10 && r.tokens_src > 0 && r.tokens_summary > 0);
        assert!(r.skipped.is_none());
        assert!(report_text(&r).contains("messages résumés"));

        // Le résumeur est le modèle du rôle `compaction`.
        let cfg = d.services.config.config();
        let asked = summarizer_requests(&p);
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0].model,
            cfg.alias_model(&cfg.role_alias("compaction")).unwrap()
        );

        // Canonique marqué, un seul nœud, projection = résumé + queue verbatim.
        let s = &d.services;
        let nodes = s.context.lcm.active_nodes(&sid).await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].summary.contains("PostgreSQL 17"));
        assert!(nodes[0].anchors.iter().any(|a| a.value == "PROJ-7"));
        let history = s.context.history.load(&sid, 0).await.unwrap();
        assert!(history.iter().any(|e| e.compacted));
        assert!(
            !history.last().unwrap().compacted,
            "la queue reste verbatim"
        );

        let conv = SessionConversation::new(
            s.clone(),
            &sid,
            "openrouter:deepseek/deepseek-v4-pro",
            penelope_context::TiersBuilder::new()
                .soul("Pénélope.")
                .build(),
            0,
        );
        let texts: Vec<String> = conv
            .request_messages()
            .await
            .unwrap()
            .iter()
            .map(|m| m.text())
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("Résumé de la conversation antérieure"))
        );
        assert!(
            !texts.iter().any(|t| t.starts_with("question 0 ")),
            "les tours résumés ne sont plus envoyés"
        );
        assert!(texts.iter().any(|t| t.starts_with("question 19 ")));

        // Coût et trace.
        let roles = s.budget.report("role", Some(&sid), None, 10).await.unwrap();
        assert!(roles.iter().any(|r| r.key == "compaction"));
        let events = s.events.session_events(&sid, 0).await.unwrap();
        let ev = events
            .iter()
            .find(|e| e.kind == "context.compacted")
            .expect("événement de compaction");
        assert_eq!(ev.payload["trigger"], "manual");

        // Rien de neuf : pas de second appel au résumeur, même forcé.
        let again = compact(&d, &sid, Trigger::Background, None).await.unwrap();
        assert_eq!(again.published, 0);
        assert!(again.skipped.is_some());
        assert_eq!(summarizer_requests(&p).len(), 1);
    }

    #[tokio::test]
    async fn a_summary_ready_during_a_turn_waits_for_its_end() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        let _active = d.bus.begin("t_en_cours", &sid, &Origin::Cli);
        p.reply(SUMMARY);

        let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
        assert!(r.deferred && r.published == 0, "{r:?}");
        assert!(report_text(&r).contains("fin du tour"));
        let s = &d.services;
        assert!(s.context.lcm.active_nodes(&sid).await.unwrap().is_empty());

        // Pendant l'attente, aucune autre compaction ne prépare un lot concurrent.
        let other = compact(&d, &sid, Trigger::Background, None).await.unwrap();
        assert!(other.deferred && other.published == 0);
        assert_eq!(summarizer_requests(&p).len(), 1);

        d.bus.end(&sid, "t_en_cours");
        let published = publish_pending(&d, &sid)
            .await
            .unwrap()
            .expect("publication");
        assert_eq!(published.published, 1);
        assert_eq!(s.context.lcm.active_nodes(&sid).await.unwrap().len(), 1);
        assert!(
            publish_pending(&d, &sid).await.unwrap().is_none(),
            "publié une fois"
        );
    }

    #[tokio::test]
    async fn a_failed_summary_cools_down_until_compact_is_forced() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        p.reply("Désolé, je ne peux pas résumer.");

        let err = compact(&d, &sid, Trigger::Manual, None).await.unwrap_err();
        assert!(err.to_string().contains("résumé a échoué"), "{err}");
        let cooldown = load_cooldown(&d, &sid).await;
        assert_eq!(cooldown.failures, 1);
        let events = d.services.events.session_events(&sid, 0).await.unwrap();
        assert!(events.iter().any(|e| e.kind == "context.compaction_failed"));

        // La tâche de fond respecte le cooldown, et le dit (issue #40)…
        let bg = compact(&d, &sid, Trigger::Background, None).await.unwrap();
        assert!(bg.skipped.unwrap().contains("échec récent"));
        let events = d.services.events.session_events(&sid, 0).await.unwrap();
        let skipped = events
            .iter()
            .find(|e| e.kind == "context.compaction_skipped")
            .expect("saut journalisé");
        assert!(
            skipped.payload["reason"]
                .as_str()
                .unwrap()
                .contains("échec récent")
        );
        assert_eq!(skipped.payload["trigger"], "background");
        assert_eq!(summarizer_requests(&p).len(), 1);

        // … `/compact` le lève.
        p.reply(SUMMARY);
        let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
        assert_eq!(r.published, 1);
        assert_eq!(load_cooldown(&d, &sid).await.failures, 0);
    }

    #[tokio::test]
    async fn a_turn_over_the_threshold_compacts_in_the_background() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        // Fenêtre réduite du modèle principal : la conversation dépasse le seuil de fond.
        let cfg = d.services.config.config();
        let main = strip_provider(cfg.alias_model("main").unwrap()).to_string();
        d.services
            .catalog
            .upsert(vec![ModelInfo::minimal(&main, "deepseek", 40_000)]);

        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Je reprends où nous en étions.");
        p.reply(SUMMARY);
        d.enqueue_message(&sid, "on continue ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = d.services.turns.claim("test").await.unwrap().unwrap();
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "{out:?}"
        );
        d.services.turns.complete(&turn).await.unwrap();

        let s = &d.services;
        let mut nodes = Vec::new();
        for _ in 0..100 {
            nodes = s.context.lcm.active_nodes(&sid).await.unwrap();
            if !nodes.is_empty() && !d.compaction.is_running(&sid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(nodes.len(), 1, "la compaction de fond a publié son résumé");
        let by_turn = s.budget.report("turn", Some(&sid), None, 10).await.unwrap();
        assert_eq!(
            by_turn.len(),
            1,
            "le résumé est attribué au tour qui l'a déclenché"
        );
        assert_eq!(by_turn[0].calls, 3, "classifieur, réponse, résumé");
    }

    /// Issue #18 : sur une fenêtre de 1,3 M, le plafond `max_prompt_tokens` suffit à
    /// déclencher la compaction de fond (ici 20 k pour une conversation de 24 k ; en
    /// production 120 k).
    #[tokio::test]
    async fn max_prompt_tokens_compacts_a_huge_window_in_the_background() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        let cfg = d.services.config.config();
        let main = strip_provider(cfg.alias_model("main").unwrap()).to_string();
        d.services
            .catalog
            .upsert(vec![ModelInfo::minimal(&main, "z-ai", 1_300_000)]);
        d.publish_config("test", |c| {
            c.context.max_prompt_tokens = 20_000;
            Ok(vec!["context.max_prompt_tokens".into()])
        })
        .unwrap();

        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Je reprends où nous en étions.");
        p.reply(SUMMARY);
        d.enqueue_message(&sid, "on continue ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = d.services.turns.claim("test").await.unwrap().unwrap();
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "{out:?}"
        );
        d.services.turns.complete(&turn).await.unwrap();

        let mut nodes = Vec::new();
        for _ in 0..100 {
            nodes = d.services.context.lcm.active_nodes(&sid).await.unwrap();
            if !nodes.is_empty() && !d.compaction.is_running(&sid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            nodes.len(),
            1,
            "compaction de fond malgré la grande fenêtre"
        );

        let view = context_view(&d.services, &sid, None).await.unwrap();
        assert_eq!(view["window"], 1_300_000);
        assert_eq!(view["compaction_at"], 20_000);
        assert!(view["last_prompt_tokens"].as_i64().is_some(), "{view}");
    }

    #[tokio::test]
    async fn a_proven_overflow_compacts_then_retries_once() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        p.reply(r#"{"complexity":"medium"}"#);
        p.push(Scripted::ContextOverflow);
        p.reply(SUMMARY);
        p.reply("C'est reparti.");
        d.enqueue_message(&sid, "et maintenant ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = d.services.turns.claim("test").await.unwrap().unwrap();
        match d.run_turn(&turn).await {
            crate::agent::TurnOutcome::Answered { text, .. } => {
                assert_eq!(text, "C'est reparti.")
            }
            other => panic!("{other:?}"),
        }
        d.services.turns.complete(&turn).await.unwrap();

        assert_eq!(
            d.services
                .context
                .lcm
                .active_nodes(&sid)
                .await
                .unwrap()
                .len(),
            1
        );
        let last = p.requests().last().unwrap().clone();
        assert!(
            last.messages
                .iter()
                .any(|m| m.text().contains("Résumé de la conversation antérieure")),
            "la nouvelle requête part avec le résumé"
        );

        // Un second dépassement dans le même tour n'entraîne pas de boucle : une seule
        // compaction, puis l'échec est dit. La compaction est une frontière : le message
        // suivant repasse par le classifieur (#82).
        p.reply(r#"{"complexity":"medium"}"#);
        p.push(Scripted::ContextOverflow);
        p.reply(SUMMARY);
        p.push(Scripted::ContextOverflow);
        d.enqueue_message(&sid, "encore ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = d.services.turns.claim("test").await.unwrap().unwrap();
        match d.run_turn(&turn).await {
            crate::agent::TurnOutcome::Failed { error } => {
                assert!(error.contains("/compact"), "{error}")
            }
            other => panic!("{other:?}"),
        }
        d.services.turns.complete(&turn).await.unwrap();
        assert_eq!(summarizer_requests(&p).len(), 2, "une compaction par tour");
    }

    fn kinds(events: &[penelope_kernel::event::Event]) -> Vec<&str> {
        events.iter().map(|e| e.kind.as_str()).collect()
    }

    /// Issue #40 : l'estimation locale reste sous le seuil, mais le prompt réellement
    /// facturé le dépasse : la compaction de fond est demandée, dite, puis publiée.
    #[tokio::test]
    async fn the_billed_prompt_size_requests_a_background_compaction() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        let cfg = d.services.config.config();
        let main = strip_provider(cfg.alias_model("main").unwrap()).to_string();
        d.services
            .catalog
            .upsert(vec![ModelInfo::minimal(&main, "z-ai", 1_300_000)]);
        d.publish_config("test", |c| {
            c.context.max_prompt_tokens = 50_000;
            Ok(vec!["context.max_prompt_tokens".into()])
        })
        .unwrap();
        let threshold = background_threshold(&d.services, cfg.alias_model("main").unwrap());
        assert!(threshold > 30_000, "{threshold}");
        p.set_usage(penelope_llm::types::Usage {
            prompt: threshold + 2_000,
            ..Default::default()
        });

        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Je reprends où nous en étions.");
        p.reply(SUMMARY);
        d.enqueue_message(&sid, "on continue ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = d.services.turns.claim("test").await.unwrap().unwrap();
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "{out:?}"
        );
        d.services.turns.complete(&turn).await.unwrap();

        let mut nodes = Vec::new();
        for _ in 0..100 {
            nodes = d.services.context.lcm.active_nodes(&sid).await.unwrap();
            if !nodes.is_empty() && !d.compaction.is_running(&sid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(nodes.len(), 1, "résumé publié");
        let events = d.services.events.session_events(&sid, 0).await.unwrap();
        let requested = events
            .iter()
            .position(|e| e.kind == "context.compaction_requested")
            .unwrap_or_else(|| panic!("{:?}", kinds(&events)));
        assert_eq!(events[requested].payload["reason"], "taille réelle");
        let compacted = events
            .iter()
            .position(|e| e.kind == "context.compacted")
            .expect("compacté");
        assert!(requested < compacted);
        let view = context_view(&d.services, &sid, None).await.unwrap();
        assert!(view["last_compaction"].is_string(), "{view}");
    }

    /// Issue #40 : un budget de session dépassé n'empêche pas le résumé ; seul le plafond
    /// du jour l'arrête, une fois la réserve des résumés dépensée, et le saut est dit.
    #[tokio::test]
    async fn budgets_do_not_block_compaction_until_the_summary_reserve_is_spent() {
        let (_dir, d, p) = daemon().await;
        let sid = long_session(&d).await;
        let s = &d.services;
        let spend =
            |role: &str, cost: f64, session: Option<&str>| penelope_kernel::budget::UsageRecord {
                session_id: session.map(String::from),
                model: "m".into(),
                provider: "p".into(),
                role: Some(role.into()),
                cost_usd: cost,
                ..Default::default()
            };
        s.budget
            .record(spend("chat", 6.0, Some(&sid)))
            .await
            .unwrap();
        let statuses = s
            .budget
            .status(&s.config.config().budget, Some(&sid), None)
            .await
            .unwrap();
        assert!(
            statuses.iter().any(|b| b.exceeded),
            "session au-delà de son plafond"
        );
        p.reply(SUMMARY);
        let r = compact(&d, &sid, Trigger::Background, None).await.unwrap();
        assert_eq!(r.published, 1, "{r:?}");

        s.budget.record(spend("chat", 30.0, None)).await.unwrap();
        s.budget
            .record(spend("compaction", 0.6, None))
            .await
            .unwrap();
        let r = compact(&d, &sid, Trigger::Background, None).await.unwrap();
        assert!(r.skipped.as_deref().unwrap().contains("réserve"), "{r:?}");
        let events = d.services.events.session_events(&sid, 0).await.unwrap();
        assert!(
            events.iter().any(|e| e.kind == "context.compaction_skipped"
                && e.payload["reason"].as_str().unwrap().contains("réserve")),
            "{:?}",
            kinds(&events)
        );
    }

    /// Issue #40 : une session reprise après une pause, au-delà du seuil, est résumée avant
    /// l'appel au modèle, et le prompt envoyé passe sous le plafond.
    #[tokio::test]
    async fn a_cold_session_is_compacted_before_the_model_call() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::default();
        let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), shared)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let sid = long_session(&d).await;
        let cfg = d.services.config.config();
        let main_id = cfg.alias_model("main").unwrap().to_string();
        d.services.catalog.upsert(vec![ModelInfo::minimal(
            strip_provider(&main_id),
            "z-ai",
            1_300_000,
        )]);
        d.publish_config("test", |c| {
            c.context.max_prompt_tokens = 20_000;
            Ok(vec!["context.max_prompt_tokens".into()])
        })
        .unwrap();
        // Dernier appel : 300 k tokens, puis une longue pause.
        d.services
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(sid.clone()),
                model: main_id.clone(),
                provider: "openrouter".into(),
                role: Some("chat".into()),
                prompt: 300_000,
                ..Default::default()
            })
            .await
            .unwrap();
        clock.advance_ms(3 * 3_600_000);

        p.reply(r#"{"complexity":"medium"}"#);
        p.reply(SUMMARY);
        p.reply("Me revoilà.");
        d.enqueue_message(&sid, "on reprend ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = d.services.turns.claim("test").await.unwrap().unwrap();
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, crate::agent::TurnOutcome::Answered { .. }),
            "{out:?}"
        );
        d.services.turns.complete(&turn).await.unwrap();

        let requests = p.requests();
        let summary_at = requests
            .iter()
            .position(|r| {
                r.messages
                    .first()
                    .is_some_and(|m| m.text().contains("module de compaction"))
            })
            .expect("résumé demandé");
        let chat_at = requests
            .iter()
            .position(|r| {
                r.messages
                    .first()
                    .is_some_and(|m| m.text().starts_with("Tu es Pénélope"))
            })
            .expect("appel de conversation");
        assert!(summary_at < chat_at, "le résumé précède l'appel");
        let chat = &requests[chat_at];
        assert!(
            chat.messages
                .iter()
                .any(|m| m.text().contains("Résumé de la conversation antérieure")),
            "l'appel part avec le résumé"
        );
        let tokens: u64 = chat
            .messages
            .iter()
            .map(|m| d.services.context.estimator.message_tokens(&main_id, m))
            .sum();
        assert!(tokens < 20_000, "prompt sous le plafond : {tokens}");
        let events = d.services.events.session_events(&sid, 0).await.unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.kind == "context.compaction_requested"
                    && e.payload["trigger"] == "resume"),
            "{:?}",
            kinds(&events)
        );
    }
}
