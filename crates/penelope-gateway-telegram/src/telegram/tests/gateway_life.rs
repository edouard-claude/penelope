//! Vie de la passerelle : construction depuis la configuration, démarrage et boucles,
//! effets incertains, retour OAuth collé, conversations refusées, titre attendu.

use super::*;

/// Sans jeton de bot dans le magasin, Telegram n'est pas configuré ; avec, la passerelle
/// se construit sans rien appeler.
#[tokio::test]
async fn the_gateway_is_built_only_with_a_bot_token() {
    let (_d, g, _t, _p) = gateway().await;
    let core = g.daemon.clone();
    assert!(
        TelegramGateway::from_config(core.clone())
            .await
            .unwrap()
            .is_none()
    );
    core.services
        .platform
        .secrets
        .set("telegram_bot_token", "123:jeton")
        .unwrap();
    assert!(TelegramGateway::from_config(core).await.unwrap().is_some());
}

/// Au démarrage : commandes publiées, nom du bot retenu, effet incertain annoncé une
/// fois ; la boucle de réception traite les updates malgré une coupure, puis tout
/// s'arrête avec le daemon.
#[tokio::test]
async fn start_publishes_announces_polls_and_stops() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let effect = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::EffectUnknown,
            "shell",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec", "arguments": {"command": "make deploy"}}),
            vec!["Rejouer".into(), "Abandonner".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap();
    t.fail_transport(1, "coupure réseau").await;
    t.push_update(updates::text_message(5_001, OWNER, OWNER, "bonjour"))
        .await;
    let handles = g.start().await.unwrap();
    assert!(
        eventually(|| async { s.turns.pending_count().await.unwrap_or(0) == 1 }).await,
        "l'update reçu devient un tour"
    );
    assert_eq!(
        s.kv_get(BOT_USERNAME_KEY).await.unwrap().as_deref(),
        Some("penelope_test_bot")
    );
    assert!(!t.calls_to(tg::SET_MY_COMMANDS).await.is_empty());
    let flag = format!("tg.card.effect.{}", effect.id.as_str());
    assert_eq!(s.kv_get(&flag).await.unwrap().as_deref(), Some("1"));
    assert_eq!(
        g.announce_uncertain_effects().await.unwrap(),
        0,
        "une seule fois"
    );
    assert_eq!(
        s.kv_get("tg.offset").await.unwrap().as_deref(),
        Some("5002")
    );

    g.daemon.handle.shutdown();
    for h in handles {
        tokio::time::timeout(Duration::from_secs(10), h)
            .await
            .expect("boucle arrêtée avec le daemon")
            .unwrap();
    }
}

/// Issue #320 : une URL à `code=` et `state=` d'un outil tiers, qu'aucune autorisation
/// n'attend, est un message ordinaire : elle part à l'agent, et rien n'est refusé.
#[tokio::test]
async fn a_third_party_callback_url_goes_to_the_agent() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    g.process_update(&updates::in_topic(
        updates::text_message(
            5_101,
            OWNER,
            OWNER,
            "http://localhost:8080/callback?code=abc&state=inconnu",
        ),
        7,
    ))
    .await
    .unwrap();
    assert!(
        eventually(|| async { s.turns.pending_count().await.unwrap_or(0) == 1 }).await,
        "l'adresse devient un tour de l'agent"
    );
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        !sent.iter().any(|x| x.contains("Autorisation impossible")),
        "{sent:?}"
    );
}

/// Une adresse dont le `state` est attendu est consommée par l'autorisation, même
/// expirée ; le refus part dans le sujet d'où elle vient, pas dans Général (#320).
#[tokio::test]
async fn an_awaited_callback_is_answered_in_its_topic() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let pending = json!({
        "request": {
            "server": "notes", "issuer": "https://as.example", "state": "attendu",
            "verifier": "v", "redirect_uri": "http://127.0.0.1:7777/oauth/callback",
            "resource": "https://mcp.example/mcp", "scopes": [],
            "authorize_url": "https://as.example/authorize", "expires_at_ms": 0
        },
        "client_id": "c",
        "token_endpoint": "https://as.example/token"
    });
    s.kv_set("mcp.oauth.pending.attendu", &pending.to_string())
        .await
        .unwrap();
    g.process_update(&updates::in_topic(
        updates::text_message(
            5_102,
            OWNER,
            OWNER,
            "http://127.0.0.1:7777/oauth/callback?code=abc&state=attendu",
        ),
        7,
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let calls = t.calls_to(tg::SEND_MESSAGE).await;
    let refusal = calls
        .iter()
        .find(|c| {
            c["text"]
                .as_str()
                .is_some_and(|x| x.starts_with("🔐 Autorisation impossible"))
        })
        .unwrap_or_else(|| panic!("{:?}", texts(&calls)));
    assert!(refusal["text"].as_str().unwrap().contains("expirée"));
    assert_eq!(refusal["message_thread_id"], json!(7), "{refusal}");
    assert_eq!(s.turns.pending_count().await.unwrap(), 0, "pas de tour");
    assert!(
        s.kv_get("mcp.oauth.pending.attendu")
            .await
            .unwrap()
            .is_none(),
        "consommée"
    );
}

/// Un inconnu n'obtient aucune réponse ; un groupe non autorisé est retenu pour que
/// `doctor` donne son identifiant.
#[tokio::test]
async fn strangers_and_foreign_groups_get_no_answer() {
    let (_d, g, t, _p) = gateway().await;
    g.process_update(&updates::text_message(5_201, 777, 777, "salut"))
        .await
        .unwrap();
    let mut group = updates::text_message(5_202, -100_555, OWNER, "coucou");
    group["message"]["chat"]["type"] = json!("supergroup");
    group["message"]["chat"]["title"] = json!("Équipe");
    g.process_update(&group).await.unwrap();
    g.flush_outbox().await.unwrap();
    assert!(t.calls_to(tg::SEND_MESSAGE).await.is_empty());
    let seen = penelope_app::helpers::seen_chats(&g.daemon.services).await;
    assert!(
        seen.iter()
            .any(|c| c["id"] == -100_555 && c["title"] == "Équipe"),
        "{seen:?}"
    );
}

/// Un renommage demandé depuis le menu des sessions prend le message suivant pour titre ;
/// un titre vide ne change rien.
#[tokio::test]
async fn a_requested_title_is_taken_from_the_next_message() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let sid = s
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Ancien".into()),
        )
        .await
        .unwrap()
        .id
        .to_string();
    let now = s.clock.now_ms();
    s.kv_set(&format!("tg.await_title.{OWNER}"), &format!("{sid} {now}"))
        .await
        .unwrap();
    g.process_update(&updates::text_message(5_301, OWNER, OWNER, "Devis ACME"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    assert_eq!(
        s.sessions
            .get(&sid)
            .await
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Devis ACME")
    );
    assert!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.contains("Session renommée : « Devis ACME »"))
    );
    assert_eq!(s.turns.pending_count().await.unwrap(), 0, "pas de tour");
}
