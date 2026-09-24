//! Ports de la boucle (épopée #208, T09) : ce qu'un tour reçoit du reste du daemon.
//!
//! `AgentServices` remplace `Services` dans la boucle : les registres du kernel, du LLM
//! et du HITL qu'elle lit et écrit directement, plus cinq traits pour ce qui reste au
//! daemon (mode d'approbation d'une session, nature d'une session, instantanés du
//! prompt, dernier appel de la session, jobs d'outils). Chaque trait a une
//! implémentation unique dans le daemon ; `AgentServices::for_tests` en donne une en
//! mémoire, sur `Store::open_memory`, pour que la boucle se teste sans daemon.

use super::{ApprovalMode, PreviousCall, ToolExecutor};
use penelope_app::conversation::PromptPrefix;
use penelope_hitl::{ApprovalStore, PolicyEngine};
use penelope_kernel::budget::BudgetLedger;
use penelope_kernel::clock::SharedClock;
use penelope_kernel::config::{Config, ConfigStore};
use penelope_kernel::effects::EffectLedger;
use penelope_kernel::event::EventLog;
use penelope_kernel::ids::EffectId;
use penelope_kernel::session::{SessionKind, SessionStore};
use penelope_llm::types::ToolCall;
use penelope_llm::{Catalog, LlmStateMachine};
use penelope_store::Store;
use penelope_tools::ToolOutcome;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Ce que la boucle reçoit : les registres qu'elle touche et ses ports.
#[derive(Clone)]
pub struct AgentServices {
    pub store: Store,
    pub clock: SharedClock,
    pub events: EventLog,
    pub effects: EffectLedger,
    pub config: Arc<ConfigStore>,
    pub budget: BudgetLedger,
    pub catalog: Catalog,
    pub llm_state: LlmStateMachine,
    pub approvals: ApprovalStore,
    pub policies: PolicyEngine,
    pub modes: Arc<dyn SessionModes>,
    pub sessions: Arc<dyn SessionInfo>,
    pub snapshots: Arc<dyn PromptSnapshots>,
    pub cache: Arc<dyn CacheAudit>,
    pub jobs: Arc<dyn JobRunner>,
}

/// Mode d'approbation d'une session (issue #111).
#[async_trait::async_trait]
pub trait SessionModes: Send + Sync {
    /// Le mode de la session s'il a été choisi, sinon celui de la configuration.
    async fn of_session(&self, session_id: &str) -> ApprovalMode;
}

/// Ce que la boucle sait d'une session : sa nature (le budget suspend une conversation
/// du propriétaire, il arrête le reste).
#[async_trait::async_trait]
pub trait SessionInfo: Send + Sync {
    async fn kind(&self, session_id: &str) -> anyhow::Result<Option<SessionKind>>;
}

#[async_trait::async_trait]
impl SessionInfo for SessionStore {
    async fn kind(&self, session_id: &str) -> anyhow::Result<Option<SessionKind>> {
        Ok(self.get(session_id).await?.map(|s| s.kind))
    }
}

/// Instantanés du prompt système (issue #205).
#[async_trait::async_trait]
pub trait PromptSnapshots: Send + Sync {
    /// Enregistre le prompt envoyé sous l'empreinte déjà calculée ; `false` : rien écrit.
    async fn record(&self, hash: &str, prefix: &PromptPrefix) -> anyhow::Result<bool>;
    /// Cause « préfixe » précisée par les tuiles qui ont bougé (`prefixe:T1+T2`).
    async fn prefix_cause(&self, before: Option<&str>, after: &str) -> String;
}

/// Mémoire du cache de prompt (issue #17) : le dernier appel de conversation de la
/// session, auquel la requête suivante se compare.
#[async_trait::async_trait]
pub trait CacheAudit: Send + Sync {
    async fn previous_call(&self, session_id: &str) -> anyhow::Result<Option<PreviousCall>>;
}

/// Un appel qui peut partir en arrière-plan (issue #204).
pub struct JobRequest<'a> {
    pub session_id: &'a str,
    pub run_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub call: &'a ToolCall,
    /// Nom effectif de l'outil, après normalisation (`tool_call` compris).
    pub tool: &'a str,
    /// Effet déjà planifié, encore `planned` : c'est le job qui le passera
    /// `dispatching` puis à son état final (§4.2).
    pub effect: &'a EffectId,
}

/// Jobs d'outils (issue #204).
#[async_trait::async_trait]
pub trait JobRunner: Send + Sync {
    /// Transforme l'appel en job s'il le demande. `None` : l'appel suit le chemin
    /// ordinaire.
    async fn maybe_spawn(
        &self,
        execute: &(dyn ToolExecutor + Send + Sync),
        req: JobRequest<'_>,
    ) -> anyhow::Result<Option<ToolOutcome>>;
}

/// Aucun job : chaque appel suit le chemin ordinaire. Pour les tests, et pour les
/// décisions du propriétaire, qui ne lancent aucun appel.
pub struct NoJobs;

#[async_trait::async_trait]
impl JobRunner for NoJobs {
    async fn maybe_spawn(
        &self,
        _execute: &(dyn ToolExecutor + Send + Sync),
        _req: JobRequest<'_>,
    ) -> anyhow::Result<Option<ToolOutcome>> {
        Ok(None)
    }
}

/// Modes en mémoire, repli sur `tools.approval_mode` : l'implémentation des tests.
pub struct MemoryModes {
    config: Arc<ConfigStore>,
    own: Mutex<HashMap<String, ApprovalMode>>,
}

impl MemoryModes {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        MemoryModes {
            config,
            own: Mutex::new(HashMap::new()),
        }
    }

    /// Fixe le mode d'une session ; `None` : retour au mode de la configuration.
    pub fn set(&self, session_id: &str, mode: Option<ApprovalMode>) {
        let mut own = self.own.lock().unwrap_or_else(|e| e.into_inner());
        match mode {
            Some(m) => own.insert(session_id.to_string(), m),
            None => own.remove(session_id),
        };
    }
}

#[async_trait::async_trait]
impl SessionModes for MemoryModes {
    async fn of_session(&self, session_id: &str) -> ApprovalMode {
        let own = self
            .own
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .copied();
        own.or_else(|| ApprovalMode::parse(&self.config.config().tools.approval_mode))
            .unwrap_or(ApprovalMode::Reads)
    }
}

/// Ni instantané ni appel précédent : les tests de la boucle n'en lisent pas.
pub struct NoAudit;

#[async_trait::async_trait]
impl PromptSnapshots for NoAudit {
    async fn record(&self, _hash: &str, _prefix: &PromptPrefix) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn prefix_cause(&self, _before: Option<&str>, _after: &str) -> String {
        "prefixe".to_string()
    }
}

#[async_trait::async_trait]
impl CacheAudit for NoAudit {
    async fn previous_call(&self, _session_id: &str) -> anyhow::Result<Option<PreviousCall>> {
        Ok(None)
    }
}

impl AgentServices {
    /// Services de test sur une base en mémoire : configuration d'exemple (écrite, si un
    /// test la change, sous `dir`), modes en mémoire, ni instantané ni job.
    pub fn for_tests(dir: &Path, clock: SharedClock) -> anyhow::Result<AgentServices> {
        let store = Store::open_memory()?;
        let mut sample = Config::sample(42);
        sample.memory.review_max_candidates = 0;
        sample.context.auto_title = false;
        if !cfg!(target_os = "macos") {
            sample.sandbox.default_profile = "full".into();
        }
        let config = Arc::new(ConfigStore::new(
            sample,
            dir.join("config.toml"),
            Some(store.clone()),
            clock.clone(),
        ));
        let events = EventLog::new(store.clone(), clock.clone());
        Ok(AgentServices {
            effects: EffectLedger::new(store.clone(), clock.clone()),
            budget: BudgetLedger::new(store.clone(), clock.clone())
                .with_events(events.clone())
                .with_timezone({
                    let config = config.clone();
                    move || config.config().owner.timezone.clone()
                }),
            llm_state: LlmStateMachine::new(store.clone(), clock.clone()),
            approvals: ApprovalStore::new(store.clone(), clock.clone()).with_events(events.clone()),
            policies: PolicyEngine::new(store.clone(), clock.clone()),
            sessions: Arc::new(
                SessionStore::new(store.clone(), clock.clone()).with_events(events.clone()),
            ),
            modes: Arc::new(MemoryModes::new(config.clone())),
            snapshots: Arc::new(NoAudit),
            cache: Arc::new(NoAudit),
            jobs: Arc::new(NoJobs),
            catalog: Catalog::new(),
            events,
            store,
            clock,
            config,
        })
    }
}
