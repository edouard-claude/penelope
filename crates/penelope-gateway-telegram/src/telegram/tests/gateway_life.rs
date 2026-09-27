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

/// Une adresse de retour OAuth collée qui ne correspond à aucune demande est refusée,
/// avec la raison.
#[tokio::test]
async fn a_pasted_oauth_callback_without_request_is_refused() {
    let (_d, g, t, _p) = gateway().await;
    g.process_update(&updates::text_message(
        5_101,
        OWNER,
        OWNER,
        "http://127.0.0.1:7777/oauth/callback?code=abc&state=inconnu",
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|x| x.starts_with("🔐 Autorisation impossible")),
        "{sent:?}"
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
