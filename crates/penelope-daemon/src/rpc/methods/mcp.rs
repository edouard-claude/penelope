//! Administration des serveurs MCP par RPC.

use super::*;

/// Déclaration de serveur passée en paramètre : `toml` (texte d'un fichier `mcp.d`) ou
/// `config` (objet JSON).
fn mcp_config_param(p: &Value) -> anyhow::Result<penelope_mcp::config::ServerConfig> {
    if let Some(raw) = p.get("toml").and_then(|v| v.as_str()) {
        let default_name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let mut v = penelope_mcp::config::parse_file(raw, default_name)?;
        if v.len() != 1 {
            anyhow::bail!(
                "une déclaration à la fois : le fichier en contient {}",
                v.len()
            );
        }
        let cfg = v.remove(0);
        if cfg.name.is_empty() {
            anyhow::bail!("paramètre `name` manquant (ou champ `name` dans la déclaration)");
        }
        return Ok(cfg);
    }
    match p.get("config") {
        Some(c) => Ok(serde_json::from_value(c.clone())?),
        None => anyhow::bail!("paramètre `toml` manquant"),
    }
}

/// Résultat d'une modification : ce qui a changé et l'état du serveur après coup.
async fn mcp_change(
    sup: &crate::mcp::McpSupervisor,
    name: &str,
    report: &crate::mcp::ReloadReport,
) -> Value {
    let status = sup.statuses().await.into_iter().find(|s| s.name == name);
    json!({"report": report, "status": status})
}

impl Rpc {
    /// Administration des serveurs MCP.
    pub(super) async fn mcp_admin(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        match method {
            method::MCP_LIST => {
                let sup = self.mcp()?;
                let servers: Vec<Value> = sup
                    .statuses()
                    .await
                    .into_iter()
                    .map(|st| {
                        json!({
                            "name": st.name,
                            "state": st.state.as_str(),
                            "transport": st.transport,
                            "tools": st.tool_count,
                            "running": st.running,
                            "lazy": st.lazy,
                            "keychain": st.keychain,
                            "protocol": st.protocol,
                            "calls": st.calls,
                            "errors": st.errors,
                            "p95_ms": st.p95_ms.round(),
                            "last_error": st.last_error,
                        })
                    })
                    .collect();
                let invalid: Vec<Value> = sup
                    .invalid()
                    .into_iter()
                    .map(|(file, error)| json!({"file": file, "error": error}))
                    .collect();
                Ok(json!({"servers": servers, "invalid": invalid, "dir": sup.dir()}))
            }
            method::MCP_SHOW => {
                let name = required_str(p, "name")?;
                self.mcp()?.show(&name).await.map_err(anyhow::Error::msg)
            }
            method::MCP_ADD => {
                let sup = self.mcp()?;
                let cfg = mcp_config_param(p)?;
                let name = cfg.name.clone();
                let report = sup.add(cfg, false).await.map_err(anyhow::Error::msg)?;
                Ok(mcp_change(&sup, &name, &report).await)
            }
            method::MCP_EDIT => {
                let sup = self.mcp()?;
                let name = required_str(p, "name")?;
                let patch = p
                    .get("patch")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("paramètre `patch` manquant"))?;
                let report = sup.edit(&name, &patch).await.map_err(anyhow::Error::msg)?;
                Ok(mcp_change(&sup, &name, &report).await)
            }
            method::MCP_RM => {
                let name = required_str(p, "name")?;
                let report = self
                    .mcp()?
                    .remove(&name)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"report": report}))
            }
            method::MCP_ENABLE | method::MCP_DISABLE => {
                let sup = self.mcp()?;
                let name = required_str(p, "name")?;
                let report = sup
                    .set_enabled(&name, method == method::MCP_ENABLE)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(mcp_change(&sup, &name, &report).await)
            }
            method::MCP_RESTART => {
                let name = required_str(p, "name")?;
                let status = self
                    .mcp()?
                    .restart(&name)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(serde_json::to_value(status)?)
            }
            method::MCP_TEST => {
                let sup = self.mcp()?;
                let cfg = match p.get("name").and_then(|n| n.as_str()) {
                    Some(name) if p.get("toml").is_none() => sup
                        .config_of(name)
                        .await
                        .ok_or_else(|| anyhow::anyhow!("serveur MCP introuvable : `{name}`"))?,
                    _ => mcp_config_param(p)?,
                };
                Ok(sup.test(&cfg).await)
            }
            method::MCP_LOGS => {
                let name = required_str(p, "name")?;
                let n = p.get("lines").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let lines = self
                    .mcp()?
                    .logs(&name, n.clamp(1, 500))
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"name": name, "lines": lines}))
            }
            method::MCP_AUTH => {
                let name = required_str(p, "name")?;
                if let Some(callback) = p.get("callback").and_then(|c| c.as_str()) {
                    let server = crate::mcp_auth::complete(&self.daemon, callback)
                        .await
                        .map_err(anyhow::Error::msg)?;
                    if server != name {
                        anyhow::bail!("cette adresse autorise `{server}`, pas `{name}`");
                    }
                    let status = match self.daemon.hooks.mcp_supervisor() {
                        Some(sup) => sup
                            .restart(&server)
                            .await
                            .map(|st| serde_json::to_value(st).unwrap_or_default())
                            .unwrap_or_else(|e| json!({"error": e})),
                        None => Value::Null,
                    };
                    return Ok(json!({"server": server, "authorized": true, "status": status}));
                }
                let sup = self
                    .daemon
                    .hooks
                    .mcp_supervisor()
                    .ok_or_else(|| anyhow::anyhow!("superviseur MCP indisponible"))?;
                let cfg = sup
                    .config_of(&name)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("serveur MCP `{name}` inconnu"))?;
                let start = crate::mcp_auth::start(&self.daemon, &cfg, None)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let mut v = serde_json::to_value(&start)?;
                v["text"] = json!(crate::mcp_auth::prompt_text(&start));
                Ok(v)
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }

    fn mcp(&self) -> anyhow::Result<Arc<crate::mcp::McpSupervisor>> {
        self.daemon
            .hooks
            .mcp_supervisor()
            .ok_or_else(|| anyhow::anyhow!("superviseur MCP non démarré dans ce daemon"))
    }
}
