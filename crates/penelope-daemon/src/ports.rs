//! Ports du daemon : ce qu'un module reçoit au lieu de `&Daemon` (épopée #208, lot D,
//! `design/v1/decoupage-daemon.md` §2.2).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Poignée de contrôle du daemon.
#[derive(Clone)]
pub struct DaemonHandle {
    shutdown: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
    started_at_ms: i64,
    turns_done: Arc<AtomicU64>,
}

impl DaemonHandle {
    pub fn new(started_at_ms: i64) -> DaemonHandle {
        DaemonHandle {
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
