//! `penelope-daemon` : composition, supervision des tâches, arrêt propre (§3.3).

#![forbid(unsafe_code)]

pub mod agent;
pub mod bus;
pub mod conversation;
pub mod doctor;
pub mod engine;
pub mod executor;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod selfknow;
pub mod supervisor;
pub mod telegram;
pub mod vault_ops;

pub use agent::{AgentLoop, TurnOutcome};
pub use runtime::{Daemon, DaemonHandle, Services};

/// Version du daemon.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
