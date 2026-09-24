//! Les bornes d'un tour dans le journal : `turn.started` à l'ouverture, `turn.finished`
//! sur **chaque** sortie (épopée #208, tâche T4).
//!
//! Avant T4, seul un tour qui répondait était fermé : une annulation, un échec, une
//! attente d'approbation ou un plafond laissaient un `turn.started` sans fin,
//! indiscernable d'un crash (`design/v1/source-de-verite.md` §1.3, point 6). La reprise
//! après crash (§2.7) reconnaîtra un tour ouvert à cette seule absence.

use super::*;
use penelope_context::journal::{KIND_TURN_FINISHED, KIND_TURN_STARTED};

/// Identité d'un tour de la file, quand il y en a une : le tour d'une session de chat.
/// Un sous-agent ou une étape de workflow n'en ont pas.
#[derive(Debug, Clone, Default)]
pub struct TurnMeta {
    /// Identifiant de la ligne de `turn_queue`.
    pub turn_id: String,
    /// `message`, `trigger`, `resume`, `nudge`.
    pub kind: String,
    /// Numéro de tentative : un tour rejoué après crash porte le suivant.
    pub attempt: i64,
    /// Posé quand `turn.started` est écrit : un tour qui échoue avant la boucle ne l'a
    /// pas ouvert, et c'est à l'appelant de le borner ([`open_and_close`]).
    pub opened: Arc<std::sync::atomic::AtomicBool>,
}

impl TurnMeta {
    pub fn of(turn: &penelope_kernel::turn::Turn) -> Self {
        TurnMeta {
            turn_id: turn.id.to_string(),
            kind: turn.kind.as_str().to_string(),
            attempt: turn.attempts,
            opened: Arc::default(),
        }
    }
}

/// Raison écrite dans `turn.finished` (§2.2) ; `interrupted` est réservé à la reprise
/// après crash, que la boucle n'écrit jamais.
pub fn finish_reason(outcome: &anyhow::Result<TurnOutcome>) -> &'static str {
    match outcome {
        Ok(TurnOutcome::Answered { .. }) => "answered",
        Ok(TurnOutcome::AwaitingApproval { .. }) => "awaiting_approval",
        Ok(TurnOutcome::Cancelled) => "cancelled",
        Ok(TurnOutcome::BudgetExceeded { .. }) => "budget_exceeded",
        Ok(TurnOutcome::LoopAborted { .. }) => "loop_aborted",
        Ok(TurnOutcome::Failed { error }) if error.starts_with(CALLS_EXHAUSTED) => {
            "calls_exhausted"
        }
        Ok(TurnOutcome::Failed { .. }) | Err(_) => "failed",
    }
}

/// Payload de `turn.started`.
pub fn started_payload(
    spec: &TurnSpec,
    meta: Option<&TurnMeta>,
    system_hash: Option<String>,
) -> Value {
    let mut p = json!({
        "model": spec.model_id,
        "system_hash": system_hash,
        "tools_hash": crate::cache_audit::Fingerprint::tools_hash_of(&spec.tools),
    });
    add_identity(&mut p, spec.turn_id.as_deref(), meta);
    p
}

/// Payload de `turn.finished` : la raison, et ce que l'issue dit d'elle-même.
pub fn finished_payload(
    origin_turn: Option<&str>,
    meta: Option<&TurnMeta>,
    outcome: &anyhow::Result<TurnOutcome>,
) -> Value {
    let mut p = json!({"reason": finish_reason(outcome)});
    match outcome {
        Ok(TurnOutcome::Answered {
            iterations,
            cost_usd,
            ..
        }) => {
            p["iterations"] = json!(iterations);
            p["cost_usd"] = json!(cost_usd);
        }
        Ok(TurnOutcome::AwaitingApproval { approval_id }) => {
            p["approval_id"] = json!(approval_id);
        }
        Ok(TurnOutcome::BudgetExceeded {
            scope,
            spent_usd,
            limit_usd,
        }) => {
            p["scope"] = json!(scope);
            p["spent_usd"] = json!(spent_usd);
            p["limit_usd"] = json!(limit_usd);
        }
        Ok(TurnOutcome::LoopAborted { report, .. }) => p["report"] = json!(report),
        Ok(TurnOutcome::Failed { error }) => p["error"] = json!(error),
        Err(e) => p["error"] = json!(e.to_string()),
        Ok(TurnOutcome::Cancelled) => {}
    }
    add_identity(&mut p, origin_turn, meta);
    p
}

/// `turn_id` (la ligne de la file), `origin_turn` (la requête du propriétaire, qu'une
/// reprise après approbation partage), `kind`, `attempt` : omis hors de la file.
fn add_identity(p: &mut Value, origin_turn: Option<&str>, meta: Option<&TurnMeta>) {
    if let Some(m) = meta {
        p["turn_id"] = json!(m.turn_id);
        p["kind"] = json!(m.kind);
        p["attempt"] = json!(m.attempt);
    }
    if let Some(origin) = origin_turn {
        p["origin_turn"] = json!(origin);
    }
}

impl AgentLoop {
    /// Exécute (ou reprend) un tour sur un transcript quelconque, borné dans le journal.
    pub async fn run_conversation(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        execute: &(dyn ToolExecutor + Send + Sync),
        sink: &dyn TurnSink,
    ) -> anyhow::Result<TurnOutcome> {
        self.run_conversation_as(spec, None, conv, execute, sink)
            .await
    }

    /// Même chose, pour un tour de la file dont l'identité entre dans ses bornes.
    pub async fn run_conversation_as(
        &self,
        spec: &TurnSpec,
        meta: Option<&TurnMeta>,
        conv: &dyn Conversation,
        execute: &(dyn ToolExecutor + Send + Sync),
        sink: &dyn TurnSink,
    ) -> anyhow::Result<TurnOutcome> {
        // Ce que le modèle va lire est nommé dès l'ouverture du tour : la chaîne d'audit
        // référence le prompt système par son empreinte, et l'instantané la résout
        // (issue #205).
        let prefix = conv.prompt_prefix();
        self.services
            .events
            .append(
                EventDraft::new(
                    KIND_TURN_STARTED,
                    started_payload(spec, meta, prefix.as_ref().map(|p| p.hash())),
                )
                .session(&spec.session_id),
            )
            .await?;
        if let Some(m) = meta {
            m.opened.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let outcome = self
            .run_steps(spec, prefix.as_ref(), conv, execute, sink)
            .await;
        close_turn(&self.services, spec, meta, &outcome).await;
        outcome
    }
}

/// Écrit `turn.finished`. Son échec ne change pas l'issue du tour : il ne coûte que la
/// borne, que la reprise après crash sait refermer.
pub async fn close_turn(
    s: &Services,
    spec: &TurnSpec,
    meta: Option<&TurnMeta>,
    outcome: &anyhow::Result<TurnOutcome>,
) {
    let payload = finished_payload(spec.turn_id.as_deref(), meta, outcome);
    append_bound(s, &spec.session_id, KIND_TURN_FINISHED, payload).await;
}

/// Un tour qui échoue avant la boucle (session introuvable, fournisseur indisponible)
/// n'a pas ouvert de `turn.started` : il est ouvert et fermé ici, pour qu'aucune sortie
/// ne laisse de trou. Sans effet si la boucle l'a ouvert.
pub async fn close_unopened(
    s: &Services,
    session_id: &str,
    meta: &TurnMeta,
    outcome: &anyhow::Result<TurnOutcome>,
) {
    if meta.opened.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let mut started = json!({});
    add_identity(&mut started, None, Some(meta));
    if append_bound(s, session_id, KIND_TURN_STARTED, started).await {
        let finished = finished_payload(None, Some(meta), outcome);
        append_bound(s, session_id, KIND_TURN_FINISHED, finished).await;
    }
}

async fn append_bound(s: &Services, session_id: &str, kind: &str, payload: Value) -> bool {
    match s
        .events
        .append(EventDraft::new(kind, payload).session(session_id))
        .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(session = %session_id, kind, error = %e, "borne de tour non journalisée");
            false
        }
    }
}
