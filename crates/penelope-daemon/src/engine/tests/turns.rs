use super::*;

/// #161 : la projection du modèle et la base gardent trois messages `user`
/// distincts, avec les dates de réception et non la date de réclamation.
#[tokio::test]
async fn claimed_messages_keep_separate_user_entries_and_arrival_times() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::default();
    let services = Arc::new(
        crate::runtime::Services::for_tests(dir.path().to_path_buf(), Arc::new(clock.clone()))
            .await
            .unwrap(),
    );
    let daemon = Arc::new(Daemon::from_services(services.clone()));
    let provider = Arc::new(MockProvider::new());
    provider.reply("Une réponse.");
    daemon.set_provider_override(provider.clone());
    let sid = daemon.chat_session_for(&Origin::Cli).await.unwrap();
    let mut arrivals = Vec::new();
    for text in ["premier", "complément", "correction"] {
        arrivals.push(clock.now_rfc3339());
        daemon
            .enqueue_message(&sid, text, &Origin::Cli, None)
            .await
            .unwrap();
        clock.advance_ms(1000);
    }
    let turn = claim(&daemon).await;
    assert_eq!(turn.merged_messages.len(), 2);
    assert!(matches!(
        daemon.run_turn(&turn).await,
        TurnOutcome::Answered { .. }
    ));
    let request = provider.requests().pop().unwrap();
    let user: Vec<String> = request
        .messages
        .iter()
        .filter(|m| m.role == penelope_llm::types::Role::User)
        .map(|m| m.text())
        .collect();
    assert_eq!(user.len(), 3, "{user:?}");
    assert!(user[0].ends_with("premier"), "{user:?}");
    assert_eq!(user[1], "complément");
    assert!(user[2].ends_with("correction"), "{user:?}");
    let times: Vec<String> = services
        .store
        .read({
            let sid = sid.clone();
            move |c| {
                let mut statement = c.prepare(
                    "SELECT ts FROM messages WHERE session_id=?1 AND role='user' ORDER BY seq",
                )?;
                let rows = statement.query_map([sid], |r| r.get(0))?;
                Ok(rows.collect::<penelope_store::rusqlite::Result<Vec<_>>>()?)
            }
        })
        .await
        .unwrap();
    assert_eq!(times, arrivals);
    let merge_events: i64 = services
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM events WHERE kind='turn.merged'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(merge_events, 1);
}

/// #161 : le message reçu pendant un outil rejoint la prochaine projection,
/// après le résultat, et un second appel ne le rejoue pas.
#[tokio::test]
async fn a_running_turn_absorbs_a_new_message_before_the_next_model_call() {
    let (_dir, daemon, _provider) = daemon().await;
    let sid = daemon.chat_session_for(&Origin::Cli).await.unwrap();
    daemon
        .enqueue_message(&sid, "initial", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&daemon).await;
    let tiers = crate::conversation::build_tiers(&daemon.services, "initial", &[], None).await;
    let conv = SessionConversation::new(
        daemon.services.clone(),
        &sid,
        "openrouter:mock/model",
        tiers,
        0,
    )
    .with_merge_turn(turn.clone(), None, CancelToken::new());
    conv.record(&ChatMessage::user("initial"), false)
        .await
        .unwrap();
    conv.record(
        &ChatMessage::tool_result("call-1", "test", "résultat"),
        false,
    )
    .await
    .unwrap();
    daemon
        .enqueue_message(&sid, "nouvelle consigne", &Origin::Cli, None)
        .await
        .unwrap();
    let first = conv.request_messages().await.unwrap();
    let second = conv.request_messages().await.unwrap();
    for request in [&first, &second] {
        let text: Vec<_> = request.iter().map(ChatMessage::text).collect();
        let tool = text.iter().position(|t| t.contains("résultat")).unwrap();
        let user = text
            .iter()
            .position(|t| t.contains("nouvelle consigne"))
            .unwrap();
        assert!(tool < user, "{text:?}");
        assert_eq!(
            text.iter()
                .filter(|t| t.contains("nouvelle consigne"))
                .count(),
            1
        );
        assert!(
            text.iter().any(|t| t.contains("nouveau message")),
            "{text:?}"
        );
    }
}

#[tokio::test]
async fn a_message_is_answered_and_the_transcript_persists() {
    let (_dir, d, p) = daemon().await;
    // Classifieur puis réponse (message non trivial : un « bonjour » seul ne passe
    // plus par le classifieur, issue #74).
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("Bonjour ! Que puis-je faire ?");
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.enqueue_message(
        &sid,
        "bonjour, où en est la facturation ?",
        &Origin::Cli,
        None,
    )
    .await
    .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    match out {
        TurnOutcome::Answered { text, .. } => assert!(text.contains("Bonjour")),
        other => panic!("{other:?}"),
    }
    // Le verrou de session tient jusqu'à la fin du tour : c'est le runner qui le rend.
    d.services.turns.complete(&turn).await.unwrap();

    let history = d.services.context.history.load(&sid, 0).await.unwrap();
    let texts: Vec<String> = history.iter().map(|e| e.message.text()).collect();
    assert_eq!(
        texts,
        vec![
            "bonjour, où en est la facturation ?",
            "Bonjour ! Que puis-je faire ?"
        ]
    );

    // Le classifieur a choisi `fast` pour ce message, sans le rendre collant : un
    // message simple ne doit pas enfermer la session sur le petit modèle.
    let cfg = d.services.config.config();
    let fast = cfg.alias_model("fast").unwrap().to_string();
    let main = cfg.alias_model("main").unwrap().to_string();
    assert_eq!(p.requests()[1].model, fast);
    let sess = d.services.sessions.require(&sid).await.unwrap();
    assert_eq!(sess.model_alias, None);

    // Les deux appels du tour (classifieur et réponse) sont attribués à la requête.
    let by_turn = d
        .services
        .budget
        .report("turn", Some(&sid), None, 10)
        .await
        .unwrap();
    assert_eq!(by_turn.len(), 1);
    assert_eq!(by_turn[0].key, turn.id.to_string());
    assert_eq!(by_turn[0].calls, 2);
    assert_eq!(
        by_turn[0].label.as_deref(),
        Some("bonjour, où en est la facturation ?")
    );
    let by_role = d
        .services
        .budget
        .report("role", Some(&sid), None, 10)
        .await
        .unwrap();
    let mut roles: Vec<&str> = by_role.iter().map(|r| r.key.as_str()).collect();
    roles.sort_unstable();
    assert_eq!(roles, vec!["chat", "classifier"]);

    // Le second message est reclassé ; `medium` part sur `main`, qui, lui, colle.
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Toujours là.");
    d.enqueue_message(&sid, "tu es là ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    let last = p.requests().last().unwrap().clone();
    assert_eq!(last.model, main);
    assert_eq!(last.session_id.as_deref(), Some(sid.as_str()));
    let seen: Vec<String> = last.messages.iter().map(|m| m.text()).collect();
    assert!(
        seen.iter()
            .any(|t| t.starts_with("<contexte>")
                && t.ends_with("bonjour, où en est la facturation ?")),
        "l'ancien message garde le contexte de son tour : le préfixe ne bouge pas"
    );
    assert!(
        seen.iter()
            .any(|t| t.starts_with("<contexte>") && t.ends_with("tu es là ?")),
        "le dernier porte son propre contexte volatil en tête"
    );
    let sess = d.services.sessions.require(&sid).await.unwrap();
    assert_eq!(sess.model_alias.as_deref(), Some("main"));
    assert_eq!(p.call_count(), 4);

    // Troisième message : `main` est collant, pas de nouvel appel au classifieur.
    p.reply("Encore là.");
    d.enqueue_message(&sid, "et maintenant ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    d.run_turn(&turn).await;
    assert_eq!(p.call_count(), 5, "un seul appel de plus : la réponse");
}

/// #104 : premier tour d'une session en configuration d'exemple : au plus 20
/// définitions d'outils et moins de 3 000 tokens de schémas ; les outils rares sont
/// nommés dans le message système. Décrit par `tool_describe`, `schedule_create`
/// rejoint la liste dès le tour suivant.
#[tokio::test]
async fn a_turn_offers_the_core_then_what_the_session_discovered() {
    let (_dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    let say = |text: &'static str| {
        let d = d.clone();
        let sid = sid.clone();
        async move {
            d.enqueue_message(&sid, text, &Origin::Cli, None)
                .await
                .unwrap();
            let turn = claim(&d).await;
            d.run_turn(&turn).await;
            d.services.turns.complete(&turn).await.unwrap();
        }
    };
    let offers =
        |r: &penelope_llm::types::ChatRequest, tool: &str| r.tools.iter().any(|t| t.name == tool);

    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "tool_describe".into(),
            arguments: json!({"names": ["schedule_create"]}),
        }],
    ));
    p.reply("Je peux le planifier.");
    say("Bonjour, quel jour sommes-nous ? Et rappelle-moi d'appeler Anne demain.").await;
    let first = p.requests()[0].clone();
    let est = penelope_llm::TokenEstimator::new();
    let schemas: u64 = first
        .tools
        .iter()
        .map(|t| est.tool_tokens(&first.model, t))
        .sum();
    assert!(first.tools.len() <= 20, "{} outils", first.tools.len());
    assert!(schemas < 3_000, "{schemas} tokens de schémas");
    assert!(offers(&first, "shell_exec") && offers(&first, "tool_search"));
    assert!(!offers(&first, "schedule_create"));
    assert!(first.messages[0].text().contains("`schedule_create`"));

    p.reply("Avec plaisir.");
    say("Merci.").await;
    let next = p.requests().last().unwrap().clone();
    assert!(
        offers(&next, "schedule_create"),
        "découvert au tour précédent"
    );
    assert!(next.tools.len() <= 21);
}

/// #105 : un souvenir servi par le rappel automatique compte comme rappelé ; il ne
/// compte comme utile que si la réponse le reprend.
#[tokio::test]
async fn a_recalled_memory_counts_as_useful_only_when_the_answer_uses_it() {
    let (_dir, d, p) = daemon().await;
    let s = d.services.clone();
    let vault = crate::conversation::vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("memoire.md"),
        "# Mémoire de fond\n\n## Clients\n\
             - Le client Martin est basé à Grenoble <!-- depuis: 2026-06-01 --> ^01MARTIN\n",
    )
    .unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();
    // Rien d'office dans l'instantané : le souvenir ne vient que par le rappel.
    d.publish_config("test", |c| {
        c.memory.core_budget_tokens = 0;
        c.memory.project_budget_tokens = 0;
        Ok(vec!["memory.core_budget_tokens".into()])
    })
    .unwrap();
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    let say = |text: &'static str| {
        let d = d.clone();
        let sid = sid.clone();
        async move {
            d.enqueue_message(&sid, text, &Origin::Cli, None)
                .await
                .unwrap();
            let turn = claim(&d).await;
            d.run_turn(&turn).await;
            d.services.turns.complete(&turn).await.unwrap();
        }
    };

    p.reply("Je n'ai pas cette information.");
    say("Où est basé le client Martin ?").await;
    let sig = s.memory.signals_of("01MARTIN").await.unwrap();
    assert_eq!((sig.recalls, sig.useful_recalls), (1, 0), "{sig:?}");

    p.reply("Martin est à Grenoble.");
    say("Rappelle-moi où est basé le client Martin").await;
    let sig = s.memory.signals_of("01MARTIN").await.unwrap();
    assert_eq!((sig.recalls, sig.useful_recalls), (2, 1), "{sig:?}");
}

/// #110 : un appel invalide rejoué à l'identique déclenche toujours la garde de
/// boucle, même avec les paramètres attendus dans l'erreur ; et une approbation par
/// `tool_call` porte sur les arguments de l'outil visé : « Toujours » se borne à la
/// famille de commandes, jamais au shell entier.
#[tokio::test]
async fn explained_errors_keep_the_loop_guard_and_tool_call_rules_stay_bounded() {
    let (_dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    // Trois appels invalides : le deuxième avertit, le troisième arrête (#117).
    for i in 0..3 {
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: format!("c{i}"),
                name: "fs_read".into(),
                arguments: json!({}),
            }],
        ));
    }
    p.reply("Je n'arrive pas à lire le fichier.");
    d.enqueue_message(&sid, "lis le fichier de config", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    assert!(matches!(out, TurnOutcome::LoopAborted { .. }), "{out:?}");
    d.services.turns.complete(&turn).await.unwrap();
    let history = d.services.context.history.load(&sid, 0).await.unwrap();
    assert!(
        history
            .iter()
            .any(|e| e.message.text().contains("`path` (string, requis)")),
        "l'erreur porte les paramètres attendus"
    );

    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "t1".into(),
            name: "tool_call".into(),
            arguments: json!({"name": "shell_exec", "args": {"command": "cargo test -p x"}}),
        }],
    ));
    d.enqueue_message(&sid, "lance les tests du crate x", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let id = match d.run_turn(&turn).await {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    let a = d.services.approvals.get(&id).await.unwrap().unwrap();
    assert_eq!(a.subject, "shell_exec");
    assert_eq!(a.payload["arguments"]["command"], "cargo test -p x");
    crate::agent::decide_approval(
        &d.services,
        &id,
        &penelope_hitl::Decision::approve_always("cli"),
    )
    .await
    .unwrap();
    let rules = d.services.policies.active_rules().await.unwrap();
    let rule = rules
        .iter()
        .find(|r| r.tool.as_deref() == Some("shell_exec"))
        .expect("règle");
    assert_eq!(
        rule.arg_match.as_ref().unwrap()["command"][penelope_hitl::policy::CMD_PREFIX_OP],
        "cargo test",
        "famille de commandes, pas le shell entier"
    );
}

/// #142 (décision 2) : l'abonnement ChatGPT ne sert que les tours ouverts par le
/// propriétaire. Une planification, qui tourne sans lui, se replie sur OpenRouter,
/// sans carte ni bruit, et laisse un événement.
#[tokio::test]
async fn the_subscription_only_serves_the_owner() {
    let (_dir, d, p) = daemon().await;
    d.publish_config("test", |c| {
        c.models
            .aliases
            .insert("main".into(), "codex:gpt-6-astra".into());
        c.models
            .aliases
            .insert("fast".into(), "openrouter:vendeur/rapide".into());
        c.models
            .routing
            .fallback
            .insert("main".into(), vec!["fast".into()]);
        c.providers.codex.enabled = true;
        Ok(vec!["models.aliases.main".into()])
    })
    .unwrap();

    // Le propriétaire parle : son abonnement répond.
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    p.reply("Bonjour.");
    d.enqueue_message(&sid, "salut", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(
        p.requests().last().expect("appel").model,
        "codex:gpt-6-astra"
    );

    // Une planification vise le même alias : elle repasse par OpenRouter.
    let origin = Origin::Internal {
        source: "schedule".into(),
    };
    let sid = d.chat_session_for(&origin).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    p.reply("Rapport prêt.");
    d.enqueue_message(&sid, "le rapport", &origin, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(
        p.requests().last().expect("appel").model,
        "openrouter:vendeur/rapide",
        "la planification ne passe pas par l'abonnement"
    );
    let events = d.services.events.range(0, 500).await.unwrap();
    let fallback = events
        .iter()
        .find(|e| e.kind == "llm.codex_scope_fallback")
        .expect("l'événement trace le repli");
    assert_eq!(fallback.payload["work"], "schedule");
    assert_eq!(fallback.payload["replaced"], true);
    // Aucune carte : le repli est silencieux.
    assert!(d.services.approvals.pending(10).await.unwrap().is_empty());
}

/// #142 (décision 3) : la jauge du plan alerte une fois par fenêtre, et distingue un
/// quota d'une panne.
#[tokio::test]
async fn the_plan_gauge_alerts_once_per_window() {
    use penelope_llm::codex::QuotaWindow;
    let (_dir, d, _p) = daemon().await;
    d.publish_config("test", |c| {
        c.providers.codex.enabled = true;
        Ok(vec!["providers.codex.enabled".into()])
    })
    .unwrap();
    let s = &d.services;
    let quota = |used: f64, reset_at: i64| penelope_llm::Quota {
        primary: Some(QuotaWindow {
            used_percent: used,
            window_minutes: 300,
            reset_at,
        }),
        plan_type: "pro".into(),
        ..Default::default()
    };

    // Sous le seuil : rien.
    crate::codex_quota::store(s, &quota(40.0, 1_790_000_000))
        .await
        .unwrap();
    assert!(crate::codex_quota::check_alert(&d).await.unwrap().is_none());

    // Au-delà : une alerte, une seule.
    crate::codex_quota::store(s, &quota(81.0, 1_790_000_000))
        .await
        .unwrap();
    let first = crate::codex_quota::check_alert(&d)
        .await
        .unwrap()
        .expect("alerte");
    assert!(first.contains("81 %"), "{first}");
    assert!(first.contains("pas une panne"), "{first}");
    crate::codex_quota::store(s, &quota(90.0, 1_790_000_000))
        .await
        .unwrap();
    assert!(
        crate::codex_quota::check_alert(&d).await.unwrap().is_none(),
        "une seule alerte par fenêtre"
    );

    // Fenêtre suivante : l'alerte reprend son droit.
    crate::codex_quota::store(s, &quota(85.0, 1_790_018_000))
        .await
        .unwrap();
    assert!(crate::codex_quota::check_alert(&d).await.unwrap().is_some());
}

#[tokio::test]
async fn penelope_reports_her_own_model_and_state() {
    let (_dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "self_status".into(),
            arguments: json!({}),
        }],
    ));
    p.reply("Je tourne sur le modèle de l'alias main.");
    d.enqueue_message(&sid, "C'est quel LLM ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    match d.run_turn(&turn).await {
        TurnOutcome::Answered { .. } => {}
        other => panic!("{other:?}"),
    }
    let main = d
        .services
        .config
        .config()
        .alias_model("main")
        .unwrap()
        .to_string();
    let history = d.services.context.history.load(&sid, 0).await.unwrap();
    let report = history
        .iter()
        .find(|e| e.message.name.as_deref() == Some("self_status"))
        .map(|e| e.message.text())
        .expect("résultat de self_status");
    let v: Value = serde_json::from_str(&report).unwrap();
    assert_eq!(v["this_turn"]["alias"], "main");
    assert_eq!(v["this_turn"]["model"]["id"], main);
    assert!(v["machine"]["arch"].is_string(), "{v}");
    assert!(v["penelope"]["uptime_s"].is_number(), "{v}");
    // Le préfixe dit au modèle que son état n'est pas secret.
    let first = p.requests()[0].clone();
    assert!(first.messages[0].text().contains("self_status"));
}

#[tokio::test]
async fn an_armed_intent_comes_back_with_the_message_that_mentions_it() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let intent = s
        .intents
        .create(
            "rappeler le changelog de la 0.3",
            vec!["déploiement".into()],
            None,
            86_400_000,
            3,
            None,
        )
        .await
        .unwrap();
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    p.reply("Noté, et voici le changelog.");
    d.enqueue_message(
        &sid,
        "on prépare le déploiement de vendredi",
        &Origin::Cli,
        None,
    )
    .await
    .unwrap();
    let turn = claim(&d).await;
    d.run_turn(&turn).await;
    d.run_turn(&turn).await; // rejoué : même bloc, pas de second tir

    let req = p.requests()[0].clone();
    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == penelope_llm::types::Role::User)
        .unwrap()
        .text();
    assert!(
        last_user.contains("rappeler le changelog de la 0.3"),
        "{last_user}"
    );
    assert!(
        p.requests()[1]
            .messages
            .iter()
            .any(|m| m.text().contains("changelog de la 0.3"))
    );
    let after = s
        .intents
        .all()
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.id == intent.id)
        .unwrap();
    assert_eq!(after.tirs, 1);
}

#[tokio::test]
async fn replaying_a_turn_does_not_duplicate_the_user_message() {
    let (_dir, d, p) = daemon().await;
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("première");
    p.reply("seconde");
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.enqueue_message(&sid, "salut", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    d.run_turn(&turn).await;
    d.run_turn(&turn).await;
    let users = d
        .services
        .context
        .history
        .load(&sid, 0)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.message.text() == "salut")
        .count();
    assert_eq!(users, 1);
}

/// Chemin complet d'une approbation : suspension, décision, reprise en file.
#[tokio::test]
async fn an_approval_suspends_then_a_resume_turn_finishes_the_work() {
    let (_dir, d, p) = daemon().await;
    p.reply(r#"{"complexity":"medium"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "fs_write".into(),
            arguments: json!({"path": "hello.txt", "content": "bonjour"}),
        }],
    ));
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.enqueue_message(&sid, "écris hello.txt", &Origin::Cli, None)
        .await
        .unwrap();
    let mut events = d.bus.subscribe();
    let turn = claim(&d).await;
    let approval_id = match d.run_turn(&turn).await {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    d.services.turns.complete(&turn).await.unwrap();

    // La carte d'approbation est passée sur le bus.
    let mut saw_card = false;
    while let Ok(ev) = events.try_recv() {
        if let BusKind::Event(TurnEvent::Approval { tool, .. }) = &ev.kind {
            assert_eq!(tool, "fs_write");
            saw_card = true;
        }
    }
    assert!(saw_card);

    crate::agent::decide_approval(
        &d.services,
        &approval_id,
        &penelope_hitl::Decision::approve_once("cli"),
    )
    .await
    .unwrap();
    d.enqueue_resume(&sid, &approval_id, &Origin::Cli)
        .await
        .unwrap();
    p.reply("C'est écrit.");
    let resume = claim(&d).await;
    assert_eq!(resume.kind, TurnKind::Resume);
    match d.run_turn(&resume).await {
        TurnOutcome::Answered { text, .. } => assert_eq!(text, "C'est écrit."),
        other => panic!("{other:?}"),
    }
    let ws = default_workspaces(&d.services);
    assert_eq!(
        std::fs::read_to_string(ws[0].join("hello.txt")).unwrap(),
        "bonjour"
    );
}

#[tokio::test]
async fn a_missing_key_fails_with_an_actionable_message() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.enqueue_message(&sid, "bonjour", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    match d.run_turn(&turn).await {
        TurnOutcome::Failed { error } => {
            assert!(error.contains("secret set openrouter_api_key"), "{error}")
        }
        other => panic!("{other:?}"),
    }
}
