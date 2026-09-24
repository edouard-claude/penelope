use super::*;

/// #165 : la session technique d'un run n'a pas de sujet ; la destination de la
/// carte doit survivre au clic, même si le callback ne répète pas le thread id.
#[tokio::test]
#[allow(clippy::too_many_lines)] // gel 0.17 : scénario de test bout en bout
async fn workflow_approval_confirmation_stays_in_the_cards_topic() {
    let (dir, g, t, _p) = gateway().await;
    let s = &g.daemon.services;
    let group = -100_165;
    let topic = 1558;
    let origin = Origin::Telegram {
        chat_id: group,
        topic_id: Some(topic),
        message_id: None,
    };
    let run = crate::workflow::start_run(
        &g.daemon,
        "build-verify",
        json!({"objectif": "corriger le dépôt"}),
        &origin,
        None,
        0,
    )
    .await
    .unwrap();
    let session = s.sessions.get(&run.session_id).await.unwrap().unwrap();
    assert_eq!((session.tg_chat_id, session.tg_topic_id), (None, None));
    let approval = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "fs_write",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "fs_write", "arguments": {"path": "note.txt", "content": "ok"}}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&run.session_id),
            Some(&run.id),
            false,
        )
        .await
        .unwrap();
    g.send_approval_card(group, Some(topic), &approval)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    assert_eq!(card["message_thread_id"], topic);
    let token = inline_buttons(&card)
        .into_iter()
        .find(|(label, _)| label.contains("Autoriser"))
        .unwrap()
        .1;
    // Nouveau daemon sur le même store : la destination de la carte et l'action
    // survivent ensemble au redémarrage.
    let reopened = Arc::new(
        crate::runtime::Services::for_tests(
            dir.path().to_path_buf(),
            Arc::new(TestClock::default()),
        )
        .await
        .unwrap(),
    );
    let d2 = Arc::new(Daemon::from_services(reopened));
    let t2 = MockTransport::new();
    let g2 = TelegramGateway::with_transport(d2, t2.clone());
    g2.callback("cb165", &token, group, None, 1001, OWNER)
        .await
        .unwrap();
    g2.flush_outbox().await.unwrap();
    let replies = t2.calls_to(tg::SEND_MESSAGE).await;
    let confirmation = replies
        .iter()
        .find(|m| m["text"].as_str().is_some_and(|x| x.contains("Autoriser")))
        .expect("confirmation du clic");
    assert_eq!(confirmation["chat_id"], group);
    assert_eq!(confirmation["message_thread_id"], topic);
    g2.daemon
        .services
        .kv_set(
            &format!("tg.approval_destination.{}", approval.id.as_str()),
            "",
        )
        .await
        .unwrap();
    assert_eq!(
        g2.recorded_approval_destination(&approval).await,
        Some((group, Some(topic))),
        "sans destination de carte, l'origine du run prend le relais"
    );

    // Un second run dans le même groupe garde son propre sujet, y compris pour
    // Refuser puis « Déjà tranché » sur l'autre bouton de la carte.
    let second_topic = 1559;
    let second_origin = Origin::Telegram {
        chat_id: group,
        topic_id: Some(second_topic),
        message_id: None,
    };
    let second = crate::workflow::start_run(
        &g2.daemon,
        "build-verify",
        json!({"objectif": "autre correctif"}),
        &second_origin,
        None,
        0,
    )
    .await
    .unwrap();
    let second_approval = g2
        .daemon
        .services
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "fs_write",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "fs_write", "arguments": {"path": "other.txt", "content": "x"}}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&second.session_id),
            Some(&second.id),
            false,
        )
        .await
        .unwrap();
    t2.clear().await;
    g2.send_approval_card(group, Some(second_topic), &second_approval)
        .await
        .unwrap();
    g2.flush_outbox().await.unwrap();
    let second_card = t2.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let buttons = inline_buttons(&second_card);
    let deny = buttons
        .iter()
        .find(|(label, _)| label.contains("Refuser"))
        .unwrap()
        .1
        .clone();
    let approve = buttons
        .iter()
        .find(|(label, _)| label.contains("Autoriser"))
        .unwrap()
        .1
        .clone();
    t2.clear().await;
    g2.callback("cb166", &deny, group, None, 1002, OWNER)
        .await
        .unwrap();
    g2.callback("cb167", &approve, group, None, 1002, OWNER)
        .await
        .unwrap();
    g2.flush_outbox().await.unwrap();
    let replies = t2.calls_to(tg::SEND_MESSAGE).await;
    assert!(
        replies
            .iter()
            .any(|m| m["text"].as_str().is_some_and(|x| x.contains("Refusé")))
    );
    assert!(replies.iter().any(|m| {
        m["text"]
            .as_str()
            .is_some_and(|x| x.contains("Déjà tranché"))
    }));
    assert!(
        replies
            .iter()
            .all(|m| m["message_thread_id"] == second_topic),
        "{replies:?}"
    );
    assert_eq!(
        g2.daemon
            .services
            .approvals
            .get(second_approval.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .state,
        ApprovalState::Denied
    );

    let reason_approval = g2
        .daemon
        .services
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "fs_write",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "fs_write", "arguments": {"path": "reason.txt", "content": "x"}}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&second.session_id),
            Some(&second.id),
            false,
        )
        .await
        .unwrap();
    t2.clear().await;
    g2.send_approval_card(group, Some(second_topic), &reason_approval)
        .await
        .unwrap();
    g2.flush_outbox().await.unwrap();
    let reason_card = t2.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let reason = inline_buttons(&reason_card)
        .into_iter()
        .find(|(label, _)| label.contains("raison"))
        .unwrap()
        .1;
    g2.callback("cb-reason", &reason, group, None, 1003, OWNER)
        .await
        .unwrap();
    assert_eq!(
        g2.daemon
            .services
            .kv_get(&approval_reason_key(group, Some(second_topic)))
            .await
            .unwrap(),
        Some(reason_approval.id.as_str().to_string())
    );
    assert_eq!(
        g2.daemon
            .services
            .kv_get(&approval_reason_key(group, Some(topic)))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn workflow_budget_and_destructive_cards_keep_the_topic() {
    let (_dir, g, t, _p) = gateway().await;
    let group = -100_166;
    let topic = 1660;
    let run = crate::workflow::start_run(
        &g.daemon,
        "build-verify",
        json!({"objectif": "vérifier"}),
        &Origin::Telegram {
            chat_id: group,
            topic_id: Some(topic),
            message_id: None,
        },
        None,
        0,
    )
    .await
    .unwrap();
    let s = &g.daemon.services;
    let budget = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::BudgetExceeded,
            "budget",
            penelope_kernel::risk::RiskClass::Write,
            json!({"budget": true, "scope": "run", "run_id": run.id, "spent": 21.0, "limit": 20.0}),
            vec!["Arrêter".into()],
            Some(&run.session_id),
            Some(&run.id),
            false,
        )
        .await
        .unwrap();
    g.send_approval_card(group, Some(topic), &budget)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let stop = inline_buttons(&card)
        .into_iter()
        .find(|(label, _)| label.contains("Arrêter"))
        .unwrap()
        .1;
    t.clear().await;
    g.callback("cb-budget", &stop, group, None, 2001, OWNER)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    assert!(
        t.calls_to(tg::SEND_MESSAGE)
            .await
            .iter()
            .all(|m| m["message_thread_id"] == topic)
    );

    let destructive = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Destructive,
            json!({"tool": "shell_exec", "arguments": {"command": "rm note.txt"}, "double": true}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&run.session_id),
            Some(&run.id),
            false,
        )
        .await
        .unwrap();
    t.clear().await;
    g.send_approval_card(group, Some(topic), &destructive)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let approve = inline_buttons(&card)
        .into_iter()
        .find(|(label, _)| label.contains("Autoriser"))
        .unwrap()
        .1;
    t.clear().await;
    g.callback("cb-double", &approve, group, None, 2002, OWNER)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let next = t.calls_to(tg::SEND_MESSAGE).await;
    assert!(
        next.iter().any(|m| m["text"]
            .as_str()
            .is_some_and(|x| x.contains("Seconde confirmation"))),
        "{next:?}"
    );
    assert!(
        next.iter().all(|m| m["message_thread_id"] == topic),
        "{next:?}"
    );

    // Une carte d'effet incertain renvoyée après redémarrage lit l'origine du
    // run, car sa session technique ne porte ni chat ni sujet.
    let uncertain = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::EffectUnknown,
            "git_push",
            penelope_kernel::risk::RiskClass::External,
            json!({"request": {"branch": "fix/165"}}),
            vec!["C'est fait".into(), "Relancer".into(), "Ignorer".into()],
            Some(&run.session_id),
            Some(&run.id),
            false,
        )
        .await
        .unwrap();
    t.clear().await;
    assert_eq!(g.announce_uncertain_effects().await.unwrap(), 1);
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    assert_eq!(card["message_thread_id"], topic);
    assert!(card["text"].as_str().unwrap().contains("git_push"));
    assert_eq!(
        s.approvals
            .get(uncertain.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .state,
        ApprovalState::Pending
    );
}

/// Issue #186 : le menu prépare un plan, sans lancer ni ouvrir le formulaire.
#[tokio::test]
async fn a_workflow_menu_starts_a_plan_conversation() {
    let (_d, g, t, _p) = gateway().await;
    let s = &g.daemon.services;
    g.process_update(&updates::text_message(300, OWNER, OWNER, "/wf"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let menu = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        menu.contains("build-verify") && !menu.contains("Usage"),
        "{menu}"
    );
    let launch = button(&t, "▶️ Construire puis vérifier").await;
    g.process_update(&updates::callback(301, OWNER, &launch, 900))
        .await
        .unwrap();
    settle_click(&g).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(sent.contains("prépare le plan"), "{sent}");
    assert!(
        s.runs.list(None, 5).await.unwrap().is_empty(),
        "rien lancé avant vas-y"
    );
}

/// Issue #35 : `/run ticket-to-deploy` sans paramètres ne force aucun formulaire ; le
/// modèle reçoit la demande et répond par une question sur le ticket.
#[tokio::test]
async fn run_without_parameters_turns_into_a_conversation() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Sur quel ticket ? Voici tes tickets ouverts : #7647, #7650.");
    g.process_update(&updates::text_message(
        130,
        OWNER,
        OWNER,
        "/run ticket-to-deploy",
    ))
    .await
    .unwrap();
    drain(&g).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        !sent.iter().any(|m| m.starts_with("📝")),
        "aucun formulaire : {sent:?}"
    );
    assert!(
        sent.iter().any(|m| m.contains("Sur quel ticket ?")),
        "{sent:?}"
    );
    let asked = p
        .requests()
        .last()
        .unwrap()
        .messages
        .iter()
        .map(|m| m.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        asked.contains("ticket_url") && asked.contains("workflow_plan"),
        "{asked}"
    );
    assert!(
        g.daemon
            .services
            .runs
            .list(None, 5)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Issue #186 : l'ancienne carte de lancement ne contourne plus le gate du plan.
#[tokio::test]
async fn a_workflow_launch_card_cannot_bypass_the_plan_gate() {
    let (_d, g, t, p) = gateway().await;
    let d = g.daemon.clone();
    d.hooks
        .set_orchestrator(Arc::new(crate::workflow::WorkflowOrchestrator {
            daemon: d.clone(),
        }));
    d.services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    let brief = "Ticket #7647 : export CSV vide depuis la 2.3. Piste : filtre de dates.";
    p.reply(r#"{"complexity":"medium"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "w1".into(),
            name: "workflow_start".into(),
            arguments: json!({
                "id": "ticket-to-deploy",
                "params": {"ticket_id": "7647", "ticket_url": "https://tracker.example/issues/7647"},
                "brief": brief
            }),
        }],
    ));
    g.process_update(&updates::text_message(
        140,
        OWNER,
        OWNER,
        "ok, on le traite",
    ))
    .await
    .unwrap();
    drain(&g).await;
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let text = card["text"].as_str().unwrap();
    assert!(
        text.contains("Lancer") && text.contains("7647") && text.contains("filtre de dates"),
        "{text}"
    );
    let labels: Vec<String> = inline_buttons(&card).into_iter().map(|(l, _)| l).collect();
    assert_eq!(labels, vec!["▶️ Lancer", "⏸ Pas encore"]);

    p.reply("Je prépare d'abord un plan.");
    let launch = button(&t, "▶️ Lancer").await;
    g.process_update(&updates::callback(141, OWNER, &launch, 1400))
        .await
        .unwrap();
    settle_click(&g).await;
    drain(&g).await;
    assert!(d.services.runs.list(None, 5).await.unwrap().is_empty());
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.iter().any(|m| m.contains("plan")), "{sent:?}");
}

#[tokio::test]
async fn plan_go_button_persists_approval_without_starting_a_run() {
    use penelope_workflow::plan::{Phase, Plan, PlanDraft, PlanGate, PlanStep, PlanStore};
    let (_d, g, t, _p) = gateway().await;
    let plans = PlanStore::new(g.daemon.services.store.clone());
    let draft = PlanDraft {
        workflow_id: "build-verify".into(),
        params: json!({"objectif":"réparer le build"}),
        brief: Some("Bug de build".into()),
        plan: Plan::new(
            "Réparer le build",
            vec![PlanStep::new(Phase::Tests, "Reproduire l'échec")],
        )
        .unwrap(),
    };
    plans.save("plan-session", &draft).await.unwrap();
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: Some(21),
        message_id: Some(400),
    };
    g.send_plan_card(&origin, "plan-session", &draft)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let go = button(&t, "Vas-y").await;
    g.process_update(&updates::callback(401, OWNER, &go, 1401))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(
        plans
            .get("plan-session")
            .await
            .unwrap()
            .unwrap()
            .plan
            .gate(),
        PlanGate::Ready
    );
    assert!(
        g.daemon
            .services
            .runs
            .list(None, 5)
            .await
            .unwrap()
            .is_empty()
    );
}
