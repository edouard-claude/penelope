//! Les services de la boucle d'agent sur ceux du daemon (§4.4 de
//! `design/v1/boucle-et-outils.md`, épopée #208, T10) : la boucle vit dans la crate
//! `penelope-agent` ; restent ici les ports que seul le daemon sait implémenter sur la
//! base, et les entrées en `&Services` qui en dérivent `AgentServices`. Elles ne peuvent
//! pas descendre : `penelope-agent` ne nomme pas `Services` (il porterait le moteur de
//! contexte, `reach.rs`), et `penelope-app` ne connaît pas la boucle.

use penelope_agent::{
    AgentServices, JobRequest, JobRunner, MemoryModes, NoAudit, NoJobs, PromptSnapshots,
    SessionModes, ToolExecutor, TurnMeta, TurnOutcome,
};
use penelope_app::services::Services;
use penelope_hitl::Decision;
use penelope_tools::ToolOutcome;
use std::sync::Arc;

/// Les services de la boucle, sur ceux du daemon : mêmes registres (le ledger de coûts
/// garde son observateur d'alerte), ports implémentés par les modules du daemon.
pub fn services_of(s: &Arc<Services>) -> Arc<AgentServices> {
    with_ports(
        s,
        Arc::new(crate::approval_mode::KvModes(s.clone())),
        Arc::new(crate::prompt_snapshot::StoredSnapshots(s.clone())),
        Arc::new(DaemonJobs(s.clone())),
    )
}

/// Les registres seuls, pour les entrées sans port (décision, bornes de tour) en `&Services`.
fn registries_of(s: &Services) -> Arc<AgentServices> {
    with_ports(
        s,
        Arc::new(MemoryModes::new(s.config.clone())),
        Arc::new(NoAudit),
        Arc::new(NoJobs),
    )
}

fn with_ports(
    s: &Services,
    modes: Arc<dyn SessionModes>,
    snapshots: Arc<dyn PromptSnapshots>,
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
        jobs,
        attempts: Arc::new(penelope_app::journal::JournalAttempts(s.events.clone())),
    })
}

/// Le port `JobRunner` sur les jobs d'outils du daemon (`tool_jobs.rs`, issue #204).
struct DaemonJobs(Arc<Services>);

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
    s: &Services,
    approval_id: &str,
    decision: &Decision,
) -> anyhow::Result<bool> {
    penelope_agent::decide_approval(&registries_of(s), approval_id, decision).await
}

/// Borne un tour qui n'a pas ouvert la sienne (voir `penelope_agent::close_unopened`).
pub async fn close_unopened(
    s: &Services,
    session_id: &str,
    meta: &TurnMeta,
    outcome: &anyhow::Result<TurnOutcome>,
) {
    penelope_agent::close_unopened(&registries_of(s), session_id, meta, outcome).await
}

/// Referme les tours interrompus par l'arrêt (voir `penelope_agent::close_interrupted_turns`).
pub async fn close_interrupted_turns(s: &Services) -> anyhow::Result<usize> {
    penelope_agent::close_interrupted_turns(&registries_of(s)).await
}
