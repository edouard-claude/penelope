//! Façade de la boucle d'agent (§4.4 de `design/v1/boucle-et-outils.md`, épopée #208,
//! T10) : la boucle vit dans la crate `penelope-agent` ; restent ici les ports sur les
//! services du daemon et les entrées que ses modules appellent avec `&Services`, sous
//! leur ancien nom.

pub use penelope_agent::*;
use penelope_hitl::Decision;
use penelope_tools::ToolOutcome;
use std::sync::Arc;

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

/// Les registres seuls, pour les entrées sans port (décision, bornes de tour) en `&Services`.
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
        attempts: Arc::new(penelope_app::journal::JournalAttempts(s.events.clone())),
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

/// Tranche une approbation (voir `penelope_agent::decide_approval`).
pub async fn decide_approval(
    s: &crate::runtime::Services,
    approval_id: &str,
    decision: &Decision,
) -> anyhow::Result<bool> {
    penelope_agent::decide_approval(&registries_of(s), approval_id, decision).await
}

/// Borne un tour qui n'a pas ouvert la sienne (voir `penelope_agent::close_unopened`).
pub async fn close_unopened(
    s: &crate::runtime::Services,
    session_id: &str,
    meta: &TurnMeta,
    outcome: &anyhow::Result<TurnOutcome>,
) {
    penelope_agent::close_unopened(&registries_of(s), session_id, meta, outcome).await
}

/// Referme les tours interrompus par l'arrêt (voir `penelope_agent::close_interrupted_turns`).
pub async fn close_interrupted_turns(s: &crate::runtime::Services) -> anyhow::Result<usize> {
    penelope_agent::close_interrupted_turns(&registries_of(s)).await
}
