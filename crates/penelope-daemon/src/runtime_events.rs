//! Contrat public des événements runtime, établi après commit dans le journal.

use crate::runtime::Daemon;
use futures::{SinkExt, StreamExt};
use penelope_kernel::event::Event;
use penelope_kernel::event::EventLog;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::http::{Response, StatusCode};

/// Secret chargé du magasin local au démarrage, jamais journalisé.
#[derive(Clone)]
pub struct StreamConsumer {
    pub token: String,
    pub kinds: Vec<String>,
}

#[derive(Clone)]
struct Selection {
    consumer: StreamConsumer,
    after_id: i64,
}

/// Projection exclusivement publique : le corps privé et ses empreintes ne quittent
/// jamais le daemon. L'identifiant SQLite sert d'ID idempotent et d'horloge globale.
pub fn public_frame(event: &Event) -> Value {
    json!({
        "event_id": event.id,
        "sequence": event.id,
        "session_seq": event.seq,
        "timestamp": event.ts,
        "kind": event.kind,
        "session_id": event.session_id,
        "run_id": event.run_id,
        "payload": bounded_redacted(&event.payload),
        "actuation": null,
    })
}

/// Descendu dans `penelope-app` pour l'exécuteur (T24), réexporté jusqu'à T30.
pub use penelope_app::helpers::bounded_redacted;

fn rejected(
    status: StatusCode,
) -> tokio_tungstenite::tungstenite::handshake::server::ErrorResponse {
    Response::builder()
        .status(status)
        .body(Some("accès refusé".into()))
        .expect("statut HTTP constant")
}

fn allowed(consumer: &StreamConsumer, kind: &str) -> bool {
    consumer.kinds.is_empty() || consumer.kinds.iter().any(|k| k == kind)
}

async fn send_event(
    ws: &mut WebSocketStream<TcpStream>,
    event: &Event,
    selection: &Selection,
    last: &mut i64,
) -> anyhow::Result<()> {
    if event.id <= *last {
        return Ok(());
    }
    *last = event.id;
    if allowed(&selection.consumer, &event.kind) {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ws.send(Message::Text(public_frame(event).to_string().into())),
        )
        .await??;
    }
    Ok(())
}

async fn replay(
    ws: &mut WebSocketStream<TcpStream>,
    log: &EventLog,
    selection: &Selection,
    last: &mut i64,
) -> anyhow::Result<()> {
    loop {
        let batch = log.range(*last, 500).await?;
        let count = batch.len();
        for event in batch {
            send_event(ws, &event, selection, last).await?;
        }
        if count < 500 {
            return Ok(());
        }
    }
}

/// Une connexion WebSocket de lecture seule. Abonnement avant rejeu : les écritures
/// concomitantes sont reçues ou récupérées au journal, jamais perdues.
// Le callback de tungstenite impose Response<Option<String>> comme erreur HTTP.
#[allow(clippy::result_large_err)]
pub async fn serve_connection(
    socket: TcpStream,
    log: Arc<EventLog>,
    consumers: Vec<StreamConsumer>,
) -> anyhow::Result<()> {
    let mut live = log.subscribe();
    let selected = Arc::new(Mutex::new(None::<Selection>));
    let slot = selected.clone();
    let mut ws = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio_tungstenite::accept_hdr_async(
            socket,
            move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                  response| {
                if request.uri().path() != "/events" {
                    return Err(rejected(StatusCode::NOT_FOUND));
                }
                let token = request
                    .headers()
                    .get("authorization")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|h| h.strip_prefix("Bearer "));
                let Some(consumer) = consumers
                    .iter()
                    .find(|c| token.is_some_and(|token| token == c.token))
                    .cloned()
                else {
                    return Err(rejected(StatusCode::UNAUTHORIZED));
                };
                let after_id = request
                    .uri()
                    .query()
                    .and_then(|q| {
                        url::form_urlencoded::parse(q.as_bytes()).find(|(key, _)| key == "after_id")
                    })
                    .and_then(|(_, value)| value.parse::<i64>().ok())
                    .unwrap_or(0)
                    .max(0);
                *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Selection { consumer, after_id });
                Ok(response)
            },
        ),
    )
    .await??;
    let selection = selected
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or_else(|| anyhow::anyhow!("handshake sans consommateur"))?;
    let mut last = selection.after_id;
    replay(&mut ws, &log, &selection, &mut last).await?;
    loop {
        tokio::select! {
            next = live.recv() => match next {
                Ok(event) if event.id == last + 1 => {
                    send_event(&mut ws, &event, &selection, &mut last).await?;
                }
                Ok(event) if event.id > last => {
                    replay(&mut ws, &log, &selection, &mut last).await?;
                }
                Ok(_) => {},
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    replay(&mut ws, &log, &selection, &mut last).await?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}, // Canal de lecture seule ; aucune commande d'actuation.
            },
        }
    }
    Ok(())
}

/// Écoute locale activée par la configuration. Un secret absent ou trop court empêche
/// tout bind : le flux ne démarre jamais avec une authentification vide.
pub async fn serve(daemon: Arc<Daemon>) -> anyhow::Result<()> {
    let config = daemon.services.config.config();
    if config.observability.runtime_consumers.is_empty() {
        return Ok(());
    }
    let mut consumers = Vec::new();
    let mut tokens = std::collections::BTreeSet::new();
    for configured in &config.observability.runtime_consumers {
        let token = daemon
            .services
            .platform
            .secrets
            .get(&configured.token_secret)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "secret `{}` absent pour le consommateur runtime `{}`",
                    configured.token_secret,
                    configured.name
                )
            })?;
        if token.len() < 32 {
            anyhow::bail!(
                "secret `{}` trop court pour le consommateur runtime `{}` (32 caractères minimum)",
                configured.token_secret,
                configured.name
            );
        }
        if !tokens.insert(token.clone()) {
            anyhow::bail!("jeton runtime partagé par plusieurs consommateurs");
        }
        penelope_observe::register_secret(&token);
        consumers.push(StreamConsumer {
            token,
            kinds: configured.kinds.clone(),
        });
    }
    let listener = tokio::net::TcpListener::bind(&config.observability.runtime_stream_bind).await?;
    tracing::info!(bind = %config.observability.runtime_stream_bind, "flux runtime prêt");
    let log = Arc::new(daemon.services.events.clone());
    loop {
        let (socket, _) = listener.accept().await?;
        let log = log.clone();
        let consumers = consumers.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_connection(socket, log, consumers).await {
                tracing::debug!(%error, "connexion au flux runtime terminée");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use penelope_kernel::clock::TestClock;
    use penelope_kernel::config::RuntimeConsumer;
    use penelope_kernel::event::Event;
    use penelope_kernel::event::{EventDraft, EventLog};
    use penelope_store::Store;
    use serde_json::json;
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[test]
    fn public_frame_keeps_order_and_redacts_payload() {
        let secret = "sk_test_FauxSecret1234567890";
        let event = Event {
            id: 42,
            session_id: Some("s1".into()),
            run_id: Some("r1".into()),
            seq: 7,
            ts: "2026-09-22T00:00:00Z".into(),
            kind: "runtime.tool".into(),
            payload: json!({"tool": "shell_exec", "args": {"token": secret}}),
            hash: "hash".into(),
            prev_hash: "previous".into(),
        };
        let frame = public_frame(&event);
        assert_eq!(frame["event_id"], 42);
        assert_eq!(frame["sequence"], 42);
        assert_eq!(frame["session_seq"], 7);
        assert_eq!(frame["kind"], "runtime.tool");
        assert!(!frame.to_string().contains(secret));
        assert!(frame.get("hash").is_none());
        assert_eq!(frame["actuation"], serde_json::Value::Null);
    }

    #[test]
    fn public_frame_bounds_legacy_payloads_too() {
        let event = Event {
            id: 1,
            session_id: None,
            run_id: None,
            seq: 1,
            ts: "2026-09-22T00:00:00Z".into(),
            kind: "legacy".into(),
            payload: json!({"output": "x".repeat(70_000)}),
            hash: String::new(),
            prev_hash: String::new(),
        };
        let frame = public_frame(&event);
        assert_eq!(frame["payload"]["truncated"], true);
        assert!(frame.to_string().len() < 1024);
    }

    #[tokio::test]
    async fn websocket_replays_then_streams_filtered_committed_events() {
        let log = Arc::new(EventLog::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        ));
        let old = log
            .append(EventDraft::new("runtime.tool", json!({"tool": "fs_read"})))
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_log = log.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            serve_connection(
                socket,
                server_log,
                vec![StreamConsumer {
                    token: "local-secret".into(),
                    kinds: vec!["runtime.tool".into()],
                }],
            )
            .await
            .unwrap();
        });
        let mut request = format!("ws://{addr}/events?after_id=0")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer local-secret".parse().unwrap());
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap();
        let first: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(first["event_id"], old.id);
        log.append(EventDraft::new("session.closed", json!({})))
            .await
            .unwrap();
        let fresh = log
            .append(EventDraft::new("runtime.tool", json!({"tool": "fs_list"})))
            .await
            .unwrap();
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let next: Value = serde_json::from_str(next.to_text().unwrap()).unwrap();
        assert_eq!(next["event_id"], fresh.id);
        ws.close(None).await.unwrap();
        server.await.unwrap();
    }

    /// Un client WebSocket authentifié, pour un consommateur au filtre `kinds`.
    async fn consumer(log: Arc<EventLog>, kinds: &[&str]) -> WebSocketStream<TcpStream> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let kinds = kinds.iter().map(|k| k.to_string()).collect();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let consumer = StreamConsumer {
                token: "local-secret".into(),
                kinds,
            };
            let _ = serve_connection(socket, log, vec![consumer]).await;
        });
        let mut request = format!("ws://{addr}/events?after_id=0")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer local-secret".parse().unwrap());
        let stream = TcpStream::connect(addr).await.unwrap();
        tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap()
            .0
    }

    async fn frame(ws: &mut WebSocketStream<TcpStream>) -> Value {
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(next.to_text().unwrap()).unwrap()
    }

    fn assistant(text: &str) -> Value {
        use penelope_context::journal::{AssistantPayload, ConvEvent};
        let content = penelope_llm::types::ChatMessage::assistant(text).content;
        ConvEvent::Assistant(Box::new(AssistantPayload {
            content,
            ..Default::default()
        }))
        .payload()
    }

    /// T18 : le contenu de la conversation ne sort que pour qui le demande ; un filtre
    /// par kind exact (`runtime.tool`) n'en reçoit rien, en rejeu comme en direct.
    #[tokio::test]
    async fn a_consumer_of_runtime_tool_receives_no_conv_event() {
        let log = Arc::new(EventLog::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        ));
        let conv = |kind: &str, payload: Value| EventDraft::new(kind, payload).session("s1");
        log.append(conv("conv.assistant", assistant("avant")))
            .await
            .unwrap();
        let old = log
            .append(EventDraft::new("runtime.tool", json!({"tool": "fs_read"})))
            .await
            .unwrap();
        let mut ws = consumer(log.clone(), &["runtime.tool"]).await;
        assert_eq!(frame(&mut ws).await["event_id"], old.id);
        log.append(conv("conv.assistant", assistant("pendant")))
            .await
            .unwrap();
        log.append(conv("conv.user", json!({"v": 1, "content": []})))
            .await
            .unwrap();
        let fresh = log
            .append(EventDraft::new("runtime.tool", json!({"tool": "fs_list"})))
            .await
            .unwrap();
        let next = frame(&mut ws).await;
        assert_eq!(next["kind"], "runtime.tool", "aucun conv.* entre les deux");
        assert_eq!(next["event_id"], fresh.id);
    }

    /// T18 : sans filtre, un `conv.assistant` arrive rédigé, et borné à 64 Kio comme
    /// n'importe quel payload.
    #[tokio::test]
    async fn an_unfiltered_consumer_receives_conv_assistant_redacted_and_bounded() {
        let log = Arc::new(EventLog::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        ));
        let secret = "sk-FauxSecretDeConversation1234567890";
        let said = format!("la clé est {secret}");
        let small = log
            .append(EventDraft::new("conv.assistant", assistant(&said)).session("s1"))
            .await
            .unwrap();
        let big = log
            .append(EventDraft::new("conv.assistant", assistant(&"x".repeat(70_000))).session("s1"))
            .await
            .unwrap();
        let mut ws = consumer(log, &[]).await;
        let first = frame(&mut ws).await;
        assert_eq!(first["event_id"], small.id);
        assert_eq!(first["kind"], "conv.assistant");
        assert!(!first.to_string().contains(secret), "rédigé");
        assert!(
            first["payload"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("la clé est")
        );
        let second = frame(&mut ws).await;
        assert_eq!(second["event_id"], big.id);
        assert_eq!(second["payload"]["truncated"], true);
        assert!(second["payload"]["bytes"].as_u64().unwrap() > 64 * 1024);
    }

    #[tokio::test]
    async fn websocket_rejects_unauthenticated_clients() {
        let log = Arc::new(EventLog::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(
                serve_connection(
                    socket,
                    log,
                    vec![StreamConsumer {
                        token: "local-secret".into(),
                        kinds: Vec::new(),
                    }],
                )
                .await
                .is_err()
            );
        });
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let error = tokio_tungstenite::client_async(format!("ws://{addr}/events"), stream)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("401"), "{error}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stream_does_not_bind_without_a_valid_stored_secret() {
        let dir = tempfile::tempdir().unwrap();
        let services = Arc::new(
            crate::runtime::Services::for_tests(
                dir.path().to_path_buf(),
                Arc::new(TestClock::default()),
            )
            .await
            .unwrap(),
        );
        services
            .config
            .mutate("test", |config| {
                config.observability.runtime_stream_bind = "127.0.0.1:9469".into();
                config.observability.runtime_consumers = vec![RuntimeConsumer {
                    name: "watchdog".into(),
                    token_secret: "runtime_watchdog".into(),
                    kinds: vec![],
                }];
                Ok(vec!["observability.runtime_consumers".into()])
            })
            .unwrap();
        let daemon = Arc::new(crate::runtime::Daemon::from_services(services.clone()));
        let error = serve(daemon.clone()).await.unwrap_err();
        assert!(error.to_string().contains("absent"), "{error}");
        services
            .platform
            .secrets
            .set("runtime_watchdog", "court")
            .unwrap();
        let error = serve(daemon).await.unwrap_err();
        assert!(error.to_string().contains("trop court"), "{error}");
    }
}
