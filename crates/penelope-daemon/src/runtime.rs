//! Composition du daemon : tout est câblé ici, une seule fois (§3.1).
//!
//! Les crates métier ne se connaissent pas entre elles au-delà de leurs contrats : c'est
//! ce module qui les assemble, et lui seul.

use penelope_kernel::clock::{SharedClock, SystemClock};
use penelope_kernel::config::Config;
use penelope_kernel::event::EventDraft;
use std::path::PathBuf;
use std::sync::Arc;

pub use crate::ports::Handle;
use crate::ports::{McpAdmin, ProviderSource, Slot};
// `Services` et ce qu'il assemble sont descendus dans `penelope-app` (T21) : réexportés
// sous leur ancien chemin.
pub use penelope_app::services::{
    BUNDLED_SKILLS, SUBSYSTEMS, Services, reload_skills, workflow_known, workflow_known_with,
};

/// Le daemon.
pub struct Daemon {
    pub services: Arc<Services>,
    pub handle: Handle,
    /// Événements des tours, attentes de réponse, tours actifs.
    pub bus: Arc<crate::bus::Bus>,
    /// Branchements optionnels : canal de message, MCP, orchestration.
    pub hooks: Hooks,
    /// Compactions de fond en cours et demandées (§5.4).
    pub compaction: Arc<crate::compaction::State>,
    /// Runs de workflow pilotés par ce processus (§12.7).
    pub workflows: Arc<crate::workflow::State>,
    /// Calcul des embeddings : dernier échec, rattrapage en cours (issue #11).
    pub embeddings: Arc<crate::embeddings::State>,
    /// Boucles de fond surveillées : vivantes, paniques, relances (issue #84).
    pub tasks: Arc<crate::tasks::Tasks>,
    /// Providers construits à la demande (la clé peut arriver après le démarrage).
    pub providers: Arc<Providers>,
}

/// Providers des modèles : construits à la première demande, reconstruits après
/// [`Providers::invalidate`], ou imposés par les tests.
pub struct Providers {
    services: Arc<Services>,
    set: tokio::sync::Mutex<Option<Arc<penelope_llm::ProviderSet>>>,
    /// Provider imposé, pour les tests et les suites sans réseau.
    provider_override: std::sync::RwLock<Option<Arc<dyn penelope_llm::Provider>>>,
}

impl Providers {
    pub fn new(services: Arc<Services>) -> Providers {
        Providers {
            services,
            set: tokio::sync::Mutex::new(None),
            provider_override: std::sync::RwLock::new(None),
        }
    }

    /// Impose un provider pour tous les modèles (tests, suites sans réseau).
    pub fn set_override(&self, p: Arc<dyn penelope_llm::Provider>) {
        if let Ok(mut g) = self.provider_override.write() {
            *g = Some(p);
        }
    }

    /// Oublie les providers construits : la prochaine demande les reconstruit avec la
    /// configuration et les secrets du moment.
    pub async fn invalidate(&self) {
        *self.set.lock().await = None;
    }
}

#[async_trait::async_trait]
impl crate::ports::ProviderSource for Providers {
    /// Provider d'un modèle. La construction est retentée tant qu'elle échoue : une clé
    /// posée après le démarrage est prise en compte au tour suivant, sans redémarrage.
    async fn provider_for(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        if let Some(p) = self.provider_override.read().ok().and_then(|g| g.clone()) {
            return Ok(p);
        }
        let mut guard = self.set.lock().await;
        if guard.is_none() {
            let s = &self.services;
            let cfg = s.config.config();
            // Le fournisseur Codex ne vit que si un compte ChatGPT est connecté : c'est
            // le daemon qui tient les jetons et leur rotation (issue #142).
            let codex = match crate::codex_auth::load(s) {
                Ok(Some(g)) if g.disconnected.is_none() => Some(penelope_llm::CodexAccess {
                    tokens: Arc::new(crate::codex_auth::DaemonTokens::new(s.clone())),
                    installation_id: crate::codex_auth::installation_id(s).await,
                    quota_sink: Some(Arc::new(crate::codex_quota::QuotaWriter::new(s.clone()))),
                }),
                _ => None,
            };
            let set = penelope_llm::build_providers(
                &cfg,
                s.platform.secrets.as_ref(),
                s.catalog.clone(),
                codex,
            )
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

    fn provider_override_active(&self) -> Option<Arc<dyn penelope_llm::Provider>> {
        self.provider_override.read().ok().and_then(|g| g.clone())
    }
}

/// Points de branchement des sous-systèmes qui démarrent après le daemon.
#[derive(Default)]
pub struct Hooks {
    pub messenger: Slot<dyn crate::executor::Messenger>,
    pub mcp: Slot<dyn crate::executor::McpGateway>,
    pub orchestrator: Slot<dyn crate::executor::Orchestrator>,
    /// Livraison durable des tours (la passerelle du canal).
    pub delivery: Slot<dyn crate::bus::ChannelDelivery>,
    /// Administration des serveurs MCP (`mcp.*`), par son port.
    pub mcp_supervisor: Slot<dyn McpAdmin>,
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
        self.delivery()
    }
    /// Le canal de livraison branché, sous le nom de son port.
    pub fn delivery(&self) -> Option<Arc<dyn crate::bus::ChannelDelivery>> {
        self.delivery.get()
    }
    pub fn mcp_supervisor(&self) -> Option<Arc<dyn McpAdmin>> {
        self.mcp_supervisor.read().ok().and_then(|g| g.clone())
    }
    /// Branchements du moteur de workflows.
    pub fn workflow(&self) -> crate::workflow::Ports {
        crate::workflow::Ports {
            messenger: self.messenger.clone(),
            mcp: self.mcp.clone(),
            orchestrator: self.orchestrator.clone(),
            mcp_supervisor: self.mcp_supervisor.clone(),
        }
    }
    /// Branchements de l'ordonnanceur.
    pub fn scheduler(&self) -> crate::scheduler::Ports {
        crate::scheduler::Ports {
            messenger: self.messenger.clone(),
            delivery: self.delivery.clone(),
            mcp: self.mcp_supervisor.clone(),
            orchestrator: self.orchestrator.clone(),
        }
    }
    /// Branche un superviseur MCP : passerelle des outils et administration.
    pub fn set_mcp<T: McpAdmin + 'static>(&self, sup: Arc<T>) {
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
        let hooks = Hooks::default();
        Daemon {
            handle: Handle::new(started_at_ms),
            bus: Arc::new(crate::bus::Bus::new()),
            compaction: Arc::new(crate::compaction::State::with_messenger(
                hooks.messenger.clone(),
            )),
            workflows: Arc::new(crate::workflow::State::with_ports(hooks.workflow())),
            embeddings: Arc::default(),
            tasks: Arc::new(crate::tasks::Tasks::default()),
            providers: Arc::new(Providers::new(services.clone())),
            services,
            hooks,
        }
    }

    /// Calcul des embeddings, vu des modules qui ne tiennent pas le daemon.
    pub fn embedder(&self) -> crate::embeddings::Embedder {
        crate::embeddings::Embedder {
            services: self.services.clone(),
            providers: self.providers.clone(),
            state: self.embeddings.clone(),
        }
    }

    /// Contexte du rêve et de l'ingestion (`penelope-dream`, T26) : ce qu'ils lisent du
    /// daemon, sans le daemon.
    pub fn dream(&self) -> penelope_dream::Context {
        penelope_dream::Context {
            services: self.services.clone(),
            providers: self.providers.clone(),
            embeddings: self.embeddings.clone(),
        }
    }

    /// Contexte des boucles de fond surveillées.
    pub fn supervision(&self) -> crate::ports::Supervision {
        crate::ports::Supervision {
            tasks: self.tasks.clone(),
            handle: self.handle.clone(),
            clock: self.services.clock.clone(),
            events: self.services.events.clone(),
        }
    }

    pub fn provider_override_active(&self) -> Option<Arc<dyn penelope_llm::Provider>> {
        self.providers.provider_override_active()
    }

    /// Impose un provider pour tous les modèles (tests, suites sans réseau).
    pub fn set_provider_override(&self, p: Arc<dyn penelope_llm::Provider>) {
        self.providers.set_override(p);
    }

    pub async fn invalidate_providers(&self) {
        self.providers.invalidate().await;
    }

    pub async fn provider_for(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        self.providers.provider_for(model_id).await
    }

    /// Reprise au démarrage (§17) : leases, effets, requêtes LLM, tâches MCP, runs.
    pub async fn recover(&self) -> anyhow::Result<RecoveryReport> {
        let s = &self.services;

        crate::history::seal_legacy(s).await?;
        let turns = s.turns.recover_on_boot().await?;
        crate::agent::close_interrupted_turns(s).await?;
        // Avant les effets : un job dont le processus est mort devient `failed` sans être
        // relancé, son effet aussi, et aucune carte `effect_unknown` (décision 0012).
        let lost_jobs = crate::tool_jobs::store(s).recover_on_boot().await?;
        if !lost_jobs.is_empty() {
            tracing::warn!(
                count = lost_jobs.len(),
                "jobs d'outils perdus au redémarrage"
            );
        }
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
            tool_jobs_lost: lost_jobs.len() as u64,
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
        self.services.publish_config(source, mutate)
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
            outbox_failed: s
                .store
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT count(*) FROM tg_outbox WHERE state = 'failed'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .await? as u64,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryReport {
    pub turns_requeued: u64,
    pub effects_unknown: u64,
    pub llm_unknown: u64,
    pub runs_resumed: u64,
    pub mcp_orphans_killed: u64,
    /// Jobs d'outils que le redémarrage a interrompus (issue #204).
    #[serde(default)]
    pub tool_jobs_lost: u64,
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
        if let Ok(s) = std::fs::read_to_string("/proc/self/statm")
            && let Some(pages) = s
                .split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<f64>().ok())
        {
            return pages * 4096.0 / 1_048_576.0;
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_kernel::config::ApplyResult;

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        (dir, Arc::new(s))
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
}
