//! Cycle de vie des serveurs : chargement de `mcp.d`, connexion, pompe, arrêt.

use super::*;

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

    pub(super) fn now_ms(&self) -> i64 {
        self.services.clock.now_ms()
    }

    pub(super) fn now(&self) -> String {
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

    pub(super) fn dir_fingerprint(&self) -> String {
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

    pub(super) async fn slot(&self, name: &str) -> Option<Arc<Slot>> {
        self.slots.read().await.get(name).cloned()
    }

    /// Connexion vivante, démarrée au besoin, dans le respect du backoff.
    pub(super) async fn ensure_live(&self, slot: &Arc<Slot>) -> Result<Arc<McpClient>, String> {
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
    pub(super) async fn connect(
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
    pub(super) async fn refresh_tools(&self, slot: &Arc<Slot>) -> Result<usize, String> {
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
    pub(super) async fn connection_lost(&self, slot: &Arc<Slot>, e: &McpError) {
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
    pub(super) async fn stop_slot(&self, slot: &Arc<Slot>) -> Vec<String> {
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
}
