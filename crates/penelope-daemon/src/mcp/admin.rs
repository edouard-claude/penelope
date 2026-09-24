//! Entretien, administration (`penelope mcp …`) et persistance des serveurs.

use super::*;

impl McpSupervisor {
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

    pub(super) async fn row(&self, name: &str) -> (Option<String>, usize) {
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

    pub(super) async fn persist(&self, slot: &Arc<Slot>) {
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

    pub(super) async fn delete_row(&self, name: &str) {
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

#[async_trait::async_trait]
impl crate::ports::McpAdmin for McpSupervisor {
    async fn statuses(&self) -> Vec<ServerStatus> {
        McpSupervisor::statuses(self).await
    }
    fn invalid(&self) -> Vec<(String, String)> {
        McpSupervisor::invalid(self)
    }
    fn dir(&self) -> &std::path::Path {
        McpSupervisor::dir(self)
    }
    async fn show(&self, name: &str) -> Result<Value, String> {
        McpSupervisor::show(self, name).await
    }
    async fn prompts(&self, name: &str) -> Result<Vec<Value>, String> {
        McpSupervisor::prompts(self, name).await
    }
    async fn get_prompt(&self, name: &str, prompt: &str, args: Value) -> Result<Value, String> {
        McpSupervisor::get_prompt(self, name, prompt, args).await
    }
    async fn logs(&self, name: &str, n: usize) -> Result<Vec<String>, String> {
        McpSupervisor::logs(self, name, n).await
    }
    async fn restart(&self, name: &str) -> Result<ServerStatus, String> {
        McpSupervisor::restart(self, name).await
    }
    async fn test(&self, cfg: &ServerConfig) -> Value {
        McpSupervisor::test(self, cfg).await
    }
    async fn config_of(&self, name: &str) -> Option<ServerConfig> {
        McpSupervisor::config_of(self, name).await
    }
    async fn add(&self, cfg: ServerConfig, replace: bool) -> Result<ReloadReport, String> {
        McpSupervisor::add(self, cfg, replace).await
    }
    async fn edit(&self, name: &str, patch: &Value) -> Result<ReloadReport, String> {
        McpSupervisor::edit(self, name, patch).await
    }
    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<ReloadReport, String> {
        McpSupervisor::set_enabled(self, name, enabled).await
    }
    async fn remove(&self, name: &str) -> Result<ReloadReport, String> {
        McpSupervisor::remove(self, name).await
    }
    async fn reload(&self) -> ReloadReport {
        McpSupervisor::reload(self).await
    }
    async fn task_status(&self, server: &str, task_ref: &str) -> Result<Value, String> {
        McpSupervisor::task_status(self, server, task_ref).await
    }
    fn take_notices(&self) -> Vec<String> {
        McpSupervisor::take_notices(self)
    }
}
