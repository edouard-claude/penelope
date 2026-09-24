//! `penelope-daemon` : composition, supervision des tâches, arrêt propre (§3.3).

#![forbid(unsafe_code)]

pub mod agent;
pub mod approval_mode;
pub mod audit;
pub mod backup;
pub mod budget_alert;
pub mod bus;
pub mod cache_audit;
pub mod codex_auth;
pub mod codex_quota;
pub mod codex_scope;
pub mod compaction;
pub mod concepts;
pub mod conversation;
pub mod doctor;
pub mod dream;
pub mod elicitation;
pub mod embeddings;
pub mod engine;
pub mod episodes;
pub mod executor;
pub mod helpers;
pub mod hermes;
pub mod history;
pub mod images;
pub mod ingest;
pub mod machine;
pub mod mcp;
pub mod mcp_auth;
pub mod media;
pub mod mem_audit;
pub mod mem_split;
pub mod onboarding;
pub mod ports;
pub mod prompt_snapshot;
pub mod purge;
pub mod review;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod runtime_events;
pub mod scheduler;
pub mod secret_shelf;
pub mod selfdocs;
pub mod selfknow;
pub mod session_notes;
pub mod session_ops;
pub mod session_project;
pub mod skill_deps;
pub mod skill_install;
pub mod supervisor;
pub mod tasks;
pub mod telegram;
#[cfg(test)]
mod ticket_to_deploy_e2e;
pub mod titles;
pub mod tool_jobs;
pub mod tools_on_demand;
pub mod upgrade;
pub mod usage_feedback;
pub mod vault_git;
pub mod vault_inventory;
pub mod vault_ops;
pub mod vision;
pub mod voice;
#[cfg(test)]
mod wiki_e2e;
pub mod workflow;

pub use agent::{AgentLoop, TurnOutcome};
pub use ports::Handle;
pub use runtime::{Daemon, Services};

/// Version du daemon.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
