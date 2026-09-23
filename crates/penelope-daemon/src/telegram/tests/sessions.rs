use super::*;

/// #72 : `/export` d'une session inconnue dit « introuvable » au lieu d'envoyer un
/// fichier vide.
#[tokio::test]
async fn exporting_an_unknown_session_says_so() {
    let (_d, g, t, _p) = gateway().await;
    g.daemon
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    g.process_update(&updates::text_message(
        96,
        OWNER,
        OWNER,
        "/export s_inexistante",
    ))
    .await
    .unwrap();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        g.flush_outbox().await.unwrap();
        if !t.calls_to(tg::SEND_MESSAGE).await.is_empty() {
            break;
        }
    }
    assert!(
        t.calls_to("sendDocument").await.is_empty(),
        "aucun fichier ne doit partir"
    );
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|m| m.contains("aucune session ne correspond") || m.contains("introuvable")),
        "{sent:?}"
    );
}

#[tokio::test]
async fn compact_summarises_the_chat_session_and_reports_back() {
    let (_d, g, t, p) = gateway().await;
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let sid = g.daemon.chat_session_for(&origin).await.unwrap();
    let h = &g.daemon.services.context.history;
    for i in 0..40 {
        let m = if i % 2 == 0 {
            penelope_llm::types::ChatMessage::user(format!("q{i} {}", "mot ".repeat(500)))
        } else {
            penelope_llm::types::ChatMessage::assistant(format!("r{i} {}", "mot ".repeat(500)))
        };
        h.append(&sid, &m, 600, 0, false, None).await.unwrap();
    }
    p.reply(r#"{"objectif": "tester /compact", "fait": "tout"}"#);

    g.process_update(&updates::text_message(95, OWNER, OWNER, "/compact"))
        .await
        .unwrap();
    // Le bilan arrive quand le résumé est publié, sans bloquer la file des updates.
    let mut out = Vec::new();
    for _ in 0..100 {
        g.flush_outbox().await.unwrap();
        out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        if !out.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        out.iter().any(|m| m.contains("messages résumés")),
        "{out:?}"
    );
    assert_eq!(
        g.daemon
            .services
            .context
            .lcm
            .active_nodes(&sid)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn new_starts_a_fresh_session_for_the_chat() {
    let (_d, g, _t, _p) = gateway().await;
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let first = g.daemon.chat_session_for(&origin).await.unwrap();
    g.process_update(&updates::text_message(30, OWNER, OWNER, "/new refonte"))
        .await
        .unwrap();
    let second = g.daemon.chat_session_for(&origin).await.unwrap();
    assert_ne!(first, second);
}

/// Issue #14 : `/sessions` rend un bouton par session ; un clic lie le chat à la session
/// et édite le message, les sessions fermées n'apparaissent qu'à la demande.
#[tokio::test]
async fn sessions_menu_switches_with_a_click() {
    let (_d, g, t, _p) = gateway().await;
    let d = g.daemon.clone();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let first = d.chat_session_for(&chat).await.unwrap();
    d.services
        .sessions
        .set_title(&first, "Refonte du site", false)
        .await
        .unwrap();
    d.enqueue_message(&first, "en attente", &chat, None)
        .await
        .unwrap();
    let other = d
        .services
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Budget 2027".into()),
        )
        .await
        .unwrap()
        .id
        .to_string();
    let closed = d
        .services
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Vieux sujet".into()),
        )
        .await
        .unwrap()
        .id
        .to_string();
    d.services
        .sessions
        .set_state(&closed, "closed")
        .await
        .unwrap();

    let buttons = inline_buttons;
    g.process_update(&updates::text_message(90, OWNER, OWNER, "/sessions"))
        .await
        .unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let menu = buttons(sent.last().unwrap());
    let labels: Vec<&str> = menu.iter().map(|(l, _)| l.as_str()).collect();
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("▶️ ⏳1 Refonte du site")),
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("Vieux sujet")),
        "{labels:?}"
    );
    let switch = menu
        .iter()
        .find(|(l, _)| l.contains("Budget 2027"))
        .unwrap()
        .1
        .clone();

    g.process_update(&updates::callback(91, OWNER, &switch, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    let bound = d
        .services
        .sessions
        .find_by_topic(OWNER, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound.id.to_string(), other);
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let edited = edits.last().expect("message édité");
    assert_eq!(edited["message_id"], 1001);
    let labels: Vec<String> = buttons(edited).into_iter().map(|(l, _)| l).collect();
    assert!(
        labels.iter().any(|l| l.starts_with("▶️ Budget 2027")),
        "{labels:?}"
    );
    // La session qui a perdu le chat garde sa file et travaille en fond (#112).
    assert!(
        labels.iter().any(|l| l.starts_with("⏳1 Refonte du site")),
        "{labels:?}"
    );

    // Fermées à la demande, puis « ⋯ » > Fermer sur la session d'origine.
    let show_closed = buttons(edited)
        .into_iter()
        .find(|(l, _)| l == "Voir les fermées")
        .unwrap()
        .1;
    g.process_update(&updates::callback(92, OWNER, &show_closed, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let all = buttons(edits.last().unwrap());
    assert!(
        all.iter().any(|(l, _)| l.starts_with("🔒 Vieux sujet")),
        "{all:?}"
    );
    let refonte = all
        .iter()
        .position(|(l, _)| l.starts_with("⏳1 Refonte du site"))
        .unwrap();
    let more = all[refonte + 1].1.clone();
    g.process_update(&updates::callback(93, OWNER, &more, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let close = buttons(edits.last().unwrap())
        .into_iter()
        .find(|(l, _)| l.contains("Fermer"))
        .unwrap()
        .1;
    g.process_update(&updates::callback(94, OWNER, &close, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(
        d.services
            .sessions
            .get(&first)
            .await
            .unwrap()
            .unwrap()
            .state,
        "closed"
    );
    let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
    assert!(
        answers.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .starts_with("Session fermée"),
        "{answers:?}"
    );

    // Renommer : le message suivant devient le titre, sans ouvrir de tour.
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let rows = buttons(edits.last().unwrap());
    let budget = rows
        .iter()
        .position(|(l, _)| l.contains("Budget 2027"))
        .unwrap();
    g.process_update(&updates::callback(95, OWNER, &rows[budget + 1].1, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let rename = buttons(edits.last().unwrap())
        .into_iter()
        .find(|(l, _)| l.contains("Renommer"))
        .unwrap()
        .1;
    g.process_update(&updates::callback(96, OWNER, &rename, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    g.process_update(&updates::text_message(
        97,
        OWNER,
        OWNER,
        "Budget prévisionnel",
    ))
    .await
    .unwrap();
    assert_eq!(
        d.services
            .sessions
            .get(&other)
            .await
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Budget prévisionnel")
    );
    assert!(d.services.turns.claim("test").await.unwrap().is_none());
}

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
    assert!(d.kv_get(&held_key(&first)).await.unwrap().is_none());
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

/// #112 : `/new` sur un fil dont la session travaille annonce les tours qui seraient
/// perdus avant de fermer ; « garder en fond » crée la nouvelle sans fermer l'ancienne.
#[tokio::test]
async fn new_says_what_it_would_lose_before_closing() {
    let (_d, g, t, _p) = gateway().await;
    let d = g.daemon.clone();
    let s = d.services.clone();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let a = d.chat_session_for(&chat).await.unwrap();
    for text in ["un", "deux"] {
        d.enqueue_message(&a, text, &chat, None).await.unwrap();
    }
    g.process_update(&updates::text_message(710, OWNER, OWNER, "/new"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let ask = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
    assert!(
        ask["text"].as_str().unwrap().contains("2 tour(s) en file"),
        "{ask}"
    );
    assert_eq!(s.sessions.get(&a).await.unwrap().unwrap().state, "active");
    assert_eq!(
        s.turns.queued_for(&a).await.unwrap(),
        2,
        "rien n'est encore perdu"
    );

    let buttons = inline_buttons(&ask);
    let keep = buttons
        .iter()
        .find(|(l, _)| l.contains("Garder"))
        .unwrap()
        .1
        .clone();
    let close = buttons
        .iter()
        .find(|(l, _)| l.contains("Fermer"))
        .unwrap()
        .1
        .clone();
    g.process_update(&updates::callback(711, OWNER, &keep, 9001))
        .await
        .unwrap();
    settle_click(&g).await;
    let b = d.chat_session_for(&chat).await.unwrap();
    assert_ne!(b, a);
    assert_eq!(s.sessions.get(&a).await.unwrap().unwrap().state, "active");
    assert_eq!(s.turns.queued_for(&a).await.unwrap(), 2, "gardée en fond");

    // Le bouton « Fermer » d'une autre question ferme la session du fil, avec sa file.
    d.enqueue_message(&b, "trois", &chat, None).await.unwrap();
    g.process_update(&updates::text_message(712, OWNER, OWNER, "/new"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let ask = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
    let close_b = inline_buttons(&ask)
        .into_iter()
        .find(|(l, _)| l.contains("Fermer"))
        .unwrap()
        .1;
    let _ = close;
    g.process_update(&updates::callback(713, OWNER, &close_b, 9002))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(s.sessions.get(&b).await.unwrap().unwrap().state, "closed");
    assert_eq!(s.turns.queued_for(&b).await.unwrap(), 0);
}

/// Issues #10 et #112 : après `/fork`, seul le fork répond dans le fil ; les messages
/// en attente de la session d'origine s'exécutent en fond et leurs réponses sont
/// retenues pour le retour ; `/close` arrête une session et vide sa file.
#[tokio::test]
async fn after_a_fork_only_the_fork_answers() {
    let (_d, g, t, p) = gateway().await;
    let d = g.daemon.clone();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let first = d.chat_session_for(&chat).await.unwrap();
    d.pin_model(&first, Some("main")).await.unwrap();
    for text in ["vieux message 1", "vieux message 2"] {
        d.enqueue_message(&first, text, &chat, None).await.unwrap();
    }
    g.process_update(&updates::text_message(80, OWNER, OWNER, "/fork"))
        .await
        .unwrap();
    let fork = d.chat_session_for(&chat).await.unwrap();
    assert_ne!(fork, first);
    d.pin_model(&fork, Some("main")).await.unwrap();
    g.flush_outbox().await.unwrap();
    let notice = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        notice.contains("continue en fond (2 tours en file)"),
        "{notice}"
    );

    p.reply("réponse de fond 1 et 2");
    for i in 0..3 {
        p.reply(&format!("réponse {i}"));
        g.process_update(&updates::text_message(
            81 + i,
            OWNER,
            OWNER,
            &format!("question {i}"),
        ))
        .await
        .unwrap();
        drain(&g).await;
    }

    let states = |sid: String| {
        let store = d.services.store.clone();
        async move {
            store
                .read(move |c| {
                    let mut st = c.prepare(
                        "SELECT state FROM turn_queue WHERE session_id = ?1 ORDER BY enqueued_at",
                    )?;
                    let rows = st.query_map([sid], |r| r.get::<_, String>(0))?;
                    Ok(rows.collect::<Result<Vec<String>, _>>()?)
                })
                .await
                .unwrap()
        }
    };
    assert_eq!(
        states(first.clone()).await,
        vec!["done", "merged"],
        "exécutés en fond"
    );
    assert_eq!(states(fork.clone()).await, vec!["done", "done", "done"]);
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    for i in 0..3 {
        assert!(
            sent.iter().any(|x| x == &format!("réponse {i}")),
            "{sent:?}"
        );
    }
    assert!(
        !sent.iter().any(|x| x.contains("réponse de fond")),
        "retenues tant que le fork a le fil : {sent:?}"
    );
    assert!(d.kv_get(&held_key(&first)).await.unwrap().is_some());
    assert_eq!(
        d.services
            .sessions
            .find_by_topic(OWNER, None)
            .await
            .unwrap()
            .unwrap()
            .id
            .as_str(),
        fork.as_str(),
        "une seule session liée au chat"
    );

    // `/close` : la file du fork est vidée, le chat n'a plus de session liée.
    d.enqueue_message(&fork, "encore un", &chat, None)
        .await
        .unwrap();
    g.process_update(&updates::text_message(90, OWNER, OWNER, "/close"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    assert_eq!(
        states(fork.clone()).await.last().unwrap(),
        "pending",
        "fermer demande confirmation (issue #30)"
    );
    let confirm = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
    let token = inline_buttons(&confirm)
        .into_iter()
        .find(|(l, _)| l.contains("Confirmer"))
        .expect("bouton Confirmer")
        .1;
    g.process_update(&updates::callback(91, OWNER, &token, 5000))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(states(fork.clone()).await.last().unwrap(), "cancelled");
    assert!(
        d.services
            .sessions
            .find_by_topic(OWNER, None)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        d.services.sessions.get(&fork).await.unwrap().unwrap().state,
        "closed"
    );
}

/// Issue #2 : une session reçoit un titre lisible après son premier échange ; il
/// complète le message « Nouvelle session », se change par `/title` et s'affiche
/// dans `/sessions`.
#[tokio::test]
async fn sessions_get_a_readable_title() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .publish_config("test", |c| {
            c.context.auto_title = true;
            Ok(vec!["context.auto_title".into()])
        })
        .unwrap();
    g.process_update(&updates::text_message(40, OWNER, OWNER, "/new"))
        .await
        .unwrap();
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let sid = g.daemon.chat_session_for(&origin).await.unwrap();

    p.reply(r#"{"complexity":"low"}"#);
    p.reply("On garde la maquette verte pour Zéphyr.");
    p.reply("« Refonte du site Zéphyr. »");
    g.process_update(&updates::text_message(
        41,
        OWNER,
        OWNER,
        "Quelle maquette pour la refonte du site Zéphyr ?",
    ))
    .await
    .unwrap();
    drain(&g).await;
    let mut title = None;
    for _ in 0..100 {
        title = g
            .daemon
            .services
            .sessions
            .get(&sid)
            .await
            .unwrap()
            .unwrap()
            .title;
        if title.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(title.as_deref(), Some("Refonte du site Zéphyr"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert!(
        edits.iter().any(|e| e["text"]
            .as_str()
            .unwrap()
            .contains("Refonte du site Zéphyr")),
        "{edits:?}"
    );

    g.process_update(&updates::text_message(
        42,
        OWNER,
        OWNER,
        "/title Maquette Zéphyr",
    ))
    .await
    .unwrap();
    g.process_update(&updates::text_message(43, OWNER, OWNER, "/sessions"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.iter().any(|x| x.contains("renommée")), "{sent:?}");
    assert!(sent.last().unwrap().contains("Maquette Zéphyr"), "{sent:?}");
    // Un titre posé à la main n'est jamais remplacé par un titre automatique.
    assert!(
        !g.daemon
            .services
            .sessions
            .set_title(&sid, "Autre", true)
            .await
            .unwrap()
    );
}
