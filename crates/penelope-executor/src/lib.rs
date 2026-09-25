//! `penelope-executor` : exécution des outils natifs (épopée #208, T24,
//! `design/v1/decoupage-daemon.md` §3.2).
//!
//! L'exécuteur ne connaît ni la boucle ni l'orchestrateur ni le daemon : la boucle
//! l'appelle par `ToolExecutor`, et il n'atteint le reste que par les ports de
//! `penelope-app` (`Admin`, `Orchestrator`, `Messenger`, `McpGateway`).

#![forbid(unsafe_code)]

pub mod executor;
pub mod images;
pub mod jobs;
pub mod selfdocs;
pub mod selfknow;
pub mod tools_on_demand;
pub mod vision;
pub mod voice;

/// Version compilée, la même que celle du daemon (workspace).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
