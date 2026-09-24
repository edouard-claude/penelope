//! Les bornes d'un tour dans le journal : `turn.started` à l'ouverture, `turn.finished`
//! sur **chaque** sortie (épopée #208, tâche T4).
//!
//! Avant T4, seul un tour qui répondait était fermé : une annulation, un échec, une
//! attente d'approbation ou un plafond laissaient un `turn.started` sans fin,
//! indiscernable d'un crash (`design/v1/source-de-verite.md` §1.3, point 6). La reprise
//! après crash (§2.7) reconnaîtra un tour ouvert à cette seule absence.

use super::*;
use penelope_context::journal::{
    KIND_TURN_FINISHED, KIND_TURN_STARTED, TurnCall, TurnEnd, TurnIdentity, finished_payload,
    started_payload,
};

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
    /// pas ouvert, et c'est à l'appelant de le borner ([`close_unopened`]).
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

impl TurnMeta {
    fn identity(&self) -> TurnIdentity {
        TurnIdentity {
            turn_id: self.turn_id.clone(),
            kind: self.kind.clone(),
            attempt: self.attempt,
        }
    }
}

/// La sortie du tour, dans le vocabulaire du journal.
fn turn_end(outcome: &anyhow::Result<TurnOutcome>) -> TurnEnd {
    match outcome {
        Ok(TurnOutcome::Answered {
            iterations,
            cost_usd,
            ..
        }) => TurnEnd::Answered {
            iterations: *iterations,
            cost_usd: *cost_usd,
        },
        Ok(TurnOutcome::AwaitingApproval { approval_id }) => TurnEnd::AwaitingApproval {
            approval_id: approval_id.clone(),
        },
        Ok(TurnOutcome::Cancelled) => TurnEnd::Cancelled,
        Ok(TurnOutcome::BudgetExceeded {
            scope,
            spent_usd,
            limit_usd,
        }) => TurnEnd::BudgetExceeded {
            scope: scope.clone(),
            spent_usd: *spent_usd,
            limit_usd: *limit_usd,
        },
        Ok(TurnOutcome::LoopAborted { report, .. }) => TurnEnd::LoopAborted {
            report: report.clone(),
        },
        Ok(TurnOutcome::Failed { error }) if error.starts_with(CALLS_EXHAUSTED) => {
            TurnEnd::CallsExhausted {
                error: error.clone(),
            }
        }
        Ok(TurnOutcome::Failed { error }) => TurnEnd::Failed {
            error: error.clone(),
        },
        Err(e) => TurnEnd::Failed {
            error: e.to_string(),
        },
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
        let call = TurnCall {
            model: spec.model_id.clone(),
            system_hash: prefix.as_ref().map(|p| p.hash()),
            tools_hash: crate::cache_audit::Fingerprint::tools_hash_of(&spec.tools),
        };
        let id = meta.map(TurnMeta::identity);
        self.services
            .events
            .append(
                EventDraft::new(
                    KIND_TURN_STARTED,
                    started_payload(Some(&call), spec.turn_id.as_deref(), id.as_ref()),
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
    let id = meta.map(TurnMeta::identity);
    let payload = finished_payload(&turn_end(outcome), spec.turn_id.as_deref(), id.as_ref());
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
    let id = meta.identity();
    let started = started_payload(None, None, Some(&id));
    if append_bound(s, session_id, KIND_TURN_STARTED, started).await {
        let finished = finished_payload(&turn_end(outcome), None, Some(&id));
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
