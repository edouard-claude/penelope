//! La socket locale : accepter, authentifier, servir.

use super::stream::write_line;
use super::*;
use tokio::io::AsyncWriteExt;

/// Sert la socket locale jusqu'à l'arrêt du daemon.
pub async fn serve(daemon: Arc<Core>) -> anyhow::Result<()> {
    let path = daemon.services.platform.dirs.socket_path();
    let listener = penelope_platform::ipc::IpcListener::bind(&path).await?;
    serve_on(daemon, listener).await
}

/// Sert une socket déjà ouverte. Le daemon l'ouvre **avant** de lancer ses boucles : un
/// second daemon s'arrête ainsi avant d'avoir touché à la file des tours.
pub async fn serve_on(
    daemon: Arc<Core>,
    listener: penelope_platform::ipc::IpcListener,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    tracing::info!(
        socket = %daemon.services.platform.dirs.socket_path().display(),
        "RPC à l'écoute"
    );
    let rpc = Arc::new(Rpc::new(daemon.clone()));
    // Jeton de session : sans lui, la socket ne sert rien (issue #91).
    let token: Arc<str> = listener.token().into();

    loop {
        if daemon.handle.is_shutting_down() {
            return Ok(());
        }
        let accept = tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept());
        let stream = match accept.await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "connexion RPC refusée");
                continue;
            }
            Err(_) => continue, // délai : on revérifie l'arrêt
        };

        let rpc = rpc.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<RpcRequest>(&line) {
                    // Même utilisateur ne veut pas dire propriétaire : un processus confiné
                    // n'a pas le jeton, rien ne s'exécute pour lui.
                    Ok(req)
                        if !penelope_platform::ipc::tokens_match(req.auth.as_deref(), &token) =>
                    {
                        tracing::warn!(methode = %req.method, "requête RPC sans jeton valide refusée");
                        RpcResponse::err(
                            req.id,
                            penelope_kernel::api::DENIED,
                            "unauthorized : jeton RPC absent ou invalide (daemon redémarré ? \
                             relancer la commande)",
                        )
                    }
                    Ok(req) if req.method == method::CHAT_STREAM || req.method == method::TAIL => {
                        let id = req.id.clone();
                        // Fin de la connexion côté client : plus personne ne lira ce flux.
                        let closed = async { while let Ok(Some(_)) = lines.next_line().await {} };
                        if let Err(e) = rpc.handle_streaming(req, &mut write, closed).await {
                            let r = RpcResponse::err(id, classify(&e), e.to_string());
                            let _ = write_line(
                                &mut write,
                                &serde_json::to_value(r).unwrap_or_default(),
                            )
                            .await;
                        }
                        continue;
                    }
                    Ok(req) => rpc.handle(req).await,
                    Err(e) => RpcResponse::err(None, PARSE_ERROR, e.to_string()),
                };
                let mut body = serde_json::to_string(&response).unwrap_or_default();
                body.push('\n');
                if write.write_all(body.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}
