//! Composition du daemon : tout est câblé ici, une seule fois (§3.1).
//!
//! Les crates métier ne se connaissent pas entre elles au-delà de leurs contrats : c'est
//! ce module qui les assemble, et lui seul.

use penelope_context::{ContextEngine, HistoryStore, Lcm};
use penelope_hitl::{ApprovalStore, PolicyEngine};
use penelope_kernel::budget::BudgetLedger;
use penelope_kernel::clock::{SharedClock, SystemClock};
use penelope_kernel::config::{ApplyResult, Config, ConfigStore};
use penelope_kernel::effects::EffectLedger;
use penelope_kernel::event::{EventDraft, EventLog};
use penelope_kernel::session::SessionStore;
use penelope_kernel::turn::TurnQueue;
use penelope_llm::{Catalog, LlmStateMachine, TokenEstimator};
use penelope_mcp::ToolRegistry;
use penelope_memory::{CandidateStore, IntentStore, MemoryIndex};
use penelope_platform::Platform;
use penelope_skills::SkillRegistry;
use penelope_store::Store;
use penelope_telegram::{ActionStore, TemplateRegistry};
use penelope_workflow::{RunStore, ScheduleStore, WorkflowRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Tous les services, assemblés.
pub struct Services {
    pub platform: Arc<Platform>,
    pub store: Store,
    pub clock: SharedClock,
    pub events: EventLog,
    pub effects: EffectLedger,
    pub config: Arc<ConfigStore>,
    pub sessions: SessionStore,
    pub turns: TurnQueue,
    pub budget: BudgetLedger,
    pub catalog: Catalog,
    pub llm_state: LlmStateMachine,
    pub context: ContextEngine,
    pub memory: MemoryIndex,
    pub candidates: CandidateStore,
    pub intents: IntentStore,
    pub skills: SkillRegistry,
    pub mcp_tools: ToolRegistry,
    pub approvals: ApprovalStore,
    pub policies: PolicyEngine,
    pub templates: Arc<TemplateRegistry>,
    pub actions: ActionStore,
    pub workflows: WorkflowRegistry,
    pub runs: RunStore,
    pub schedules: ScheduleStore,
    /// Demandes des serveurs MCP au propriétaire (§8.4, issue #12).
    pub elicitations: Arc<crate::elicitation::Broker>,
}

impl Services {
    /// Construit tous les services à partir d'une racine.
    pub async fn bootstrap(home: Option<PathBuf>, clock: SharedClock) -> anyhow::Result<Services> {
        let platform = Arc::new(Platform::bootstrap(home)?);
        let dirs = &platform.dirs;

        let store = Store::open(dirs.db_path())?;
        let config = Arc::new(ConfigStore::load_or_create(
            dirs.config_file(),
            Some(store.clone()),
            clock.clone(),
            0,
        )?);
        let cfg = config.config();

        let events = EventLog::new(store.clone(), clock.clone());
        let effects = EffectLedger::new(store.clone(), clock.clone());
        let sessions = SessionStore::new(store.clone(), clock.clone());
        let lease_ttl = penelope_kernel::config::parse_duration(&cfg.runners.lease_ttl)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(60_000);
        let turns = TurnQueue::new(store.clone(), clock.clone(), lease_ttl);
        let budget = BudgetLedger::new(store.clone(), clock.clone()).with_timezone({
            let config = config.clone();
            move || config.config().owner.timezone.clone()
        });

        let catalog = Catalog::new();
        let llm_state = LlmStateMachine::new(store.clone(), clock.clone());
        let estimator = TokenEstimator::new();
        let history = HistoryStore::new(store.clone(), clock.clone());
        let lcm = Lcm::new(store.clone(), clock.clone());
        let context = ContextEngine::new(history, lcm, estimator, catalog.clone(), clock.clone());

        let memory = MemoryIndex::new(store.clone(), clock.clone()).with_half_life({
            let config = config.clone();
            move || config.config().memory.half_life_days
        });
        let candidates = CandidateStore::new(store.clone(), clock.clone());
        let intents = IntentStore::new(store.clone(), clock.clone());
        let skills = SkillRegistry::new(store.clone());
        let mcp_tools = ToolRegistry::new(
            store.clone(),
            cfg.mcp.sticky_set_max,
            cfg.mcp.schema_max_bytes,
            cfg.mcp.eager_total_max_bytes,
        );

        let approvals = ApprovalStore::new(store.clone(), clock.clone());
        let policies = PolicyEngine::new(store.clone(), clock.clone());
        let mut templates = TemplateRegistry::with_builtins();
        templates.load_dir(&dirs.templates());
        let actions = ActionStore::new(store.clone(), clock.clone(), cfg.owner.telegram_user_id);

        let known = workflow_known(&cfg, &mcp_tools).await;
        let workflows = WorkflowRegistry::with_bundled(&known);
        workflows.load_dir(
            &dirs.workflows(),
            penelope_workflow::registry::Scope::User,
            &known,
        );
        // Second passage : un workflow utilisateur peut en appeler un autre du même dossier.
        let known = workflow_known_with(&cfg, &mcp_tools, &workflows).await;
        workflows.load_dir(
            &dirs.workflows(),
            penelope_workflow::registry::Scope::User,
            &known,
        );
        let runs = RunStore::new(store.clone(), clock.clone());
        let schedules = ScheduleStore::new(store.clone(), clock.clone(), &cfg.owner.timezone);

        Ok(Services {
            platform,
            store,
            clock,
            events,
            effects,
            config,
            sessions,
            turns,
            budget,
            catalog,
            llm_state,
            context,
            memory,
            candidates,
            intents,
            skills,
            mcp_tools,
            approvals,
            policies,
            templates: Arc::new(templates),
            actions,
            workflows,
            runs,
            schedules,
            elicitations: Arc::default(),
        })
    }

    /// Variante de test : racine temporaire, secrets en mémoire, horloge fictive.
    pub async fn for_tests(root: PathBuf, clock: SharedClock) -> anyhow::Result<Services> {
        let platform = Arc::new(Platform::for_tests(root.clone())?);
        let store = Store::open(platform.dirs.db_path())?;
        // Sans revue de fond ni titre automatique par défaut : ils consommeraient les
        // réponses scriptées des tests.
        let mut sample = Config::sample(42);
        sample.memory.review_max_candidates = 0;
        sample.context.auto_title = false;
        // Pas de fenêtre de regroupement par défaut : un test qui l'exerce la règle
        // lui-même (issue #49).
        sample.telegram.text_group_window_ms = 0;
        let config = Arc::new(ConfigStore::new(
            sample,
            platform.dirs.config_file(),
            Some(store.clone()),
            clock.clone(),
        ));
        let cfg = config.config();
        let catalog = Catalog::new();
        let context = ContextEngine::new(
            HistoryStore::new(store.clone(), clock.clone()),
            Lcm::new(store.clone(), clock.clone()),
            TokenEstimator::new(),
            catalog.clone(),
            clock.clone(),
        );
        let mcp_tools = ToolRegistry::new(store.clone(), 30, 8192, 65536);
        let known = workflow_known(&cfg, &mcp_tools).await;

        Ok(Services {
            events: EventLog::new(store.clone(), clock.clone()),
            effects: EffectLedger::new(store.clone(), clock.clone()),
            sessions: SessionStore::new(store.clone(), clock.clone()),
            turns: TurnQueue::new(store.clone(), clock.clone(), 60_000),
            budget: BudgetLedger::new(store.clone(), clock.clone()).with_timezone({
                let config = config.clone();
                move || config.config().owner.timezone.clone()
            }),
            llm_state: LlmStateMachine::new(store.clone(), clock.clone()),
            memory: MemoryIndex::new(store.clone(), clock.clone()).with_half_life({
                let config = config.clone();
                move || config.config().memory.half_life_days
            }),
            candidates: CandidateStore::new(store.clone(), clock.clone()),
            intents: IntentStore::new(store.clone(), clock.clone()),
            skills: SkillRegistry::new(store.clone()),
            approvals: ApprovalStore::new(store.clone(), clock.clone()),
            policies: PolicyEngine::new(store.clone(), clock.clone()),
            templates: Arc::new(TemplateRegistry::with_builtins()),
            actions: ActionStore::new(store.clone(), clock.clone(), 42),
            workflows: WorkflowRegistry::with_bundled(&known),
            runs: RunStore::new(store.clone(), clock.clone()),
            schedules: ScheduleStore::new(store.clone(), clock.clone(), "Indian/Reunion"),
            elicitations: Arc::default(),
            mcp_tools,
            context,
            catalog,
            config,
            clock,
            store,
            platform,
        })
    }
}

/// Ce que la validation des workflows doit connaître.
pub async fn workflow_known(cfg: &Config, mcp_tools: &ToolRegistry) -> penelope_workflow::Known {
    let mut known = penelope_workflow::Known {
        model_aliases: cfg.models.aliases.keys().cloned().collect(),
        native_tools: penelope_tools::all_tools()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect(),
        templates: penelope_telegram::templates::CATALOG
            .iter()
            .map(|s| s.to_string())
            .collect(),
        max_depth: cfg.workflows.max_depth,
        ..Default::default()
    };
    known.workflow_ids = penelope_workflow::bundled::all()
        .into_iter()
        .map(|w| w.metadata.id)
        .collect();
    let _ = mcp_tools; // les outils MCP sont connus dynamiquement, absence = avertissement
    known
}

/// Comme [`workflow_known`], avec les workflows déjà chargés : un workflow peut en
/// appeler un autre écrit par l'utilisateur.
pub async fn workflow_known_with(
    cfg: &Config,
    mcp_tools: &ToolRegistry,
    workflows: &WorkflowRegistry,
) -> penelope_workflow::Known {
    let mut known = workflow_known(cfg, mcp_tools).await;
    known.workflow_ids.extend(workflows.ids());
    known
}

/// Poignée de contrôle du daemon.
#[derive(Clone)]
pub struct DaemonHandle {
    shutdown: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
    started_at_ms: i64,
    turns_done: Arc<AtomicU64>,
}

impl DaemonHandle {
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
    pub fn request_restart(&self) {
        self.restart.store(true, Ordering::SeqCst);
        self.shutdown.store(true, Ordering::SeqCst);
    }
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
    pub fn wants_restart(&self) -> bool {
        self.restart.load(Ordering::SeqCst)
    }
    pub fn uptime_s(&self, now_ms: i64) -> u64 {
        ((now_ms - self.started_at_ms).max(0) / 1000) as u64
    }
    pub fn turns_done(&self) -> u64 {
        self.turns_done.load(Ordering::SeqCst)
    }
    pub fn record_turn(&self) {
        self.turns_done.fetch_add(1, Ordering::SeqCst);
    }
}

/// Le daemon.
pub struct Daemon {
    pub services: Arc<Services>,
    pub handle: DaemonHandle,
    /// Événements des tours, attentes de réponse, tours actifs.
    pub bus: Arc<crate::bus::Bus>,
    /// Branchements optionnels : canal de message, MCP, orchestration.
    pub hooks: Hooks,
    /// Compactions de fond en cours et demandées (§5.4).
    pub compaction: crate::compaction::State,
    /// Runs de workflow pilotés par ce processus (§12.7).
    pub workflows: crate::workflow::State,
    /// Calcul des embeddings : dernier échec, rattrapage en cours (issue #11).
    pub embeddings: crate::embeddings::State,
    /// Boucles de fond surveillées : vivantes, paniques, relances (issue #84).
    pub tasks: Arc<crate::tasks::Tasks>,
    /// Providers construits à la demande (la clé peut arriver après le démarrage).
    providers: tokio::sync::Mutex<Option<Arc<penelope_llm::ProviderSet>>>,
    /// Provider imposé, pour les tests et les suites sans réseau.
    provider_override: std::sync::RwLock<Option<Arc<dyn penelope_llm::Provider>>>,
}

/// Points de branchement des sous-systèmes qui démarrent après le daemon.
#[derive(Default)]
pub struct Hooks {
    pub messenger: std::sync::RwLock<Option<Arc<dyn crate::executor::Messenger>>>,
    pub mcp: std::sync::RwLock<Option<Arc<dyn crate::executor::McpGateway>>>,
    pub orchestrator: std::sync::RwLock<Option<Arc<dyn crate::executor::Orchestrator>>>,
    pub telegram: std::sync::RwLock<Option<Arc<dyn crate::bus::ChannelDelivery>>>,
    /// Superviseur MCP concret, pour l'administration (`mcp.*`).
    pub mcp_supervisor: std::sync::RwLock<Option<Arc<crate::mcp::McpSupervisor>>>,
}

impl Hooks {
    pub fn messenger(&self) -> Option<Arc<dyn crate::executor::Messenger>> {
        self.messenger.read().ok().and_then(|g| g.clone())
    }
    pub fn set_orchestrator(&self, o: Arc<dyn crate::executor::Orchestrator>) {
        if let Ok(mut g) = self.orchestrator.write() {
            *g = Some(o);
        }
    }
    pub fn mcp(&self) -> Option<Arc<dyn crate::executor::McpGateway>> {
        self.mcp.read().ok().and_then(|g| g.clone())
    }
    pub fn orchestrator(&self) -> Option<Arc<dyn crate::executor::Orchestrator>> {
        self.orchestrator.read().ok().and_then(|g| g.clone())
    }
    pub fn telegram(&self) -> Option<Arc<dyn crate::bus::ChannelDelivery>> {
        self.telegram.read().ok().and_then(|g| g.clone())
    }
    pub fn mcp_supervisor(&self) -> Option<Arc<crate::mcp::McpSupervisor>> {
        self.mcp_supervisor.read().ok().and_then(|g| g.clone())
    }
    /// Branche un superviseur MCP : passerelle des outils et administration.
    pub fn set_mcp(&self, sup: Arc<crate::mcp::McpSupervisor>) {
        if let Ok(mut g) = self.mcp.write() {
            *g = Some(sup.clone() as Arc<dyn crate::executor::McpGateway>);
        }
        if let Ok(mut g) = self.mcp_supervisor.write() {
            *g = Some(sup);
        }
    }
}

impl Daemon {
    pub async fn new(home: Option<PathBuf>) -> anyhow::Result<Daemon> {
        let clock: SharedClock = Arc::new(SystemClock);
        let services = Arc::new(Services::bootstrap(home, clock.clone()).await?);
        Ok(Daemon::from_services(services))
    }

    pub fn from_services(services: Arc<Services>) -> Daemon {
        let started_at_ms = services.clock.now_ms();
        Daemon {
            services,
            handle: DaemonHandle {
                shutdown: Arc::new(AtomicBool::new(false)),
                restart: Arc::new(AtomicBool::new(false)),
                started_at_ms,
                turns_done: Arc::new(AtomicU64::new(0)),
            },
            bus: Arc::new(crate::bus::Bus::new()),
            hooks: Hooks::default(),
            compaction: crate::compaction::State::default(),
            workflows: crate::workflow::State::default(),
            embeddings: crate::embeddings::State::default(),
            tasks: Arc::new(crate::tasks::Tasks::default()),
            providers: tokio::sync::Mutex::new(None),
            provider_override: std::sync::RwLock::new(None),
        }
    }

    /// Impose un provider pour tous les modèles (tests, suites sans réseau).
    /// Provider imposé (tests), s'il y en a un.
    pub fn provider_override_active(&self) -> Option<Arc<dyn penelope_llm::Provider>> {
        self.provider_override.read().ok().and_then(|g| g.clone())
    }

    pub fn set_provider_override(&self, p: Arc<dyn penelope_llm::Provider>) {
        if let Ok(mut g) = self.provider_override.write() {
            *g = Some(p);
        }
    }

    /// Oublie les providers construits : la prochaine demande les reconstruit avec la
    /// configuration et les secrets du moment.
    pub async fn invalidate_providers(&self) {
        *self.providers.lock().await = None;
    }

    /// Provider d'un modèle. La construction est retentée tant qu'elle échoue : une clé
    /// posée après le démarrage est prise en compte au tour suivant, sans redémarrage.
    pub async fn provider_for(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        if let Some(p) = self.provider_override.read().ok().and_then(|g| g.clone()) {
            return Ok(p);
        }
        let mut guard = self.providers.lock().await;
        if guard.is_none() {
            let s = &self.services;
            let cfg = s.config.config();
            let set =
                penelope_llm::build_providers(&cfg, s.platform.secrets.as_ref(), s.catalog.clone())
                    .map_err(|e| {
                        format!(
                            "aucun provider utilisable : {e}. Poser la clé avec \
                         `penelope secret set openrouter_api_key`"
                        )
                    })?;
            *guard = Some(Arc::new(set));
        }
        let set = guard.as_ref().expect("providers construits");
        set.get(model_id)
            .ok_or_else(|| format!("aucun provider configuré pour `{model_id}`"))
    }

    /// Reprise au démarrage (§17) : leases, effets, requêtes LLM, tâches MCP, runs.
    pub async fn recover(&self) -> anyhow::Result<RecoveryReport> {
        let s = &self.services;

        let turns = s.turns.recover_on_boot().await?;
        let unknown_effects = s.effects.recover_on_boot().await?;
        let unknown_llm = s.llm_state.recover_on_boot().await?;
        let runs = s.runs.recover_on_boot().await?;
        let orphans = {
            use penelope_platform::ProcessHost;
            s.platform
                .processes
                .reap_orphans(&s.platform.dirs.pid_dir())
                .unwrap_or_default()
        };

        let report = RecoveryReport {
            turns_requeued: turns as u64,
            effects_unknown: unknown_effects.len() as u64,
            llm_unknown: unknown_llm.len() as u64,
            runs_resumed: runs.len() as u64,
            mcp_orphans_killed: orphans.len() as u64,
        };

        s.events
            .append(EventDraft::new(
                "daemon.recovered",
                serde_json::to_value(report).unwrap_or_default(),
            ))
            .await?;

        // Chaque effet `unknown` devient une demande HITL : jamais de retry silencieux.
        // Une seule par effet, même après plusieurs redémarrages (#83) ; la passerelle
        // Telegram la pousse au propriétaire dès qu'elle est prête.
        for e in unknown_effects {
            if s.approvals
                .pending_for_effect(e.id.as_str())
                .await?
                .is_some()
            {
                continue;
            }
            let _ = s
                .approvals
                .create(
                    penelope_hitl::ApprovalKind::EffectUnknown,
                    &e.tool
                        .clone()
                        .unwrap_or_else(|| e.kind.as_str().to_string()),
                    penelope_kernel::risk::RiskClass::Unknown,
                    serde_json::json!({
                        "effect_id": e.id.as_str(),
                        "request": e.request,
                        "attempts": e.attempts,
                        // L'appel de conversation qui l'a lancé : la reprise du tour
                        // retrouve la décision par lui (`find_for_call`).
                        "call_id": e.step_id,
                    }),
                    vec![
                        crate::agent::EFFECT_DONE.into(),
                        crate::agent::EFFECT_RETRY.into(),
                        crate::agent::EFFECT_IGNORE.into(),
                    ],
                    e.session_id.as_deref(),
                    e.run_id.as_deref(),
                    false,
                )
                .await;
        }
        Ok(report)
    }

    /// Publie une génération de configuration et collecte les résultats par sous-système.
    pub fn publish_config<F>(&self, source: &str, mutate: F) -> anyhow::Result<u64>
    where
        F: FnOnce(&mut Config) -> penelope_kernel::Result<Vec<String>>,
    {
        let g = self.services.config.mutate(source, mutate)?;
        for subsystem in SUBSYSTEMS {
            self.services.config.record_apply(
                subsystem,
                ApplyResult::AppliedLive {
                    generation: g.generation,
                },
            );
        }
        Ok(g.generation)
    }

    pub async fn status(&self) -> anyhow::Result<penelope_kernel::api::StatusReport> {
        let s = &self.services;
        let cfg = s.config.config();
        let mcp = match self.hooks.mcp_supervisor() {
            Some(sup) => sup.statuses().await,
            None => Vec::new(),
        };
        Ok(penelope_kernel::api::StatusReport {
            version: crate::VERSION.to_string(),
            uptime_s: self.handle.uptime_s(s.clock.now_ms()),
            config_generation: s.config.generation(),
            sessions_active: s
                .sessions
                .list(None, 1000)
                .await?
                .iter()
                .filter(|x| x.state == "active")
                .count() as u64,
            runs_active: s
                .runs
                .list(Some(penelope_workflow::RunState::Running), 1000)
                .await?
                .len() as u64,
            approvals_pending: s.approvals.count_pending().await? as u64,
            mcp_ready: mcp.iter().filter(|m| m.state.is_usable()).count() as u64,
            mcp_total: mcp.len() as u64,
            turns_queued: s.turns.pending_count().await? as u64,
            rss_mb: rss_mb(),
            spent_today_usd: s.budget.spent_today().await?,
            telegram: if cfg.telegram.token.is_empty() {
                "non configuré".into()
            } else {
                cfg.telegram.mode.clone()
            },
            runners_alive: self.tasks.alive("runner-") as u64,
            runners_expected: cfg.runners.count.max(1) as u64,
        })
    }
}

/// Skills livrées avec le binaire (portée `bundled`) : une skill utilisateur ou de dépôt du
/// même nom l'emporte.
pub const BUNDLED_SKILLS: &[(&str, &str)] = &[(
    "wiki-markdown",
    include_str!("../skills/wiki-markdown/SKILL.md"),
)];

/// Recharge les skills : livrées (réécrites depuis le binaire), puis celles de l'utilisateur.
pub async fn reload_skills(s: &Services) -> anyhow::Result<u64> {
    let bundled = s.platform.dirs.bundled_skills();
    for (name, body) in BUNDLED_SKILLS {
        let dir = bundled.join(name);
        std::fs::create_dir_all(&dir)?;
        penelope_kernel::config::atomic_write(&dir.join("SKILL.md"), body.as_bytes())?;
    }
    Ok(s.skills
        .reload(Some(&bundled), &s.platform.dirs.skills(), None)
        .await?)
}

/// Sous-systèmes abonnés aux générations de configuration (§4.4).
pub const SUBSYSTEMS: &[&str] = &[
    "context",
    "memory",
    "mcp",
    "skills",
    "workflows",
    "telegram",
    "llm",
    "hitl",
    "observability",
    "runners",
    "sandbox",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryReport {
    pub turns_requeued: u64,
    pub effects_unknown: u64,
    pub llm_unknown: u64,
    pub runs_resumed: u64,
    pub mcp_orphans_killed: u64,
}

impl RecoveryReport {
    pub fn is_clean(&self) -> bool {
        self.effects_unknown == 0 && self.llm_unknown == 0
    }
}

/// Mémoire résidente approchée, en mégaoctets.
pub fn rss_mb() -> f64 {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("/bin/ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            && let Ok(s) = String::from_utf8(out.stdout)
            && let Ok(kb) = s.trim().parse::<f64>()
        {
            return kb / 1024.0;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(pages) = s
                .split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<f64>().ok())
            {
                return pages * 4096.0 / 1_048_576.0;
            }
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        (dir, Arc::new(s))
    }

    #[tokio::test]
    async fn bootstrap_wires_everything() {
        let (_d, s) = services().await;
        assert_eq!(s.config.generation(), 1);
        assert!(s.platform.dirs.vault().is_dir());
        assert!(s.workflows.get("ticket-to-deploy").is_some());
        assert!(s.templates.get("tool_approval").is_some());
        assert_eq!(s.store.integrity().unwrap(), "ok");
    }

    #[tokio::test]
    async fn status_reports_a_coherent_snapshot() {
        let (_d, s) = services().await;
        let d = Daemon::from_services(s.clone());
        let st = d.status().await.unwrap();
        assert_eq!(st.config_generation, 1);
        assert_eq!(st.sessions_active, 0);
        assert_eq!(st.approvals_pending, 0);
        assert_eq!(st.version, crate::VERSION);
    }

    /// CA 17 : au démarrage, tout est repris et rien n'est exécuté deux fois.
    #[tokio::test]
    async fn recovery_requeues_and_asks_instead_of_retrying() {
        let (_d, s) = services().await;

        // Un tour réclamé mais jamais terminé.
        s.sessions
            .create(penelope_kernel::session::SessionKind::Chat, None)
            .await
            .unwrap();
        let sessions = s.sessions.list(None, 10).await.unwrap();
        let sid = sessions[0].id.to_string();
        s.turns
            .enqueue(
                &sid,
                penelope_kernel::turn::TurnKind::Message,
                serde_json::json!({"text":"salut"}),
                None,
                0,
            )
            .await
            .unwrap();
        s.turns.claim("runner-mort").await.unwrap().unwrap();

        // Un effet parti mais jamais confirmé.
        let spec = penelope_kernel::effects::EffectSpec::new(
            penelope_kernel::effects::EffectKind::Mcp,
            "mcp__forge__create_pr",
            serde_json::json!({"title":"fix"}),
        )
        .session(&sid);
        let id = match s.effects.plan(spec).await.unwrap() {
            penelope_kernel::effects::Planned::Fresh(id) => id,
            other => panic!("{other:?}"),
        };
        s.effects.dispatching(&id).await.unwrap();

        let d = Daemon::from_services(s.clone());
        let r = d.recover().await.unwrap();
        assert_eq!(r.turns_requeued, 1);
        assert_eq!(r.effects_unknown, 1);
        assert!(!r.is_clean(), "un effet incertain doit être signalé");

        // Une demande HITL a été créée, l'effet n'a pas été rejoué.
        let pending = s.approvals.pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, penelope_hitl::ApprovalKind::EffectUnknown);

        // Le tour est de nouveau réclamable.
        assert!(s.turns.claim("runner-neuf").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn config_generations_are_published_to_every_subsystem() {
        let (_d, s) = services().await;
        let d = Daemon::from_services(s.clone());
        let generation = d
            .publish_config("cli", |c| {
                c.budget.daily_usd = 42.0;
                Ok(vec!["budget.daily_usd".into()])
            })
            .unwrap();
        assert_eq!(generation, 2);
        let results = s.config.apply_results();
        for sub in SUBSYSTEMS {
            assert_eq!(
                results.get(*sub),
                Some(&ApplyResult::AppliedLive { generation: 2 }),
                "sous-système non notifié : {sub}"
            );
        }
        assert_eq!(s.config.config().budget.daily_usd, 42.0);
    }

    #[tokio::test]
    async fn invalid_config_change_publishes_nothing() {
        let (_d, s) = services().await;
        let d = Daemon::from_services(s.clone());
        assert!(
            d.publish_config("cli", |c| {
                c.runners.count = 0;
                Ok(vec![])
            })
            .is_err()
        );
        assert_eq!(s.config.generation(), 1);
    }

    #[tokio::test]
    async fn handle_controls_shutdown_and_restart() {
        let (_d, s) = services().await;
        let d = Daemon::from_services(s);
        assert!(!d.handle.is_shutting_down());
        d.handle.record_turn();
        assert_eq!(d.handle.turns_done(), 1);
        d.handle.request_restart();
        assert!(d.handle.is_shutting_down() && d.handle.wants_restart());
    }

    #[tokio::test]
    async fn workflow_known_lists_native_tools_and_templates() {
        let (_d, s) = services().await;
        let k = workflow_known(&s.config.config(), &s.mcp_tools).await;
        assert!(k.native_tools.contains("fs_read"));
        assert!(k.templates.contains("deploy_gate"));
        assert!(k.model_aliases.contains("main"));
        assert_eq!(k.max_depth, 3);
    }
}
