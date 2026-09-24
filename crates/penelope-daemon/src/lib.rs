//! `penelope-daemon` : composition, supervision des tâches, arrêt propre (§3.3).

#![forbid(unsafe_code)]

pub mod agent;
pub mod approval_mode;
pub mod audit;
pub mod budget_alert;
pub mod cache_audit;
pub mod compaction;
pub mod conversation;
pub mod dream;
pub mod engine;
pub mod executor;
pub mod history;
pub mod images;
pub mod ingest;
pub mod prompt_snapshot;
pub mod purge;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod runtime_events;
pub mod scheduler;
pub mod selfdocs;
pub mod selfknow;
pub mod session_ops;
pub mod supervisor;
pub mod titles;
pub mod tool_jobs;
pub mod tools_on_demand;
pub mod vision;
pub mod voice;
#[cfg(test)]
mod wiki_e2e;
pub mod workflow;

// Modules descendus dans `penelope-app` (T21), réexportés sous leur ancien chemin.
pub use penelope_app::{
    bus, codex_scope, elicitation, helpers, machine, media, ports, tasks, testing,
};
// Hôte MCP sorti dans `penelope-mcp-host` (T25), réexporté sous ses anciens chemins
// jusqu'à T30.
pub use penelope_mcp_host as mcp;
pub use penelope_mcp_host::auth as mcp_auth;

// Modules descendus dans `penelope-vault` (T22), réexportés sous leur ancien chemin.
pub use penelope_vault::{
    concepts, embeddings, episodes, mem_audit, mem_split, review, secret_shelf, session_notes,
    session_project, usage_feedback, vault_git, vault_inventory, vault_ops,
};

// Exploitation sortie dans `penelope-ops` (T28), réexportée sous ses anciens chemins
// jusqu'à T30.
pub use penelope_ops::{
    backup, codex_auth, codex_quota, doctor, hermes, skill_deps, skill_install, upgrade,
};

// Mémoire qui mûrit sortie dans `penelope-dream` (T26), réexportée sous ses anciens
// chemins jusqu'à T30.
pub use penelope_dream::onboarding;

pub use agent::{AgentLoop, TurnOutcome};
pub use ports::Handle;
pub use runtime::{Daemon, Services};

/// Version du daemon.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
