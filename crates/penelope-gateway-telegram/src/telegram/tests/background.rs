use super::*;

/// Issue #10, règle précisée : une session quittée finit son tour en cours, mais sa
/// réponse, ses messages et ses approbations sont mis de côté derrière une seule
/// notification ; les nouveaux messages vont au focus ; le bouton « Basculer » envoie
/// tout dans l'ordre.
#[tokio::test]
async fn background_sessions_hold_their_replies_until_switched_back() {
    let (_d, g, t, p) = gateway().await;
    let d = g.daemon.clone();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let first = d.chat_session_for(&chat).await.unwrap();
    d.services
        .sessions
        .set_title(&first, "Refonte", false)
        .await
        .unwrap();
    let asked = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: Some(77),
    };
    d.enqueue_message(&first, "longue question", &asked, None)
        .await
        .unwrap();
    // Tour déjà pris par un runner quand l'utilisateur change de session.
    let running = d.services.turns.claim("test").await.unwrap().unwrap();
    g.process_update(&updates::text_message(100, OWNER, OWNER, "/fork"))
        .await
        .unwrap();
    let fork = d.chat_session_for(&chat).await.unwrap();
    assert_ne!(fork, first);

    p.reply(r#"{"complexity":"low"}"#);
    p.reply("réponse de fond");
    crate::runner::process(&d, running, Duration::from_secs(30)).await;
    let m: Arc<dyn Messenger> = g.clone();
    m.send_session_text(&first, &chat, "question de fond ?")
        .await
        .unwrap();
    let approval = d
        .services
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"arguments": {"cmd": "make deploy"}}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&first),
            None,
            false,
        )
        .await
        .unwrap();
    g.deliver(
        "t-approbation",
        &first,
        &chat,
        &TurnOutcome::AwaitingApproval {
            approval_id: approval.id.0.clone(),
        },
    )
    .await;
    g.flush_outbox().await.unwrap();

    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let out = texts(&sent);
    assert!(
        !out.iter()
            .any(|x| x.contains("réponse de fond") || x.contains("question de fond")),
        "{out:?}"
    );
    let notices: Vec<&Value> = sent
        .iter()
        .filter(|c| c["text"].as_str().is_some_and(|x| x.contains("📬")))
        .collect();
    assert_eq!(notices.len(), 1, "une seule notification : {out:?}");
    assert_eq!(notices[0]["disable_notification"], true);
    let edits = texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
    assert!(
        edits
            .iter()
            .any(|x| x.contains("2 réponses et 1 approbation en attente dans « Refonte »")),
        "{edits:?}"
    );

    // Un nouveau message va au focus, qui répond aussitôt.
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("réponse du fork");
    g.process_update(&updates::text_message(101, OWNER, OWNER, "et maintenant ?"))
        .await
        .unwrap();
    drain(&g).await;
    g.flush_outbox().await.unwrap();
    assert!(texts(&t.calls_to(tg::SEND_MESSAGE).await).contains(&"réponse du fork".to_string()));

    // Basculer : tout part dans l'ordre, la notification perd son bouton.
    let switch = inline_buttons(notices[0])[0].clone();
    assert_eq!(switch.0, "↪️ Basculer");
    g.process_update(&updates::callback(102, OWNER, &switch.1, 5000))
        .await
        .unwrap();
    settle_click(&g).await;
    g.flush_outbox().await.unwrap();
    assert_eq!(
        d.services
            .sessions
            .find_by_topic(OWNER, None)
            .await
            .unwrap()
            .unwrap()
            .id
            .to_string(),
        first
    );
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let out = texts(&sent);
    let at = |needle: &str| out.iter().position(|x| x.contains(needle)).unwrap();
    assert!(at("réponse de fond") < at("question de fond ?"), "{out:?}");
    assert!(at("question de fond ?") < at("make deploy"), "{out:?}");
    let answer = sent
        .iter()
        .find(|c| {
            c["text"]
                .as_str()
                .is_some_and(|x| x.contains("réponse de fond"))
        })
        .unwrap();
    assert_eq!(answer["reply_parameters"]["message_id"], 77);
    assert!(
        texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await)
            .iter()
            .any(|x| x.contains("envoyées ci-dessous"))
    );
    assert!(
        d.services
            .kv_get(&held_key(&first))
            .await
            .unwrap()
            .is_none()
    );
}

/// #112 et #161 : deux messages en file dans A, bascule vers B : A répond une fois
/// en fond, sa réponse est retenue puis délivrée au retour ; A au-delà de
/// son plafond s'arrête sans toucher à B.
#[tokio::test]
async fn a_left_session_keeps_working_and_answers_on_return() {
    let (_d, g, t, p) = gateway().await;
    let d = g.daemon.clone();
    let s = d.services.clone();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let a = d.chat_session_for(&chat).await.unwrap();
    d.pin_model(&a, Some("main")).await.unwrap();
    for text in ["prépare le rapport", "et la synthèse"] {
        d.enqueue_message(&a, text, &chat, None).await.unwrap();
    }
    let b = s
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Budget 2027".into()),
        )
        .await
        .unwrap()
        .id
        .to_string();
    d.pin_model(&b, Some("main")).await.unwrap();
    g.process_update(&updates::text_message(
        700,
        OWNER,
        OWNER,
        &format!("/switch {b}"),
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let notice = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        notice.contains("continue en fond (2 tours en file)"),
        "{notice}"
    );

    p.reply("rapport et synthèse de fond");
    drain(&g).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(!sent.iter().any(|x| x.contains("de fond")), "{sent:?}");

    g.process_update(&updates::text_message(
        701,
        OWNER,
        OWNER,
        &format!("/switch {a}"),
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|x| x.contains("rapport et synthèse de fond")),
        "{sent:?}"
    );

    // A en fond au-delà de son plafond de session : elle s'arrête, B répond.
    g.process_update(&updates::text_message(
        702,
        OWNER,
        OWNER,
        &format!("/switch {b}"),
    ))
    .await
    .unwrap();
    s.sessions.set_budget(&a, Some(0.01)).await.unwrap();
    s.budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(a.clone()),
            model: "mock".into(),
            provider: "mock".into(),
            cost_usd: 1.0,
            ..Default::default()
        })
        .await
        .unwrap();
    d.enqueue_message(&a, "encore une chose", &chat, None)
        .await
        .unwrap();
    p.reply("réponse de B");
    g.process_update(&updates::text_message(703, OWNER, OWNER, "question pour B"))
        .await
        .unwrap();
    drain(&g).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.iter().any(|x| x == "réponse de B"), "{sent:?}");
    assert_eq!(s.turns.queued_for(&a).await.unwrap(), 0, "A s'est arrêtée");
    assert_eq!(
        p.requests()
            .iter()
            .filter(|r| r
                .messages
                .iter()
                .any(|m| m.text().contains("encore une chose")))
            .count(),
        0,
        "A n'a rien dépensé de plus"
    );
}
