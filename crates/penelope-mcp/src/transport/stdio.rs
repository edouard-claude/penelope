use super::*;

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
        match self.write_raw(payload).await {
            Ok(()) => Ok(()),
            Err(e) => Err(self.after_write_error(e).await),
        }
    }

    async fn write_raw(&self, payload: &str) -> std::result::Result<(), String> {
        use tokio::io::AsyncWriteExt;
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| "stdin du serveur fermé".to_string())?;
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    /// Une écriture refusée (« Broken pipe ») veut presque toujours dire que le serveur est
    /// mort avant d'avoir lu : on attend un instant la fin du processus pour la citer au
    /// lieu du seul code d'erreur (issue #114).
    async fn after_write_error(&self, e: String) -> McpError {
        for _ in 0..125 {
            if self.death_line().is_some() {
                return McpError::Transport(self.closed_reason().await);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        McpError::Transport(e)
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
