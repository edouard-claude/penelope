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

use crate::runtime::Services;
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

/// Ouvre le transport d'un serveur.
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    async fn open(&self, cfg: &ServerConfig) -> Result<Arc<dyn Transport>, String>;
}

// ------------------------------------------------------------------ processus

/// Connecteur réel : stdio sous le profil de bac à sable du serveur, ou HTTP.
pub struct ProcessConnector {
    services: Arc<Services>,
    host: Arc<penelope_platform::UnixProcessHost>,
}

impl ProcessConnector {
    pub fn new(services: Arc<Services>) -> Self {
        let host = Arc::new(penelope_platform::UnixProcessHost::new(
            services.platform.dirs.pid_dir(),
        ));
        ProcessConnector { services, host }
    }

    /// Profil de bac à sable d'un serveur stdio. `full` exige que le serveur figure dans
    /// `sandbox.allow_full_for` (§13.2).
    fn profile(&self, cfg: &ServerConfig) -> Result<penelope_platform::Profile, String> {
        use penelope_platform::{Profile, ProfileKind};
        let s = &self.services;
        let data_dir = s.platform.dirs.data().join("mcp-data").join(&cfg.name);
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        Ok(match ProfileKind::parse(&cfg.sandbox_profile) {
            Some(ProfileKind::Full) => {
                let allowed = s.config.config().sandbox.allow_full_for.clone();
                if !allowed.iter().any(|n| n == &cfg.name) {
                    return Err(format!(
                        "le profil `full` de `{0}` doit être autorisé explicitement : \
                         `penelope config set sandbox.allow_full_for '[\"{0}\"]'`",
                        cfg.name
                    ));
                }
                Profile::full()
            }
            Some(ProfileKind::ReadOnly) => Profile::read_only(),
            Some(ProfileKind::WorkspaceWrite) => Profile::workspace_write(data_dir),
            _ => Profile::mcp_stdio(data_dir, Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl Connector for ProcessConnector {
    async fn open(&self, cfg: &ServerConfig) -> Result<Arc<dyn Transport>, String> {
        let s = &self.services;
        let resolved = cfg
            .resolve_secrets(s.platform.secrets.as_ref())
            .map_err(|e| format!("{e} (poser le secret : `penelope secret set <nom>`)"))?;
        let dirs = &s.platform.dirs;
        match resolved.effective_transport() {
            "stdio" => {
                let program = dirs.expand(&resolved.command).to_string_lossy().to_string();
                let args: Vec<String> = resolved
                    .args
                    .iter()
                    .map(|a| dirs.expand(a).to_string_lossy().to_string())
                    .collect();
                let mut spec = penelope_platform::ProcessSpec::new(program)
                    .args(args)
                    .pid_tag(format!("mcp-{}", cfg.name));
                if !resolved.cwd.is_empty() {
                    spec = spec.cwd(dirs.expand(&resolved.cwd));
                }
                for (k, v) in &resolved.env {
                    spec = spec.env(k.clone(), v.clone());
                }
                let profile = self.profile(cfg)?;
                let t =
                    penelope_mcp::StdioTransport::spawn(self.host.clone(), spec, Some(&profile))
                        .await
                        .map_err(|e| e.to_string())?;
                Ok(t as Arc<dyn Transport>)
            }
            kind @ ("http" | "sse") => {
                let t = if kind == "sse" {
                    penelope_mcp::HttpTransport::legacy(resolved.url.clone())
                } else {
                    penelope_mcp::HttpTransport::new(resolved.url.clone())
                }
                .map_err(|e| e.to_string())?;
                let mut extra = Vec::new();
                let mut static_auth = false;
                for (k, v) in &resolved.headers {
                    if k.eq_ignore_ascii_case("authorization") {
                        t.set_authorization(Some(v.clone())).await;
                        static_auth = true;
                    } else {
                        extra.push((k.clone(), v.clone()));
                    }
                }
                t.set_extra_headers(extra).await;
                // Autorisation OAuth obtenue par `mcp auth` : jeton rafraîchi au besoin.
                if !static_auth
                    && let Some(header) =
                        crate::mcp_auth::authorization_header(s, &cfg.name, &resolved.url).await?
                {
                    t.set_authorization(Some(header)).await;
                }
                Ok(t as Arc<dyn Transport>)
            }
            other => Err(format!("transport inconnu : `{other}`")),
        }
    }
}

// ------------------------------------------------------------------ créneaux

struct Live {
    client: Arc<McpClient>,
    pump: tokio::task::JoinHandle<()>,
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

/// Ce qu'un rechargement a changé.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct ReloadReport {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub invalid: Vec<(String, String)>,
}

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
}

impl McpSupervisor {
    pub fn new(services: Arc<Services>, connector: Arc<dyn Connector>) -> Arc<McpSupervisor> {
        let dir = services.platform.dirs.mcp_d();
        let max_failures = services.config.config().mcp.max_failures.max(1);
        Arc::new_cyclic(|me| McpSupervisor {
            me: me.clone(),
            services,
            connector,
            dir,
            slots: tokio::sync::RwLock::new(BTreeMap::new()),
            fingerprint: std::sync::Mutex::new(String::new()),
            invalid: std::sync::Mutex::new(Vec::new()),
            max_failures,
        })
    }

    /// Répertoire des déclarations.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Premier chargement puis boucle d'entretien, en tâches de fond.
    pub fn start(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let maintenance = tokio::spawn(self.clone().maintenance_loop());
        vec![self.boot(), maintenance]
    }

    /// Premier chargement de `mcp.d/`, en tâche de fond.
    pub fn boot(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let report = me.reload().await;
            tracing::info!(?report, "serveurs MCP chargés");
        })
    }

    /// Entretien périodique des serveurs ; le daemon la lance sous surveillance (#84).
    pub async fn maintenance_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(MAINTENANCE_EVERY).await;
            self.maintenance().await;
        }
    }

    fn now_ms(&self) -> i64 {
        self.services.clock.now_ms()
    }

    fn now(&self) -> String {
        self.services.clock.now_rfc3339()
    }

    // -------------------------------------------------------------- chargement

    /// Relit `mcp.d/` : ajoute, met à jour ou retire les serveurs, puis lance la
    /// découverte de ceux dont les outils sont inconnus. Attend la fin des découvertes.
    pub async fn reload(&self) -> ReloadReport {
        *self.fingerprint.lock().unwrap_or_else(|p| p.into_inner()) = self.dir_fingerprint();
        let (configs, mut invalid) = penelope_mcp::config::load_dir(&self.dir);
        let mut wanted: BTreeMap<String, ServerConfig> = BTreeMap::new();
        for c in configs {
            if wanted.contains_key(&c.name) {
                invalid.push((c.name.clone(), "nom déclaré deux fois dans mcp.d".into()));
                continue;
            }
            wanted.insert(c.name.clone(), c);
        }
        *self.invalid.lock().unwrap_or_else(|p| p.into_inner()) = invalid.clone();

        let mut report = ReloadReport {
            invalid,
            ..Default::default()
        };
        let mut to_discover: Vec<Arc<Slot>> = Vec::new();
        let mut to_stop: Vec<Arc<Slot>> = Vec::new();
        {
            let mut slots = self.slots.write().await;
            let gone: Vec<String> = slots
                .keys()
                .filter(|n| !wanted.contains_key(*n))
                .cloned()
                .collect();
            for name in gone {
                if let Some(slot) = slots.remove(&name) {
                    to_stop.push(slot);
                }
                report.removed.push(name);
            }
            for (name, cfg) in wanted {
                match slots.get(&name).cloned() {
                    None => {
                        let (row_config, row_tools) = self.row(&name).await;
                        let unchanged = row_config.as_deref() == Some(config_json(&cfg).as_str());
                        let slot = Arc::new(Slot {
                            name: name.clone(),
                            config: std::sync::RwLock::new(cfg.clone()),
                            live: tokio::sync::Mutex::new(None),
                            info: std::sync::Mutex::new(Info::new(if cfg.enabled {
                                ServerState::Configured
                            } else {
                                ServerState::Disabled
                            })),
                        });
                        slot.info(|i| i.tool_count = if unchanged { row_tools } else { 0 });
                        slots.insert(name.clone(), slot.clone());
                        report.added.push(name);
                        if cfg.enabled
                            && (!unchanged
                                || row_tools == 0
                                || penelope_mcp::supervisor::should_start_eagerly(&cfg))
                        {
                            to_discover.push(slot.clone());
                        }
                        self.after_config_change(&slot, cfg.enabled).await;
                    }
                    Some(slot) if slot.config() != cfg => {
                        if let Ok(mut g) = slot.config.write() {
                            *g = cfg.clone();
                        }
                        slot.info(|i| {
                            i.backoff.reset();
                            i.next_attempt_ms = 0;
                            i.skip_probe = false;
                            i.state = if cfg.enabled {
                                ServerState::Configured
                            } else {
                                ServerState::Disabled
                            };
                        });
                        to_stop.push(slot.clone());
                        report.changed.push(name);
                        if cfg.enabled {
                            to_discover.push(slot.clone());
                        }
                        self.after_config_change(&slot, cfg.enabled).await;
                    }
                    Some(_) => {}
                }
            }
        }

        for slot in &to_stop {
            self.stop_slot(slot).await;
        }
        for name in &report.removed {
            let _ = self
                .services
                .mcp_tools
                .replace_server_tools(name, Vec::new(), &self.now())
                .await;
            self.delete_row(name).await;
        }
        let discoveries = to_discover.into_iter().map(|slot| async move {
            if let Err(e) = self.refresh_tools(&slot).await {
                tracing::warn!(server = %slot.name, error = %e, "découverte MCP en échec");
            }
        });
        futures::future::join_all(discoveries).await;
        report
    }

    /// Enregistre la nouvelle configuration ; un serveur désactivé perd ses outils.
    async fn after_config_change(&self, slot: &Arc<Slot>, enabled: bool) {
        if !enabled {
            let _ = self
                .services
                .mcp_tools
                .replace_server_tools(&slot.name, Vec::new(), &self.now())
                .await;
            slot.info(|i| i.tool_count = 0);
        }
        self.persist(slot).await;
    }

    fn dir_fingerprint(&self) -> String {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return String::new();
        };
        let mut parts: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
            .map(|e| {
                let meta = e.metadata().ok();
                let mtime = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                format!(
                    "{}:{}:{}",
                    e.file_name().to_string_lossy(),
                    mtime,
                    meta.map(|m| m.len()).unwrap_or(0)
                )
            })
            .collect();
        parts.sort();
        parts.join("|")
    }

    // -------------------------------------------------------------- connexion

    async fn slot(&self, name: &str) -> Option<Arc<Slot>> {
        self.slots.read().await.get(name).cloned()
    }

    /// Connexion vivante, démarrée au besoin, dans le respect du backoff.
    async fn ensure_live(&self, slot: &Arc<Slot>) -> Result<Arc<McpClient>, String> {
        let mut live = slot.live.lock().await;
        if let Some(l) = live.as_ref() {
            return Ok(l.client.clone());
        }
        let now = self.now_ms();
        let gate = slot.info(|i| match i.state {
            ServerState::Disabled => Some(format!(
                "le serveur `{}` est désactivé (`penelope mcp enable {}`)",
                slot.name, slot.name
            )),
            ServerState::Failed => Some(format!(
                "le serveur `{}` est en panne après {} échecs : {}. Relancer : \
                 `penelope mcp restart {}`",
                slot.name,
                i.backoff.failures,
                i.last_error.clone().unwrap_or_default(),
                slot.name
            )),
            _ if now < i.next_attempt_ms => Some(format!(
                "le serveur `{}` est indisponible, nouvel essai dans {} s : {}",
                slot.name,
                ((i.next_attempt_ms - now) / 1000).max(1),
                i.last_error.clone().unwrap_or_default()
            )),
            _ => None,
        });
        if let Some(why) = gate {
            return Err(why);
        }

        slot.info(|i| i.state = ServerState::Connecting);
        let cfg = slot.config();
        match self.connect(slot, &cfg).await {
            Ok((client, pump)) => {
                let now_s = self.now();
                slot.info(|i| {
                    i.state = ServerState::Ready;
                    i.backoff.reset();
                    i.next_attempt_ms = 0;
                    i.last_error = None;
                    i.last_ok = Some(now_s);
                    i.last_used_ms = self.now_ms();
                    i.protocol = Some(client.version().as_str().to_string());
                    i.server_info = client.server_info().clone();
                    i.capabilities =
                        serde_json::to_value(client.capabilities()).unwrap_or(Value::Null);
                });
                *live = Some(Live {
                    client: client.clone(),
                    pump,
                });
                drop(live);
                self.persist(slot).await;
                Ok(client)
            }
            Err((e, logs, auth)) => {
                let now = self.now_ms();
                let max = self.max_failures;
                slot.info(|i| {
                    i.backoff.max_failures = max;
                    i.backoff.record_failure();
                    i.next_attempt_ms = now + i.backoff.delay_ms() as i64;
                    i.state = if auth {
                        ServerState::AuthRequired
                    } else if i.backoff.exhausted() {
                        ServerState::Failed
                    } else {
                        ServerState::Connecting
                    };
                    i.last_error = Some(e.clone());
                    if !logs.is_empty() {
                        i.last_logs = logs;
                    }
                });
                drop(live);
                self.persist(slot).await;
                tracing::warn!(server = %slot.name, error = %e, "connexion MCP en échec");
                Err(e)
            }
        }
    }

    /// Ouvre, négocie et branche la boucle des messages entrants.
    ///
    /// La sonde `server/discover` (2026-07-28) peut dérouter un serveur plus ancien : en
    /// cas d'échec, un second essai sur un transport neuf passe directement par
    /// `initialize`, et ce choix est retenu pour ce serveur.
    #[allow(clippy::type_complexity)]
    async fn connect(
        &self,
        slot: &Arc<Slot>,
        cfg: &ServerConfig,
    ) -> Result<(Arc<McpClient>, tokio::task::JoinHandle<()>), (String, Vec<String>, bool)> {
        let skip_probe = slot.info(|i| i.skip_probe);
        let preferred = if skip_probe {
            ProtocolVersion::V20251125
        } else {
            cfg.protocol_version()
        };
        let transport = self
            .connector
            .open(cfg)
            .await
            .map_err(|e| (e, Vec::new(), false))?;
        // L'élicitation n'est annoncée que si un propriétaire peut y répondre (issue #12).
        let features = ClientFeatures {
            elicitation: self.services.elicitations.reachable(),
        };
        let first = McpClient::connect(
            cfg.name.clone(),
            transport.clone(),
            preferred,
            cfg.timeout_duration(),
            cfg.max_concurrency,
            features,
        )
        .await;
        let client = match first {
            Ok(c) => c,
            Err(e) if e.needs_auth() => {
                let _ = transport.close().await;
                return Err((
                    format!(
                        "{e} : autorisation requise, `/mcp auth {name}` sur Telegram ou \
                         `penelope mcp auth {name}`",
                        name = cfg.name
                    ),
                    Vec::new(),
                    true,
                ));
            }
            Err(e) if preferred.is_stateless_core() => {
                let _ = transport.close().await;
                tracing::debug!(server = %cfg.name, error = %e, "nouvel essai par initialize");
                let second = self
                    .connector
                    .open(cfg)
                    .await
                    .map_err(|e| (e, Vec::new(), false))?;
                match McpClient::connect(
                    cfg.name.clone(),
                    second.clone(),
                    ProtocolVersion::V20251125,
                    cfg.timeout_duration(),
                    cfg.max_concurrency,
                    features,
                )
                .await
                {
                    Ok(c) => {
                        slot.info(|i| i.skip_probe = true);
                        c
                    }
                    Err(e2) => {
                        let logs = second.logs(20).await;
                        let _ = second.close().await;
                        return Err((explain(cfg, &e2, &logs), logs, e2.needs_auth()));
                    }
                }
            }
            Err(e) => {
                let logs = transport.logs(20).await;
                let _ = transport.close().await;
                return Err((explain(cfg, &e, &logs), logs, false));
            }
        };
        let client = Arc::new(client);
        let pump = self.pump(
            &cfg.name,
            client.transport(),
            cfg.roots.clone(),
            client.version().has_url_elicitation(),
        );
        Ok((client, pump))
    }

    /// Requêtes et notifications venant du serveur.
    fn pump(
        &self,
        name: &str,
        transport: Arc<dyn Transport>,
        roots: Vec<String>,
        url_elicitation: bool,
    ) -> tokio::task::JoinHandle<()> {
        let me = self.me.clone();
        let name = name.to_string();
        let dirs_roots = self.roots_json(&roots);
        let mut rx = transport.incoming();
        tokio::spawn(async move {
            loop {
                let msg = match rx.recv().await {
                    Ok(m) => m,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                match msg {
                    // Le propriétaire peut mettre des minutes à répondre : la boucle continue.
                    Incoming::ServerRequest(req) if req.method == "elicitation/create" => {
                        let (me, transport, name) = (me.clone(), transport.clone(), name.clone());
                        tokio::spawn(async move {
                            let params = req.params.clone().unwrap_or(Value::Null);
                            let result = match me.upgrade() {
                                Some(sup) => sup.elicit(&name, &params, url_elicitation).await,
                                None => Ok(json!({"action": "cancel"})),
                            };
                            let _ = transport.respond(req.id.clone(), result).await;
                        });
                    }
                    Incoming::ServerRequest(req) => {
                        let result: penelope_mcp::Result<Value> = match req.method.as_str() {
                            "ping" => Ok(json!({})),
                            "roots/list" => Ok(json!({ "roots": dirs_roots })),
                            // Non annoncé : un serveur qui le demande quand même est refusé.
                            "sampling/createMessage" => Err(McpError::Denied(
                                "le sampling n'est pas autorisé pour ce serveur".into(),
                            )),
                            other => Err(McpError::Rpc {
                                code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                                message: format!("méthode inconnue : {other}"),
                                data: None,
                            }),
                        };
                        let _ = transport.respond(req.id.clone(), result).await;
                    }
                    Incoming::Notification(n) => match n.method.as_str() {
                        "notifications/tools/list_changed" => {
                            if let Some(sup) = me.upgrade() {
                                let name = name.clone();
                                tokio::spawn(async move {
                                    if let Some(slot) = sup.slot(&name).await {
                                        let _ = sup.refresh_tools(&slot).await;
                                    }
                                });
                            }
                        }
                        "notifications/message" => {
                            tracing::debug!(server = %name, params = ?n.params, "journal MCP");
                        }
                        "notifications/elicitation/complete" => {
                            let id = n
                                .params
                                .as_ref()
                                .and_then(|p| p.get("elicitationId"))
                                .and_then(|i| i.as_str())
                                .map(String::from);
                            if let (Some(sup), Some(id)) = (me.upgrade(), id) {
                                let name = name.clone();
                                tokio::spawn(async move {
                                    sup.services.elicitations.complete(&name, &id).await;
                                });
                            }
                        }
                        _ => {}
                    },
                    Incoming::Response(_) => {}
                }
            }
        })
    }

    /// Relit la liste des outils d'un serveur et l'inscrit au registre.
    async fn refresh_tools(&self, slot: &Arc<Slot>) -> Result<usize, String> {
        let client = self.ensure_live(slot).await?;
        let cfg = slot.config();
        let descriptors = match client.list_tools().await {
            Ok(d) => d,
            Err(McpError::Rpc { code, .. }) if code == penelope_mcp::protocol::METHOD_NOT_FOUND => {
                Vec::new()
            }
            Err(e) => {
                self.connection_lost(slot, &e).await;
                return Err(e.to_string());
            }
        };
        let tools: Vec<RegisteredTool> = descriptors
            .iter()
            .map(|d| {
                let mut t = RegisteredTool::from_descriptor(&cfg.name, d);
                if let Some(r) = cfg
                    .tool_risk
                    .get(&d.name)
                    .and_then(|r| penelope_kernel::risk::RiskClass::parse(r))
                {
                    t.risk = r;
                }
                t
            })
            .collect();
        let n = tools.len();
        self.persist(slot).await;
        self.services
            .mcp_tools
            .replace_server_tools(&cfg.name, tools, &self.now())
            .await
            .map_err(|e| e.to_string())?;
        slot.info(|i| {
            i.tool_count = n;
            i.last_used_ms = self.now_ms();
        });
        self.persist(slot).await;
        tracing::info!(server = %cfg.name, tools = n, "outils MCP inscrits");
        Ok(n)
    }

    /// La connexion est perdue : on la ferme et on compte l'échec.
    async fn connection_lost(&self, slot: &Arc<Slot>, e: &McpError) {
        let logs = self.stop_slot(slot).await;
        let now = self.now_ms();
        let max = self.max_failures;
        slot.info(|i| {
            i.backoff.max_failures = max;
            i.backoff.record_failure();
            i.next_attempt_ms = now + i.backoff.delay_ms() as i64;
            i.state = if i.backoff.exhausted() {
                ServerState::Failed
            } else {
                ServerState::Connecting
            };
            i.last_error = Some(e.to_string());
            if !logs.is_empty() {
                i.last_logs = logs;
            }
        });
        self.persist(slot).await;
    }

    /// Ferme la connexion d'un serveur. Renvoie ses dernières lignes de journal.
    async fn stop_slot(&self, slot: &Arc<Slot>) -> Vec<String> {
        let taken = slot.live.lock().await.take();
        let Some(l) = taken else {
            return Vec::new();
        };
        let logs = l.client.transport().logs(20).await;
        let _ = l.client.close().await;
        l.pump.abort();
        slot.info(|i| {
            if matches!(i.state, ServerState::Ready | ServerState::Degraded) {
                i.state = ServerState::Configured;
            }
        });
        logs
    }

    /// Arrête tous les serveurs (arrêt du daemon).
    pub async fn stop_all(&self) {
        let slots: Vec<Arc<Slot>> = self.slots.read().await.values().cloned().collect();
        for slot in slots {
            self.stop_slot(&slot).await;
            self.persist(&slot).await;
        }
    }

    // -------------------------------------------------------------- appels

    /// Appelle un outil par son nom qualifié.
    pub async fn call(&self, qualified: &str, args: &Value) -> Result<Value, String> {
        let tool = self
            .services
            .mcp_tools
            .get(qualified)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("outil MCP inconnu : `{qualified}`"))?;
        let slot = self
            .slot(&tool.server)
            .await
            .ok_or_else(|| format!("le serveur `{}` n'est plus déclaré dans mcp.d", tool.server))?;
        let client = self.ensure_live(&slot).await?;
        let timeout = slot.config().timeout_for(&tool.name);
        let started = std::time::Instant::now();
        let call = |input: Option<Value>, state: Option<String>| {
            client.retry_tool(
                &tool.name,
                args.clone(),
                tool.output_schema.as_ref(),
                Some(timeout),
                input,
                state,
            )
        };
        let mut outcome = call(None, None).await;
        // MRTR (2026-07-28) : le serveur demande une saisie, l'appel est relancé avec les
        // réponses et son état opaque.
        let mut rounds = 0;
        while let Ok(r) = &outcome
            && r.needs_input()
            && rounds < MAX_INPUT_ROUNDS
        {
            rounds += 1;
            let url_ok = client.version().has_url_elicitation();
            let input = self
                .input_responses(&tool.server, r.input_requests.as_ref(), url_ok)
                .await;
            let state = r.request_state.clone();
            outcome = call(input, state).await;
        }
        // 2025-11-25 : l'appel exige un lien mené à bien, puis un nouvel essai.
        if let Err(McpError::Rpc { code, data, .. }) = &outcome
            && *code == penelope_mcp::protocol::URL_ELICITATION_REQUIRED
            && self.links_completed(&tool.server, data.clone()).await
        {
            outcome = call(None, None).await;
        }
        let notes = self
            .services
            .elicitations
            .notes_since(&tool.server, started);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let now = self.now_ms();
        match outcome {
            Ok(mut result) => {
                if result.needs_input() {
                    result.is_error = true;
                    result.content.push(ContentBlock::Text {
                        text: format!(
                            "[Pénélope : `{}` demande encore une saisie après {rounds} \
                             échanges, appel abandonné.]",
                            tool.server
                        ),
                    });
                }
                for note in &notes {
                    result
                        .content
                        .push(ContentBlock::Text { text: note.clone() });
                }
                let now_s = self.now();
                slot.info(|i| {
                    i.metrics.record(ms, !result.is_error);
                    i.last_used_ms = now;
                    i.last_ok = Some(now_s);
                    if i.state == ServerState::Degraded {
                        i.state = ServerState::Ready;
                    }
                });
                self.persist(&slot).await;
                Ok(result_json(&result))
            }
            Err(e) => {
                slot.info(|i| {
                    i.metrics.record(ms, false);
                    i.last_used_ms = now;
                });
                match &e {
                    McpError::Transport(_) => self.connection_lost(&slot, &e).await,
                    McpError::Timeout { .. } => {
                        slot.info(|i| {
                            i.state = ServerState::Degraded;
                            i.last_error = Some(e.to_string());
                        });
                        self.persist(&slot).await;
                    }
                    _ => self.persist(&slot).await,
                }
                let mut message = format!("`{qualified}` : {e}");
                for note in &notes {
                    message.push('\n');
                    message.push_str(note);
                }
                Err(message)
            }
        }
    }

    /// Demande `elicitation/create` d'un serveur : présentée au propriétaire, résultat
    /// `ElicitResult`. Le mode URL n'est accepté que s'il a été annoncé.
    pub async fn elicit(
        &self,
        server: &str,
        params: &Value,
        url_allowed: bool,
    ) -> penelope_mcp::Result<Value> {
        let invalid = |message: String| McpError::Rpc {
            code: penelope_mcp::protocol::INVALID_PARAMS,
            message,
            data: None,
        };
        if params.get("mode").and_then(|m| m.as_str()) == Some("url") && !url_allowed {
            return Err(invalid(
                "mode `url` non annoncé pour cette version du protocole".into(),
            ));
        }
        let timeout = match self.slot(server).await {
            Some(slot) => slot.config().elicitation_duration(),
            None => Duration::from_secs(600),
        };
        self.services
            .elicitations
            .ask(server, params, timeout)
            .await
            .map(|o| o.result())
            .map_err(invalid)
    }

    /// Réponses aux `inputRequests` d'un résultat MRTR : élicitations et racines. Le
    /// sampling, jamais annoncé, reste sans réponse.
    async fn input_responses(
        &self,
        server: &str,
        requests: Option<&Value>,
        url_allowed: bool,
    ) -> Option<Value> {
        let requests = requests?.as_object()?;
        let mut out = serde_json::Map::new();
        for (key, request) in requests {
            let params = request.get("params").cloned().unwrap_or(json!({}));
            match request.get("method").and_then(|m| m.as_str()) {
                Some("elicitation/create") => {
                    if let Ok(v) = self.elicit(server, &params, url_allowed).await {
                        out.insert(key.clone(), v);
                    }
                }
                Some("roots/list") => {
                    let roots = match self.slot(server).await {
                        Some(slot) => self.roots_json(&slot.config().roots),
                        None => Vec::new(),
                    };
                    out.insert(key.clone(), json!({ "roots": roots }));
                }
                other => {
                    tracing::debug!(server, method = ?other, "demande MRTR sans réponse");
                }
            }
        }
        Some(Value::Object(out))
    }

    /// Erreur −32042 : chaque lien exigé est présenté au propriétaire ; vrai quand tous ont
    /// été acceptés et que le serveur en a signalé la fin.
    async fn links_completed(&self, server: &str, data: Option<Value>) -> bool {
        let Some(links) = data
            .as_ref()
            .and_then(|d| d.get("elicitations"))
            .and_then(|e| e.as_array())
            .filter(|l| !l.is_empty())
        else {
            return false;
        };
        let timeout = match self.slot(server).await {
            Some(slot) => slot.config().elicitation_duration(),
            None => Duration::from_secs(600),
        };
        let broker = &self.services.elicitations;
        for link in links {
            let Some(id) = link.get("elicitationId").and_then(|i| i.as_str()) else {
                return false;
            };
            match broker.ask(server, link, timeout).await {
                Ok(o) if o.accepted() => {
                    if !broker.wait_completion(server, id, timeout).await {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }

    /// Racines déclarées, telles que `roots/list` les rend.
    fn roots_json(&self, roots: &[String]) -> Vec<Value> {
        roots
            .iter()
            .map(|r| {
                let path = self.services.platform.dirs.expand(r);
                json!({
                    "uri": format!("file://{}", path.display()),
                    "name": path.file_name().map(|n| n.to_string_lossy().to_string()),
                })
            })
            .collect()
    }

    /// État d'une tâche MCP (`tasks/get`) et, une fois terminée, son résultat
    /// (`tasks/result`, sinon celui que porte la tâche). `{status, task, result}`.
    pub async fn task_status(&self, server: &str, task_ref: &str) -> Result<Value, String> {
        let slot = self
            .slot(server)
            .await
            .ok_or_else(|| format!("serveur MCP inconnu : `{server}`"))?;
        let client = self.ensure_live(&slot).await?;
        let v = client
            .task_get(task_ref)
            .await
            .map_err(|e| format!("tasks/get `{task_ref}` : {e}"))?;
        let task = v.get("task").cloned().unwrap_or(v);
        let status = task["status"]
            .as_str()
            .or_else(|| task["state"].as_str())
            .unwrap_or("working")
            .to_string();
        let terminal =
            penelope_mcp::tasks::TaskState::parse(&status).is_some_and(|s| s.is_terminal());
        let result = if terminal && status != "cancelled" && status != "canceled" {
            match client.task_result(task_ref).await {
                Ok(r) => r,
                Err(McpError::Rpc { code, .. })
                    if code == penelope_mcp::protocol::METHOD_NOT_FOUND =>
                {
                    task.get("result").cloned().unwrap_or(Value::Null)
                }
                Err(e) => return Err(format!("tasks/result `{task_ref}` : {e}")),
            }
        } else {
            Value::Null
        };
        Ok(json!({"status": status, "task": task, "result": result}))
    }

    // -------------------------------------------------------------- entretien

    /// Rechargement si `mcp.d` a changé, arrêt des inactifs, santé, reconnexions.
    pub async fn maintenance(&self) {
        let changed = {
            let now = self.dir_fingerprint();
            let g = self.fingerprint.lock().unwrap_or_else(|p| p.into_inner());
            *g != now
        };
        if changed {
            let report = self.reload().await;
            tracing::info!(?report, "mcp.d rechargé");
        }

        let now = self.now_ms();
        let cfg = self.services.config.config();
        let slots: Vec<Arc<Slot>> = self.slots.read().await.values().cloned().collect();
        let mut running: Vec<(Arc<Slot>, i64)> = Vec::new();
        for slot in &slots {
            let c = slot.config();
            let is_live = slot.live.lock().await.is_some();
            let last_used = slot.info(|i| i.last_used_ms);
            if is_live {
                let idle = (now - last_used) as u128 >= c.idle_duration().as_millis();
                if c.lazy_start && !c.eager_schemas && idle {
                    tracing::debug!(server = %slot.name, "serveur MCP inactif arrêté");
                    self.stop_slot(slot).await;
                    self.persist(slot).await;
                    continue;
                }
                if now - last_used >= HEALTH_AFTER_MS {
                    let client = slot.live.lock().await.as_ref().map(|l| l.client.clone());
                    if let Some(client) = client {
                        match client.health().await {
                            Ok(()) => slot.info(|i| i.last_used_ms = now),
                            Err(e) => {
                                self.connection_lost(slot, &e).await;
                                continue;
                            }
                        }
                    }
                }
                running.push((slot.clone(), last_used));
            } else if c.enabled
                && penelope_mcp::supervisor::should_start_eagerly(&c)
                && slot.info(|i| i.state == ServerState::Connecting && now >= i.next_attempt_ms)
            {
                let _ = self.refresh_tools(slot).await;
            }
        }
        // Plafond de processus : on arrête les serveurs lazy les moins récemment utilisés.
        if running.len() > cfg.mcp.max_processes {
            running.sort_by_key(|(_, t)| *t);
            let excess = running.len() - cfg.mcp.max_processes;
            for (slot, _) in running
                .iter()
                .filter(|(s, _)| s.config().lazy_start)
                .take(excess)
            {
                self.stop_slot(slot).await;
                self.persist(slot).await;
            }
        }
    }

    // -------------------------------------------------------------- administration

    /// État de chaque serveur, trié par nom.
    pub async fn statuses(&self) -> Vec<ServerStatus> {
        let slots: Vec<Arc<Slot>> = self.slots.read().await.values().cloned().collect();
        let mut out = Vec::new();
        for slot in slots {
            out.push(self.status_of(&slot).await);
        }
        out
    }

    async fn status_of(&self, slot: &Arc<Slot>) -> ServerStatus {
        let c = slot.config();
        let running = slot.live.lock().await.is_some();
        slot.info(|i| ServerStatus {
            name: slot.name.clone(),
            state: i.state,
            transport: c.effective_transport().to_string(),
            protocol: i.protocol.clone(),
            tool_count: i.tool_count,
            failures: i.backoff.failures,
            last_error: i.last_error.clone(),
            last_ok: i.last_ok.clone(),
            p50_ms: i.metrics.quantile(0.5),
            p95_ms: i.metrics.quantile(0.95),
            calls: i.metrics.calls,
            errors: i.metrics.errors,
            running,
            lazy: c.lazy_start,
        })
    }

    /// Déclarations invalides lors du dernier chargement.
    pub fn invalid(&self) -> Vec<(String, String)> {
        self.invalid.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Détail d'un serveur : état, configuration (références de secrets seulement),
    /// outils, dernières lignes de journal.
    pub async fn show(&self, name: &str) -> Result<Value, String> {
        let slot = self.slot(name).await.ok_or_else(|| unknown(name))?;
        let status = self.status_of(&slot).await;
        let tools = self
            .services
            .mcp_tools
            .list_server(name)
            .await
            .map_err(|e| e.to_string())?;
        let (server_info, capabilities) =
            slot.info(|i| (i.server_info.clone(), i.capabilities.clone()));
        Ok(json!({
            "status": status,
            "config": slot.config(),
            "server_info": server_info,
            "capabilities": capabilities,
            "tools": tools.iter().map(|t| t.short()).collect::<Vec<_>>(),
            "logs": self.logs(name, 20).await.unwrap_or_default(),
        }))
    }

    /// Prompts proposés par un serveur (`prompts/list`), pour `/p` (issue #30).
    pub async fn prompts(&self, name: &str) -> Result<Vec<Value>, String> {
        let slot = self.slot(name).await.ok_or_else(|| unknown(name))?;
        let client = self.ensure_live(&slot).await?;
        client.list_prompts().await.map_err(|e| e.to_string())
    }

    /// Un prompt rendu par son serveur (`prompts/get`).
    pub async fn get_prompt(&self, name: &str, prompt: &str, args: Value) -> Result<Value, String> {
        let slot = self.slot(name).await.ok_or_else(|| unknown(name))?;
        let client = self.ensure_live(&slot).await?;
        client
            .get_prompt(prompt, args)
            .await
            .map_err(|e| e.to_string())
    }

    /// Dernières lignes de stderr : du processus vivant, sinon celles gardées au dernier
    /// échec.
    pub async fn logs(&self, name: &str, n: usize) -> Result<Vec<String>, String> {
        let slot = self.slot(name).await.ok_or_else(|| unknown(name))?;
        let transport = slot
            .live
            .lock()
            .await
            .as_ref()
            .map(|l| l.client.transport());
        Ok(match transport {
            Some(t) => t.logs(n).await,
            None => slot.info(|i| {
                let skip = i.last_logs.len().saturating_sub(n);
                i.last_logs[skip..].to_vec()
            }),
        })
    }

    /// Arrête puis redémarre un serveur, backoff remis à zéro, outils relus.
    pub async fn restart(&self, name: &str) -> Result<ServerStatus, String> {
        let slot = self.slot(name).await.ok_or_else(|| unknown(name))?;
        if !slot.config().enabled {
            return Err(format!(
                "le serveur `{name}` est désactivé (`penelope mcp enable {name}`)"
            ));
        }
        self.stop_slot(&slot).await;
        // `skip_probe` est gardé : le binaire n'a pas changé, sa réponse à la sonde non plus.
        slot.info(|i| {
            i.backoff.reset();
            i.next_attempt_ms = 0;
            i.state = ServerState::Configured;
        });
        let result = self.refresh_tools(&slot).await;
        let status = self.status_of(&slot).await;
        result.map(|_| status)
    }

    /// Essai à blanc : connexion neuve, négociation, liste des outils, fermeture. Le
    /// serveur en service n'est pas touché.
    pub async fn test(&self, cfg: &ServerConfig) -> Value {
        let started = std::time::Instant::now();
        let probe = Arc::new(Slot {
            name: cfg.name.clone(),
            config: std::sync::RwLock::new(cfg.clone()),
            live: tokio::sync::Mutex::new(None),
            info: std::sync::Mutex::new(Info::new(ServerState::Configured)),
        });
        match self.connect(&probe, cfg).await {
            Ok((client, pump)) => {
                let tools = client.list_tools().await;
                let logs = client.transport().logs(10).await;
                let _ = client.close().await;
                pump.abort();
                match tools {
                    Ok(t) => json!({
                        "ok": true,
                        "protocol": client.version().as_str(),
                        "server_info": client.server_info(),
                        "tools": t.len(),
                        "names": t.iter().take(30).map(|d| d.name.clone()).collect::<Vec<_>>(),
                        "ms": started.elapsed().as_millis() as u64,
                        "logs": logs,
                    }),
                    Err(e) => json!({
                        "ok": false,
                        "error": e.to_string(),
                        "logs": logs,
                        "auth_required": e.needs_auth(),
                    }),
                }
            }
            Err((e, logs, auth)) => {
                json!({"ok": false, "error": e, "logs": logs, "auth_required": auth})
            }
        }
    }

    /// Configuration courante d'un serveur déclaré.
    pub async fn config_of(&self, name: &str) -> Option<ServerConfig> {
        self.slot(name).await.map(|s| s.config())
    }

    /// Ajoute (ou remplace, avec `replace`) la déclaration d'un serveur, puis recharge.
    pub async fn add(&self, cfg: ServerConfig, replace: bool) -> Result<ReloadReport, String> {
        cfg.validate().map_err(|e| e.to_string())?;
        let path = self.dir.join(format!("{}.toml", cfg.name));
        if !replace && (path.exists() || self.slot(&cfg.name).await.is_some()) {
            return Err(format!(
                "le serveur `{}` existe déjà (`penelope mcp edit` pour le modifier)",
                cfg.name
            ));
        }
        if replace && !path.exists() && self.slot(&cfg.name).await.is_some() {
            return Err(multi_file(&cfg.name));
        }
        penelope_mcp::config::write_server(&self.dir, &cfg).map_err(|e| e.to_string())?;
        Ok(self.reload().await)
    }

    /// Modifie quelques champs d'une déclaration (`{"timeout": "60s"}`), puis recharge.
    pub async fn edit(&self, name: &str, patch: &Value) -> Result<ReloadReport, String> {
        let current = self.config_of(name).await.ok_or_else(|| unknown(name))?;
        let mut v = serde_json::to_value(&current).map_err(|e| e.to_string())?;
        let Some(fields) = patch.as_object() else {
            return Err("les modifications doivent être un objet JSON".into());
        };
        for (k, val) in fields {
            if k == "name" {
                return Err("le nom d'un serveur ne change pas : le retirer puis l'ajouter".into());
            }
            if v.get(k).is_none() {
                return Err(format!("champ inconnu : `{k}`"));
            }
            v[k] = val.clone();
        }
        let updated: ServerConfig = serde_json::from_value(v).map_err(|e| e.to_string())?;
        self.add(updated, true).await
    }

    /// Active ou désactive un serveur, en réécrivant sa déclaration.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<ReloadReport, String> {
        self.edit(name, &json!({ "enabled": enabled })).await
    }

    /// Retire la déclaration d'un serveur : processus arrêté, outils retirés.
    pub async fn remove(&self, name: &str) -> Result<ReloadReport, String> {
        let path = self.dir.join(format!("{name}.toml"));
        if !path.exists() {
            return Err(if self.slot(name).await.is_some() {
                multi_file(name)
            } else {
                unknown(name)
            });
        }
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        Ok(self.reload().await)
    }

    // -------------------------------------------------------------- persistance

    async fn row(&self, name: &str) -> (Option<String>, usize) {
        let n = name.to_string();
        self.services
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT config, tool_count FROM mcp_servers WHERE name = ?1",
                    [&n],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()?)
            })
            .await
            .ok()
            .flatten()
            .map(|(c, t)| (Some(c), t.max(0) as usize))
            .unwrap_or((None, 0))
    }

    async fn persist(&self, slot: &Arc<Slot>) {
        let cfg = slot.config();
        let (
            state,
            protocol,
            server_info,
            capabilities,
            tools,
            failures,
            last_error,
            last_ok,
            p50,
            p95,
            calls,
            errors,
        ) = slot.info(|i| {
            (
                i.state.as_str().to_string(),
                i.protocol.clone(),
                i.server_info.to_string(),
                i.capabilities.to_string(),
                i.tool_count as i64,
                i.backoff.failures as i64,
                i.last_error.clone(),
                i.last_ok.clone(),
                i.metrics.quantile(0.5),
                i.metrics.quantile(0.95),
                i.metrics.calls as i64,
                i.metrics.errors as i64,
            )
        });
        let (name, transport, config, lazy, eager, now) = (
            cfg.name.clone(),
            cfg.effective_transport().to_string(),
            config_json(&cfg),
            cfg.lazy_start,
            cfg.eager_schemas,
            self.now(),
        );
        let _ = self
            .services
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mcp_servers(name, transport, config, state, protocol, server_info,
                        capabilities, tool_count, lazy_start, eager_schemas, failures, last_error,
                        last_ok, updated_at, p50_ms, p95_ms, calls, errors)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
                     ON CONFLICT(name) DO UPDATE SET transport=excluded.transport,
                        config=excluded.config, state=excluded.state, protocol=excluded.protocol,
                        server_info=excluded.server_info, capabilities=excluded.capabilities,
                        tool_count=excluded.tool_count, lazy_start=excluded.lazy_start,
                        eager_schemas=excluded.eager_schemas, failures=excluded.failures,
                        last_error=excluded.last_error, last_ok=excluded.last_ok,
                        updated_at=excluded.updated_at, p50_ms=excluded.p50_ms,
                        p95_ms=excluded.p95_ms, calls=excluded.calls, errors=excluded.errors",
                    params![
                        name,
                        transport,
                        config,
                        state,
                        protocol,
                        server_info,
                        capabilities,
                        tools,
                        lazy as i64,
                        eager as i64,
                        failures,
                        last_error,
                        last_ok,
                        now,
                        p50,
                        p95,
                        calls,
                        errors
                    ],
                )?;
                Ok(())
            })
            .await;
    }

    async fn delete_row(&self, name: &str) {
        let n = name.to_string();
        let _ = self
            .services
            .store
            .write(move |tx| {
                tx.execute("DELETE FROM mcp_servers WHERE name = ?1", [&n])?;
                Ok(())
            })
            .await;
    }
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

#[async_trait::async_trait]
impl crate::executor::McpGateway for McpSupervisor {
    async fn call_tool(&self, qualified: &str, args: &Value) -> Result<Value, String> {
        self.call(qualified, args).await
    }

    /// Une ligne par serveur qui a des outils, sans état volatil : le préfixe du prompt
    /// ne doit pas bouger à chaque reconnexion.
    async fn server_lines(&self) -> Vec<String> {
        let slots: Vec<Arc<Slot>> = self.slots.read().await.values().cloned().collect();
        slots
            .iter()
            .filter(|s| s.config().enabled)
            .filter_map(|s| {
                s.info(|i| {
                    (i.tool_count > 0).then(|| match i.state {
                        ServerState::Failed | ServerState::AuthRequired => {
                            format!("{} : {} outils (indisponible)", s.name, i.tool_count)
                        }
                        _ => format!("{} : {} outils", s.name, i.tool_count),
                    })
                })
            })
            .collect()
    }

    async fn tool_policy(&self, qualified: &str) -> Option<String> {
        let tool = self
            .services
            .mcp_tools
            .get(qualified)
            .await
            .ok()
            .flatten()?;
        let slot = self.slot(&tool.server).await?;
        slot.config().tool_policy.get(&tool.name).cloned()
    }

    /// Outils des serveurs `eager_schemas`, exposés directement au modèle.
    async fn eager_tools(&self) -> Vec<penelope_llm::ToolDef> {
        let slots: Vec<Arc<Slot>> = self.slots.read().await.values().cloned().collect();
        let names: Vec<String> = slots
            .iter()
            .filter(|s| {
                let c = s.config();
                c.enabled && c.eager_schemas
            })
            .map(|s| s.name.clone())
            .collect();
        if names.is_empty() {
            return Vec::new();
        }
        self.services
            .mcp_tools
            .eager_schemas(&names)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|d| {
                Some(penelope_llm::ToolDef::new(
                    d.get("name")?.as_str()?,
                    d.get("description").and_then(|x| x.as_str()).unwrap_or(""),
                    d.get("inputSchema")
                        .cloned()
                        .unwrap_or(json!({"type": "object"})),
                ))
            })
            .collect()
    }
}

/// Configuration sérialisée telle qu'enregistrée : sert à détecter un changement.
fn config_json(cfg: &ServerConfig) -> String {
    serde_json::to_string(cfg).unwrap_or_default()
}

fn unknown(name: &str) -> String {
    format!("serveur MCP inconnu : `{name}` (voir `penelope mcp list`)")
}

fn multi_file(name: &str) -> String {
    format!(
        "`{name}` est déclaré dans un fichier qui en contient plusieurs : modifier ce \
         fichier à la main dans mcp.d"
    )
}

/// Message d'échec de connexion, avec les dernières lignes de stderr et, si le bac à
/// sable semble en cause, la marche à suivre.
fn explain(cfg: &ServerConfig, e: &McpError, logs: &[String]) -> String {
    let mut msg = e.to_string();
    let tail: Vec<&String> = logs
        .iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !tail.is_empty() {
        msg.push_str(" ; stderr : ");
        msg.push_str(
            &tail
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    let blocked = logs.iter().any(|l| {
        let l = l.to_lowercase();
        l.contains("operation not permitted") || l.contains("permission denied")
    });
    if blocked && cfg.sandbox_profile != "full" {
        msg.push_str(&format!(
            " → le bac à sable `{0}` bloque peut-être une écriture : `sandbox_profile = \
             \"full\"` dans sa déclaration, puis `penelope config set \
             sandbox.allow_full_for '[\"{1}\"]'`",
            cfg.sandbox_profile, cfg.name
        ));
    }
    msg
}

/// Résultat d'outil au format MCP, sans les données binaires : une image ou un audio
/// n'entre pas dans le transcript en base64.
pub fn result_json(r: &ToolResult) -> Value {
    let content: Vec<Value> = r
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({"type": "text", "text": text}),
            ContentBlock::Image { data, mime_type } => json!({
                "type": "text",
                "text": format!("[image {mime_type}, {} octets en base64, non transmise]", data.len()),
            }),
            ContentBlock::Audio { data, mime_type } => json!({
                "type": "text",
                "text": format!("[audio {mime_type}, {} octets en base64, non transmis]", data.len()),
            }),
            ContentBlock::ResourceLink {
                uri,
                name,
                description,
            } => json!({"type": "resource_link", "uri": uri, "name": name, "description": description}),
            ContentBlock::Resource {
                uri,
                text,
                mime_type,
                ..
            } => match text {
                Some(t) => json!({"type": "resource", "uri": uri, "text": t, "mimeType": mime_type}),
                None => json!({"type": "resource", "uri": uri, "mimeType": mime_type}),
            },
            ContentBlock::Other(v) => v.clone(),
        })
        .collect();
    json!({
        "content": content,
        "structuredContent": r.structured,
        "isError": r.is_error,
    })
}

/// Faux serveurs MCP en mémoire, partagés par les tests du daemon.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use penelope_mcp::transport::LoopbackTransport;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub type Handler = Arc<dyn Fn(&str, &Value) -> penelope_mcp::Result<Value> + Send + Sync>;

    /// Connecteur qui ouvre des boucles locales, un gestionnaire par serveur.
    #[derive(Default)]
    pub struct FakeConnector {
        pub handlers: Mutex<BTreeMap<String, Handler>>,
        pub opened: Mutex<Vec<String>>,
        pub fail_open: Mutex<BTreeMap<String, String>>,
        pub transports: Mutex<Vec<(String, Arc<LoopbackTransport>)>>,
        /// Serveurs qui meurent quand on leur envoie la sonde `server/discover`.
        pub dies_on_probe: Mutex<Vec<String>>,
        /// Lenteur simulée à l'ouverture (issue #73).
        pub open_delay: Mutex<Option<std::time::Duration>>,
    }

    impl FakeConnector {
        pub fn serve(&self, name: &str, handler: Handler) {
            self.handlers
                .lock()
                .unwrap()
                .insert(name.to_string(), handler);
        }
        /// Fait traîner l'ouverture : de quoi vérifier qu'un clic n'attend pas (#73).
        pub fn set_open_delay(&self, d: std::time::Duration) {
            *self.open_delay.lock().unwrap() = Some(d);
        }
        pub fn opened(&self, name: &str) -> usize {
            self.opened
                .lock()
                .unwrap()
                .iter()
                .filter(|n| *n == name)
                .count()
        }
        pub fn last_transport(&self, name: &str) -> Arc<LoopbackTransport> {
            self.transports
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.clone())
                .expect("transport ouvert")
        }
    }

    #[async_trait::async_trait]
    impl Connector for FakeConnector {
        async fn open(&self, cfg: &ServerConfig) -> Result<Arc<dyn Transport>, String> {
            self.opened.lock().unwrap().push(cfg.name.clone());
            let delay = *self.open_delay.lock().unwrap();
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            if let Some(e) = self.fail_open.lock().unwrap().get(&cfg.name) {
                return Err(e.clone());
            }
            let h = self
                .handlers
                .lock()
                .unwrap()
                .get(&cfg.name)
                .cloned()
                .ok_or_else(|| "aucun faux serveur".to_string())?;
            let dies = self.dies_on_probe.lock().unwrap().contains(&cfg.name);
            let dead = Arc::new(AtomicBool::new(false));
            let t = LoopbackTransport::new("stdio", move |m, p| {
                if dead.load(Ordering::SeqCst) {
                    return Err(McpError::Transport(
                        "le serveur a fermé la connexion".into(),
                    ));
                }
                if dies && m == "server/discover" {
                    dead.store(true, Ordering::SeqCst);
                    return Err(McpError::Transport(
                        "le serveur a fermé la connexion".into(),
                    ));
                }
                h(m, p)
            });
            self.transports
                .lock()
                .unwrap()
                .push((cfg.name.clone(), t.clone()));
            Ok(t)
        }
    }

    /// Serveur d'avant 2026 : pas de `server/discover`, `initialize`, outils donnés.
    pub fn server(tools: Arc<Mutex<Vec<Value>>>) -> Handler {
        Arc::new(move |m, p| match m {
            "server/discover" => Err(McpError::Rpc {
                code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                message: "Method not found".into(),
                data: None,
            }),
            "initialize" => Ok(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "faux", "version": "1.0"}
            })),
            "tools/list" => Ok(json!({"tools": tools.lock().unwrap().clone()})),
            "tools/call" => Ok(json!({
                "content": [{"type": "text", "text": format!("{} {}", p["name"].as_str().unwrap_or(""), p["arguments"])}]
            })),
            _ => Ok(json!({})),
        })
    }

    pub fn tool(name: &str, annotations: Value) -> Value {
        json!({
            "name": name,
            "description": format!("outil {name}"),
            "inputSchema": {"type": "object", "properties": {"project": {"type": "string"}}},
            "annotations": annotations
        })
    }

    pub fn declare(sup: &McpSupervisor, name: &str, extra: &str) {
        std::fs::create_dir_all(sup.dir()).unwrap();
        std::fs::write(
            sup.dir().join(format!("{name}.toml")),
            format!("command = \"/opt/mcp/{name}\"\n{extra}"),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::executor::McpGateway;
    use penelope_kernel::clock::TestClock;
    use penelope_kernel::risk::RiskClass;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn setup() -> (
        tempfile::TempDir,
        Arc<Services>,
        Arc<TestClock>,
        Arc<FakeConnector>,
        Arc<McpSupervisor>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let fake = Arc::new(FakeConnector::default());
        let sup = McpSupervisor::new(s.clone(), fake.clone());
        (dir, s, clock, fake, sup)
    }

    fn two_tools() -> Arc<Mutex<Vec<Value>>> {
        Arc::new(Mutex::new(vec![
            tool("list_issues", json!({"readOnlyHint": true})),
            tool("create_issue", json!({})),
        ]))
    }

    #[tokio::test]
    async fn servers_are_discovered_and_their_tools_answer_calls() {
        let (_d, s, _c, fake, sup) = setup().await;
        fake.serve("redmine", server(two_tools()));
        declare(
            &sup,
            "redmine",
            "[tool_risk]\ncreate_issue = \"destructive\"\n",
        );

        let report = sup.reload().await;
        assert_eq!(report.added, vec!["redmine"]);

        let list = s
            .mcp_tools
            .get("mcp__redmine__list_issues")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(list.risk, RiskClass::Read);
        let create = s
            .mcp_tools
            .get("mcp__redmine__create_issue")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            create.risk,
            RiskClass::Destructive,
            "surcharge de la déclaration"
        );
        assert_eq!(sup.server_lines().await, vec!["redmine : 2 outils"]);

        let v = sup
            .call("mcp__redmine__list_issues", &json!({"project": "penelope"}))
            .await
            .unwrap();
        assert!(
            v["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("list_issues")
        );
        assert_eq!(v["isError"], false);
        assert_eq!(
            fake.opened("redmine"),
            1,
            "la connexion de découverte sert à l'appel"
        );

        let st = &sup.statuses().await[0];
        assert_eq!(st.state, ServerState::Ready);
        assert_eq!((st.calls, st.tool_count, st.running), (1, 2, true));
        assert_eq!(st.protocol.as_deref(), Some("2025-06-18"));

        let (state, tools): (String, i64) = s
            .store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT state, tool_count FROM mcp_servers WHERE name = 'redmine'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!((state.as_str(), tools), ("ready", 2));
    }

    #[tokio::test]
    async fn known_tools_do_not_start_a_lazy_server_at_boot() {
        let (_d, s, _c, fake, sup) = setup().await;
        fake.serve("redmine", server(two_tools()));
        declare(&sup, "redmine", "");
        sup.reload().await;
        sup.stop_all().await;

        // Redémarrage du daemon : même base, nouveau superviseur.
        let fake2 = Arc::new(FakeConnector::default());
        fake2.serve("redmine", server(two_tools()));
        let sup2 = McpSupervisor::new(s.clone(), fake2.clone());
        sup2.reload().await;
        assert_eq!(
            fake2.opened("redmine"),
            0,
            "outils connus, serveur lazy : rien ne démarre"
        );
        assert_eq!(sup2.server_lines().await, vec!["redmine : 2 outils"]);
        assert_eq!(sup2.statuses().await[0].state, ServerState::Configured);

        sup2.call("mcp__redmine__create_issue", &json!({}))
            .await
            .unwrap();
        assert_eq!(fake2.opened("redmine"), 1, "démarrage au premier appel");
    }

    #[tokio::test]
    async fn a_broken_server_backs_off_then_fails_until_restarted() {
        let (_d, _s, clock, fake, sup) = setup().await;
        fake.serve("forge", server(two_tools()));
        fake.fail_open.lock().unwrap().insert(
            "forge".into(),
            "exécutable introuvable dans PATH : forge".into(),
        );
        declare(&sup, "forge", "");
        sup.reload().await;

        let st = &sup.statuses().await[0];
        assert_eq!((st.state, st.failures), (ServerState::Connecting, 1));
        assert!(st.last_error.as_deref().unwrap().contains("introuvable"));

        let slot = sup.slot("forge").await.unwrap();
        let e = sup.ensure_live(&slot).await.err().expect("échec attendu");
        assert!(e.contains("nouvel essai dans"), "{e}");
        assert_eq!(
            fake.opened("forge"),
            1,
            "le backoff évite de relancer aussitôt"
        );

        for _ in 0..7 {
            clock.advance_ms(301_000);
            let _ = sup.ensure_live(&slot).await;
        }
        assert_eq!(sup.statuses().await[0].state, ServerState::Failed);
        clock.advance_ms(301_000);
        let e = sup.ensure_live(&slot).await.err().expect("échec attendu");
        assert!(e.contains("penelope mcp restart forge"), "{e}");

        fake.fail_open.lock().unwrap().clear();
        let st = sup.restart("forge").await.unwrap();
        assert_eq!(
            (st.state, st.failures, st.tool_count),
            (ServerState::Ready, 0, 2)
        );
    }

    #[tokio::test]
    async fn a_server_confused_by_the_probe_is_retried_with_initialize() {
        let (_d, _s, _c, fake, sup) = setup().await;
        fake.serve("vieux", server(two_tools()));
        fake.dies_on_probe.lock().unwrap().push("vieux".into());
        declare(&sup, "vieux", "");
        sup.reload().await;

        let st = &sup.statuses().await[0];
        assert_eq!((st.state, st.tool_count), (ServerState::Ready, 2));
        assert_eq!(fake.opened("vieux"), 2, "un second processus, sans sonde");

        // Redémarré, il ne repasse pas par la sonde qui le fait tomber.
        sup.restart("vieux").await.unwrap();
        assert_eq!(fake.opened("vieux"), 3);
    }

    #[tokio::test]
    async fn changes_in_mcp_d_are_picked_up_live() {
        let (_d, s, _c, fake, sup) = setup().await;
        fake.serve("a", server(two_tools()));
        fake.serve(
            "b",
            server(Arc::new(Mutex::new(vec![tool("ping_b", json!({}))]))),
        );
        declare(&sup, "a", "");
        sup.reload().await;
        assert_eq!(fake.opened("a"), 1);

        declare(&sup, "b", "");
        sup.maintenance().await;
        assert!(s.mcp_tools.get("mcp__b__ping_b").await.unwrap().is_some());

        declare(&sup, "a", "args = [\"--verbose\"]\n");
        sup.maintenance().await;
        assert_eq!(fake.opened("a"), 2, "configuration changée : redémarré");

        std::fs::remove_file(sup.dir().join("a.toml")).unwrap();
        sup.maintenance().await;
        assert!(
            s.mcp_tools
                .get("mcp__a__list_issues")
                .await
                .unwrap()
                .is_none()
        );
        let names: Vec<String> = sup.statuses().await.into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["b"]);
        let rows: i64 = s
            .store
            .read(|c| {
                Ok(
                    c.query_row("SELECT count(*) FROM mcp_servers WHERE name='a'", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn administration_rewrites_the_declaration() {
        let (_d, s, _c, fake, sup) = setup().await;
        fake.serve("a", server(two_tools()));
        fake.serve("c", server(two_tools()));
        declare(&sup, "a", "");
        sup.reload().await;

        sup.set_enabled("a", false).await.unwrap();
        let raw = std::fs::read_to_string(sup.dir().join("a.toml")).unwrap();
        assert!(raw.contains("enabled = false"), "{raw}");
        assert_eq!(sup.statuses().await[0].state, ServerState::Disabled);
        assert!(
            s.mcp_tools
                .get("mcp__a__list_issues")
                .await
                .unwrap()
                .is_none()
        );
        assert!(sup.server_lines().await.is_empty());

        sup.set_enabled("a", true).await.unwrap();
        assert!(
            s.mcp_tools
                .get("mcp__a__list_issues")
                .await
                .unwrap()
                .is_some()
        );

        sup.edit("a", &json!({"timeout": "60s"})).await.unwrap();
        assert_eq!(sup.config_of("a").await.unwrap().timeout, "60s");
        assert!(
            sup.edit("a", &json!({"couleur": "bleu"}))
                .await
                .unwrap_err()
                .contains("champ inconnu")
        );
        assert!(sup.edit("a", &json!({"timeout": "bientôt"})).await.is_err());

        let dup = ServerConfig::stdio("a", "/opt/mcp/a", &[]);
        assert!(
            sup.add(dup, false)
                .await
                .unwrap_err()
                .contains("existe déjà")
        );
        let report = sup
            .add(ServerConfig::stdio("c", "/opt/mcp/c", &[]), false)
            .await
            .unwrap();
        assert_eq!(report.added, vec!["c"]);
        sup.remove("c").await.unwrap();
        assert!(!sup.dir().join("c.toml").exists());
        assert!(sup.config_of("c").await.is_none());
    }

    #[tokio::test]
    async fn server_requests_and_list_changes_are_handled() {
        let (_d, s, _c, fake, sup) = setup().await;
        let tools = two_tools();
        fake.serve("a", server(tools.clone()));
        declare(&sup, "a", "roots = [\"~/projets/penelope\"]\n");
        sup.reload().await;
        let t = fake.last_transport("a");

        t.push_server_request(7, "roots/list", json!({}));
        t.push_server_request(
            8,
            "elicitation/create",
            json!({"message": "mot de passe ?"}),
        );
        t.push_server_request(9, "sampling/createMessage", json!({}));
        tools
            .lock()
            .unwrap()
            .push(tool("close_issue", json!({"destructiveHint": true})));
        t.push_notification("notifications/tools/list_changed", json!({}));

        for _ in 0..50 {
            if t.responses.lock().await.len() == 3
                && s.mcp_tools
                    .get("mcp__a__close_issue")
                    .await
                    .unwrap()
                    .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let responses = t.responses.lock().await.clone();
        let by_id = |id: u64| {
            responses
                .iter()
                .find(|(i, _)| *i == json!(id))
                .map(|(_, r)| r.clone())
                .unwrap()
        };
        let roots = by_id(7).unwrap();
        assert!(
            roots["roots"][0]["uri"]
                .as_str()
                .unwrap()
                .ends_with("/projets/penelope")
        );
        // Sans propriétaire joignable : rien d'annoncé, et une demande quand même reçue est
        // annulée sans que personne ait refusé (issue #12, parcours Telegram dans
        // `telegram::tests::mcp_elicitation_is_answered_from_telegram`).
        let log = t.call_log().await;
        let (_, init) = log.iter().find(|(m, _)| m == "initialize").unwrap();
        assert!(init["capabilities"].get("elicitation").is_none(), "{init}");
        assert!(init["capabilities"].get("sampling").is_none(), "{init}");
        assert_eq!(by_id(8).unwrap()["action"], "cancel");
        assert!(by_id(9).is_err());
        let close = s
            .mcp_tools
            .get("mcp__a__close_issue")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(close.risk, RiskClass::Destructive);
    }

    #[tokio::test]
    async fn a_lost_connection_is_counted_and_the_next_call_reconnects() {
        let (_d, _s, clock, fake, sup) = setup().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let base = server(two_tools());
        let n = calls.clone();
        fake.serve(
            "a",
            Arc::new(move |m, p| {
                if m == "tools/call" && n.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(McpError::Transport(
                        "le serveur a fermé la connexion".into(),
                    ));
                }
                base(m, p)
            }),
        );
        declare(&sup, "a", "");
        sup.reload().await;

        let e = sup
            .call("mcp__a__list_issues", &json!({}))
            .await
            .unwrap_err();
        assert!(e.contains("fermé la connexion"), "{e}");
        let st = &sup.statuses().await[0];
        assert_eq!(
            (st.state, st.failures, st.running),
            (ServerState::Connecting, 1, false)
        );

        clock.advance_ms(5_000);
        sup.call("mcp__a__list_issues", &json!({})).await.unwrap();
        assert_eq!(fake.opened("a"), 2);
        assert_eq!(sup.statuses().await[0].state, ServerState::Ready);
    }

    #[tokio::test]
    async fn tool_policy_and_eager_schemas_come_from_the_declaration() {
        let (_d, _s, _c, fake, sup) = setup().await;
        fake.serve("a", server(two_tools()));
        declare(
            &sup,
            "a",
            "eager_schemas = true\n[tool_policy]\ncreate_issue = \"deny\"\n",
        );
        sup.reload().await;
        assert_eq!(
            sup.tool_policy("mcp__a__create_issue").await.as_deref(),
            Some("deny")
        );
        assert_eq!(sup.tool_policy("mcp__a__list_issues").await, None);
        let eager: Vec<String> = sup
            .eager_tools()
            .await
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(eager, vec!["mcp__a__create_issue", "mcp__a__list_issues"]);
    }

    #[tokio::test]
    async fn doctor_names_what_is_broken_and_how_to_fix_it() {
        let (_d, s, _c, fake, sup) = setup().await;
        fake.serve("ok", server(two_tools()));
        fake.fail_open
            .lock()
            .unwrap()
            .insert("panne".into(), "exécutable introuvable".into());
        declare(&sup, "ok", "");
        declare(&sup, "panne", "");
        declare(
            &sup,
            "secret",
            "[env]\nAPI_KEY = \"${SECRET:forge_token}\"\n",
        );
        fake.serve("secret", server(two_tools()));
        std::fs::write(sup.dir().join("casse.toml"), "== pas du toml ==").unwrap();
        sup.reload().await;

        let checks = crate::doctor::mcp_checks(&s, &sup).await;
        let by_id = |id: &str| checks.iter().find(|c| c.id == id).unwrap().clone();
        assert!(by_id("mcp.ok").ok);
        let panne = by_id("mcp.panne");
        assert!(
            !panne.ok && panne.detail.contains("introuvable"),
            "{panne:?}"
        );
        assert!(panne.fix.unwrap().contains("penelope mcp logs panne"));
        let secret = by_id("mcp.secret");
        assert!(
            !secret.ok && secret.detail.contains("forge_token"),
            "{secret:?}"
        );
        assert!(!by_id("mcp.invalid.casse").ok);
    }

    #[test]
    fn binary_content_never_enters_the_transcript() {
        let r = ToolResult {
            content: vec![
                ContentBlock::Text { text: "ok".into() },
                ContentBlock::Image {
                    data: "QUJD".repeat(1000),
                    mime_type: "image/png".into(),
                },
                ContentBlock::Resource {
                    uri: "file:///a.md".into(),
                    text: Some("# titre".into()),
                    blob: None,
                    mime_type: Some("text/markdown".into()),
                },
            ],
            ..Default::default()
        };
        let v = result_json(&r);
        assert_eq!(v["content"][0]["text"], "ok");
        let img = v["content"][1]["text"].as_str().unwrap();
        assert!(img.contains("image/png") && !img.contains("QUJD"), "{img}");
        assert_eq!(v["content"][2]["text"], "# titre");
    }

    /// Un vrai serveur stdio (script Python), lancé sous le profil `mcp-stdio`.
    #[tokio::test]
    async fn a_real_stdio_server_runs_under_the_sandbox() {
        let Some(python) = penelope_platform::which("python3") else {
            eprintln!("python3 absent : test stdio réel ignoré");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let script = dir.path().join("fake_mcp.py");
        std::fs::write(&script, FAKE_PY).unwrap();
        let sup = McpSupervisor::new(s.clone(), Arc::new(ProcessConnector::new(s.clone())));
        std::fs::create_dir_all(sup.dir()).unwrap();
        std::fs::write(
            sup.dir().join("pyfake.toml"),
            format!(
                "command = \"{}\"\nargs = [\"{}\"]\ntimeout = \"10s\"\n[env]\nFAKE_GREETING = \"bonjour\"\n",
                python.display(),
                script.display()
            ),
        )
        .unwrap();

        sup.reload().await;
        let st = &sup.statuses().await[0];
        assert_eq!(st.state, ServerState::Ready, "{:?}", st.last_error);
        assert_eq!(st.tool_count, 1);

        let v = sup
            .call("mcp__pyfake__echo", &json!({"text": "salut"}))
            .await
            .unwrap();
        assert_eq!(v["content"][0]["text"], "bonjour salut");
        let logs = sup.logs("pyfake", 10).await.unwrap();
        assert!(logs.iter().any(|l| l.contains("pyfake prêt")), "{logs:?}");

        let pid_file = s.platform.dirs.pid_dir().join("mcp-pyfake.pid");
        assert!(pid_file.exists());
        sup.stop_all().await;
        assert!(!pid_file.exists(), "processus arrêté, fichier PID retiré");
    }

    /// Essai contre un vrai serveur installé : poignée de main et liste des outils,
    /// aucun appel d'outil. `PENELOPE_REAL_MCP=/chemin/du/serveur cargo test -p
    /// penelope-daemon real_mcp_server -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_mcp_server_handshake() {
        let Ok(command) = std::env::var("PENELOPE_REAL_MCP") else {
            eprintln!("PENELOPE_REAL_MCP absent");
            return;
        };
        let profile = std::env::var("PENELOPE_REAL_MCP_PROFILE").unwrap_or("mcp-stdio".into());
        // `CLE=valeur,CLE2=valeur2` : variables d'environnement du serveur.
        let env: String = std::env::var("PENELOPE_REAL_MCP_ENV")
            .unwrap_or_default()
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| format!("{k} = \"{v}\"\n"))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let sup = McpSupervisor::new(s.clone(), Arc::new(ProcessConnector::new(s.clone())));
        std::fs::create_dir_all(sup.dir()).unwrap();
        std::fs::write(
            sup.dir().join("reel.toml"),
            format!(
                "command = \"{command}\"\nsandbox_profile = \"{profile}\"\ntimeout = \"20s\"\n\
                 [env]\n{env}"
            ),
        )
        .unwrap();
        let started = std::time::Instant::now();
        sup.reload().await;
        let st = sup.statuses().await.remove(0);
        eprintln!(
            "état {:?}, protocole {:?}, {} outils en {} ms, sonde évitée : {}",
            st.state,
            st.protocol,
            st.tool_count,
            started.elapsed().as_millis(),
            sup.slot("reel").await.unwrap().info(|i| i.skip_probe)
        );
        eprintln!("erreur : {:?}", st.last_error);
        eprintln!("stderr : {:?}", sup.logs("reel", 20).await.unwrap());
        let tools = s.mcp_tools.list_server("reel").await.unwrap();
        for t in tools.iter().take(10) {
            eprintln!("  {} ({})", t.qualified, t.risk.as_str());
        }
        sup.stop_all().await;
        assert_eq!(st.state, ServerState::Ready);
    }

    const FAKE_PY: &str = r#"
import json, os, sys
print("pyfake prêt", file=sys.stderr, flush=True)
greeting = os.environ.get("FAKE_GREETING", "?")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    mid = msg.get("id")
    method = msg.get("method")
    if mid is None:
        continue
    if method == "initialize":
        res = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
               "serverInfo": {"name": "pyfake", "version": "1"}}
    elif method == "tools/list":
        res = {"tools": [{"name": "echo", "description": "Renvoie le texte",
                          "inputSchema": {"type": "object",
                                          "properties": {"text": {"type": "string"}},
                                          "required": ["text"]},
                          "annotations": {"readOnlyHint": True}}]}
    elif method == "tools/call":
        text = msg["params"]["arguments"]["text"]
        res = {"content": [{"type": "text", "text": greeting + " " + text}]}
    elif method == "ping":
        res = {}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": mid,
                          "error": {"code": -32601, "message": "Method not found"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": mid, "result": res}), flush=True)
"#;
}
