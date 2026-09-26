//! En-têtes et statuts du transport HTTP : ce que Pénélope envoie, ce qu'elle garde de
//! la réponse, et comment un refus remonte.

use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serveur qui rend les réponses données, une par connexion, et les requêtes reçues
/// (en-têtes compris).
async fn server(responses: Vec<String>) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for resp in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                let done = text.find("\r\n\r\n").is_some_and(|end| {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    buf.len() >= end + 4 + len
                });
                if done || n == 0 {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        }
    });
    (url, rx)
}

fn json_reply(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

const T: Duration = Duration::from_secs(5);

/// L'autorisation et les en-têtes du serveur partent avec chaque requête ; l'identifiant
/// de session rendu par le serveur est gardé et renvoyé ensuite.
#[tokio::test]
async fn authorization_extra_headers_and_session_are_sent() {
    let ok = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
    let ok2 = r#"{"jsonrpc":"2.0","id":2,"result":{"ok":true}}"#;
    let (url, mut seen) = server(vec![
        json_reply("200 OK", "mcp-session-id: sess-42\r\n", ok),
        json_reply("200 OK", "", ok2),
    ])
    .await;
    let t = HttpTransport::new(url).unwrap();
    t.set_authorization(Some("Bearer jeton".into())).await;
    t.set_extra_headers(vec![("X-Api-Key".into(), "cle".into())])
        .await;
    assert_eq!(t.session_id().await, None);
    let r = t.request("tools/list", json!({}), T).await.unwrap();
    assert_eq!(r, json!({"ok": true}));
    assert_eq!(t.session_id().await.as_deref(), Some("sess-42"));
    t.request("tools/list", json!({}), T).await.unwrap();

    let first = seen.recv().await.unwrap().to_lowercase();
    assert!(first.contains("authorization: bearer jeton"), "{first}");
    assert!(first.contains("x-api-key: cle"), "{first}");
    assert!(!first.contains("mcp-session-id"), "{first}");
    let second = seen.recv().await.unwrap().to_lowercase();
    assert!(second.contains("mcp-session-id: sess-42"), "{second}");
}

/// 401 : le défi `WWW-Authenticate` remonte pour lancer OAuth ; un 4xx qui porte une
/// erreur JSON-RPC la rend telle quelle ; un 5xx est une erreur HTTP.
#[tokio::test]
async fn refusals_come_back_typed() {
    let (url, _seen) = server(vec![
        "HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Bearer resource_metadata=\"https://x/.well-known\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
        json_reply("400 Bad Request", "", r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"paramètre"}}"#),
        json_reply("503 Service Unavailable", "", r#"{"oups":1}"#),
    ])
    .await;
    let t = HttpTransport::new(url).unwrap();
    match t.request("tools/list", json!({}), T).await.unwrap_err() {
        McpError::Unauthorized {
            status,
            www_authenticate,
        } => {
            assert_eq!(status, 401);
            assert!(www_authenticate.contains("resource_metadata"));
        }
        other => panic!("401 attendu : {other}"),
    }
    match t.request("tools/list", json!({}), T).await.unwrap_err() {
        McpError::Rpc { code, message, .. } => {
            assert_eq!(code, -32602);
            assert_eq!(message, "paramètre");
        }
        other => panic!("erreur JSON-RPC attendue : {other}"),
    }
    match t.request("tools/list", json!({}), T).await.unwrap_err() {
        McpError::Http { status, .. } => assert_eq!(status, 503),
        other => panic!("erreur HTTP attendue : {other}"),
    }
}

/// Un serveur SSE historique ne reçoit pas les en-têtes MCP récents.
#[tokio::test]
async fn a_legacy_server_gets_no_protocol_header() {
    let ok = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
    let (url, mut seen) = server(vec![json_reply("200 OK", "", ok)]).await;
    let t = HttpTransport::legacy(url).unwrap();
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28"});
    t.request("tools/call", json!({"name": "x", "_meta": meta}), T)
        .await
        .unwrap();
    let req = seen.recv().await.unwrap().to_lowercase();
    assert!(!req.contains("mcp-protocol-version"), "{req}");
    assert!(!req.contains("mcp-method"), "{req}");
}
