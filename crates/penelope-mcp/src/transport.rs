//! Transports MCP : stdio, Streamable HTTP, HTTP+SSE historique (§8.3).
//!
//! Le transport ne connaît pas la sémantique MCP : il transporte des enveloppes
//! JSON-RPC, appareille les réponses par identifiant, et publie les entrants non
//! sollicités (notifications, requêtes serveur) sur un canal de diffusion.

use crate::error::{McpError, Result};
use crate::protocol::{Incoming, Notification, Request, Response, decode};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, broadcast, oneshot};

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value>;
    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()>;
    /// Réponse à une requête **du serveur** (sampling, elicitation, roots).
    async fn respond(&self, id: serde_json::Value, result: Result<serde_json::Value>)
    -> Result<()>;
    fn incoming(&self) -> broadcast::Receiver<Incoming>;
    async fn close(&self) -> Result<()>;
    /// Diagnostic : dernières lignes de stderr du serveur (stdio).
    async fn logs(&self, _n: usize) -> Vec<String> {
        Vec::new()
    }
}

/// Appariement requête/réponse partagé par tous les transports.
pub struct Pending {
    next_id: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Response>>>,
}

impl Default for Pending {
    fn default() -> Self {
        Pending {
            next_id: AtomicU64::new(1),
            waiting: Mutex::new(HashMap::new()),
        }
    }
}

impl Pending {
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn register(&self, id: u64) -> oneshot::Receiver<Response> {
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(id, tx);
        rx
    }

    pub async fn resolve(&self, resp: Response) {
        let Some(id) = resp.id.as_u64() else { return };
        if let Some(tx) = self.waiting.lock().await.remove(&id) {
            let _ = tx.send(resp);
        }
    }

    pub async fn cancel(&self, id: u64) {
        self.waiting.lock().await.remove(&id);
    }

    pub async fn fail_all(&self) {
        self.waiting.lock().await.clear();
    }

    pub async fn in_flight(&self) -> usize {
        self.waiting.lock().await.len()
    }
}

/// Convertit une réponse JSON-RPC en résultat.
pub fn unwrap_response(r: Response) -> Result<serde_json::Value> {
    if let Some(e) = r.error {
        return Err(McpError::Rpc {
            code: e.code,
            message: e.message,
            data: e.data,
        });
    }
    Ok(r.result.unwrap_or(serde_json::Value::Null))
}

// ------------------------------------------------------------------ stdio

/// Transport stdio : processus enfant lancé sous le profil `mcp-stdio` (§8.3).
pub struct StdioTransport {
    pending: Arc<Pending>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    incoming_tx: broadcast::Sender<Incoming>,
    stderr_buf: Arc<Mutex<Vec<String>>>,
    child: Mutex<Option<penelope_platform::process::Child>>,
    host: Arc<penelope_platform::UnixProcessHost>,
}

impl StdioTransport {
    /// Lance le serveur et démarre les boucles de lecture.
    pub async fn spawn(
        host: Arc<penelope_platform::UnixProcessHost>,
        spec: penelope_platform::ProcessSpec,
        sandbox: Option<&penelope_platform::Profile>,
    ) -> Result<Arc<StdioTransport>> {
        use penelope_platform::ProcessHost;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut child = host
            .spawn(spec, sandbox)
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;

        let stdin = child.stdin();
        let stdout = child.stdout();
        let stderr = child.stderr();

        let (tx, _) = broadcast::channel(256);
        let t = Arc::new(StdioTransport {
            pending: Arc::new(Pending::default()),
            stdin: Mutex::new(stdin),
            incoming_tx: tx.clone(),
            stderr_buf: Arc::new(Mutex::new(Vec::new())),
            child: Mutex::new(Some(child)),
            host,
        });

        if let Some(out) = stdout {
            let pending = t.pending.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match decode(&line) {
                        Some(Incoming::Response(r)) => pending.resolve(r).await,
                        Some(other) => {
                            let _ = tx.send(other);
                        }
                        None => tracing::debug!(line = %line, "ligne stdio non JSON-RPC ignorée"),
                    }
                }
                pending.fail_all().await;
            });
        }

        if let Some(err) = stderr {
            let buf = t.stderr_buf.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut g = buf.lock().await;
                    if g.len() >= 500 {
                        g.remove(0);
                    }
                    g.push(line);
                }
            });
        }

        Ok(t)
    }

    async fn write_line(&self, payload: &str) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| McpError::Transport("stdin du serveur fermé".into()))?;
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Transport for StdioTransport {
    fn kind(&self) -> &'static str {
        "stdio"
    }

    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value> {
        let id = self.pending.next_id();
        let rx = self.pending.register(id).await;
        let req = Request::new(id, method, params);
        self.write_line(&serde_json::to_string(&req)?).await?;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => unwrap_response(resp),
            Ok(Err(_)) => Err(McpError::Transport(
                "le serveur a fermé la connexion".into(),
            )),
            Err(_) => {
                self.pending.cancel(id).await;
                // Notification d'annulation, comme l'exige le protocole.
                let _ = self
                    .notify(
                        "notifications/cancelled",
                        serde_json::json!({"requestId": id, "reason": "timeout"}),
                    )
                    .await;
                Err(McpError::Timeout {
                    method: method.to_string(),
                    ms: timeout.as_millis() as u64,
                })
            }
        }
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let n = Notification::new(method, params);
        self.write_line(&serde_json::to_string(&n)?).await
    }

    async fn respond(
        &self,
        id: serde_json::Value,
        result: Result<serde_json::Value>,
    ) -> Result<()> {
        let body = match result {
            Ok(v) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => serde_json::json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code": e.rpc_code(), "message": e.to_string()}
            }),
        };
        self.write_line(&body.to_string()).await
    }

    fn incoming(&self) -> broadcast::Receiver<Incoming> {
        self.incoming_tx.subscribe()
    }

    async fn close(&self) -> Result<()> {
        use penelope_platform::ProcessHost;
        // Fermeture de stdin d'abord : la plupart des serveurs s'arrêtent seuls.
        drop(self.stdin.lock().await.take());
        if let Some(mut c) = self.child.lock().await.take() {
            self.host
                .terminate(&mut c, std::time::Duration::from_secs(5))
                .await
                .map_err(|e| McpError::Transport(e.to_string()))?;
        }
        self.pending.fail_all().await;
        Ok(())
    }

    async fn logs(&self, n: usize) -> Vec<String> {
        let g = self.stderr_buf.lock().await;
        g.iter().rev().take(n).rev().cloned().collect()
    }
}

// ------------------------------------------------------------------ HTTP

/// En-têtes propres au transport HTTP (§8.3).
#[derive(Debug, Clone, Default)]
pub struct HttpHeaders {
    pub session_id: Option<String>,
    pub extra: Vec<(String, String)>,
    pub authorization: Option<String>,
}

/// Transport Streamable HTTP (2025-03-26 et suivantes) et HTTP+SSE historique.
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: Mutex<HttpHeaders>,
    incoming_tx: broadcast::Sender<Incoming>,
    pending: Arc<Pending>,
    /// Vrai pour le transport historique GET SSE + POST endpoint.
    legacy_sse: bool,
    /// En 2026-07-28, les en-têtes `Mcp-Method` et `Mcp-Name` accompagnent la requête.
    send_mcp_headers: bool,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Result<Arc<HttpTransport>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let (tx, _) = broadcast::channel(256);
        Ok(Arc::new(HttpTransport {
            client,
            url: url.into(),
            headers: Mutex::new(HttpHeaders::default()),
            incoming_tx: tx,
            pending: Arc::new(Pending::default()),
            legacy_sse: false,
            send_mcp_headers: true,
        }))
    }

    pub fn legacy(url: impl Into<String>) -> Result<Arc<HttpTransport>> {
        let t = Self::new(url)?;
        // `Arc::get_mut` fonctionne : personne d'autre ne détient encore la référence.
        let mut t = Arc::try_unwrap(t).map_err(|_| McpError::Transport("arc partagé".into()))?;
        t.legacy_sse = true;
        t.send_mcp_headers = false;
        Ok(Arc::new(t))
    }

    pub async fn set_authorization(&self, value: Option<String>) {
        self.headers.lock().await.authorization = value;
    }

    pub async fn set_extra_headers(&self, extra: Vec<(String, String)>) {
        self.headers.lock().await.extra = extra;
    }

    pub async fn session_id(&self) -> Option<String> {
        self.headers.lock().await.session_id.clone()
    }

    pub fn set_protocol_headers(&mut self, on: bool) {
        self.send_mcp_headers = on;
    }

    async fn post(
        &self,
        body: serde_json::Value,
        method_name: &str,
        timeout: std::time::Duration,
    ) -> Result<reqwest::Response> {
        let h = self.headers.lock().await.clone();
        let mut req = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .timeout(timeout)
            .json(&body);
        if self.send_mcp_headers {
            req = req
                .header("Mcp-Method", method_name)
                .header("Mcp-Name", "penelope");
        }
        if let Some(s) = &h.session_id {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(a) = &h.authorization {
            req = req.header("Authorization", a);
        }
        for (k, v) in &h.extra {
            req = req.header(k.as_str(), v.as_str());
        }
        req.send()
            .await
            .map_err(|e| McpError::Transport(format!("POST {} : {e}", self.url)))
    }
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    fn kind(&self) -> &'static str {
        if self.legacy_sse { "sse" } else { "http" }
    }

    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value> {
        let id = self.pending.next_id();
        let req = Request::new(id, method, params);
        let resp = self
            .post(serde_json::to_value(&req)?, method, timeout)
            .await?;

        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            let www = resp
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            return Err(McpError::Unauthorized {
                status,
                www_authenticate: www,
            });
        }
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(McpError::Http { status, body });
        }

        // Le serveur peut renvoyer l'identifiant de session à conserver.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.headers.lock().await.session_id = Some(sid.to_string());
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("text/event-stream") {
            // Flux SSE : on consomme jusqu'à la réponse portant notre identifiant, en
            // publiant au passage les notifications (progression, logs).
            let body = resp
                .text()
                .await
                .map_err(|e| McpError::Transport(e.to_string()))?;
            let mut result = None;
            for payload in sse_payloads(&body) {
                match decode(&payload) {
                    Some(Incoming::Response(r)) if r.id == id => {
                        result = Some(unwrap_response(r));
                    }
                    Some(other) => {
                        let _ = self.incoming_tx.send(other);
                    }
                    None => {}
                }
            }
            return result.unwrap_or_else(|| {
                Err(McpError::Transport(
                    "flux SSE terminé sans réponse à la requête".into(),
                ))
            });
        }

        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let r: Response = serde_json::from_value(v)?;
        unwrap_response(r)
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let n = Notification::new(method, params);
        let resp = self
            .post(
                serde_json::to_value(&n)?,
                method,
                std::time::Duration::from_secs(30),
            )
            .await?;
        let status = resp.status().as_u16();
        if status >= 400 && status != 405 {
            let body = resp.text().await.unwrap_or_default();
            return Err(McpError::Http { status, body });
        }
        Ok(())
    }

    async fn respond(
        &self,
        id: serde_json::Value,
        result: Result<serde_json::Value>,
    ) -> Result<()> {
        let body = match result {
            Ok(v) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => serde_json::json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code": e.rpc_code(),"message": e.to_string()}
            }),
        };
        let _ = self
            .post(body, "response", std::time::Duration::from_secs(30))
            .await?;
        Ok(())
    }

    fn incoming(&self) -> broadcast::Receiver<Incoming> {
        self.incoming_tx.subscribe()
    }

    async fn close(&self) -> Result<()> {
        self.pending.fail_all().await;
        Ok(())
    }
}

/// Extrait les charges utiles `data:` d'un corps SSE complet.
pub fn sse_payloads(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in body.split("\n\n").flat_map(|b| b.split("\r\n\r\n")) {
        let mut data = String::new();
        for line in block.lines() {
            if let Some(rest) = line.trim_end_matches('\r').strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }
        if !data.is_empty() && data != "[DONE]" {
            out.push(data);
        }
    }
    out
}

// ------------------------------------------------------------------ mock

/// Gestionnaire d'un serveur en boucle : `(méthode, paramètres) -> résultat`.
pub type LoopbackHandler =
    Arc<dyn Fn(&str, &serde_json::Value) -> Result<serde_json::Value> + Send + Sync>;

/// Réponse enregistrée par [`LoopbackTransport`] : identifiant de la requête du serveur,
/// puis résultat ou message d'erreur.
pub type LoopbackResponse = (
    serde_json::Value,
    std::result::Result<serde_json::Value, String>,
);

/// Serveur simulé en processus, pour la suite de conformance (§8, CA 8).
///
/// Il implémente le même contrat que les transports réels : c'est ce qui permet de tester
/// la négociation de version et toutes les primitives sans réseau ni sous-processus.
pub struct LoopbackTransport {
    handler: LoopbackHandler,
    incoming_tx: broadcast::Sender<Incoming>,
    pub calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    /// Réponses du client aux requêtes du serveur : `(id, résultat ou message d'erreur)`.
    pub responses: Arc<Mutex<Vec<LoopbackResponse>>>,
    kind: &'static str,
}

impl LoopbackTransport {
    pub fn new<F>(kind: &'static str, handler: F) -> Arc<LoopbackTransport>
    where
        F: Fn(&str, &serde_json::Value) -> Result<serde_json::Value> + Send + Sync + 'static,
    {
        let (tx, _) = broadcast::channel(64);
        Arc::new(LoopbackTransport {
            handler: Arc::new(handler),
            incoming_tx: tx,
            calls: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(Vec::new())),
            kind,
        })
    }

    /// Pousse une notification non sollicitée vers le client.
    pub fn push_notification(&self, method: &str, params: serde_json::Value) {
        let _ = self
            .incoming_tx
            .send(Incoming::Notification(Notification::new(method, params)));
    }

    /// Pousse une requête du serveur (sampling, elicitation, roots).
    pub fn push_server_request(&self, id: u64, method: &str, params: serde_json::Value) {
        let _ = self
            .incoming_tx
            .send(Incoming::ServerRequest(Request::new(id, method, params)));
    }

    pub async fn call_log(&self) -> Vec<(String, serde_json::Value)> {
        self.calls.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl Transport for LoopbackTransport {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        _timeout: std::time::Duration,
    ) -> Result<serde_json::Value> {
        self.calls
            .lock()
            .await
            .push((method.to_string(), params.clone()));
        (self.handler)(method, &params)
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        self.calls.lock().await.push((method.to_string(), params));
        Ok(())
    }

    async fn respond(
        &self,
        id: serde_json::Value,
        result: Result<serde_json::Value>,
    ) -> Result<()> {
        self.responses
            .lock()
            .await
            .push((id, result.map_err(|e| e.to_string())));
        Ok(())
    }

    fn incoming(&self) -> broadcast::Receiver<Incoming> {
        self.incoming_tx.subscribe()
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn pending_matches_responses_by_id() {
        let p = Pending::default();
        let id = p.next_id();
        let rx = p.register(id).await;
        assert_eq!(p.in_flight().await, 1);
        p.resolve(Response {
            jsonrpc: "2.0".into(),
            id: json!(id),
            result: Some(json!({"ok":true})),
            error: None,
        })
        .await;
        let r = rx.await.unwrap();
        assert_eq!(r.result, Some(json!({"ok":true})));
        assert_eq!(p.in_flight().await, 0);
    }

    #[tokio::test]
    async fn unmatched_response_is_dropped_without_panic() {
        let p = Pending::default();
        p.resolve(Response {
            jsonrpc: "2.0".into(),
            id: json!(999),
            result: Some(json!({})),
            error: None,
        })
        .await;
        assert_eq!(p.in_flight().await, 0);
    }

    #[test]
    fn rpc_error_becomes_an_mcp_error() {
        let e = unwrap_response(Response {
            jsonrpc: "2.0".into(),
            id: json!(1),
            result: None,
            error: Some(crate::protocol::RpcError {
                code: -32022,
                message: "version".into(),
                data: None,
            }),
        })
        .unwrap_err();
        match e {
            McpError::Rpc { code, .. } => assert_eq!(code, -32022),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sse_payload_extraction() {
        let body = "event: message\ndata: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n";
        assert_eq!(
            sse_payloads(body),
            vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
        );
    }

    #[tokio::test]
    async fn loopback_records_calls_and_answers() {
        let t = LoopbackTransport::new("stdio", |method, _p| match method {
            "tools/list" => Ok(json!({"tools":[{"name":"a","inputSchema":{}}]})),
            _ => Err(McpError::Rpc {
                code: -32601,
                message: "inconnu".into(),
                data: None,
            }),
        });
        let r = t
            .request("tools/list", json!({}), std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(r["tools"][0]["name"], "a");
        assert!(
            t.request("autre", json!({}), std::time::Duration::from_secs(1))
                .await
                .is_err()
        );
        let log = t.call_log().await;
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, "tools/list");
    }

    #[tokio::test]
    async fn loopback_publishes_notifications() {
        let t = LoopbackTransport::new("stdio", |_, _| Ok(json!({})));
        let mut rx = t.incoming();
        t.push_notification("notifications/tools/list_changed", json!({}));
        let msg = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match msg {
            Incoming::Notification(n) => {
                assert_eq!(n.method, "notifications/tools/list_changed")
            }
            other => panic!("{other:?}"),
        }
    }
}
