//! Ports du daemon : ce qu'un module reçoit au lieu de `&Daemon` (épopée #208, lot D,
//! `design/v1/decoupage-daemon.md` §2.2).

use penelope_kernel::clock::SharedClock;
use penelope_kernel::event::EventLog;
use penelope_llm::Provider;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Providers des modèles, construits à la demande (`Daemon::provider_for` jusqu'ici).
#[async_trait::async_trait]
pub trait ProviderSource: Send + Sync {
    /// Provider d'un modèle ; l'erreur est lisible par le propriétaire.
    async fn provider_for(&self, model_id: &str) -> Result<Arc<dyn Provider>, String>;
    /// Provider imposé (tests, suites sans réseau), s'il y en a un.
    fn provider_override_active(&self) -> Option<Arc<dyn Provider>>;
}

/// Ce qu'une boucle de fond surveillée reçoit (`tasks::spawn_supervised`) : le registre
/// des boucles, le signal d'arrêt, l'horloge et le journal d'audit.
#[derive(Clone)]
pub struct Supervision {
    pub tasks: Arc<crate::tasks::Tasks>,
    pub handle: Handle,
    pub clock: SharedClock,
    pub events: EventLog,
}

/// Poignée de contrôle du daemon : signal d'arrêt et de redémarrage, compteurs.
#[derive(Clone)]
pub struct Handle {
    shutdown: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
    started_at_ms: i64,
    turns_done: Arc<AtomicU64>,
}

impl Handle {
    pub fn new(started_at_ms: i64) -> Handle {
        Handle {
            shutdown: Arc::new(AtomicBool::new(false)),
            restart: Arc::new(AtomicBool::new(false)),
            started_at_ms,
            turns_done: Arc::new(AtomicU64::new(0)),
        }
    }
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
