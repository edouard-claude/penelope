//! `penelope-daemon` : composition, supervision des tâches, arrêt propre (§3.3).

#![forbid(unsafe_code)]

pub mod agent;
pub mod approval_mode;
pub mod audit;
pub mod cache_audit;
pub mod compaction;
#[cfg(test)]
mod dream;
pub mod engine;
#[cfg(test)]
mod executor;
pub mod history;
#[cfg(test)]
mod ingest;
pub mod prompt_snapshot;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod runtime_events;
pub mod selfknow;
pub mod supervisor;
pub mod tool_jobs;
#[cfg(test)]
mod wiki_e2e;
pub mod workflow;

/// Version du daemon.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
