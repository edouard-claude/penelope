//! `penelope-agenda-mcp` : l'agenda du propriétaire, en lecture, par CalDAV (#295).
//!
//! Un serveur MCP sur stdio, à part du cœur comme le pont de messagerie : Pénélope le
//! déclare dans `mcp.d/agenda.toml` et l'atteint par `tool_call`, le digest du matin par
//! la clé `digest.agenda`. Quatre outils, tous en lecture : `calendar_list`,
//! `events_today`, `events_range`, `event_search`. Les identifiants viennent de
//! l'environnement (`AGENDA_URL`, `AGENDA_USER`, `AGENDA_PASSWORD`), le mot de passe par
//! `${SECRET:…}` dans la déclaration, jamais en clair.
//!
//! ```text
//!  stdin (JSON-RPC) ─► server ─► tools ─► caldav (PROPFIND, REPORT) ─► ical (VEVENT, RRULE)
//!                                   ▲                                        │
//!                                   └──── occurrences dans le fuseau demandé ◄┘
//! ```

#![forbid(unsafe_code)]

pub mod caldav;
pub mod config;
pub mod ical;
pub mod server;
pub mod tools;

pub use config::Settings;
pub use server::Server;
pub use tools::Agenda;

/// Lit la configuration dans l'environnement et sert le protocole sur stdin/stdout
/// jusqu'à la fermeture de l'entrée. L'erreur est lisible par le propriétaire (`penelope
/// mcp logs agenda` la montre) et ne cite jamais le mot de passe.
pub async fn run_stdio() -> Result<(), String> {
    let settings = Settings::from_env()?;
    let agenda = Agenda::new(settings).map_err(|e| e.to_string())?;
    Server::new(agenda).serve_stdio().await
}
