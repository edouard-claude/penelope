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

    fn profile(&self, cfg: &ServerConfig) -> Result<penelope_platform::Profile, String> {
        stdio_profile(&self.services, cfg)
    }
}

/// Profil de bac à sable d'un serveur stdio. `full` exige que le serveur figure dans
/// `sandbox.allow_full_for` (§13.2). Les profils imposés refusent les mêmes lectures que
/// le shell (`sandbox.deny_read` : clés SSH, secrets, base, configuration) et ferment le
/// trousseau : un paquet tiers ne lit pas ce que `shell_exec` ne lit pas (issue #89). Un
/// chemin refusé qui contient le répertoire de données du serveur ou une de ses racines
/// reste lisible pour eux. Un serveur de `sandbox.allow_keychain_for` garde tout cela et
/// joint le trousseau, sans plus (issue #122).
pub fn stdio_profile(
    s: &Services,
    cfg: &ServerConfig,
) -> Result<penelope_platform::Profile, String> {
    use penelope_platform::{Profile, ProfileKind};
    let data_dir = s.platform.dirs.data().join("mcp-data").join(&cfg.name);
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let mut profile = match ProfileKind::parse(&cfg.sandbox_profile) {
        Some(ProfileKind::Full) => {
            let allowed = s.config.config().sandbox.allow_full_for.clone();
            if !allowed.iter().any(|n| n == &cfg.name) {
                return Err(format!(
                    "le profil `full` de `{0}` doit être autorisé explicitement : \
                     `penelope config set sandbox.allow_full_for '[\"{0}\"]'`",
                    cfg.name
                ));
            }
            return Ok(Profile::full());
        }
        Some(ProfileKind::ReadOnly) => Profile::read_only(),
        Some(ProfileKind::WorkspaceWrite) => Profile::workspace_write(data_dir.clone()),
        _ => Profile::mcp_stdio(data_dir.clone(), Vec::new()),
    };
    let mut kept: Vec<std::path::PathBuf> = vec![data_dir];
    kept.extend(cfg.roots.iter().map(|r| s.platform.dirs.expand(r)));
    profile.deny_read = crate::executor::denied_reads(s)
        .into_iter()
        .filter(|d| !kept.iter().any(|k| k.starts_with(d)))
        .collect();
    Ok(profile.with_keychain(declared_for_keychain(s, cfg)))
}

fn declared_for_keychain(s: &Services, cfg: &ServerConfig) -> bool {
    s.config
        .config()
        .sandbox
        .allow_keychain_for
        .iter()
        .any(|n| n == &cfg.name)
}

/// Vrai si le processus du serveur joint le trousseau : serveur stdio déclaré dans
/// `sandbox.allow_keychain_for`, ou profil `full` autorisé. Un serveur distant n'a pas de
/// processus local, donc pas de trousseau (issue #122).
pub fn keychain_open(s: &Services, cfg: &ServerConfig) -> bool {
    if cfg.effective_transport() != "stdio" {
        return false;
    }
    match penelope_platform::ProfileKind::parse(&cfg.sandbox_profile) {
        Some(penelope_platform::ProfileKind::Full) => s
            .config
            .config()
            .sandbox
            .allow_full_for
            .iter()
            .any(|n| n == &cfg.name),
        _ => declared_for_keychain(s, cfg),
    }
}

/// Un serveur confiné qui échoue sur le trousseau n'y voit qu'« introuvable », même pour
/// un secret bien rangé : la phrase qui nomme le bac à sable et le réglage, au lieu de
/// laisser chercher le secret ailleurs (issue #122). `None` si le texte ne parle pas du
/// trousseau ou si le serveur le joint.
pub fn keychain_hint(s: &Services, cfg: &ServerConfig, text: &str) -> Option<String> {
    const CUES: [&str; 5] = ["keychain", "keyring", "trousseau", "secitem", "errsec"];
    let lower = text.to_lowercase();
    if cfg.effective_transport() != "stdio"
        || keychain_open(s, cfg)
        || !CUES.iter().any(|c| lower.contains(c))
    {
        return None;
    }
    Some(format!(
        "[Pénélope : `{0}` tourne sous bac à sable (`{1}`) et le trousseau macOS lui est \
         fermé : ce qu'il y cherche lui paraît introuvable, même rangé. Le secret n'est \
         donc pas forcément absent. Si `{0}` doit lire ses propres identifiants : \
         `penelope config set sandbox.allow_keychain_for '[\"{0}\"]'`. Sinon, lui passer \
         le secret par son environnement : `${{SECRET:nom}}` dans la table [env] de sa \
         déclaration.]",
        cfg.name, cfg.sandbox_profile
    ))
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
    /// Changements d'outils à dire au propriétaire (#92).
    notices: std::sync::Mutex<Vec<String>>,
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
            notices: std::sync::Mutex::new(Vec::new()),
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
        let keychain = keychain_open(&self.services, &slot.config());
        if let Some(l) = live.as_ref() {
            if l.keychain == keychain {
                return Ok(l.client.clone());
            }
            // L'accès au trousseau a changé depuis le lancement (réglage posé ou retiré) :
            // le processus repart sous le profil en vigueur (issue #122).
            if let Some(old) = live.take() {
                tracing::info!(server = %slot.name, keychain, "trousseau modifié, relance");
                let _ = old.client.close().await;
                old.pump.abort();
            }
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
                    keychain,
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
                        let msg = self.explain(cfg, &e2, &logs);
                        return Err((msg, logs, e2.needs_auth()));
                    }
                }
            }
            Err(e) => {
                let logs = transport.logs(20).await;
                let _ = transport.close().await;
                return Err((self.explain(cfg, &e, &logs), logs, false));
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

    /// [`explain`], plus la phrase du trousseau si l'échec en parle (issue #122).
    fn explain(&self, cfg: &ServerConfig, e: &McpError, logs: &[String]) -> String {
        let mut msg = explain(cfg, e, logs);
        let seen = format!("{e}\n{}", logs.join("\n"));
        if let Some(hint) = keychain_hint(&self.services, cfg, &seen) {
            msg.push(' ');
            msg.push_str(&hint);
        }
        msg
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
        let report = self
            .services
            .mcp_tools
            .replace_server_tools(&cfg.name, tools, &self.now())
            .await
            .map_err(|e| e.to_string())?;
        self.watch_tool_changes(&cfg.name, &report).await;
        slot.info(|i| {
            i.tool_count = n;
            i.last_used_ms = self.now_ms();
        });
        self.persist(slot).await;
        tracing::info!(server = %cfg.name, tools = n, "outils MCP inscrits");
        Ok(n)
    }

    /// Rug pull et tool poisoning (#92) : un outil dont la description, le schéma ou les
    /// annotations changent perd ses règles « Toujours », et le propriétaire le sait ; une
    /// description où le détecteur local voit une consigne est journalisée.
    async fn watch_tool_changes(&self, server: &str, report: &penelope_mcp::ReplaceReport) {
        let s = &self.services;
        for (tool, flags) in &report.flagged {
            tracing::warn!(outil = %tool, motifs = ?flags, "description d'outil MCP suspecte");
            let _ = s
                .events
                .append(penelope_kernel::event::EventDraft::new(
                    "mcp_tool_suspicious",
                    json!({"server": server, "tool": tool, "findings": flags}),
                ))
                .await;
        }
        if report.changed.is_empty() {
            return;
        }
        let rules = s.policies.active_rules().await.unwrap_or_default();
        for tool in &report.changed {
            let mut revoked = 0usize;
            for r in rules.iter().filter(|r| {
                r.tool.as_deref() == Some(tool.as_str())
                    && r.decision == penelope_kernel::risk::PolicyDecision::Auto
            }) {
                if s.policies.revoke(&r.id).await.unwrap_or(false) {
                    revoked += 1;
                }
            }
            let _ = s
                .events
                .append(penelope_kernel::event::EventDraft::new(
                    "mcp.tool_changed",
                    json!({"server": server, "tool": tool, "rules_revoked": revoked}),
                ))
                .await;
            tracing::warn!(outil = %tool, regles = revoked, "outil MCP modifié");
            if revoked > 0 {
                // Remis au propriétaire par la maintenance, qui tient le canal de message.
                let notice = format!(
                    "🔁 L'outil `{tool}` du serveur `{server}` a changé (description, schéma \
                     ou annotations) depuis ton accord : {revoked} règle(s) « Toujours » \
                     révoquée(s), la prochaine utilisation redemande."
                );
                match self.notices.lock() {
                    Ok(mut g) => g.push(notice),
                    Err(p) => p.into_inner().push(notice),
                }
            }
        }
    }

    /// Messages pour le propriétaire, en attente d'envoi (#92).
    pub fn take_notices(&self) -> Vec<String> {
        match self.notices.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(p) => std::mem::take(&mut *p.into_inner()),
        }
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
    pub async fn call(
        &self,
        qualified: &str,
        args: &Value,
        from: crate::elicitation::Destination,
    ) -> Result<Value, String> {
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
        // Le serveur peut demander une confirmation pendant l'appel : elle doit revenir
        // dans la conversation qui l'a provoqué (issue #143). Le garde tombe avec
        // l'appel, réussi ou non.
        let _scope = self.services.elicitations.scope(&tool.server, from);
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
                if result.is_error {
                    let said: Vec<&str> = result
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if let Some(hint) =
                        keychain_hint(&self.services, &slot.config(), &said.join("\n"))
                    {
                        result.content.push(ContentBlock::Text { text: hint });
                    }
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
                    // Le serveur répond mais refuse nos requêtes : `mcp list` et `doctor`
                    // doivent le dire, un état « prêt » mentirait (issue #126).
                    McpError::Rpc { code, .. }
                        if *code == penelope_mcp::protocol::HEADER_MISMATCH =>
                    {
                        slot.info(|i| {
                            i.state = ServerState::Degraded;
                            i.last_error = Some(e.to_string());
                        });
                        self.persist(&slot).await;
                    }
                    McpError::Timeout { .. } => {
                        slot.info(|i| {
                            i.state = ServerState::Degraded;
                            i.last_error = Some(e.to_string());
                        });
                        self.persist(&slot).await;
                    }
                    _ => self.persist(&slot).await,
                }
                let mut message = format!("`{qualified}` : {}", call_error(&e));
                if let Some(hint) = keychain_hint(&self.services, &slot.config(), &message) {
                    message.push('\n');
                    message.push_str(&hint);
                }
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
            keychain: keychain_open(&self.services, &c),
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
                // Un vrai appel d'outil : `initialize` et `tools/list` ne disent rien des
                // en-têtes d'un `tools/call` (issue #126).
                let call = match &tools {
                    Ok(t) => match probe_tool(cfg, t) {
                        Some(name) => {
                            let r = client
                                .call_tool(&name, json!({}), None, Some(cfg.timeout_for(&name)))
                                .await;
                            Some(match r {
                                Ok(res) if res.is_error => json!({
                                    "tool": name, "ok": true,
                                    "note": "l'outil répond en erreur, le protocole passe",
                                }),
                                Ok(_) => json!({"tool": name, "ok": true}),
                                // Réponse structurée du serveur à l'appel (arguments, erreur
                                // métier) : l'échange a eu lieu, le protocole passe.
                                Err(McpError::Rpc { code, message, .. })
                                    if !PROTOCOL_FAULTS.contains(&code) =>
                                {
                                    json!({
                                        "tool": name, "ok": true,
                                        "note": format!(
                                            "le serveur refuse cet appel sans argument ({code} : \
                                             {message}), le protocole passe"
                                        ),
                                    })
                                }
                                Err(e) => json!({
                                    "tool": name, "ok": false,
                                    "error": call_error(&e),
                                }),
                            })
                        }
                        None => None,
                    },
                    Err(_) => None,
                };
                let logs = client.transport().logs(10).await;
                let _ = client.close().await;
                pump.abort();
                match tools {
                    Ok(t) if call.as_ref().is_some_and(|c| c["ok"] == false) => {
                        let c = call.unwrap_or_default();
                        json!({
                            "ok": false,
                            "protocol": client.version().as_str(),
                            "tools": t.len(),
                            "error": format!(
                                "{} outil(s) listés, mais l'appel de `{}` échoue : {}",
                                t.len(),
                                c["tool"].as_str().unwrap_or("?"),
                                c["error"].as_str().unwrap_or("?")
                            ),
                            "call": c,
                            "logs": logs,
                        })
                    }
                    Ok(t) => json!({
                        "ok": true,
                        "protocol": client.version().as_str(),
                        "server_info": client.server_info(),
                        "tools": t.len(),
                        "names": t.iter().take(30).map(|d| d.name.clone()).collect::<Vec<_>>(),
                        "call": call,
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
        let before = v.clone();
        for (k, val) in fields {
            if k == "name" {
                return Err("le nom d'un serveur ne change pas : le retirer puis l'ajouter".into());
            }
            let Some(current) = v.get(k) else {
                return Err(format!("champ inconnu : `{k}`"));
            };
            // Une valeur seule vaut une liste d'un élément (`roots "/a"`), comme pour
            // `config set` (issue #138).
            v[k] = penelope_kernel::config::list_value(k, current, val.clone())?;
        }
        let updated: ServerConfig = serde_json::from_value(v).map_err(|_| {
            fields
                .keys()
                .map(|k| {
                    format!(
                        "`{k}` attend {}",
                        penelope_kernel::config::expected_shape(&before[k])
                    )
                })
                .collect::<Vec<_>>()
                .join(" ; ")
        })?;
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
    async fn call_tool(
        &self,
        qualified: &str,
        args: &Value,
        from: crate::elicitation::Destination,
    ) -> Result<Value, String> {
        self.call(qualified, args, from).await
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

/// Erreurs JSON-RPC qui disent que la requête elle-même est fautive (en-têtes, forme) :
/// `mcp test` échoue sur elles, pas sur un refus de l'outil (issue #126).
const PROTOCOL_FAULTS: [i32; 2] = [
    penelope_mcp::protocol::HEADER_MISMATCH,
    penelope_mcp::protocol::INVALID_REQUEST,
];

/// Outil qu'essaie `penelope mcp test` : en lecture d'après ses annotations (et sans
/// surcharge contraire de la déclaration), sans argument requis, jamais refusé par la
/// politique ; les `list`, `get` et `whoami` d'abord.
fn probe_tool(
    cfg: &ServerConfig,
    tools: &[penelope_mcp::protocol::ToolDescriptor],
) -> Option<String> {
    let mut candidates: Vec<&penelope_mcp::protocol::ToolDescriptor> = tools
        .iter()
        .filter(|t| {
            t.annotations["readOnlyHint"] == true && t.annotations["destructiveHint"] != true
        })
        .filter(|t| {
            t.input_schema["required"]
                .as_array()
                .is_none_or(|r| r.is_empty())
        })
        .filter(|t| cfg.tool_risk.get(&t.name).is_none_or(|r| r == "read"))
        .filter(|t| cfg.tool_policy.get(&t.name).is_none_or(|p| p != "deny"))
        .collect();
    let rank = |n: &str| {
        let n = n.to_lowercase();
        if ["whoami", "list", "get"].iter().any(|k| n.contains(k)) {
            0
        } else {
            1
        }
    };
    candidates.sort_by_key(|t| (rank(&t.name), t.name.clone()));
    candidates.first().map(|t| t.name.clone())
}

/// Erreur d'un appel d'outil, pour le modèle et pour `mcp test` : un désaccord d'en-têtes
/// (-32020) est un défaut du client, pas des arguments (issue #126).
fn call_error(e: &McpError) -> String {
    match e {
        McpError::Rpc { code, .. } if *code == penelope_mcp::protocol::HEADER_MISMATCH => {
            format!(
                "{e}\n[Pénélope : le serveur refuse les en-têtes HTTP de la requête (-32020). \
                 C'est un défaut du client MCP de Pénélope, pas des arguments : ne réessaie \
                 pas et ne reformule pas l'appel, signale-le au propriétaire.]"
            )
        }
        other => other.to_string(),
    }
}

/// Message d'échec de connexion, avec les dernières lignes de stderr et, si le bac à
/// sable semble en cause, la marche à suivre.
fn explain(cfg: &ServerConfig, e: &McpError, logs: &[String]) -> String {
    let mut msg = e.to_string();
    // Les lignes ajoutées par le transport (fin du processus, sortie vide) sont déjà dans
    // l'erreur (issue #114).
    let tail: Vec<&String> = logs
        .iter()
        .filter(|l| !l.starts_with("(rien sur la sortie d'erreur") && !l.starts_with("(processus "))
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
pub(crate) mod testing;

#[cfg(test)]
mod tests;
