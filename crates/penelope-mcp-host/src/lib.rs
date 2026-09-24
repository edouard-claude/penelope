//! Superviseur MCP (§8.6) : les serveurs déclarés dans `mcp.d/` démarrent, négocient,
//! remplissent le registre et répondent aux appels.
//!
//! ```text
//! mcp.d/*.toml ──► rechargement (au démarrage, puis dès qu'un fichier change)
//!                    │
//!                    ├─ outils inconnus ou config changée ─► découverte (tools/list)
//!                    ├─ outils déjà connus, serveur lazy ───► rien ne démarre
//!                    │
//! tool_call ─────► démarrage à la demande ─► tools/call ─► métriques
//!                    │
//!                    ├─ panne : connexion fermée, backoff 1 s → 5 min, `failed` à 8 échecs
//!                    └─ inactif au-delà d'`idle_timeout` (lazy) : arrêt
//! ```
//!
//! La logique ne dépend pas des processus : un [`Connector`] ouvre les transports. Le
//! daemon utilise [`ProcessConnector`] (stdio sous bac à sable, HTTP) ; les tests, une
//! boucle locale.

#![forbid(unsafe_code)]

use penelope_app::services::Services;
use penelope_mcp::McpError;
use penelope_mcp::client::McpClient;
use penelope_mcp::config::ServerConfig;
use penelope_mcp::protocol::{ClientFeatures, ContentBlock, Incoming, ProtocolVersion, ToolResult};
use penelope_mcp::registry::RegisteredTool;
use penelope_mcp::supervisor::{Backoff, ServerMetrics, ServerState, ServerStatus};
use penelope_mcp::transport::Transport;
use penelope_store::rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// Intervalle de la boucle d'entretien : rechargement, arrêt des inactifs, santé.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(15);
/// Un serveur actif mais silencieux depuis ce délai reçoit un `ping`.
const HEALTH_AFTER_MS: i64 = 60_000;
/// Relances MRTR au plus, avant d'abandonner un appel qui demande encore une saisie.
const MAX_INPUT_ROUNDS: usize = 4;

mod admin;
pub mod auth;
mod connector;
mod gateway;
mod lifecycle;
mod render;
mod tools;

pub use connector::{Connector, ProcessConnector, keychain_hint, keychain_open, stdio_profile};
pub use render::result_json;
use render::*;

// ------------------------------------------------------------------ créneaux

struct Live {
    client: Arc<McpClient>,
    pump: tokio::task::JoinHandle<()>,
    /// Accès au trousseau accordé au lancement : s'il change, le processus repart sous
    /// le nouveau profil (issue #122).
    keychain: bool,
}

struct Info {
    state: ServerState,
    backoff: Backoff,
    next_attempt_ms: i64,
    last_error: Option<String>,
    last_ok: Option<String>,
    /// Dernières lignes de stderr, gardées après un échec pour `mcp logs`.
    last_logs: Vec<String>,
    metrics: ServerMetrics,
    last_used_ms: i64,
    protocol: Option<String>,
    server_info: Value,
    capabilities: Value,
    tool_count: usize,
    /// Ce serveur ne gère pas la sonde `server/discover` : on passe par `initialize`.
    skip_probe: bool,
}

struct Slot {
    name: String,
    config: std::sync::RwLock<ServerConfig>,
    live: tokio::sync::Mutex<Option<Live>>,
    info: std::sync::Mutex<Info>,
}

impl Slot {
    fn config(&self) -> ServerConfig {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    fn info<R>(&self, f: impl FnOnce(&mut Info) -> R) -> R {
        let mut g = self.info.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }
}

pub use penelope_app::ports::ReloadReport;

/// Le superviseur.
pub struct McpSupervisor {
    me: Weak<McpSupervisor>,
    services: Arc<Services>,
    connector: Arc<dyn Connector>,
    dir: PathBuf,
    slots: tokio::sync::RwLock<BTreeMap<String, Arc<Slot>>>,
    fingerprint: std::sync::Mutex<String>,
    invalid: std::sync::Mutex<Vec<(String, String)>>,
    max_failures: u32,
    /// Changements d'outils à dire au propriétaire (#92).
    notices: std::sync::Mutex<Vec<String>>,
}

impl Info {
    fn new(state: ServerState) -> Self {
        Info {
            state,
            backoff: Backoff::default(),
            next_attempt_ms: 0,
            last_error: None,
            last_ok: None,
            last_logs: Vec::new(),
            metrics: ServerMetrics::default(),
            last_used_ms: 0,
            protocol: None,
            server_info: Value::Null,
            capabilities: Value::Null,
            tool_count: 0,
            skip_probe: false,
        }
    }
}

/// Faux serveurs MCP en mémoire, partagés par les tests du daemon.
pub mod testing;

#[cfg(test)]
mod tests;
