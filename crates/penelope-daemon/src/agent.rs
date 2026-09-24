//! Boucle d'agent : un tour, de bout en bout (§3.3, §4.2, §9).
//!
//! Invariants :
//! - tout effet non `readOnly` est **planifié dans le ledger avant** exécution ;
//! - un outil qui exige une approbation suspend **ce tour seulement**, et l'appel reste
//!   dans le transcript : la reprise le retrouve par son identifiant ;
//! - le détecteur de boucles arrête le tour plutôt que de laisser tourner ;
//! - une erreur d'outil est renvoyée au modèle, pas au harnais.
//!
//! Une itération commence **toujours** par résoudre les appels d'outils en attente à la
//! fin du transcript, puis appelle le modèle. Un premier passage et une reprise après
//! approbation suivent donc exactement le même chemin.

use penelope_app::journal::Provenance;
use penelope_hitl::{ApprovalKind, ApprovalState, Decision};
use penelope_kernel::effects::{EffectKind, EffectSpec, Planned};
use penelope_kernel::event::EventDraft;
use penelope_kernel::risk::{PolicyDecision, PolicyWindow, RiskClass};
use penelope_llm::provider::{CancelToken, Provider, collect_stream_observed};
use penelope_llm::types::*;
use penelope_tools::{LoopDetector, LoopVerdict, ToolOutcome};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

mod attempts;
mod cache;
mod decisions;
mod executor;
mod guards;
mod loop_abort;
mod model;
mod pending;
mod pipeline;
mod ports;
mod rules;
mod spec;
mod turn;
mod turn_log;

use attempts::Attempts;
pub use attempts::{EMPTY_RETRY_PROMPT, MAX_ATTEMPTS_PER_TURN};
pub use cache::{
    CACHE_TTL_MS, Fingerprint, Observed, PreviousCall, STICKY_MS, miss_cause, sticky_upstream,
};
pub use decisions::{EFFECT_DONE, EFFECT_IGNORE, EFFECT_RETRY};
pub use executor::{call_arguments, effective_arguments, wants_network};
pub use guards::budget_exceeded_text;
use guards::{TurnContext, default_chain, run_guards};
pub use loop_abort::{LOOP_STOP_NOTE, last_result_of, split_choices};
#[cfg(test)]
use model::fit_modalities;
pub use pending::pending_calls;
pub use penelope_app::conversation::{Compactor, Conversation, MemoryConversation};
pub use penelope_app::outcome::{NullSink, RecordingSink, TurnEvent, TurnOutcome, TurnSink};
pub use penelope_app::tool_executor::{CallInfo, ToolExecutor};
use pipeline::Pending;
pub(crate) use pipeline::effect_kind;
pub use pipeline::{ApprovalMode, declared_allow, local_draft_allow};
pub use pipeline::{server_of, without_intention};
pub use ports::{
    AgentServices, CacheAudit, JobRequest, JobRunner, MemoryModes, NoAudit, NoJobs,
    PromptSnapshots, SessionInfo, SessionModes,
};
pub use rules::{MAX_FAMILIES_PER_CLICK, always_creates_no_rule};
pub use rules::{arg_pattern, arg_patterns};
pub use spec::{AgentLoop, CALLS_EXHAUSTED, TURN_CALLS, TurnRequest, TurnSpec};
pub use turn_log::TurnMeta;

// Façade du daemon (§4.4 de `design/v1/boucle-et-outils.md`, épopée #208, T09). Ce qui
// précède et `agent/` partiront dans la crate `penelope-agent` (T10) ; ce qui suit reste
// au daemon : les ports sur ses services et les entrées que ses modules appellent avec
// `&Services`, sous leur ancien nom.

/// Les services de la boucle, sur ceux du daemon : mêmes registres (le ledger de coûts
/// garde son observateur d'alerte), ports implémentés par les modules du daemon.
pub fn services_of(s: &Arc<crate::runtime::Services>) -> Arc<AgentServices> {
    with_ports(
        s,
        Arc::new(crate::approval_mode::KvModes(s.clone())),
        Arc::new(crate::prompt_snapshot::StoredSnapshots(s.clone())),
        Arc::new(crate::cache_audit::UsageAudit(s.clone())),
        Arc::new(DaemonJobs(s.clone())),
    )
}

/// Les registres seuls, pour les entrées qui ne lisent aucun port (décision du
/// propriétaire, bornes de tour) et que leurs appelants servent en `&Services`.
fn registries_of(s: &crate::runtime::Services) -> Arc<AgentServices> {
    with_ports(
        s,
        Arc::new(MemoryModes::new(s.config.clone())),
        Arc::new(NoAudit),
        Arc::new(NoAudit),
        Arc::new(NoJobs),
    )
}

fn with_ports(
    s: &crate::runtime::Services,
    modes: Arc<dyn SessionModes>,
    snapshots: Arc<dyn PromptSnapshots>,
    cache: Arc<dyn CacheAudit>,
    jobs: Arc<dyn JobRunner>,
) -> Arc<AgentServices> {
    Arc::new(AgentServices {
        store: s.store.clone(),
        clock: s.clock.clone(),
        events: s.events.clone(),
        effects: s.effects.clone(),
        config: s.config.clone(),
        budget: s.budget.clone(),
        catalog: s.catalog.clone(),
        llm_state: s.llm_state.clone(),
        approvals: s.approvals.clone(),
        policies: s.policies.clone(),
        modes,
        sessions: Arc::new(s.sessions.clone()),
        snapshots,
        cache,
        jobs,
    })
}

/// Le port `JobRunner` sur les jobs d'outils du daemon (`tool_jobs.rs`, issue #204).
struct DaemonJobs(Arc<crate::runtime::Services>);

#[async_trait::async_trait]
impl JobRunner for DaemonJobs {
    async fn maybe_spawn(
        &self,
        execute: &(dyn ToolExecutor + Send + Sync),
        req: JobRequest<'_>,
    ) -> anyhow::Result<Option<ToolOutcome>> {
        crate::tool_jobs::maybe_spawn(&self.0, execute, req).await
    }
}

/// Tranche une approbation (voir `decisions::decide_approval`).
pub async fn decide_approval(
    s: &crate::runtime::Services,
    approval_id: &str,
    decision: &Decision,
) -> anyhow::Result<bool> {
    decisions::decide_approval(&registries_of(s), approval_id, decision).await
}

/// Borne un tour qui n'a pas ouvert la sienne (voir `turn_log::close_unopened`).
pub async fn close_unopened(
    s: &crate::runtime::Services,
    session_id: &str,
    meta: &TurnMeta,
    outcome: &anyhow::Result<TurnOutcome>,
) {
    turn_log::close_unopened(&registries_of(s), session_id, meta, outcome).await
}

/// Referme les tours interrompus par l'arrêt (voir `turn_log::close_interrupted_turns`).
pub async fn close_interrupted_turns(s: &crate::runtime::Services) -> anyhow::Result<usize> {
    turn_log::close_interrupted_turns(&registries_of(s)).await
}

#[cfg(test)]
mod clone_policy_tests;

#[cfg(test)]
mod tests;
