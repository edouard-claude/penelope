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
    let n = negotiate(
        tr.as_ref(),
        ProtocolVersion::V20260728,
        t(500),
        ClientFeatures::default(),
    )
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
    let n = negotiate(
        tr.as_ref(),
        ProtocolVersion::V20260728,
        t(500),
        ClientFeatures::default(),
    )
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
        "tools/list" if !seen.load(std::sync::atomic::Ordering::SeqCst) => Err(McpError::Rpc {
            code: -32603,
            message: "Received a request before initialization completed.".into(),
            data: None,
        }),
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
        ClientFeatures::default(),
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
    assert!(
        pos("initialize") < pos("notifications/initialized"),
        "{order:?}"
    );
    assert!(
        pos("notifications/initialized") < pos("tools/list"),
        "{order:?}"
    );
    assert!(!looks_like_discovery(&json!({})), "réponse vide");
    assert!(looks_like_discovery(
        &json!({"protocolVersion": "2026-07-28"})
    ));
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
    let n = negotiate(
        tr.as_ref(),
        ProtocolVersion::V20260728,
        t(500),
        ClientFeatures::default(),
    )
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
        negotiate(
            http.as_ref(),
            ProtocolVersion::V20260728,
            t(500),
            ClientFeatures::default()
        )
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
    let n = negotiate(
        tr.as_ref(),
        ProtocolVersion::V20260728,
        t(500),
        ClientFeatures::default(),
    )
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
    let e = negotiate(
        tr.as_ref(),
        ProtocolVersion::V20260728,
        t(500),
        ClientFeatures::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, McpError::NoCommonVersion { .. }), "{e:?}");
}

async fn client(tr: Arc<LoopbackTransport>) -> McpClient {
    McpClient::connect(
        "test",
        tr,
        ProtocolVersion::V20260728,
        t(500),
        4,
        ClientFeatures::default(),
    )
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
    assert_eq!(
        params["_meta"]["io.modelcontextprotocol/clientInfo"]["name"],
        "penelope"
    );
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
    assert_eq!(p["_meta"]["io.modelcontextprotocol/logLevel"], "debug");

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
