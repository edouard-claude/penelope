//! Banc d'essai de l'orchestrateur sans daemon : un [`Context`] sur `Services::for_tests`,
//! un provider simulé, et les branchements que la composition poserait (canal du
//! propriétaire, MCP, orchestrateur des outils, livraison du canal).

use super::*;
use penelope_agent::{AgentServices, MemoryModes, NoAudit, NoJobs};
use penelope_app::bus::{Bus, ChannelDelivery};
use penelope_app::ports::{Handle, McpAdmin, McpGateway, Messenger, Orchestrator};
use penelope_app::testing::MockProviders;
use penelope_kernel::clock::SharedClock;
use penelope_llm::mock::MockProvider;

pub(crate) struct Harness {
    pub dir: tempfile::TempDir,
    pub cx: Context,
    pub provider: Arc<MockProvider>,
    pub ports: Ports,
    pub delivery: Slot<dyn ChannelDelivery>,
}

impl std::ops::Deref for Harness {
    type Target = Context;
    fn deref(&self) -> &Context {
        &self.cx
    }
}

impl Harness {
    /// Services de test, provider simulé, `WorkflowOrchestrator` branché sur ce contexte.
    pub async fn new(clock: SharedClock) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let provider = Arc::new(MockProvider::new());
        let ports = Ports::default();
        let cx = Context {
            agent: agent_services(&s),
            handle: Handle::new(s.clock.now_ms()),
            providers: MockProviders::new(provider.clone()),
            bus: Arc::new(Bus::new()),
            workflows: Arc::new(State::with_ports(ports.clone())),
            embeddings: Arc::default(),
            admin: None,
            services: s,
        };
        ports.orchestrator.set(Some(Arc::new(WorkflowOrchestrator {
            context: cx.clone(),
        })));
        // Le même emplacement que `Services.channel.delivery`, comme `Hooks` au daemon.
        let delivery = cx.services.channel.delivery.clone();
        Harness {
            dir,
            cx,
            provider,
            ports,
            delivery,
        }
    }

    /// Branchements de l'ordonnanceur, comme `Hooks::scheduler`.
    pub fn scheduler(&self) -> crate::scheduler::Ports {
        crate::scheduler::Ports {
            messenger: self.ports.messenger.clone(),
            delivery: self.delivery.clone(),
            mcp: self.ports.mcp_supervisor.clone(),
            orchestrator: self.ports.orchestrator.clone(),
        }
    }

    pub fn set_messenger(&self, m: Arc<dyn Messenger>) {
        self.ports.messenger.set(Some(m));
    }

    /// Branche un superviseur MCP : passerelle des outils et administration.
    pub fn set_mcp<T: McpAdmin + 'static>(&self, sup: Arc<T>) {
        self.ports.mcp.set(Some(sup.clone() as Arc<dyn McpGateway>));
        self.ports.mcp_supervisor.set(Some(sup));
    }

    pub fn orchestrator(&self) -> Option<Arc<dyn Orchestrator>> {
        self.ports.orchestrator.get()
    }
}

/// Les registres de la boucle sur ces services, ports en mémoire (mode d'approbation de
/// la configuration, ni instantanés, ni audit de cache, ni jobs).
pub(crate) fn agent_services(s: &Services) -> Arc<AgentServices> {
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
        modes: Arc::new(MemoryModes::new(s.config.clone())),
        sessions: Arc::new(s.sessions.clone()),
        snapshots: Arc::new(NoAudit),
        jobs: Arc::new(NoJobs),
        attempts: Arc::new(penelope_app::journal::JournalAttempts(s.events.clone())),
        judge: Arc::new(penelope_app::judge::NoJudge),
    })
}
