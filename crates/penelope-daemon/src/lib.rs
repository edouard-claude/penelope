//! `penelope-daemon` : composition, supervision des tâches, arrêt propre (§3.3).

#![forbid(unsafe_code)]

pub mod agent;
pub mod approval_mode;
pub mod audit;
pub mod cache_audit;
pub mod compaction;
pub mod dream;
pub mod engine;
pub mod executor;
pub mod history;
pub mod ingest;
pub mod prompt_snapshot;
pub mod purge;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod runtime_events;
pub mod scheduler;
pub mod selfknow;
pub mod session_ops;
pub mod supervisor;
pub mod tool_jobs;
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

// Exécuteur des outils natifs sorti dans `penelope-executor` (T24), réexporté sous ses
// anciens chemins jusqu'à T30 ; `executor`, `selfknow` et `tool_jobs` en restent les
// façades.
pub use penelope_executor::{images, selfdocs, tools_on_demand, vision, voice};
// Conversation de session, compaction, titres et alerte de budget sortis dans
// `penelope-conversation` (T23), réexportés sous leurs anciens chemins jusqu'à T30 ;
// `compaction` garde une façade (`compaction::context_of`).
pub use penelope_conversation as conversation;
pub use penelope_conversation::{budget_alert, titles};

pub use agent::{AgentLoop, TurnOutcome};
pub use ports::Handle;
pub use runtime::{Daemon, Services};

/// Version du daemon.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
