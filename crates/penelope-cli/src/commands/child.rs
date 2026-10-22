//! `penelope run --stdio` (#395) : le côté enfant d'un run supervisé par le daemon.
//!
//! `probe` sert la suite du protocole ; `sub_agent` fait tourner un sous-agent sur ses
//! propres services (même base, même configuration, ses providers), le host MCP restant
//! dans le daemon. Les jobs d'arrière-plan n'y existent pas (un job lancé ici mourrait avec
//! l'enfant), et la rotation des jetons Codex reste au daemon.

use penelope_agent::{AgentServices, NoJobs};
use penelope_app::child::{ChildCtx, ChildRun, Probe};
use penelope_app::services::Services;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::OnceCell;

/// Les providers d'un enfant : ceux de la configuration, Codex en jetons passifs.
struct ChildProviders {
    services: Arc<Services>,
    set: tokio::sync::Mutex<Option<Arc<penelope_llm::ProviderSet>>>,
}

#[async_trait::async_trait]
impl penelope_app::ports::ProviderSource for ChildProviders {
    async fn provider_for(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        let mut guard = self.set.lock().await;
        if guard.is_none() {
            let s = &self.services;
            let codex = penelope_ops::codex_auth::access_passive(s).await;
            let set = penelope_llm::build_providers(
                &s.config.config(),
                s.platform.secrets.as_ref(),
                s.catalog.clone(),
                codex,
            )
            .map_err(|e| format!("aucun provider utilisable dans l'enfant : {e}"))?;
            *guard = Some(Arc::new(set));
        }
        guard
            .as_ref()
            .expect("providers construits")
            .resolve(model_id)
    }

    fn provider_override_active(&self) -> Option<Arc<dyn penelope_llm::Provider>> {
        None
    }
}

#[async_trait::async_trait]
impl penelope_agent::FallbackProviders for ChildProviders {
    async fn fallback_provider(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        penelope_app::ports::ProviderSource::provider_for(self, model_id).await
    }
}

/// Les services de l'enfant, montés au premier run qui en a besoin.
struct Mounted {
    services: Arc<Services>,
    providers: Arc<ChildProviders>,
    agent: Arc<AgentServices>,
}

async fn mount() -> Result<Mounted, String> {
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let services = Arc::new(
        Services::bootstrap(None, clock, None)
            .await
            .map_err(|e| format!("services de l'enfant : {e}"))?,
    );
    let providers = Arc::new(ChildProviders {
        services: services.clone(),
        set: tokio::sync::Mutex::new(None),
    });
    let s = &services;
    let agent = Arc::new(AgentServices {
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
        modes: Arc::new(penelope_daemon::approval_mode::KvModes(s.clone())),
        sessions: Arc::new(s.sessions.clone()),
        snapshots: Arc::new(penelope_daemon::prompt_snapshot::StoredSnapshots(s.clone())),
        // Pas de job d'arrière-plan dans un enfant : il mourrait avec lui.
        jobs: Arc::new(NoJobs),
        attempts: Arc::new(penelope_app::journal::JournalAttempts(s.events.clone())),
        judge: Arc::new(penelope_app::judge::NoJudge),
        hooks: Arc::new(penelope_executor::executor::ExecHookGate(s.clone())),
    })
    .with_judge(Arc::new(penelope_app::model_judge::ModelJudge::new(
        s.clone(),
        providers.clone(),
    )));
    Ok(Mounted {
        services,
        providers,
        agent,
    })
}

/// Le run de l'enfant, selon son genre.
#[derive(Default)]
pub(super) struct CliChild {
    mounted: OnceCell<Mounted>,
}

#[async_trait::async_trait]
impl ChildRun for CliChild {
    async fn run(&self, kind: &str, payload: Value, ctx: ChildCtx) -> Result<Value, String> {
        if kind != penelope_orchestrator::workflow::child::KIND {
            return Probe.run(kind, payload, ctx).await;
        }
        let m = self.mounted.get_or_try_init(mount).await?;
        let job: penelope_orchestrator::workflow::child::SubAgentJob =
            serde_json::from_value(payload).map_err(|e| format!("tâche illisible : {e}"))?;
        let ports = penelope_orchestrator::workflow::Ports::default();
        ports.mcp.set(Some(Arc::new(penelope_app::child::ChildMcp(
            ctx.relay.clone(),
        ))));
        let cx = penelope_orchestrator::Context {
            services: m.services.clone(),
            providers: m.providers.clone(),
            handle: penelope_app::ports::Handle::new(m.services.clock.now_ms()),
            bus: Arc::new(penelope_app::bus::Bus::new()),
            workflows: Arc::new(penelope_orchestrator::workflow::State::with_ports(ports)),
            embeddings: Arc::default(),
            agent: m.agent.clone(),
            admin: None,
        };
        penelope_orchestrator::workflow::child::run_job(&cx, job, &ctx.cancel).await
    }
}
