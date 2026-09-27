//! Tous les services, assemblés une fois : ce que chaque module reçoit en `&Services`.
//!
//! Descendus de `penelope-daemon/src/runtime.rs` (épopée #208, T21) : la composition
//! reste au daemon, les services qu'elle assemble vivent ici.

use penelope_context::{ContextEngine, HistoryStore, Lcm};
use penelope_hitl::{ApprovalStore, PolicyEngine};
use penelope_kernel::budget::BudgetLedger;
use penelope_kernel::clock::SharedClock;
use penelope_kernel::config::{ApplyResult, Config, ConfigStore};
use penelope_kernel::effects::EffectLedger;
use penelope_kernel::event::EventLog;
use penelope_kernel::session::SessionStore;
use penelope_kernel::turn::TurnQueue;
use penelope_llm::{Catalog, LlmStateMachine, TokenEstimator};
use penelope_mcp::ToolRegistry;
use penelope_memory::{CandidateStore, IntentStore, MemoryIndex};
use penelope_platform::Platform;
use penelope_skills::SkillRegistry;
use penelope_store::Store;
use penelope_workflow::{RunStore, ScheduleStore, WorkflowRegistry};
use std::path::PathBuf;
use std::sync::Arc;

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
    /// Le canal du propriétaire, branché par sa passerelle (T36) : gabarits de cartes.
    pub channel: crate::channel::Channel,
    pub workflows: WorkflowRegistry,
    pub runs: RunStore,
    pub schedules: ScheduleStore,
    /// Demandes des serveurs MCP au propriétaire (§8.4, issue #12).
    pub elicitations: Arc<crate::elicitation::Broker>,
    /// Jobs d'outils qui tournent dans ce processus (issue #204) : leur jeton
    /// d'annulation, pour que `/stop` les traverse comme il traverse un outil.
    pub jobs: Arc<crate::jobs::Running>,
}

impl Services {
    /// Construit tous les services à partir d'une racine. `cards` : les cartes du canal,
    /// passées par la composition, pour valider les workflows dès leur chargement.
    pub async fn bootstrap(
        home: Option<PathBuf>,
        clock: SharedClock,
        cards: Option<crate::channel::CardsOf>,
    ) -> anyhow::Result<Services> {
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
        let sessions = SessionStore::new(store.clone(), clock.clone()).with_events(events.clone());
        let lease_ttl = penelope_kernel::config::parse_duration(&cfg.runners.lease_ttl)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(60_000);
        let turns = TurnQueue::new(store.clone(), clock.clone(), lease_ttl);
        let budget = BudgetLedger::new(store.clone(), clock.clone())
            .with_events(events.clone())
            .with_timezone({
                let config = config.clone();
                move || config.config().owner.timezone.clone()
            });

        let catalog = Catalog::new();
        let llm_state = LlmStateMachine::new(store.clone(), clock.clone());
        let estimator = TokenEstimator::new();
        let history = HistoryStore::new(store.clone(), clock.clone(), events.clone());
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

        let approvals =
            ApprovalStore::new(store.clone(), clock.clone()).with_events(events.clone());
        let policies = PolicyEngine::new(store.clone(), clock.clone());
        let channel = crate::channel::Channel::default();
        if let Some(cards) = cards {
            channel
                .cards
                .set(Some(cards(&dirs.templates(), store.clone())));
        }

        let known = workflow_known(&cfg, &mcp_tools, &channel).await;
        let workflows = WorkflowRegistry::with_bundled(&known);
        workflows.load_dir(
            &dirs.workflows(),
            penelope_workflow::registry::Scope::User,
            &known,
        );
        // Second passage : un workflow utilisateur peut en appeler un autre du même dossier.
        let known = workflow_known_with(&cfg, &mcp_tools, &workflows, &channel).await;
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
            channel,
            workflows,
            runs,
            schedules,
            elicitations: Arc::default(),
            jobs: Arc::default(),
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
        // Sans bac à sable sur cette plateforme, le profil imposé refuserait toute commande
        // (échec fermé) : les tests de logique (condensé de tests, ledger, e2e) y tournent
        // sans profil. Sur macOS, ils gardent Seatbelt (issue #102).
        if !cfg!(target_os = "macos") {
            sample.sandbox.default_profile = "full".into();
        }
        let config = Arc::new(ConfigStore::new(
            sample,
            platform.dirs.config_file(),
            Some(store.clone()),
            clock.clone(),
        ));
        let cfg = config.config();
        let catalog = Catalog::new();
        let events = EventLog::new(store.clone(), clock.clone());
        let context = ContextEngine::new(
            HistoryStore::new(store.clone(), clock.clone(), events.clone()),
            Lcm::new(store.clone(), clock.clone()),
            TokenEstimator::new(),
            catalog.clone(),
            clock.clone(),
        );
        let mcp_tools = ToolRegistry::new(store.clone(), 30, 8192, 65536);
        let channel = crate::channel::Channel::default();
        let known = workflow_known(&cfg, &mcp_tools, &channel).await;

        Ok(Services {
            events: events.clone(),
            effects: EffectLedger::new(store.clone(), clock.clone()),
            sessions: SessionStore::new(store.clone(), clock.clone()).with_events(events.clone()),
            turns: TurnQueue::new(store.clone(), clock.clone(), 60_000),
            budget: BudgetLedger::new(store.clone(), clock.clone())
                .with_events(events.clone())
                .with_timezone({
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
            approvals: ApprovalStore::new(store.clone(), clock.clone()).with_events(events.clone()),
            policies: PolicyEngine::new(store.clone(), clock.clone()),
            channel,
            workflows: WorkflowRegistry::with_bundled(&known),
            runs: RunStore::new(store.clone(), clock.clone()),
            schedules: ScheduleStore::new(store.clone(), clock.clone(), "Indian/Reunion"),
            elicitations: Arc::default(),
            jobs: Arc::default(),
            mcp_tools,
            context,
            catalog,
            config,
            clock,
            store,
            platform,
        })
    }

    /// Lit une valeur de la table `kv`. Seule famille d'accès du daemon (épopée #208, T05).
    pub async fn kv_get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let k = key.to_string();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare("SELECT v FROM kv WHERE k = ?1")?;
                let mut rows = st.query([&k])?;
                Ok(match rows.next()? {
                    Some(r) => Some(r.get::<_, String>(0)?),
                    None => None,
                })
            })
            .await?)
    }

    pub async fn kv_delete(&self, key: &str) -> anyhow::Result<()> {
        let k = key.to_string();
        self.store
            .write(move |tx| {
                tx.execute("DELETE FROM kv WHERE k = ?1", [k])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn kv_set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let (k, v) = (key.to_string(), value.to_string());
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO kv(k, v, ts)
                     VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
                    penelope_store::rusqlite::params![k, v],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Applique une modification de configuration et la déclare appliquée à chaud par
    /// chaque sous-système.
    pub fn publish_config<F>(&self, source: &str, mutate: F) -> anyhow::Result<u64>
    where
        F: FnOnce(&mut Config) -> penelope_kernel::Result<Vec<String>>,
    {
        let g = self.config.mutate(source, mutate)?;
        for subsystem in SUBSYSTEMS {
            self.config.record_apply(
                subsystem,
                ApplyResult::AppliedLive {
                    generation: g.generation,
                },
            );
        }
        Ok(g.generation)
    }
}

/// Ce que la validation des workflows doit connaître. Les gabarits de cartes viennent
/// du canal branché (T36) ; sans canal, ils ne sont pas vérifiés.
pub async fn workflow_known(
    cfg: &Config,
    mcp_tools: &ToolRegistry,
    channel: &crate::channel::Channel,
) -> penelope_workflow::Known {
    let mut known = penelope_workflow::Known {
        model_aliases: cfg.models.aliases.keys().cloned().collect(),
        native_tools: penelope_tools::all_tools()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect(),
        templates: channel.catalog().into_iter().collect(),
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
    channel: &crate::channel::Channel,
) -> penelope_workflow::Known {
    let mut known = workflow_known(cfg, mcp_tools, channel).await;
    known.workflow_ids.extend(workflows.ids());
    known
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
        // Réécrite seulement si elle a changé : une réécriture identique déplaçait sa date
        // de modification et relançait le rechargement suivant, chaque minute (#118).
        let path = dir.join("SKILL.md");
        if std::fs::read(&path).ok().as_deref() != Some(body.as_bytes()) {
            penelope_kernel::config::atomic_write(&path, body.as_bytes())?;
        }
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
        assert_eq!(s.store.integrity().unwrap(), "ok");
    }

    /// Un canal qui ne sait rendre qu'un gabarit.
    struct OneCard;

    #[async_trait::async_trait]
    impl crate::channel::Cards for OneCard {
        fn catalog(&self) -> Vec<String> {
            vec!["deploy_gate".into()]
        }
        fn template(&self, _id: &str) -> Option<crate::channel::CardTemplate> {
            None
        }
    }

    #[tokio::test]
    async fn workflow_known_lists_native_tools_and_the_channel_templates() {
        let (_d, s) = services().await;
        let k = workflow_known(&s.config.config(), &s.mcp_tools, &s.channel).await;
        assert!(
            k.templates.is_empty(),
            "sans canal, pas de gabarit à vérifier"
        );
        s.channel.cards.set(Some(Arc::new(OneCard)));
        let k = workflow_known(&s.config.config(), &s.mcp_tools, &s.channel).await;
        assert!(k.native_tools.contains("fs_read"));
        assert!(k.templates.contains("deploy_gate"));
        assert!(k.model_aliases.contains("main"));
        assert_eq!(k.max_depth, 3);
    }
}
