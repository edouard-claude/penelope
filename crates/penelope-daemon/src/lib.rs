//! `penelope-daemon` : composition, supervision des tâches, arrêt propre (§3.3).

#![forbid(unsafe_code)]

pub mod agent;
pub mod budget_alert;
pub mod bus;
pub mod cache_audit;
pub mod compaction;
pub mod conversation;
pub mod doctor;
pub mod dream;
pub mod elicitation;
pub mod engine;
pub mod episodes;
pub mod executor;
pub mod hermes;
pub mod images;
pub mod ingest;
pub mod mcp;
pub mod mcp_auth;
pub mod media;
pub mod review;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod scheduler;
pub mod selfknow;
pub mod session_ops;
pub mod supervisor;
pub mod telegram;
#[cfg(test)]
mod ticket_to_deploy_e2e;
pub mod titles;
pub mod upgrade;
pub mod vault_ops;
pub mod workflow;

pub use agent::{AgentLoop, TurnOutcome};
pub use runtime::{Daemon, DaemonHandle, Services};

/// Version du daemon.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
