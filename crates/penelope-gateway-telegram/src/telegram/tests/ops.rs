use super::*;

/// #133 : pour un tour planifié, la réponse finale est le livrable, livrée une fois.
/// Un `send_message` du même contenu la remplace ; un message intermédiaire différent
/// s'y ajoute ; un `send_message` en échec ne l'empêche pas.
#[tokio::test]
async fn a_scheduled_digest_is_delivered_once() {
    const DIGEST: &str = "🧭 Veille agents IA — 19/09\n\n6 retenus sur 41 : récit par projet.";
    for (case, sent, expected) in [
        ("même contenu", json!({"text": DIGEST}), 1),
        (
            "intermédiaire",
            json!({"text": "Limite GitHub atteinte, je continue."}),
            2,
        ),
        ("échec", json!({}), 1),
    ] {
        let (_d, g, t, p) = gateway().await;
        let s = g.daemon.services.clone();
        g.daemon
            .publish_config("test", |c| {
                c.models.routing.classifier = false;
                Ok(vec!["models.routing.classifier".into()])
            })
            .unwrap();
        // Comme sur l'instance : `send_message` autorisé par une règle.
        s.policies
            .create_rule(
                penelope_hitl::policy::RuleScope::Tool,
                Some("send_message"),
                None,
                None,
                penelope_kernel::risk::PolicyDecision::Auto,
                penelope_kernel::risk::PolicyWindow::Always,
                None,
            )
            .await
            .unwrap();
        let sched = s
            .schedules
            .create(
                penelope_workflow::TriggerKind::Cron,
                json!({"expr": "33 8 * * *"}),
                json!({"type": "prompt", "prompt": "Fais la veille",
                       "origin": {"channel": "telegram", "chat_id": OWNER}}),
                json!({}),
            )
            .await
            .unwrap();
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "send_message".into(),
                arguments: sent,
            }],
        ));
        p.reply(&format!(
            "Veille du 19/09\n{}",
            DIGEST.split_once('\n').unwrap().1
        ));
        penelope_orchestrator::scheduler::run_now(
            &penelope_daemon::workflow::context_of(&g.daemon),
            &g.daemon.hooks.scheduler(),
            &sched.id,
        )
        .await
        .unwrap();
        let turn = s.turns.claim("t").await.unwrap().expect("tour planifié");
        penelope_daemon::runner::process(&daemon_of(&g), turn, Duration::from_secs(30)).await;
        g.flush_outbox().await.unwrap();
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        let digests = sent.iter().filter(|x| x.contains("6 retenus")).count();
        assert_eq!(digests, 1, "{case} : {sent:?}");
        assert_eq!(sent.len(), expected, "{case} : {sent:?}");
    }
}

/// #129 : le rappel de #97 passe par la même carte ; il part lui aussi avec la    /// #129 : le rappel de #97 passe par la même carte ; il part lui aussi avec la
/// commande telle quelle, au lieu d'échouer jusqu'à l'expiration.
#[tokio::test]
async fn the_reminder_of_a_go_template_command_is_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::default();
    let s = Arc::new(
        penelope_app::services::Services::for_tests(
            dir.path().to_path_buf(),
            Arc::new(clock.clone()),
        )
        .await
        .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    d.publish_config("test", |c| {
        c.telegram.rate_per_chat_per_s = 1_000.0;
        Ok(vec!["telegram.rate_per_chat_per_s".into()])
    })
    .unwrap();
    let t = MockTransport::new();
    let g = TelegramGateway::with_transport(d.core.clone(), t.clone());
    g.register();
    s.approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec",
                   "arguments": {"command": "kubectl get pods -o go-template='{{.metadata.name}}'"}}),
            vec![],
            None,
            None,
            false,
        )
        .await
        .unwrap();
    clock.advance_ms(3_600_000 + 1);
    penelope_daemon::supervisor::maintenance_pass(&d)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(sent.contains("Rappel 1/2"), "{sent}");
    assert!(
        sent.contains("{{.metadata.name}}"),
        "carte du rappel : {sent}"
    );
}

#[tokio::test]
async fn routing_and_costs_are_readable_from_telegram() {
    let (_d, g, t, p) = gateway().await;
    g.process_update(&updates::text_message(40, OWNER, OWNER, "/models"))
        .await
        .unwrap();
    g.process_update(&updates::text_message(41, OWNER, OWNER, "/model auto off"))
        .await
        .unwrap();
    g.process_update(&updates::text_message(42, OWNER, OWNER, "/model"))
        .await
        .unwrap();

    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let sid = g.daemon.chat_session_for(&origin).await.unwrap();
    g.daemon
        .services
        .budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(sid.clone()),
            turn_id: Some("t_x".into()),
            role: Some("chat".into()),
            model: "z-ai/glm-5.3".into(),
            provider: "openrouter".into(),
            cost_usd: 0.0123,
            ..Default::default()
        })
        .await
        .unwrap();
    g.process_update(&updates::text_message(43, OWNER, OWNER, "/budget"))
        .await
        .unwrap();
    g.process_update(&updates::text_message(44, OWNER, OWNER, "/budget sessions"))
        .await
        .unwrap();
    drain(&g).await;
    assert_eq!(p.call_count(), 0);

    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(out[0].contains("Adaptatif"), "{out:?}");
    assert!(out[0].contains("simple"), "{out:?}");
    assert!(out[1].contains("Routage fixe"), "{out:?}");
    assert!(out[2].contains("Modèle de cette session"), "{out:?}");
    assert!(out[2].contains("tout passe par"), "{out:?}");
    assert!(!g.daemon.services.config.config().models.routing.classifier);
    assert!(out[3].contains("0,0123 $"), "{out:?}");
    assert!(out[3].contains("t_x"), "{out:?}");
    assert!(out[4].contains(&sid), "{out:?}");
}

#[tokio::test]
async fn model_buttons_pin_the_session_then_give_it_back_to_the_router() {
    let (_d, g, t, p) = gateway().await;
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let sid = g.daemon.chat_session_for(&origin).await.unwrap();
    let cfg = g.daemon.services.config.config();
    let main = cfg.alias_model("main").unwrap().to_string();
    let reasoning = cfg.alias_model("reasoning").unwrap().to_string();

    g.process_update(&updates::text_message(70, OWNER, OWNER, "/model"))
        .await
        .unwrap();
    drain(&g).await;
    let buttons = model_buttons(&t).await;
    let labels: Vec<&str> = buttons.iter().map(|(l, _)| l.as_str()).collect();
    assert!(labels[0].starts_with("fast · "), "{labels:?}");
    assert!(labels[1].starts_with("main · "), "{labels:?}");
    assert!(labels[2].starts_with("reasoning · "), "{labels:?}");
    assert_eq!(labels.last(), Some(&"✅ 🔀 Automatique"));
    assert!(
        !labels
            .iter()
            .any(|l| l.contains("embedding") || l.contains("stt")),
        "{labels:?}"
    );

    // Clic sur `main` : la session est épinglée, le menu se met à jour.
    let main_token = buttons[1].1.clone();
    g.process_update(&updates::callback(71, OWNER, &main_token, 700))
        .await
        .unwrap();
    settle_click(&g).await;
    let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
    assert_eq!(answers.last().unwrap()["text"], "Session épinglée sur main");
    let edited = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let edited = edited.last().unwrap();
    assert!(
        edited["text"].as_str().unwrap().contains("Épinglé sur"),
        "{edited}"
    );
    assert!(
        edited["reply_markup"]["inline_keyboard"][1][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("✅ main"),
        "{edited}"
    );

    // Le message suivant part sur `main`, sans appel au classifieur.
    p.reply("Réponse de main.");
    g.process_update(&updates::text_message(72, OWNER, OWNER, "ok"))
        .await
        .unwrap();
    drain(&g).await;
    assert_eq!(
        p.call_count(),
        1,
        "pas de classifieur quand la session est épinglée"
    );
    assert_eq!(p.requests()[0].model, main);

    // Le même bouton sert encore : un jeton de menu n'est pas à usage unique.
    g.process_update(&updates::callback(73, OWNER, &main_token, 700))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(
        t.calls_to(tg::ANSWER_CALLBACK_QUERY).await.last().unwrap()["text"],
        "Session épinglée sur main"
    );

    // En texte : `/model reasoning` épingle, `/model auto` rend la main au routeur.
    g.process_update(&updates::text_message(74, OWNER, OWNER, "/model reasoning"))
        .await
        .unwrap();
    p.reply("Réponse de reasoning.");
    g.process_update(&updates::text_message(75, OWNER, OWNER, "et là ?"))
        .await
        .unwrap();
    drain(&g).await;
    assert_eq!(p.requests().last().unwrap().model, reasoning);

    let auto_token = model_buttons(&t).await.last().unwrap().1.clone();
    g.process_update(&updates::callback(76, OWNER, &auto_token, 701))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(
        t.calls_to(tg::ANSWER_CALLBACK_QUERY).await.last().unwrap()["text"],
        "Session en automatique"
    );
    assert!(g.daemon.pinned_model(&sid).await.is_none());

    g.process_update(&updates::text_message(77, OWNER, OWNER, "/model inconnu"))
        .await
        .unwrap();
    drain(&g).await;
    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        out.iter()
            .any(|m| m.contains("reasoning") && m.contains("📌")),
        "{out:?}"
    );
    assert!(out.last().unwrap().contains("alias inconnu"), "{out:?}");
}

#[tokio::test]
async fn mcp_servers_are_visible_and_restartable_from_telegram() {
    use penelope_mcp_host::testing::{FakeConnector, declare, server, tool};
    let (_d, g, t, _p) = gateway().await;
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "redmine",
        server(Arc::new(std::sync::Mutex::new(vec![tool(
            "list_issues",
            json!({"readOnlyHint": true}),
        )]))),
    );
    let sup = penelope_mcp_host::testing::supervisor(g.daemon.services.clone(), fake.clone());
    declare(&sup, "redmine", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());
    std::fs::write(sup.dir().join("casse.toml"), "transport = \"stdio\"\n").unwrap();
    sup.reload().await;

    for (i, text) in [
        "/mcp",
        "/mcp redmine",
        "/mcp restart redmine",
        "/mcp logs redmine",
    ]
    .iter()
    .enumerate()
    {
        g.process_update(&updates::text_message(80 + i as i64, OWNER, OWNER, text))
            .await
            .unwrap();
    }
    drain(&g).await;
    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        out[0].contains("redmine") && out[0].contains("prêt"),
        "{out:?}"
    );
    assert!(
        out[0].contains("casse"),
        "déclaration invalide signalée : {out:?}"
    );
    assert!(out[1].contains("list_issues"), "{out:?}");
    assert!(out[2].contains("redémarré"), "{out:?}");
    assert!(out[3].contains("Aucune ligne"), "{out:?}");
    assert_eq!(fake.opened("redmine"), 2);
}

/// Issue #30 : `/schedules`, 🗑, Confirmer : la planification est supprimée et le
/// message édité sur place.
#[tokio::test]
async fn a_schedule_is_deleted_after_confirmation() {
    let (_d, g, t, _p) = gateway().await;
    let s = &g.daemon.services;
    let sched = s
        .schedules
        .create(
            penelope_workflow::TriggerKind::Cron,
            json!({"expr": "0 9 * * 1"}),
            json!({"type": "notify", "template": "⏰ Revue hebdo"}),
            json!({}),
        )
        .await
        .unwrap();
    g.process_update(&updates::text_message(310, OWNER, OWNER, "/schedules"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let delete = button(&t, "🗑").await;
    g.process_update(&updates::callback(311, OWNER, &delete, 910))
        .await
        .unwrap();
    settle_click(&g).await;
    let confirm_screen = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert!(
        texts(&confirm_screen)
            .join("\n")
            .contains("Supprimer le déclencheur"),
        "écran de confirmation : {confirm_screen:?}"
    );
    assert_eq!(
        s.schedules.list().await.unwrap().len(),
        1,
        "rien de supprimé avant la confirmation"
    );
    let confirm = button(&t, "Confirmer").await;
    g.process_update(&updates::callback(312, OWNER, &confirm, 910))
        .await
        .unwrap();
    settle_click(&g).await;
    assert!(
        !s.schedules
            .list()
            .await
            .unwrap()
            .iter()
            .any(|x| x.id == sched.id),
        "déclencheur supprimé"
    );
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let last = edits.last().unwrap();
    assert_eq!(last["message_id"], 910, "même message redessiné");
    assert!(
        last["text"].as_str().unwrap().contains("Aucun déclencheur"),
        "{last}"
    );
}

/// #124 : `/schedules` dit où livre chaque planification ; `/schedules ici <id>` dans
/// un sujet d'un groupe autorisé l'y déplace, et 📍 fait de même pour la conversation
/// de l'écran ; le titre du groupe et le nom du sujet sont retenus au passage.
#[tokio::test]
async fn a_schedule_is_moved_to_the_topic_it_is_asked_from() {
    let (_d, g, t, _p) = gateway().await;
    let s = &g.daemon.services;
    let chat: i64 = -1_001_234_567_890;
    g.daemon
        .publish_config("test", move |c| {
            c.telegram.allowed_chats = vec![chat];
            Ok(vec!["telegram.allowed_chats".into()])
        })
        .unwrap();
    let sched = s
        .schedules
        .create(
            penelope_workflow::TriggerKind::Cron,
            json!({"expr": "0 9 * * *"}),
            json!({"type": "notify", "template": "🧭 Veille"}),
            json!({}),
        )
        .await
        .unwrap();
    g.process_update(&updates::text_message(410, OWNER, OWNER, "/schedules"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let listed = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        listed.contains("vers : conversation privée (par défaut)"),
        "{listed}"
    );

    let mut u = updates::in_topic(
        updates::text_message(411, chat, OWNER, &format!("/schedules ici {}", sched.id)),
        21,
    );
    u["message"]["chat"] = json!({"id": chat, "type": "supergroup", "title": "Chantiers"});
    u["message"]["reply_to_message"] =
        json!({"message_id": 21, "forum_topic_created": {"name": "Veille IA"}});
    g.process_update(&u).await.unwrap();
    g.flush_outbox().await.unwrap();
    let said = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        said.contains("livrera désormais ici : sujet « Veille IA », groupe « Chantiers »"),
        "{said}"
    );
    let moved = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert_eq!(moved.target["origin"]["chat_id"], chat);
    assert_eq!(moved.target["origin"]["topic_id"], 21);

    // 📍 depuis la conversation privée : retour chez le propriétaire.
    g.process_update(&updates::text_message(412, OWNER, OWNER, "/schedules"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let here = button(&t, "📍").await;
    g.process_update(&updates::callback(413, OWNER, &here, 920))
        .await
        .unwrap();
    settle_click(&g).await;
    let back = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert_eq!(back.target["origin"]["chat_id"], OWNER);
    assert_eq!(back.target["origin"]["topic_id"], Value::Null);
}

/// Issue #30 : `/mcp`, 🔄 sur un serveur : le superviseur le redémarre et le message
/// affiche son état à jour.
#[tokio::test]
async fn an_mcp_server_is_restarted_from_its_menu() {
    use penelope_mcp_host::testing::{FakeConnector, declare, server, tool};
    let (_d, g, t, _p) = gateway().await;
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "redmine",
        server(Arc::new(std::sync::Mutex::new(vec![tool(
            "list_issues",
            json!({"readOnlyHint": true}),
        )]))),
    );
    let sup = penelope_mcp_host::testing::supervisor(g.daemon.services.clone(), fake.clone());
    declare(&sup, "redmine", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());
    let opened = fake.opened("redmine");

    g.process_update(&updates::text_message(320, OWNER, OWNER, "/mcp"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let restart = button(&t, "🔄").await;
    g.process_update(&updates::callback(321, OWNER, &restart, 920))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(fake.opened("redmine"), opened + 1, "redémarrage demandé");
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    let last = edits.last().expect("message redessiné");
    assert_eq!(last["message_id"], 920);
    assert!(
        last["text"].as_str().unwrap().contains("redmine")
            && last["text"].as_str().unwrap().contains("prêt"),
        "{last}"
    );
    // Le clic est acquitté tout de suite, sans attendre la poignée de main du
    // serveur : l'issue est dans la carte redessinée (issue #73).
    let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert!(answers[0]["text"].is_null(), "{answers:?}");
}

/// Issue #30 : le digest renvoie vers un écran précis par lien profond, une fois le bot
/// connu.
#[tokio::test]
async fn the_digest_links_to_screens_once_the_bot_is_known() {
    let (_d, g, _t, _p) = gateway().await;
    let d = &g.daemon;
    assert!(
        penelope_app::helpers::deep_link(&d.services, "approvals")
            .await
            .is_none()
    );
    d.services
        .kv_set(BOT_USERNAME_KEY, "penelope_test_bot")
        .await
        .unwrap();
    assert_eq!(
        penelope_app::helpers::deep_link(&d.services, "approvals")
            .await
            .as_deref(),
        Some("https://t.me/penelope_test_bot?start=approvals")
    );
    d.services
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "native__shell",
            penelope_kernel::risk::RiskClass::Write,
            json!({"arguments": {}}),
            Vec::new(),
            None,
            None,
            false,
        )
        .await
        .unwrap();
    let digest = penelope_dream::digest_text(
        &d.dream(),
        penelope_orchestrator::scheduler::digest_inputs(&d.services).await,
        d.hooks.mcp_supervisor(),
    )
    .await
    .unwrap();
    assert!(
        digest.contains("[ouvrir](https://t.me/penelope_test_bot?start=approvals)"),
        "{digest}"
    );
    assert!(
        markdown_to_html(&digest)
            .contains("<a href=\"https://t.me/penelope_test_bot?start=approvals\">")
    );
}

/// Issue #30 : chaque commande du catalogue appelée sans argument rend un clavier ou un
/// texte sans « Usage : », et aucune ne tombe dans un affichage générique.
#[tokio::test]
async fn every_catalog_command_without_arguments_opens_a_screen() {
    let (_d, g, t, _p) = gateway().await;
    g.daemon
        .publish_config("test", |c| {
            c.upgrade.base_url = "http://127.0.0.1:9".into();
            Ok(vec!["upgrade.base_url".into()])
        })
        .unwrap();
    // Réponses différées (résumé, rêve, audit) ou fichier : pas de message immédiat
    // attendu ; ces traitements sont détachés de la boucle des updates (issue #69).
    let deferred = ["compact", "dream", "export", "audit"];
    let mut update = 400;
    for c in penelope_telegram::commands::all() {
        let before = t.calls_to(tg::SEND_MESSAGE).await.len();
        let edits_before = t.calls_to(tg::EDIT_MESSAGE_TEXT).await.len();
        update += 1;
        g.process_update(&updates::text_message(
            update,
            OWNER,
            OWNER,
            &format!("/{}", c.name),
        ))
        .await
        .unwrap_or_else(|e| panic!("/{} : {e}", c.name));
        g.flush_outbox().await.unwrap();
        let mut new = t.calls_to(tg::SEND_MESSAGE).await.split_off(before);
        new.extend(
            t.calls_to(tg::EDIT_MESSAGE_TEXT)
                .await
                .split_off(edits_before),
        );
        if new.is_empty() {
            assert!(deferred.contains(&c.name), "/{} n'a rien répondu", c.name);
            continue;
        }
        for call in &new {
            let text = call["text"].as_str().unwrap_or_default();
            assert!(!text.contains("Usage"), "/{} : {text}", c.name);
            assert!(
                !text.contains("Commande inconnue") && !text.contains("pas encore branchée"),
                "/{} : {text}",
                c.name
            );
        }
    }
    // Lien profond : `/start <charge>` ouvre l'écran visé.
    let before = t.calls_to(tg::SEND_MESSAGE).await.len();
    g.process_update(&updates::text_message(
        499,
        OWNER,
        OWNER,
        "/start runs_stuck",
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let opened = texts(&t.calls_to(tg::SEND_MESSAGE).await.split_off(before)).join("\n");
    assert!(opened.contains("Runs en pause ou bloqués"), "{opened}");
}

#[tokio::test]
async fn schedules_are_listed_paused_and_run_from_telegram() {
    let (_d, g, t, _p) = gateway().await;
    let sched = g
        .daemon
        .services
        .schedules
        .create(
            penelope_workflow::TriggerKind::Cron,
            json!({"expr": "0 9 * * 1"}),
            json!({"type": "notify", "template": "⏰ Revue hebdo"}),
            json!({}),
        )
        .await
        .unwrap();
    let id = sched.id.clone();
    for (i, text) in [
        "/schedules".to_string(),
        format!("/schedules pause {id}"),
        format!("/schedules run {id}"),
        "/schedules".to_string(),
    ]
    .iter()
    .enumerate()
    {
        g.process_update(&updates::text_message(90 + i as i64, OWNER, OWNER, text))
            .await
            .unwrap();
    }
    drain(&g).await;
    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        out[0].contains("0 9 * * 1") && out[0].contains("Revue hebdo"),
        "{out:?}"
    );
    assert!(out[1].contains("en pause"), "{out:?}");
    // Le tir immédiat envoie le rappel lui-même, puis la confirmation.
    assert!(
        out.iter()
            .any(|m| m.contains("⏰ Revue hebdo") && !m.contains("cron")),
        "{out:?}"
    );
    assert!(out.iter().any(|m| m.contains("déclenché")), "{out:?}");
    assert!(out.last().unwrap().contains("⏸"), "{out:?}");
}

/// Issue #39 : une veille planifiée depuis une conversation répond encore après `/new`,
/// et un échec arrive au propriétaire avec « Relancer maintenant ».
#[tokio::test]
async fn a_scheduled_prompt_answers_after_new_and_warns_on_failure() {
    let (_d, g, t, p) = gateway().await;
    let d = g.daemon.clone();
    let s = &d.services;
    d.services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let created_in = d.chat_session_for(&chat).await.unwrap();
    let sched = penelope_orchestrator::scheduler::create(
        s,
        penelope_workflow::TriggerKind::Cron,
        json!({"expr": "30 8 * * *"}),
        json!({"type": "prompt", "label": "Veille agents IA", "prompt": "Fais la veille du jour",
               "origin_session": created_in, "origin": chat.to_value()}),
        json!({}),
    )
    .await
    .unwrap();
    let id = sched["id"].as_str().unwrap().to_string();

    // La conversation d'origine est remplacée par une nouvelle.
    g.process_update(&updates::text_message(150, OWNER, OWNER, "/new"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();

    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Veille du jour : deux annonces à lire.");
    penelope_orchestrator::scheduler::run_now(
        &penelope_daemon::workflow::context_of(&d),
        &d.hooks.scheduler(),
        &id,
    )
    .await
    .unwrap();
    drain(&g).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|m| m.contains("deux annonces")),
        "réponse dans le chat : {sent:?}"
    );
    let after = s.schedules.get(&id).await.unwrap().unwrap();
    assert_eq!(after.runs, 1, "{after:?}");

    // Échec du modèle : alerte avec ses boutons, erreur gardée.
    t.clear().await;
    p.reply(r#"{"complexity":"low"}"#);
    p.push(penelope_llm::mock::Scripted::Error(
        penelope_llm::types::LlmErrorKind::Other,
        "fournisseur indisponible".into(),
    ));
    penelope_orchestrator::scheduler::run_now(
        &penelope_daemon::workflow::context_of(&d),
        &d.hooks.scheduler(),
        &id,
    )
    .await
    .unwrap();
    drain(&g).await;
    let calls = t.calls_to(tg::SEND_MESSAGE).await;
    let alert = calls
        .iter()
        .find(|c| {
            c["text"].as_str().is_some_and(|x| {
                x.contains("La planification « Veille agents IA » n'a pas pu s'exécuter")
            })
        })
        .unwrap_or_else(|| panic!("alerte : {:?}", texts(&calls)));
    let labels: Vec<String> = inline_buttons(alert).into_iter().map(|(l, _)| l).collect();
    assert_eq!(
        labels,
        vec!["🔁 Relancer maintenant", "📅 Voir la planification"]
    );
    let failed = s.schedules.get(&id).await.unwrap().unwrap();
    assert_eq!(failed.runs, 1);
    assert!(failed.last_error.is_some());

    // « Relancer maintenant » redéclenche la planification.
    let rerun = button(&t, "🔁 Relancer maintenant").await;
    g.process_update(&updates::callback(151, OWNER, &rerun, 1500))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(s.turns.pending_count().await.unwrap(), 1, "relancée");
}

/// T36 : l'inventaire `commands` de `self_status` vient du canal branché ; l'exécuteur
/// ne connaît plus la liste des commandes Telegram.
#[tokio::test]
async fn the_channel_lists_the_telegram_commands() {
    let (_dir, g, _t, _p) = gateway().await;
    let commands = g.daemon.services.channel.commands();
    assert!(
        commands.iter().any(|c| c["command"] == "/run"),
        "{commands:?}"
    );
    assert_eq!(commands.len(), penelope_telegram::commands::all().len());
}
