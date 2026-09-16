//! `penelope-daemon` : composition, supervision des tâches, arrêt propre (§3.3).

#![forbid(unsafe_code)]

pub mod agent;
pub mod doctor;
pub mod rpc;
pub mod runtime;

pub use agent::{AgentLoop, TurnOutcome};
pub use runtime::{Daemon, DaemonHandle, Services};

/// Version du daemon.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
