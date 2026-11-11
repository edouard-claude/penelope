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
/// Un serveur vivant, sans usage ni sonde réussie depuis ce délai, reçoit un `ping`.
const HEALTH_AFTER_MS: i64 = 60_000;
/// Relances MRTR au plus, avant d'abandonner un appel qui demande encore une saisie.
const MAX_INPUT_ROUNDS: usize = 4;

mod admin;
pub mod auth;
mod connector;
mod gateway;
mod lifecycle;
mod render;
mod resources;
mod tools;

pub use connector::{
    Connector, ProcessConnector, keychain_hint, keychain_open, stdio_profile, stdio_spec,
};
pub use render::result_json;
use render::*;

// ------------------------------------------------------------------ créneaux

struct Live {
    client: Arc<McpClient>,
    pump: tokio::task::JoinHandle<()>,
    /// Accès au trousseau accordé au lancement : s'il change, le processus repart sous
    /// le nouveau profil (issue #122).
    keychain: bool,
    /// Expiration du jeton OAuth injecté au lancement d'un serveur stdio (#405).
    expires_ms: Option<i64>,
    /// URI des ressources abonnées **sur cette connexion** (#293) : une connexion neuve
    /// repart sans, et l'appel suivant de `subscribe_resource` repose l'abonnement.
    subscriptions: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

impl Live {
    fn has_subscriptions(&self) -> bool {
        self.subscriptions
            .lock()
            .map(|s| !s.is_empty())
            .unwrap_or_else(|p| !p.into_inner().is_empty())
    }
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
    /// Dernier usage : connexion, appel d'outil, lecture de ressource. C'est lui qui
    /// mesure l'inactivité ; la sonde de santé n'y touche pas (#457).
    last_used_ms: i64,
    /// Dernière sonde de santé réussie : elle espace les sondes, rien d'autre.
    last_probe_ms: i64,
    /// Appels en cours (`InUse`). Tant qu'il y en a, le serveur n'est ni sondé ni arrêté.
    in_use: u32,
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

    /// Marque un appel en cours, jusqu'à la chute du garde. À poser avant `ensure_live` :
    /// l'entretien relit ce compteur sous le verrou de la connexion avant d'arrêter.
    fn in_use(self: &Arc<Self>) -> InUse {
        self.info(|i| i.in_use += 1);
        InUse(self.clone())
    }
}

/// Un appel en cours sur un serveur (#457). `last_used_ms` n'est écrit qu'à la fin d'un
/// appel : sans ce garde, un outil qui travaille trois minutes passait pour un serveur
/// muet, recevait un `ping` auquel il ne pouvait pas répondre et perdait sa connexion en
/// plein travail ; et une saisie attendue du propriétaire aussi longtemps que
/// `idle_timeout` le faisait arrêter.
struct InUse(Arc<Slot>);

impl Drop for InUse {
    fn drop(&mut self) {
        self.0.info(|i| i.in_use = i.in_use.saturating_sub(1));
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
            last_probe_ms: 0,
            in_use: 0,
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
