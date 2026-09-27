use super::*;

mod http_headers;

/// #126 : les en-têtes reprennent le corps ; `Mcp-Name` seulement pour les méthodes
/// qui nomment une cible ; rien pour une requête d'avant 2026.
#[test]
fn mirrored_headers_follow_the_body() {
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28"});
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "clickup_search", "arguments": {}, "_meta": meta}});
    assert_eq!(
        mirrored_headers(&call),
        vec![
            ("MCP-Protocol-Version", "2026-07-28".to_string()),
            ("Mcp-Method", "tools/call".to_string()),
            ("Mcp-Name", "clickup_search".to_string()),
        ]
    );
    let read = json!({"method": "resources/read", "params": {"uri": "file:///a", "_meta": meta}});
    assert_eq!(
        mirrored_headers(&read)[2],
        ("Mcp-Name", "file:///a".to_string())
    );
    let list = json!({"method": "tools/list", "params": {"_meta": meta}});
    assert_eq!(mirrored_headers(&list).len(), 2, "pas de Mcp-Name");
    let old = json!({"method": "tools/call", "params": {"name": "x"}});
    assert!(mirrored_headers(&old).is_empty(), "avant 2026 : rien");
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
    assert!(mirrored_headers(&response).is_empty());
}

#[test]
fn unsafe_header_values_use_the_base64_sentinel() {
    assert_eq!(header_value("us-west1"), "us-west1");
    assert_eq!(
        header_value("Hello, 世界"),
        "=?base64?SGVsbG8sIOS4lueVjA==?="
    );
    assert_eq!(header_value(" padded "), "=?base64?IHBhZGRlZCA=?=");
    assert_eq!(header_value("line1\nline2"), "=?base64?bGluZTEKbGluZTI=?=");
    assert_eq!(
        header_value("=?base64?literal?="),
        "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
    );
}

#[test]
fn a_json_rpc_error_in_a_4xx_keeps_its_code() {
    let e = rpc_error_in(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32020,"message":"x"}}"#);
    assert!(
        matches!(e, Some(McpError::Rpc { code: -32020, .. })),
        "{e:?}"
    );
    assert!(rpc_error_in("<html>Bad Request</html>").is_none());
}

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

/// #114 : écrire à un serveur déjà mort (« Broken pipe ») cite sa fin, pas le code
/// d'erreur du système.
#[tokio::test]
async fn writing_to_a_dead_server_says_how_it_died() {
    let t = dying("exit 5").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let e = t
        .request(
            "initialize",
            serde_json::json!({}),
            std::time::Duration::from_secs(10),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("sorti avec le code 5"), "{e}");
    assert!(!e.contains("Broken pipe"), "{e}");
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
