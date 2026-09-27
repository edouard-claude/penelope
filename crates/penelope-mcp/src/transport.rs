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

mod stdio;
pub use stdio::StdioTransport;

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
    /// Version négociée par `initialize` : en HTTP, à partir de 2025-06-18, chaque requête
    /// suivante la porte dans `MCP-Protocol-Version`. Sans effet hors HTTP.
    async fn set_protocol_version(&self, _version: &str) {}
}

/// Plafond d'une attente prolongée par des requêtes du serveur.
const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Appariement requête/réponse partagé par tous les transports.
pub struct Pending {
    next_id: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Response>>>,
    /// Requêtes du serveur encore sans réponse du client (une élicitation attend le
    /// propriétaire).
    serving: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl Default for Pending {
    fn default() -> Self {
        Pending {
            next_id: AtomicU64::new(1),
            waiting: Mutex::new(HashMap::new()),
            serving: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }
}

impl Pending {
    /// Une requête du serveur part vers un abonné ; `respond` la clôt.
    pub fn server_request_opened(&self, id: &serde_json::Value) {
        if let Ok(mut g) = self.serving.lock() {
            g.insert(id.to_string());
        }
    }

    pub fn server_request_closed(&self, id: &serde_json::Value) {
        if let Ok(mut g) = self.serving.lock() {
            g.remove(&id.to_string());
        }
    }

    /// Le serveur attend une réponse du client.
    pub fn serving(&self) -> bool {
        self.serving.lock().map(|g| !g.is_empty()).unwrap_or(false)
    }

    /// Attend `fut` au plus `timeout`, sans compter le temps où le serveur attend lui-même
    /// une réponse du client : une élicitation qui patiente pour le propriétaire ne fait
    /// pas expirer l'appel qui l'a déclenchée (issue #12). Plafond : 24 h.
    pub async fn wait<F: std::future::Future>(
        &self,
        fut: F,
        timeout: std::time::Duration,
    ) -> Option<F::Output> {
        tokio::pin!(fut);
        let tick = std::time::Duration::from_millis(100);
        let started = tokio::time::Instant::now();
        let mut counted = std::time::Duration::ZERO;
        loop {
            let step = tick.min(timeout.saturating_sub(counted));
            tokio::select! {
                out = &mut fut => return Some(out),
                _ = tokio::time::sleep(step) => {}
            }
            if !self.serving() {
                counted += step;
            }
            if counted >= timeout || started.elapsed() >= MAX_WAIT {
                return None;
            }
        }
    }

    /// Publie un entrant non sollicité ; une requête du serveur est suivie jusqu'à sa
    /// réponse, si quelqu'un l'écoute.
    pub fn publish(&self, tx: &broadcast::Sender<Incoming>, incoming: Incoming) {
        if let Incoming::ServerRequest(r) = &incoming
            && tx.receiver_count() > 0
        {
            self.server_request_opened(&r.id);
        }
        let _ = tx.send(incoming);
    }

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

// ------------------------------------------------------------------ HTTP

/// En-têtes propres au transport HTTP (§8.3).
#[derive(Debug, Clone, Default)]
pub struct HttpHeaders {
    pub session_id: Option<String>,
    pub extra: Vec<(String, String)>,
    pub authorization: Option<String>,
    /// Version négociée par `initialize` (2025-06-18 à 2025-11-25).
    pub protocol_version: Option<String>,
}

/// En-têtes miroirs du corps d'une requête 2026-07-28 : `MCP-Protocol-Version` (la
/// version de son `_meta`), `Mcp-Method` (sa méthode) et, pour `tools/call`,
/// `prompts/get` et `resources/read`, `Mcp-Name` (`params.name` ou `params.uri`). Le
/// corps fait foi : le serveur refuse tout désaccord (-32020, issue #126). Une requête
/// d'une version antérieure, sans version dans son `_meta`, n'en porte aucun.
pub fn mirrored_headers(body: &serde_json::Value) -> Vec<(&'static str, String)> {
    let Some(method) = body.get("method").and_then(|m| m.as_str()) else {
        return Vec::new();
    };
    let params = body.get("params");
    let Some(version) = params
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(|v| v.as_str())
    else {
        return Vec::new();
    };
    let mut out = vec![
        ("MCP-Protocol-Version", version.to_string()),
        ("Mcp-Method", header_value(method)),
    ];
    let name = match method {
        "tools/call" | "prompts/get" => params.and_then(|p| p.get("name")),
        "resources/read" => params.and_then(|p| p.get("uri")),
        _ => None,
    };
    if let Some(name) = name.and_then(|n| n.as_str()) {
        out.push(("Mcp-Name", header_value(name)));
    }
    out
}

/// Valeur d'en-tête : telle quelle en ASCII visible sans espace au bord, sinon (et pour
/// une valeur qui ressemble à la sentinelle) en `=?base64?…?=` de son UTF-8.
pub fn header_value(v: &str) -> String {
    use base64::Engine;
    let safe = !v.is_empty()
        && v.bytes()
            .all(|b| b == b' ' || b == b'\t' || (0x21..=0x7E).contains(&b))
        && !v.starts_with([' ', '\t'])
        && !v.ends_with([' ', '\t'])
        && !(v.starts_with("=?base64?") && v.ends_with("?="));
    if safe {
        v.to_string()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(v.as_bytes())
        )
    }
}

/// Erreur JSON-RPC portée par une réponse HTTP 4xx (2026-07-28 : version refusée,
/// désaccord d'en-têtes, méthode inconnue) : son code reste lisible par l'appelant.
fn rpc_error_in(body: &str) -> Option<McpError> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let e = v.get("error")?;
    Some(McpError::Rpc {
        code: i32::try_from(e.get("code")?.as_i64()?).ok()?,
        message: e
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string(),
        data: e.get("data").cloned(),
    })
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
    /// Streamable HTTP : en-têtes miroirs du corps et version négociée (le transport
    /// historique n'en porte aucun).
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

    async fn post(
        &self,
        body: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<reqwest::Response> {
        let h = self.headers.lock().await.clone();
        let mirrored = if self.send_mcp_headers {
            mirrored_headers(&body)
        } else {
            Vec::new()
        };
        let mut req = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .timeout(timeout)
            .json(&body);
        if mirrored.is_empty()
            && self.send_mcp_headers
            && let Some(v) = &h.protocol_version
        {
            req = req.header("MCP-Protocol-Version", v);
        }
        for (k, v) in &mirrored {
            req = req.header(*k, v);
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

    /// Réponse à la requête `id` : JSON direct, ou flux SSE lu **au fil de l'eau** pour
    /// publier aussitôt les requêtes du serveur (une élicitation attend sa réponse avant
    /// que le flux se termine).
    async fn read_response(
        &self,
        id: u64,
        mut resp: reqwest::Response,
    ) -> Result<serde_json::Value> {
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
            if status < 500
                && let Some(e) = rpc_error_in(&body)
            {
                return Err(e);
            }
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

        if !content_type.contains("text/event-stream") {
            let v: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| McpError::Transport(e.to_string()))?;
            let r: Response = serde_json::from_value(v)?;
            return unwrap_response(r);
        }

        // Flux SSE : on consomme jusqu'à la réponse portant notre identifiant, en publiant
        // au passage notifications et requêtes du serveur.
        let mut buf: Vec<u8> = Vec::new();
        let mut ended = false;
        loop {
            while let Some((end, sep)) = event_end(&buf).or(if ended && !buf.is_empty() {
                Some((buf.len(), 0))
            } else {
                None
            }) {
                let event = String::from_utf8_lossy(&buf[..end]).to_string();
                buf.drain(..end + sep);
                for payload in sse_payloads(&event) {
                    match decode(&payload) {
                        Some(Incoming::Response(r)) if r.id == id => return unwrap_response(r),
                        Some(other) => self.pending.publish(&self.incoming_tx, other),
                        None => {}
                    }
                }
            }
            if ended {
                return Err(McpError::Transport(
                    "flux SSE terminé sans réponse à la requête".into(),
                ));
            }
            match resp.chunk().await {
                Ok(Some(bytes)) => buf.extend_from_slice(&bytes),
                Ok(None) => ended = true,
                Err(e) => return Err(McpError::Transport(e.to_string())),
            }
        }
    }
}

/// Fin du premier événement SSE complet du tampon : (position, longueur du séparateur).
fn event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
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
        let exchange = async {
            // Délai tenu par `Pending::wait`, suspendu tant que le serveur attend le client.
            let resp = self.post(serde_json::to_value(&req)?, MAX_WAIT).await?;
            self.read_response(id, resp).await
        };
        match self.pending.wait(exchange, timeout).await {
            Some(r) => r,
            None => Err(McpError::Timeout {
                method: method.to_string(),
                ms: timeout.as_millis() as u64,
            }),
        }
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let n = Notification::new(method, params);
        let resp = self
            .post(
                serde_json::to_value(&n)?,
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
        self.pending.server_request_closed(&id);
        let body = match result {
            Ok(v) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => serde_json::json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code": e.rpc_code(),"message": e.to_string()}
            }),
        };
        let _ = self.post(body, std::time::Duration::from_secs(30)).await?;
        Ok(())
    }

    fn incoming(&self) -> broadcast::Receiver<Incoming> {
        self.incoming_tx.subscribe()
    }

    async fn close(&self) -> Result<()> {
        self.pending.fail_all().await;
        Ok(())
    }

    async fn set_protocol_version(&self, version: &str) {
        self.headers.lock().await.protocol_version = Some(version.to_string());
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
mod tests;
