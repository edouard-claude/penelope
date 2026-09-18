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

// ------------------------------------------------------------------ stdio

/// Fin du processus d'un serveur stdio : comment, et après combien de temps (#114).
#[derive(Debug, Clone, Copy)]
struct Death {
    exit: penelope_platform::process::ExitInfo,
    lived_ms: u128,
}

/// Transport stdio : processus enfant lancé sous le profil `mcp-stdio` (§8.3).
pub struct StdioTransport {
    pending: Arc<Pending>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    incoming_tx: broadcast::Sender<Incoming>,
    stderr_buf: Arc<Mutex<Vec<String>>>,
    child: Arc<Mutex<Option<penelope_platform::process::Child>>>,
    host: Arc<penelope_platform::UnixProcessHost>,
    death: Arc<std::sync::Mutex<Option<Death>>>,
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
        let started = std::time::Instant::now();
        let t = Arc::new(StdioTransport {
            pending: Arc::new(Pending::default()),
            stdin: Mutex::new(stdin),
            incoming_tx: tx.clone(),
            stderr_buf: Arc::new(Mutex::new(Vec::new())),
            child: Arc::new(Mutex::new(Some(child))),
            host,
            death: Arc::new(std::sync::Mutex::new(None)),
        });

        if let Some(out) = stdout {
            let pending = t.pending.clone();
            let tx = tx.clone();
            let (child, death) = (t.child.clone(), t.death.clone());
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match decode(&line) {
                        Some(Incoming::Response(r)) => pending.resolve(r).await,
                        Some(other) => pending.publish(&tx, other),
                        None => tracing::debug!(line = %line, "ligne stdio non JSON-RPC ignorée"),
                    }
                }
                // Sortie standard fermée : le processus est mort, ou va l'être. Son code
                // de sortie et sa durée de vie sont notés avant de réveiller les appels en
                // attente, qui les citent (issue #114).
                let exit = match child.lock().await.as_mut() {
                    Some(c) => c.wait_exit(std::time::Duration::from_secs(2)).await,
                    None => None,
                };
                if let Some(exit) = exit
                    && let Ok(mut g) = death.lock()
                {
                    *g = Some(Death {
                        exit,
                        lived_ms: started.elapsed().as_millis(),
                    });
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

    /// « sorti avec le code 1 après 40 ms », si le processus est mort.
    fn death_line(&self) -> Option<String> {
        let d = (*self.death.lock().ok()?)?;
        let lived = if d.lived_ms < 2_000 {
            format!("{} ms", d.lived_ms)
        } else {
            format!("{:.1} s", d.lived_ms as f64 / 1_000.0)
        };
        Some(format!("{} après {lived}", d.exit.describe()))
    }

    /// Ce qu'on sait de la fermeture de la connexion : la mort du processus et ce qu'il a
    /// écrit en dernier sur sa sortie d'erreur, ou qu'il n'y a rien écrit.
    async fn closed_reason(&self) -> String {
        let Some(death) = self.death_line() else {
            return "le serveur a fermé la connexion".into();
        };
        let stderr = self.stderr_buf.lock().await;
        match stderr.last() {
            None => format!(
                "le serveur s'est arrêté : {death}, sans rien écrire sur sa sortie d'erreur"
            ),
            Some(last) => format!(
                "le serveur s'est arrêté : {death} ; dernière ligne d'erreur : {}",
                last.chars().take(300).collect::<String>()
            ),
        }
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

        match self.pending.wait(rx, timeout).await {
            Some(Ok(resp)) => unwrap_response(resp),
            Some(Err(_)) => Err(McpError::Transport(self.closed_reason().await)),
            None => {
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
        self.pending.server_request_closed(&id);
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

    /// Dernières lignes de la sortie d'erreur, suivies de la fin du processus s'il est
    /// mort ; une sortie vide est dite, jamais rendue en liste vide (issue #114).
    async fn logs(&self, n: usize) -> Vec<String> {
        let mut lines: Vec<String> = {
            let g = self.stderr_buf.lock().await;
            g.iter().rev().take(n).rev().cloned().collect()
        };
        let death = self.death_line();
        if lines.is_empty() {
            lines.push(match &death {
                Some(d) => format!("(rien sur la sortie d'erreur ; processus {d})"),
                None => "(rien sur la sortie d'erreur)".into(),
            });
        } else if let Some(d) = death {
            lines.push(format!("(processus {d})"));
        }
        lines
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
            let resp = self
                .post(serde_json::to_value(&req)?, method, MAX_WAIT)
                .await?;
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
        self.pending.server_request_closed(&id);
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

    async fn dying(script: &str) -> Arc<StdioTransport> {
        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(penelope_platform::UnixProcessHost::new(
            dir.path().join("pids"),
        ));
        let spec = penelope_platform::ProcessSpec::new("/bin/sh")
            .arg("-c")
            .arg(script);
        StdioTransport::spawn(host, spec, None).await.unwrap()
    }

    /// #114 : un serveur qui sort aussitôt avec un code non nul le dit, avec sa durée de
    /// vie et l'absence de sortie d'erreur.
    #[tokio::test]
    async fn a_server_that_exits_at_once_says_how() {
        let t = dying("exit 3").await;
        let e = t
            .request(
                "initialize",
                serde_json::json!({}),
                std::time::Duration::from_secs(10),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("sorti avec le code 3 après"), "{e}");
        assert!(e.contains(" ms") || e.contains(" s"), "{e}");
        assert!(e.contains("sans rien écrire sur sa sortie d'erreur"), "{e}");
        let logs = t.logs(10).await;
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert!(logs[0].contains("rien sur la sortie d'erreur") && logs[0].contains("code 3"));
    }

    /// #114 : un serveur tué par un signal le dit.
    #[tokio::test]
    async fn a_server_killed_by_a_signal_says_so() {
        let t = dying("kill -9 $$").await;
        let e = t
            .request(
                "initialize",
                serde_json::json!({}),
                std::time::Duration::from_secs(10),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("tué par le signal 9 (SIGKILL)"), "{e}");
    }

    /// #114 : les lignes écrites sur la sortie d'erreur avant de mourir restent, avec le
    /// code.
    #[tokio::test]
    async fn stderr_lines_are_kept_with_the_exit_code() {
        let t = dying("echo 'xcrun: error: pont indisponible' >&2; exit 1").await;
        let e = t
            .request(
                "initialize",
                serde_json::json!({}),
                std::time::Duration::from_secs(10),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("sorti avec le code 1"), "{e}");
        let logs = t.logs(10).await;
        assert!(
            logs.iter().any(|l| l.contains("pont indisponible")),
            "{logs:?}"
        );
        assert!(logs.last().unwrap().contains("code 1"), "{logs:?}");
    }
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

    /// Le délai d'un appel ne court pas tant que le serveur attend une réponse du client.
    #[tokio::test]
    async fn waiting_for_the_client_does_not_count_against_the_timeout() {
        use std::time::Duration;
        let p = Arc::new(Pending::default());
        let expired = p.wait(std::future::pending::<()>(), Duration::from_millis(250));
        assert!(expired.await.is_none());

        p.server_request_opened(&json!(7));
        let p2 = p.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            p2.server_request_closed(&json!(7));
        });
        let slow = async {
            tokio::time::sleep(Duration::from_millis(700)).await;
            "fini"
        };
        assert_eq!(p.wait(slow, Duration::from_millis(250)).await, Some("fini"));
    }

    /// Streamable HTTP : une requête du serveur au milieu du flux SSE est publiée tout de
    /// suite, la réponse du client part en POST, puis le flux livre le résultat, même
    /// au-delà du délai de l'appel.
    #[tokio::test]
    async fn sse_server_requests_are_published_before_the_stream_ends() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + len {
                        return text[head_end + 4..].to_string();
                    }
                }
                if n == 0 {
                    return String::new();
                }
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (answered_tx, answered_rx) = oneshot::channel::<String>();
        tokio::spawn(async move {
            let (mut call, _) = listener.accept().await.unwrap();
            let body = read_request(&mut call).await;
            assert!(body.contains("tools/call"), "{body}");
            call.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
                  data: {\"jsonrpc\":\"2.0\",\"id\":\"e1\",\"method\":\"elicitation/create\",\
                  \"params\":{\"message\":\"Confirmer ?\"}}\n\n",
            )
            .await
            .unwrap();
            call.flush().await.unwrap();
            let (mut answer, _) = listener.accept().await.unwrap();
            let posted = read_request(&mut answer).await;
            answer
                .write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
            call.write_all(b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[]}}\n\n")
                .await
                .unwrap();
            let _ = answered_tx.send(posted);
        });

        let t = HttpTransport::new(url).unwrap();
        let mut incoming = t.incoming();
        let client = t.clone();
        tokio::spawn(async move {
            if let Ok(Incoming::ServerRequest(r)) = incoming.recv().await {
                // Le propriétaire met plus longtemps que le délai de l'appel à répondre.
                tokio::time::sleep(Duration::from_millis(800)).await;
                client
                    .respond(r.id, Ok(json!({"action": "accept"})))
                    .await
                    .unwrap();
            }
        });
        let r = t
            .request(
                "tools/call",
                json!({"name": "x"}),
                Duration::from_millis(400),
            )
            .await
            .unwrap();
        assert_eq!(r, json!({"content": []}));
        let posted = answered_rx.await.unwrap();
        assert!(posted.contains("\"action\":\"accept\""), "{posted}");
        assert!(posted.contains("\"id\":\"e1\""), "{posted}");
    }

    #[test]
    fn sse_events_are_split_on_blank_lines() {
        assert_eq!(event_end(b"data: a\n\ndata: b"), Some((7, 2)));
        assert_eq!(event_end(b"data: a\r\n\r\n"), Some((7, 4)));
        assert_eq!(event_end(b"data: a\n"), None);
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
