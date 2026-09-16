//! Client MCP : négociation de version et primitives (§8.2, §8.4).

use crate::error::{McpError, Result};
use crate::protocol::*;
use crate::transport::Transport;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Ce que la négociation a établi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Negotiated {
    pub version: ProtocolVersion,
    pub capabilities: ServerCapabilities,
    pub server_info: Value,
    /// Mode sans état (2026-07-28) : pas de `initialize`, `_meta` par requête.
    pub stateless: bool,
    pub instructions: Option<String>,
}

pub struct McpClient {
    pub name: String,
    transport: Arc<dyn Transport>,
    negotiated: Negotiated,
    default_timeout: Duration,
    /// Concurrence maximale par serveur (4 par défaut, §8.3).
    permits: Arc<Semaphore>,
    log_level: Option<String>,
}

impl McpClient {
    /// Négociation complète (§8.2).
    ///
    /// 1. `server/discover` en 2026-07-28 : succès ⇒ mode sans état.
    /// 2. Échec (méthode inconnue, 404, 405) ⇒ `initialize` avec la version la plus
    ///    récente de l'ancienne famille, puis descente selon la réponse du serveur.
    /// 3. `UnsupportedProtocolVersion` (−32022) ⇒ meilleure version commune annoncée.
    pub async fn connect(
        name: impl Into<String>,
        transport: Arc<dyn Transport>,
        preferred: ProtocolVersion,
        timeout: Duration,
        max_concurrency: usize,
    ) -> Result<McpClient> {
        let name = name.into();
        let negotiated = negotiate(transport.as_ref(), preferred, timeout).await?;

        let client = McpClient {
            name,
            transport,
            negotiated,
            default_timeout: timeout,
            permits: Arc::new(Semaphore::new(max_concurrency.max(1))),
            log_level: None,
        };

        // En mode historique, le handshake se termine par `notifications/initialized`.
        if !client.negotiated.stateless {
            let _ = client
                .transport
                .notify("notifications/initialized", json!({}))
                .await;
        }
        Ok(client)
    }

    pub fn version(&self) -> ProtocolVersion {
        self.negotiated.version
    }
    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.negotiated.capabilities
    }
    pub fn server_info(&self) -> &Value {
        &self.negotiated.server_info
    }
    pub fn instructions(&self) -> Option<&str> {
        self.negotiated.instructions.as_deref()
    }
    pub fn transport_kind(&self) -> &'static str {
        self.transport.kind()
    }
    pub fn transport(&self) -> Arc<dyn Transport> {
        self.transport.clone()
    }
    pub fn negotiated(&self) -> &Negotiated {
        &self.negotiated
    }

    pub fn set_log_level(&mut self, level: Option<String>) {
        self.log_level = level;
    }

    /// Requête avec `_meta` adapté à la version, sous contrôle de concurrence.
    async fn call(
        &self,
        method: &str,
        mut params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| McpError::Transport("sémaphore fermé".into()))?;

        if self.negotiated.stateless {
            let meta = request_meta(self.negotiated.version, self.log_level.as_deref(), None);
            if let Some(o) = params.as_object_mut() {
                o.insert("_meta".into(), meta);
            }
        }
        self.transport
            .request(method, params, timeout.unwrap_or(self.default_timeout))
            .await
    }

    // ------------------------------------------------------------ outils

    /// `tools/list`, **paginé** (§8.4).
    pub async fn list_tools(&self) -> Result<Vec<ToolDescriptor>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let v = self.call("tools/list", params, None).await?;
            if let Some(a) = v.get("tools").and_then(|t| t.as_array()) {
                out.extend(a.iter().filter_map(ToolDescriptor::parse));
            }
            match v.get("nextCursor").and_then(|c| c.as_str()) {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    /// `tools/call`. Valide `structuredContent` contre `outputSchema` si fourni.
    pub async fn call_tool(
        &self,
        tool: &str,
        args: Value,
        output_schema: Option<&Value>,
        timeout: Option<Duration>,
    ) -> Result<ToolResult> {
        let v = self
            .call(
                "tools/call",
                json!({"name": tool, "arguments": args}),
                timeout,
            )
            .await?;
        let result = ToolResult::parse(&v);

        if let (Some(schema), Some(structured)) = (output_schema, &result.structured)
            && let Err(e) = penelope_kernel::schema::validate_ok(schema, structured)
        {
            // Le résultat n'est pas jeté : l'écart est signalé au modèle, qui peut
            // se corriger (§8.4, « erreurs d'exécution renvoyées au modèle »).
            tracing::warn!(
                server = %self.name, tool, error = %e,
                "structuredContent non conforme à outputSchema"
            );
            let mut r = result;
            r.is_error = true;
            r.content.push(ContentBlock::Text {
                text: format!(
                    "[avertissement du harnais : la sortie structurée ne respecte pas \
                         outputSchema — {e}]"
                ),
            });
            return Ok(r);
        }
        Ok(result)
    }

    // ------------------------------------------------------------ ressources

    pub async fn list_resources(&self) -> Result<Vec<Value>> {
        self.paginated("resources/list", "resources").await
    }

    pub async fn list_resource_templates(&self) -> Result<Vec<Value>> {
        self.paginated("resources/templates/list", "resourceTemplates")
            .await
    }

    pub async fn read_resource(&self, uri: &str) -> Result<Vec<ContentBlock>> {
        let v = self
            .call("resources/read", json!({ "uri": uri }), None)
            .await?;
        Ok(v.get("contents")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().map(ContentBlock::parse).collect())
            .unwrap_or_default())
    }

    pub async fn subscribe_resource(&self, uri: &str) -> Result<()> {
        self.call("resources/subscribe", json!({ "uri": uri }), None)
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------ prompts

    pub async fn list_prompts(&self) -> Result<Vec<Value>> {
        self.paginated("prompts/list", "prompts").await
    }

    pub async fn get_prompt(&self, name: &str, args: Value) -> Result<Value> {
        self.call(
            "prompts/get",
            json!({"name": name, "arguments": args}),
            None,
        )
        .await
    }

    /// `completion/complete` : autocomplétion des arguments de prompts (§8.4).
    pub async fn complete(&self, reference: Value, argument: Value) -> Result<Vec<String>> {
        let v = self
            .call(
                "completion/complete",
                json!({"ref": reference, "argument": argument}),
                None,
            )
            .await?;
        Ok(v.get("completion")
            .and_then(|c| c.get("values"))
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    // ------------------------------------------------------------ divers

    /// Santé : `ping` sur les versions antérieures, `server/discover` léger en 2026-07-28.
    pub async fn health(&self) -> Result<()> {
        if self.negotiated.stateless {
            self.call("server/discover", json!({}), Some(Duration::from_secs(10)))
                .await?;
        } else {
            self.call("ping", json!({}), Some(Duration::from_secs(10)))
                .await?;
        }
        Ok(())
    }

    /// Niveau de log : `_meta.logLevel` en 2026-07-28, `logging/setLevel` avant.
    pub async fn apply_log_level(&mut self, level: &str) -> Result<()> {
        if self.negotiated.version.logging_via_meta() {
            self.log_level = Some(level.to_string());
            return Ok(());
        }
        if !self.negotiated.capabilities.logging {
            return Ok(());
        }
        self.call("logging/setLevel", json!({ "level": level }), None)
            .await?;
        Ok(())
    }

    /// Abonnement aux changements (§8.4).
    ///
    /// 2026-07-28 : `subscriptions/listen` avec opt-in explicite. Versions antérieures :
    /// les notifications `notifications/*/list_changed` arrivent d'elles-mêmes.
    pub async fn listen_changes(&self) -> Result<bool> {
        if !self.negotiated.version.is_stateless_core() {
            return Ok(self.negotiated.capabilities.tools_list_changed
                || self.negotiated.capabilities.resources_list_changed
                || self.negotiated.capabilities.prompts_list_changed);
        }
        let params = json!({
            "toolsListChanged": self.negotiated.capabilities.tools,
            "promptsListChanged": self.negotiated.capabilities.prompts,
            "resourcesListChanged": self.negotiated.capabilities.resources,
            "resourceSubscriptions": self.negotiated.capabilities.resources_subscribe,
        });
        match self.call("subscriptions/listen", params, None).await {
            Ok(_) => Ok(true),
            // Un serveur qui ne gère pas l'abonnement n'est pas en faute.
            Err(McpError::Rpc { code, .. }) if code == METHOD_NOT_FOUND => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Annulation d'une requête en cours (§8.4).
    pub async fn cancel(&self, request_id: Value, reason: &str) -> Result<()> {
        self.transport
            .notify(
                "notifications/cancelled",
                json!({"requestId": request_id, "reason": reason}),
            )
            .await
    }

    /// Polling d'une tâche (extension Tasks, §8.4).
    pub async fn task_get(&self, task_ref: &str) -> Result<Value> {
        self.call("tasks/get", json!({ "taskId": task_ref }), None)
            .await
    }

    pub async fn close(&self) -> Result<()> {
        self.transport.close().await
    }

    async fn paginated(&self, method: &str, key: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let v = match self.call(method, params, None).await {
                Ok(v) => v,
                // Une primitive non annoncée renvoie « méthode inconnue » : ce n'est pas
                // une panne du serveur.
                Err(McpError::Rpc { code, .. }) if code == METHOD_NOT_FOUND => break,
                Err(e) => return Err(e),
            };
            if let Some(a) = v.get(key).and_then(|t| t.as_array()) {
                out.extend(a.iter().cloned());
            }
            match v.get("nextCursor").and_then(|c| c.as_str()) {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }
}

/// Négociation (§8.2), extraite pour être testable indépendamment du client.
pub async fn negotiate(
    transport: &dyn Transport,
    preferred: ProtocolVersion,
    timeout: Duration,
) -> Result<Negotiated> {
    // 1. Mode sans état : `server/discover`.
    if preferred.is_stateless_core() {
        let probe_timeout = if transport.kind() == "stdio" {
            Duration::from_secs(3) // sonde courte pour stdio (§8.2)
        } else {
            timeout
        };
        let params = json!({"_meta": request_meta(preferred, None, None)});
        match transport
            .request("server/discover", params, probe_timeout)
            .await
        {
            Ok(v) if looks_like_discovery(&v) => {
                let version = v
                    .get("protocolVersion")
                    .and_then(|s| s.as_str())
                    .and_then(ProtocolVersion::parse)
                    .unwrap_or(preferred);
                return Ok(Negotiated {
                    version,
                    capabilities: ServerCapabilities::parse(
                        v.get("capabilities").unwrap_or(&Value::Null),
                    ),
                    server_info: v.get("serverInfo").cloned().unwrap_or(Value::Null),
                    stateless: true,
                    instructions: v
                        .get("instructions")
                        .and_then(|s| s.as_str())
                        .map(String::from),
                });
            }
            // Certains serveurs (le pont MCP de Xcode) répondent à une méthode inconnue par
            // un résultat `isError` plutôt que par -32601 : ce n'est pas une découverte.
            Ok(v) => {
                tracing::debug!(
                    response = %v,
                    "server/discover sans découverte, repli sur initialize"
                );
            }
            Err(e) if is_method_absent(&e) => {
                tracing::debug!("server/discover absent, repli sur initialize");
            }
            Err(e) if e.needs_auth() => return Err(e),
            Err(e) if !e.is_retryable() => {
                tracing::debug!(error = %e, "server/discover en échec, repli sur initialize");
            }
            // Un serveur stdio d'avant 2026 peut ignorer une requête reçue avant
            // `initialize` : la sonde courte expire, on passe au handshake historique.
            Err(McpError::Timeout { .. }) if transport.kind() == "stdio" => {
                tracing::debug!("server/discover sans réponse en stdio, repli sur initialize");
            }
            Err(e) => return Err(e),
        }
    }

    // 2. Handshake historique, en partant de la version la plus récente de la famille.
    let start = if preferred.is_stateless_core() {
        ProtocolVersion::V20251125
    } else {
        preferred
    };
    initialize_with(transport, start, timeout).await
}

async fn initialize_with(
    transport: &dyn Transport,
    version: ProtocolVersion,
    timeout: Duration,
) -> Result<Negotiated> {
    let params = json!({
        "protocolVersion": version.as_str(),
        "capabilities": client_capabilities(version),
        "clientInfo": client_info(),
    });
    match transport.request("initialize", params, timeout).await {
        Ok(v) => {
            // Le serveur impose sa version : on la prend si on la connaît.
            let agreed = v
                .get("protocolVersion")
                .and_then(|s| s.as_str())
                .and_then(ProtocolVersion::parse)
                .unwrap_or(version);
            Ok(Negotiated {
                version: agreed,
                capabilities: ServerCapabilities::parse(
                    v.get("capabilities").unwrap_or(&Value::Null),
                ),
                server_info: v.get("serverInfo").cloned().unwrap_or(Value::Null),
                stateless: false,
                instructions: v
                    .get("instructions")
                    .and_then(|s| s.as_str())
                    .map(String::from),
            })
        }
        // 3. Version refusée : on prend la meilleure version commune annoncée.
        Err(McpError::Rpc {
            code,
            data,
            message,
            ..
        }) if code == UNSUPPORTED_PROTOCOL_VERSION => {
            let announced = announced_versions(data.as_ref());
            let best = announced
                .iter()
                .filter_map(|s| ProtocolVersion::parse(s))
                .max();
            match best {
                Some(b) if b != version => Box::pin(initialize_with(transport, b, timeout)).await,
                _ => Err(McpError::NoCommonVersion {
                    server: if announced.is_empty() {
                        vec![message]
                    } else {
                        announced
                    },
                }),
            }
        }
        Err(e) => Err(e),
    }
}

fn announced_versions(data: Option<&Value>) -> Vec<String> {
    let Some(d) = data else { return Vec::new() };
    for key in ["supported", "supportedVersions", "versions"] {
        if let Some(a) = d.get(key).and_then(|x| x.as_array()) {
            return a
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
        }
    }
    if let Some(s) = d.as_str() {
        return vec![s.to_string()];
    }
    Vec::new()
}

/// Vrai si l'erreur signifie « cette méthode n'existe pas ici » (§8.2 : méthode inconnue,
/// 404, 405).
/// Une réponse à `server/discover` qui décrit vraiment un serveur : au moins une version,
/// une identité ou des capacités, et pas un résultat en échec.
fn looks_like_discovery(v: &Value) -> bool {
    v.get("isError").and_then(|e| e.as_bool()) != Some(true)
        && ["protocolVersion", "serverInfo", "capabilities"]
            .iter()
            .any(|k| v.get(*k).is_some_and(|x| !x.is_null()))
}

fn is_method_absent(e: &McpError) -> bool {
    match e {
        McpError::Rpc { code, .. } => *code == METHOD_NOT_FOUND,
        McpError::Http { status, .. } => *status == 404 || *status == 405,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::LoopbackTransport;

    fn t(timeout_ms: u64) -> Duration {
        Duration::from_millis(timeout_ms)
    }

    /// Serveur 2026-07-28 : `server/discover` répond, mode sans état.
    #[tokio::test]
    async fn negotiates_stateless_core() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => Ok(json!({
                "protocolVersion":"2026-07-28",
                "capabilities":{"tools":{"listChanged":true}},
                "serverInfo":{"name":"forge","version":"2.0"},
                "instructions":"utilise create_pr"
            })),
            _ => Err(McpError::Rpc {
                code: METHOD_NOT_FOUND,
                message: "inconnu".into(),
                data: None,
            }),
        });
        let n = negotiate(tr.as_ref(), ProtocolVersion::V20260728, t(500))
            .await
            .unwrap();
        assert_eq!(n.version, ProtocolVersion::V20260728);
        assert!(n.stateless);
        assert!(n.capabilities.tools_list_changed);
        assert_eq!(n.instructions.as_deref(), Some("utilise create_pr"));
    }

    /// Serveur 2025-06-18 derrière la même URL : `server/discover` échoue, `initialize`
    /// prend le relais et le serveur impose sa version.
    #[tokio::test]
    async fn falls_back_to_initialize_and_takes_the_server_version() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => Err(McpError::Http {
                status: 404,
                body: String::new(),
            }),
            "initialize" => Ok(json!({
                "protocolVersion":"2025-06-18",
                "capabilities":{"tools":{},"resources":{"subscribe":true}},
                "serverInfo":{"name":"vieux"}
            })),
            _ => Ok(json!({})),
        });
        let n = negotiate(tr.as_ref(), ProtocolVersion::V20260728, t(500))
            .await
            .unwrap();
        assert_eq!(n.version, ProtocolVersion::V20250618);
        assert!(!n.stateless);
        assert!(n.capabilities.resources_subscribe);
    }

    /// Issue #9 : le pont MCP de Xcode répond à `server/discover` par un résultat
    /// `isError`. Ce n'est pas une découverte : le handshake historique doit suivre, sinon
    /// la première requête reçoit -32603 (« request before initialization completed »).
    #[tokio::test]
    async fn an_is_error_result_to_discover_falls_back_to_initialize() {
        let initialized = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = initialized.clone();
        let tr = LoopbackTransport::new("stdio", move |m, _| match m {
            "server/discover" => Ok(json!({
                "content": [{
                    "type": "text",
                    "text": "The message contained an unknown method 'server/discover'"
                }],
                "isError": true
            })),
            "initialize" => {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "xcode-tools", "version": "25317"}
                }))
            }
            "tools/list" if !seen.load(std::sync::atomic::Ordering::SeqCst) => {
                Err(McpError::Rpc {
                    code: -32603,
                    message: "Received a request before initialization completed.".into(),
                    data: None,
                })
            }
            "tools/list" => Ok(json!({"tools": [{
                "name": "BuildProject",
                "inputSchema": {"type": "object"}
            }]})),
            _ => Ok(json!({})),
        });
        let client = McpClient::connect(
            "xcode",
            tr.clone(),
            ProtocolVersion::V20260728,
            t(500),
            4,
        )
        .await
        .unwrap();
        assert!(!client.negotiated().stateless);
        assert_eq!(client.version(), ProtocolVersion::V20250618);
        assert!(initialized.load(std::sync::atomic::Ordering::SeqCst));
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);

        let order: Vec<String> = tr.call_log().await.into_iter().map(|(m, _)| m).collect();
        let pos = |m: &str| order.iter().position(|x| x == m).unwrap();
        assert!(pos("initialize") < pos("notifications/initialized"), "{order:?}");
        assert!(pos("notifications/initialized") < pos("tools/list"), "{order:?}");
        assert!(!looks_like_discovery(&json!({})), "réponse vide");
        assert!(looks_like_discovery(&json!({"protocolVersion": "2026-07-28"})));
    }

    /// Un serveur stdio qui ignore `server/discover` (la sonde expire) passe quand même
    /// par `initialize`.
    #[tokio::test]
    async fn a_silent_discover_probe_on_stdio_falls_back_to_initialize() {
        let tr = LoopbackTransport::new("stdio", |m, _| match m {
            "server/discover" => Err(McpError::Timeout {
                method: "server/discover".into(),
                ms: 3_000,
            }),
            "initialize" => Ok(json!({
                "protocolVersion":"2025-06-18",
                "capabilities":{"tools":{}},
                "serverInfo":{"name":"go-mcp"}
            })),
            _ => Ok(json!({})),
        });
        let n = negotiate(tr.as_ref(), ProtocolVersion::V20260728, t(500))
            .await
            .unwrap();
        assert_eq!(n.version, ProtocolVersion::V20250618);
        assert!(!n.stateless);

        // En HTTP, un délai dépassé reste une panne : rien ne dit que le serveur existe.
        let http = LoopbackTransport::new("http", |_, _| {
            Err(McpError::Timeout {
                method: "server/discover".into(),
                ms: 500,
            })
        });
        assert!(
            negotiate(http.as_ref(), ProtocolVersion::V20260728, t(500))
                .await
                .is_err()
        );
    }

    /// −32022 : on descend vers la meilleure version commune annoncée.
    #[tokio::test]
    async fn unsupported_version_picks_the_best_common() {
        let tr = LoopbackTransport::new("stdio", |m, p| match m {
            "server/discover" => Err(McpError::Rpc {
                code: METHOD_NOT_FOUND,
                message: "inconnu".into(),
                data: None,
            }),
            "initialize" => {
                let asked = p["protocolVersion"].as_str().unwrap_or("");
                if asked == "2024-11-05" {
                    Ok(json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}))
                } else {
                    Err(McpError::Rpc {
                        code: UNSUPPORTED_PROTOCOL_VERSION,
                        message: "version refusée".into(),
                        data: Some(json!({"supported":["2024-11-05"]})),
                    })
                }
            }
            _ => Ok(json!({})),
        });
        let n = negotiate(tr.as_ref(), ProtocolVersion::V20260728, t(500))
            .await
            .unwrap();
        assert_eq!(n.version, ProtocolVersion::V20241105);
    }

    #[tokio::test]
    async fn no_common_version_is_an_explicit_error() {
        let tr = LoopbackTransport::new("stdio", |m, _| match m {
            "server/discover" => Err(McpError::Rpc {
                code: METHOD_NOT_FOUND,
                message: "x".into(),
                data: None,
            }),
            _ => Err(McpError::Rpc {
                code: UNSUPPORTED_PROTOCOL_VERSION,
                message: "refus".into(),
                data: Some(json!({"supported":["1999-01-01"]})),
            }),
        });
        let e = negotiate(tr.as_ref(), ProtocolVersion::V20260728, t(500))
            .await
            .unwrap_err();
        assert!(matches!(e, McpError::NoCommonVersion { .. }), "{e:?}");
    }

    async fn client(tr: Arc<LoopbackTransport>) -> McpClient {
        McpClient::connect("test", tr, ProtocolVersion::V20260728, t(500), 4)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn tools_list_is_paginated() {
        let tr = LoopbackTransport::new("http", |m, p| match m {
            "server/discover" => {
                Ok(json!({"protocolVersion":"2026-07-28","capabilities":{"tools":{}}}))
            }
            "tools/list" => {
                let cursor = p.get("cursor").and_then(|c| c.as_str());
                match cursor {
                    None => Ok(json!({
                        "tools":[{"name":"a","inputSchema":{"type":"object"}}],
                        "nextCursor":"p2"
                    })),
                    Some("p2") => Ok(json!({
                        "tools":[{"name":"b","inputSchema":{"type":"object"}}]
                    })),
                    Some(_) => Ok(json!({"tools":[]})),
                }
            }
            _ => Ok(json!({})),
        });
        let c = client(tr).await;
        let tools = c.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1].name, "b");
    }

    #[tokio::test]
    async fn stateless_requests_carry_meta() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => {
                Ok(json!({"protocolVersion":"2026-07-28","capabilities":{"tools":{}}}))
            }
            _ => Ok(json!({"tools":[]})),
        });
        let c = client(tr.clone()).await;
        c.list_tools().await.unwrap();
        let log = tr.call_log().await;
        let (_, params) = log.iter().find(|(m, _)| m == "tools/list").unwrap();
        assert_eq!(
            params["_meta"]["io.modelcontextprotocol/protocolVersion"],
            "2026-07-28"
        );
        assert_eq!(params["_meta"]["clientInfo"]["name"], "penelope");
    }

    #[tokio::test]
    async fn structured_output_is_validated_against_the_schema() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => {
                Ok(json!({"protocolVersion":"2026-07-28","capabilities":{"tools":{}}}))
            }
            "tools/call" => Ok(json!({
                "content":[{"type":"text","text":"ok"}],
                "structuredContent": {"count": "pas un nombre"}
            })),
            _ => Ok(json!({})),
        });
        let c = client(tr).await;
        let schema = json!({"type":"object","properties":{"count":{"type":"integer"}},
                            "required":["count"]});
        let r = c
            .call_tool("t", json!({}), Some(&schema), None)
            .await
            .unwrap();
        assert!(r.is_error, "un écart de schéma est signalé au modèle");
        assert!(r.render_text().contains("outputSchema"));
    }

    #[tokio::test]
    async fn valid_structured_output_passes_through() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => {
                Ok(json!({"protocolVersion":"2026-07-28","capabilities":{"tools":{}}}))
            }
            "tools/call" => Ok(json!({
                "content":[],
                "structuredContent": {"count": 3}
            })),
            _ => Ok(json!({})),
        });
        let c = client(tr).await;
        let schema = json!({"type":"object","properties":{"count":{"type":"integer"}}});
        let r = c
            .call_tool("t", json!({}), Some(&schema), None)
            .await
            .unwrap();
        assert!(!r.is_error);
        assert_eq!(r.structured.unwrap()["count"], 3);
    }

    #[tokio::test]
    async fn missing_primitive_is_not_a_failure() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => {
                Ok(json!({"protocolVersion":"2026-07-28","capabilities":{"tools":{}}}))
            }
            "prompts/list" => Err(McpError::Rpc {
                code: METHOD_NOT_FOUND,
                message: "pas de prompts".into(),
                data: None,
            }),
            _ => Ok(json!({})),
        });
        let c = client(tr).await;
        assert!(c.list_prompts().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn listen_changes_opts_in_on_2026() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => Ok(json!({
                "protocolVersion":"2026-07-28",
                "capabilities":{"tools":{"listChanged":true},"resources":{"subscribe":true}}
            })),
            "subscriptions/listen" => Ok(json!({"ok":true})),
            _ => Ok(json!({})),
        });
        let c = client(tr.clone()).await;
        assert!(c.listen_changes().await.unwrap());
        let log = tr.call_log().await;
        let (_, p) = log
            .iter()
            .find(|(m, _)| m == "subscriptions/listen")
            .unwrap();
        assert_eq!(p["toolsListChanged"], true);
        assert_eq!(p["resourceSubscriptions"], true);
    }

    #[tokio::test]
    async fn legacy_server_uses_notifications_not_subscribe() {
        let tr = LoopbackTransport::new("stdio", |m, _| match m {
            "server/discover" => Err(McpError::Rpc {
                code: METHOD_NOT_FOUND,
                message: "x".into(),
                data: None,
            }),
            "initialize" => Ok(json!({
                "protocolVersion":"2025-06-18",
                "capabilities":{"tools":{"listChanged":true}}
            })),
            _ => Ok(json!({})),
        });
        let c = client(tr.clone()).await;
        assert!(c.listen_changes().await.unwrap());
        assert!(
            !tr.call_log()
                .await
                .iter()
                .any(|(m, _)| m == "subscriptions/listen"),
            "pas d'abonnement explicite avant 2026-07-28"
        );
    }

    #[tokio::test]
    async fn log_level_uses_meta_or_set_level_depending_on_version() {
        // 2026-07-28 : par `_meta`.
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => Ok(json!({"protocolVersion":"2026-07-28",
                                           "capabilities":{"logging":{}}})),
            _ => Ok(json!({})),
        });
        let mut c = client(tr.clone()).await;
        c.apply_log_level("debug").await.unwrap();
        assert!(
            !tr.call_log()
                .await
                .iter()
                .any(|(m, _)| m == "logging/setLevel"),
            "pas d'appel dédié en mode sans état"
        );
        c.list_tools().await.unwrap();
        let log = tr.call_log().await;
        let (_, p) = log.iter().find(|(m, _)| m == "tools/list").unwrap();
        assert_eq!(p["_meta"]["logLevel"], "debug");

        // 2025-06-18 : par `logging/setLevel`.
        let tr2 = LoopbackTransport::new("stdio", |m, _| match m {
            "server/discover" => Err(McpError::Rpc {
                code: METHOD_NOT_FOUND,
                message: "x".into(),
                data: None,
            }),
            "initialize" => Ok(json!({"protocolVersion":"2025-06-18",
                                      "capabilities":{"logging":{}}})),
            _ => Ok(json!({})),
        });
        let mut c2 = client(tr2.clone()).await;
        c2.apply_log_level("debug").await.unwrap();
        assert!(
            tr2.call_log()
                .await
                .iter()
                .any(|(m, _)| m == "logging/setLevel")
        );
    }

    #[tokio::test]
    async fn initialized_notification_only_in_legacy_mode() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => Ok(json!({"protocolVersion":"2026-07-28","capabilities":{}})),
            _ => Ok(json!({})),
        });
        let _ = client(tr.clone()).await;
        assert!(
            !tr.call_log()
                .await
                .iter()
                .any(|(m, _)| m == "notifications/initialized")
        );
    }

    #[tokio::test]
    async fn health_probe_depends_on_version() {
        let tr = LoopbackTransport::new("http", |m, _| match m {
            "server/discover" => Ok(json!({"protocolVersion":"2026-07-28","capabilities":{}})),
            _ => Ok(json!({})),
        });
        let c = client(tr.clone()).await;
        c.health().await.unwrap();
        let n = tr
            .call_log()
            .await
            .iter()
            .filter(|(m, _)| m == "server/discover")
            .count();
        assert_eq!(n, 2, "négociation + sonde de santé");
    }
}
