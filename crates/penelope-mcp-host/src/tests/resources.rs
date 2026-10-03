//! Ressources (#293) : abonnement posé une fois, reposé après une reconnexion,
//! notification au journal, lecture, et serveur qui ne sait pas s'abonner.

use super::*;

/// Serveur 2025-06-18 : `resources/read` rend `content`, les URI abonnées s'ajoutent à
/// `subscribed` ; `subscribe` dit s'il déclare la capacité.
fn bridge(
    content: Arc<Mutex<String>>,
    subscribed: Arc<Mutex<Vec<String>>>,
    subscribe: bool,
) -> Handler {
    Arc::new(move |m, p| match m {
        "server/discover" => Err(McpError::Rpc {
            code: penelope_mcp::protocol::METHOD_NOT_FOUND,
            message: "Method not found".into(),
            data: None,
        }),
        "initialize" => Ok(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}, "resources": {"subscribe": subscribe}},
            "serverInfo": {"name": "pont", "version": "1.0"}
        })),
        "tools/list" => Ok(json!({"tools": [tool("lire", json!({"readOnlyHint": true}))]})),
        "resources/subscribe" => {
            subscribed
                .lock()
                .unwrap()
                .push(p["uri"].as_str().unwrap_or_default().to_string());
            Ok(json!({}))
        }
        "resources/read" => Ok(json!({"contents": [{
            "uri": p["uri"], "mimeType": "application/json",
            "text": content.lock().unwrap().clone()
        }]})),
        _ => Ok(json!({})),
    })
}

#[tokio::test]
async fn a_subscription_is_posted_once_then_reposted_after_a_reconnection() {
    let (_d, s, clock, fake, sup) = setup().await;
    let content = Arc::new(Mutex::new(r#"[{"id":1}]"#.to_string()));
    let subscribed = Arc::new(Mutex::new(Vec::new()));
    fake.serve("pont", bridge(content, subscribed.clone(), true));
    declare(&sup, "pont", "");
    sup.reload().await;

    assert_eq!(
        sup.subscribe_resource("pont", "mail://inbox").await,
        Ok(true)
    );
    assert_eq!(
        sup.subscribe_resource("pont", "mail://inbox").await,
        Ok(true),
        "idempotent sur la même connexion"
    );
    assert_eq!(
        subscribed.lock().unwrap().len(),
        1,
        "un seul resources/subscribe"
    );

    let read = sup.read_resource("pont", "mail://inbox").await.unwrap();
    assert_eq!(read["contents"][0]["text"], r#"[{"id":1}]"#);
    assert_eq!(read["contents"][0]["uri"], "mail://inbox");
    assert_eq!(read["contents"][0]["mimeType"], "application/json");

    // La notification du serveur devient un événement du journal, serveur et URI.
    fake.last_transport("pont").push_notification(
        "notifications/resources/updated",
        json!({"uri": "mail://inbox"}),
    );
    let mut seen = None;
    for _ in 0..50 {
        let events = s.events.range(0, 100).await.unwrap();
        if let Some(e) = events.iter().find(|e| e.kind == "mcp.resource_updated") {
            seen = Some(e.payload.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(seen, Some(json!({"server": "pont", "uri": "mail://inbox"})));

    // Connexion perdue : la suivante repart sans abonnement, l'appel suivant le repose.
    let slot = sup.slot("pont").await.unwrap();
    sup.connection_lost(
        &slot,
        &McpError::Transport("le serveur a fermé la connexion".into()),
    )
    .await;
    let e = sup
        .subscribe_resource("pont", "mail://inbox")
        .await
        .unwrap_err();
    assert!(e.contains("nouvel essai"), "{e}");
    clock.advance_ms(5_000);
    assert_eq!(
        sup.subscribe_resource("pont", "mail://inbox").await,
        Ok(true)
    );
    assert_eq!(fake.opened("pont"), 2, "une connexion neuve");
    assert_eq!(
        subscribed.lock().unwrap().len(),
        2,
        "reposé sur la nouvelle connexion"
    );
}

/// Un serveur sans `resources.subscribe` le dit (`Ok(false)`), un serveur inconnu est une
/// erreur, et un serveur abonné n'est jamais arrêté pour inactivité.
#[tokio::test]
async fn a_server_without_subscriptions_says_so_and_a_subscribed_one_is_never_idle() {
    let (_d, _s, clock, fake, sup) = setup().await;
    let subscribed = Arc::new(Mutex::new(Vec::new()));
    fake.serve(
        "sans",
        bridge(
            Arc::new(Mutex::new("{}".into())),
            Arc::new(Mutex::new(Vec::new())),
            false,
        ),
    );
    fake.serve(
        "avec",
        bridge(Arc::new(Mutex::new("{}".into())), subscribed.clone(), true),
    );
    declare(&sup, "sans", "");
    declare(&sup, "avec", "");
    sup.reload().await;

    assert_eq!(sup.subscribe_resource("sans", "x://y").await, Ok(false));
    assert!(subscribed.lock().unwrap().is_empty());
    let e = sup
        .subscribe_resource("inconnu", "x://y")
        .await
        .unwrap_err();
    assert!(e.contains("inconnu"), "{e}");
    assert_eq!(sup.subscribe_resource("avec", "x://y").await, Ok(true));

    // Deux heures sans appel : le serveur paresseux sans abonnement s'arrête, l'abonné
    // reste debout (sa santé est sondée, pas sa présence).
    clock.advance_ms(2 * 3_600_000);
    sup.maintenance().await;
    let running: BTreeMap<String, bool> = sup
        .statuses()
        .await
        .into_iter()
        .map(|s| (s.name, s.running))
        .collect();
    assert!(!running["sans"], "{running:?}");
    assert!(running["avec"], "{running:?}");
}
