use super::*;

#[tokio::test]
async fn a_text_message_gets_an_html_answer_as_a_reply() {
    let (_d, g, t, p) = gateway().await;
    // Accueil déjà proposé : seul l'échange compte ici.
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("Bonjour, **Edouard**.");
    g.process_update(&updates::text_message(
        1,
        OWNER,
        OWNER,
        "salut, fais le point",
    ))
    .await
    .unwrap();
    drain(&g).await;

    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["text"], "Bonjour, <b>Edouard</b>.");
    assert_eq!(sent[0]["parse_mode"], "HTML");
    assert!(sent[0]["reply_parameters"]["message_id"].is_i64());
    // Réactions : reçu, puis terminé.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reactions = t.calls_to(tg::SET_MESSAGE_REACTION).await;
    assert!(reactions.len() >= 2, "{reactions:?}");
}

/// #71 : une commande dont le traitement échoue le dit, au lieu de se taire.
#[tokio::test]
async fn a_failing_command_says_so_to_the_owner() {
    let (_d, g, t, _p) = gateway().await;
    // Ce que fait la boucle de polling quand `process_update` remonte une erreur.
    let update = updates::text_message(95, OWNER, OWNER, "/model auto on");
    g.report_failure(&update, &anyhow::anyhow!("configuration non inscriptible"))
        .await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|m| m.contains("n'a pas pu être traitée") && m.contains("non inscriptible")),
        "l'échec doit être dit avec sa raison : {sent:?}"
    );
    let reply_to = t.calls_to(tg::SEND_MESSAGE).await[0]["reply_parameters"]["message_id"].as_i64();
    assert_eq!(reply_to, Some(950), "en réponse au message fautif");
}

/// #49 : `/stop` arrête le tour en cours **et** vide la file, en disant combien.
#[tokio::test]
async fn stop_empties_the_queue_and_says_how_many() {
    let (_d, g, t, _p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    for i in 0..5 {
        g.process_update(&updates::text_message(
            300 + i,
            OWNER,
            OWNER,
            &format!("q{i}"),
        ))
        .await
        .unwrap();
    }
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 5);

    g.process_update(&updates::text_message(400, OWNER, OWNER, "/stop"))
        .await
        .unwrap();
    assert_eq!(
        g.daemon.services.turns.pending_count().await.unwrap(),
        0,
        "la file est vidée"
    );
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    let note = sent.last().cloned().unwrap_or_default();
    assert!(note.contains('5'), "le compte doit être dit : {note}");
    assert!(note.contains("annulé"), "{note}");
}

/// Issue #5 : un tour échoué porte un bouton « Réessayer » qui relance la réponse sur
/// le même transcript, sans dupliquer le message.
/// #139 : un tour arrêté par son plafond d'appels propose « Continuer », dit ce que
/// fait le bouton et le coût du tour ; une vraie erreur garde « Réessayer ». Le clic
/// reprend le même transcript, dernier résultat d'outil compris.
#[tokio::test]
async fn a_turn_out_of_calls_offers_to_continue() {
    let (_d, g, t, p) = gateway().await;
    let s = g.daemon.services.clone();
    let sid = g
        .daemon
        .chat_session_for(&Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        })
        .await
        .unwrap();
    let h = &s.context.history;
    h.append(
        &sid,
        &penelope_llm::types::ChatMessage::user("lance le test Maestro"),
        10,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    let mut call = penelope_llm::types::ChatMessage::assistant("");
    call.tool_calls = vec![ToolCall {
        id: "m1".into(),
        name: "shell_exec".into(),
        arguments: json!({"command": "maestro test flow.yaml"}),
    }];
    h.append(&sid, &call, 10, 0, false, None).await.unwrap();
    h.append(
        &sid,
        &penelope_llm::types::ChatMessage::tool_result(
            "m1",
            "shell_exec",
            "résultat Maestro : 3 écrans verts",
        ),
        10,
        0,
        false,
        None,
    )
    .await
    .unwrap();

    g.send_failure(
        OWNER,
        None,
        None,
        &sid,
        "le tour n'a pas convergé en 24 appels au modèle (reprises après approbation comprises)",
        Some(0.42),
    )
    .await
    .unwrap();
    g.send_failure(OWNER, None, None, &sid, "panne du fournisseur", None)
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let out_of_calls = &sent[sent.len() - 2];
    let text = out_of_calls["text"].as_str().unwrap();
    assert!(
        text.contains("24 appels") && text.contains("0.42 $"),
        "{text}"
    );
    assert!(!text.contains("❌"), "{text}");
    let button = &out_of_calls["reply_markup"]["inline_keyboard"][0][0];
    assert_eq!(button["text"], "▶️ Continuer (24 appels de plus)");
    assert_eq!(
        sent.last().unwrap()["reply_markup"]["inline_keyboard"][0][0]["text"],
        "🔁 Réessayer"
    );

    p.reply("Les 3 écrans sont verts : la pagination tient.");
    let token = button["callback_data"].as_str().unwrap().to_string();
    g.process_update(&updates::callback(3, OWNER, &token, 1002))
        .await
        .unwrap();
    settle_click(&g).await;
    drain(&g).await;
    let asked = p.requests().last().cloned().expect("tour repris");
    let seen: String = asked
        .messages
        .iter()
        .map(|m| m.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(seen.contains("résultat Maestro : 3 écrans verts"), "{seen}");
}

#[tokio::test]
async fn a_failed_turn_offers_a_retry_button() {
    let (_d, g, t, p) = gateway().await;
    p.reply(r#"{"complexity":"low"}"#);
    p.push(Scripted::Error(
        penelope_llm::types::LlmErrorKind::Other,
        "panne du fournisseur".into(),
    ));
    g.process_update(&updates::text_message(
        1,
        OWNER,
        OWNER,
        "salut, fais le point",
    ))
    .await
    .unwrap();
    drain(&g).await;
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let failure = sent.last().unwrap();
    assert!(
        failure["text"]
            .as_str()
            .unwrap()
            .contains("panne du fournisseur"),
        "{failure}"
    );
    let token = failure["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
        .as_str()
        .unwrap()
        .to_string();

    p.reply("Deuxième essai réussi.");
    g.process_update(&updates::callback(2, OWNER, &token, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    drain(&g).await;
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert_eq!(
        sent.last().unwrap()["text"],
        "Deuxième essai réussi.",
        "{sent:?}"
    );
    let sid = g
        .daemon
        .services
        .sessions
        .find_by_topic(OWNER, None)
        .await
        .unwrap()
        .unwrap()
        .id;
    let history = g
        .daemon
        .services
        .context
        .history
        .load(sid.as_str(), 0)
        .await
        .unwrap();
    let users = history
        .iter()
        .filter(|e| e.message.role == penelope_llm::types::Role::User)
        .count();
    assert_eq!(users, 1, "le message n'est pas rejoué");

    // Une fois la réponse obtenue, un second clic ne relance rien.
    g.process_update(&updates::callback(3, OWNER, &token, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    drain(&g).await;
    assert_eq!(t.calls_to(tg::SEND_MESSAGE).await.len(), sent.len());
}

#[tokio::test]
async fn commands_answer_without_calling_the_model() {
    let (_d, g, t, p) = gateway().await;
    g.process_update(&updates::text_message(
        20,
        OWNER,
        OWNER,
        "/model main z-ai/glm-5.3",
    ))
    .await
    .unwrap();
    g.process_update(&updates::text_message(21, OWNER, OWNER, "/status"))
        .await
        .unwrap();
    g.process_update(&updates::text_message(
        22,
        OWNER,
        OWNER,
        "/secret set openrouter_api_key sk-xxx",
    ))
    .await
    .unwrap();
    drain(&g).await;
    assert_eq!(p.call_count(), 0);
    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(out[0].contains("openrouter:z-ai/glm-5.3"), "{out:?}");
    assert!(out[1].contains("version"), "{out:?}");
    assert!(out[2].contains("jamais"), "{out:?}");
    let cfg = g.daemon.services.config.config();
    assert_eq!(cfg.alias_model("main"), Some("openrouter:z-ai/glm-5.3"));
}

/// Issue #32 : une session au plafond relevé à 20 $ n'est pas suspendue à 5 $ ; à 20 $,
/// la carte s'affiche, et « +5 $ » reprend le tour suspendu.
#[tokio::test]
async fn a_session_budget_is_raised_and_the_suspended_turn_resumes() {
    let (_d, g, t, p) = gateway().await;
    let d = g.daemon.clone();
    d.publish_config("test", |c| {
        c.models.routing.classifier = false;
        // Le jour a de la marge : c'est le plafond de la session qui est testé.
        c.budget.daily_usd = 100.0;
        Ok(vec![
            "models.routing.classifier".into(),
            "budget.daily_usd".into(),
        ])
    })
    .unwrap();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let sid = d.chat_session_for(&chat).await.unwrap();
    let spend = |usd: f64| {
        let (d, sid) = (d.clone(), sid.clone());
        async move {
            d.services
                .budget
                .record(penelope_kernel::budget::UsageRecord {
                    session_id: Some(sid),
                    model: "mock/model".into(),
                    provider: "mock".into(),
                    role: Some("chat".into()),
                    cost_usd: usd,
                    ..Default::default()
                })
                .await
                .unwrap();
        }
    };
    g.process_update(&updates::text_message(
        700,
        OWNER,
        OWNER,
        "/budget session 20",
    ))
    .await
    .unwrap();
    spend(6.0).await;
    p.reply("première réponse");
    g.process_update(&updates::text_message(701, OWNER, OWNER, "on avance"))
        .await
        .unwrap();
    drain(&g).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(sent.contains("Plafond de cette session : 20 $"), "{sent}");
    assert!(
        sent.contains("première réponse"),
        "pas suspendue à 5 $ : {sent}"
    );

    spend(15.0).await;
    let calls = p.call_count();
    g.process_update(&updates::text_message(702, OWNER, OWNER, "et la suite ?"))
        .await
        .unwrap();
    drain(&g).await;
    assert_eq!(p.call_count(), calls, "suspendu avant l'appel au modèle");
    let card = t
        .calls_to(tg::SEND_MESSAGE)
        .await
        .into_iter()
        .rev()
        .find(|c| {
            c["text"]
                .as_str()
                .is_some_and(|x| x.contains("dépensés sur 20 $"))
        })
        .expect("carte de budget");
    assert!(card["text"].as_str().unwrap().contains("continuer ?"));
    let raise = inline_buttons(&card)
        .into_iter()
        .find(|(l, _)| l == "+5 $")
        .expect("bouton +5 $")
        .1;

    p.reply("je reprends la suite");
    g.process_update(&updates::callback(703, OWNER, &raise, 704))
        .await
        .unwrap();
    settle_click(&g).await;
    drain(&g).await;
    let session = d.services.sessions.get(&sid).await.unwrap().unwrap();
    assert_eq!(
        session.budget_usd,
        Some(26.0),
        "21 $ dépensés + 5 $ : {:?} / {:?}",
        t.calls_to(tg::ANSWER_CALLBACK_QUERY).await,
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
    );
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        sent.contains("je reprends la suite"),
        "tour repris : {sent}"
    );
}

/// #204 : `/stop` coupe les jobs d'outils de la session, `/stop tout` ceux de toutes
/// les sessions — et le dit, comme pour les ingestions (#155).
#[test]
fn stopping_cuts_the_tool_jobs_and_says_so() {
    use super::StopReport;

    let one = StopReport {
        cancelled_jobs: 1,
        ..Default::default()
    };
    let out = one.render();
    assert!(!out.contains("Rien à arrêter"), "{out}");
    assert!(out.contains("1 job"), "{out}");

    let all = StopReport {
        running: true,
        cancelled_jobs: 3,
        tout: true,
        ..Default::default()
    };
    assert!(all.render().contains("3 job"), "{}", all.render());
}

/// #155 : le 21/09, `/stop` puis `/stop tout` ont répondu « Rien à arrêter » alors
/// qu'un run était **bloqué** dans le sujet depuis vingt minutes, et que la dernière
/// réponse le disait « en cours ». Un run ouvert n'est jamais « rien ».
#[test]
fn an_open_run_is_never_nothing_to_stop() {
    use super::StopReport;

    // Rien du tout : la réponse d'avant reste juste.
    assert_eq!(StopReport::default().render(), "Rien à arrêter.");

    // Le cas de l'incident : aucun tour, mais un run bloqué dans le chat.
    let blocked = StopReport {
        open: vec![("r_01M318PC88".into(), "blocked".into())],
        ..Default::default()
    };
    let out = blocked.render();
    assert!(!out.contains("Rien à arrêter"), "{out}");
    assert!(
        out.contains("r_01M318PC88") && out.contains("blocked"),
        "{out}"
    );
    assert!(out.contains("/stop tout"), "{out}");

    // `/stop tout` sur ce même run : il est nommé, pas annulé à la place du
    // propriétaire — un run annulé ne se reprend pas.
    let all = StopReport {
        left: vec!["`r_01M318PC88` (blocked)".into()],
        open: vec![("r_01M318PC88".into(), "blocked".into())],
        tout: true,
        ..Default::default()
    };
    let out = all.render();
    assert!(out.contains("laissé(s) ouvert(s)"), "{out}");
    assert!(out.contains("/run cancel"), "{out}");
    assert!(!out.contains("Rien à arrêter"), "{out}");
}

/// #155 : ce qui est annoncé est ce qui a été fait. L'ingestion n'était pas
/// interrompue malgré la phrase qui l'annonçait ; elle l'est désormais, et n'est dite
/// que lorsqu'il y en avait une.
#[test]
fn stop_promises_only_what_it_did() {
    use super::StopReport;

    // Sans ingestion en cours, le mot n'apparaît pas.
    let plain = StopReport {
        running: true,
        queued: 3,
        paused: 2,
        tout: true,
        ..Default::default()
    };
    let out = plain.render();
    assert!(out.contains("Tour arrêté, 3 message(s)"), "{out}");
    assert!(out.contains("2 run(s) de workflow mis en pause"), "{out}");
    assert!(
        !out.to_lowercase().contains("ingestion"),
        "rien à dire sur l'ingestion : {out}"
    );

    // Avec, elle est comptée et dite comme interrompue.
    let with = StopReport {
        running: true,
        ingests: 2,
        cancelled_ingests: 2,
        tout: true,
        ..Default::default()
    };
    let out = with.render();
    assert!(
        out.contains("2 ingestion(s) de document interrompue(s)"),
        "{out}"
    );

    // `/stop` simple ne l'interrompt pas : il la nomme, et dit quoi faire.
    let simple = StopReport {
        ingests: 1,
        ..Default::default()
    };
    let out = simple.render();
    assert!(!out.contains("Rien à arrêter"), "{out}");
    assert!(out.contains("1 ingestion(s) de document"), "{out}");
    assert!(out.contains("interrompt les ingestions"), "{out}");
}

/// Issue #31 : la réponse d'une boucle arrêtée arrive avec ses suites en boutons, sans
/// rapport technique, et un clic arrive dans la session comme un message du propriétaire.
#[tokio::test]
async fn a_stopped_loop_answer_offers_choices_that_become_messages() {
    use penelope_app::bus::ChannelDelivery;
    let (_d, g, t, _p) = gateway().await;
    let d = &g.daemon;
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: Some(640),
    };
    let sid = d.chat_session_for(&chat).await.unwrap();
    g.deliver(
        "t1",
        &sid,
        &chat,
        &TurnOutcome::LoopAborted {
            report: "l'outil `tool_call` a été appelé 4 fois\n\nAppels du tour :".into(),
            answer: "La messagerie répond « 401 Unauthorized » à chaque lecture.".into(),
            choices: vec!["Chercher autrement".into(), "Laisser tomber".into()],
        },
    )
    .await;
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let card = sent.last().unwrap();
    let text = card["text"].as_str().unwrap();
    assert!(text.contains("401 Unauthorized"), "{text}");
    assert!(
        !text.contains("Appels du tour"),
        "rapport technique absent : {text}"
    );
    let token = inline_buttons(card)
        .into_iter()
        .find(|(l, _)| l == "Chercher autrement")
        .expect("suite en bouton")
        .1;
    g.process_update(&updates::callback(641, OWNER, &token, 642))
        .await
        .unwrap();
    settle_click(&g).await;
    let turn = d
        .services
        .turns
        .claim("test")
        .await
        .unwrap()
        .expect("message mis en file");
    assert_eq!(turn.session_id, sid);
    assert!(
        turn.payload.to_string().contains("Chercher autrement"),
        "{}",
        turn.payload
    );
}

/// #115 : une valeur absente se montre en « ? », jamais en `null`.
#[test]
fn an_absent_value_is_shown_as_a_question_mark() {
    let v = json!({"ms": 42, "name": "redmine", "vide": ""});
    assert_eq!(shown(&v["ms"]), "42");
    assert_eq!(shown(&v["name"]), "redmine");
    assert_eq!(shown(&v["absent"]), "?");
    assert_eq!(shown(&v["vide"]), "?");
    assert_eq!(
        format!("✅ redmine répond ({} ms)", shown(&json!({})["ms"])),
        "✅ redmine répond (? ms)"
    );
}

/// #115 : aucune bulle ne se construit depuis une valeur JSON brute. Le code des
/// écrans et des commandes n'en passe aucune à `format!`, et les écrans usuels ne
/// montrent jamais `null`.
#[tokio::test]
async fn no_bubble_ever_shows_null() {
    let re =
        regex::Regex::new(r#"^\s*[a-z_.]+\["[a-z_]+"\](\["[a-z_]+"\]|\[[0-9]+\])*,?\s*$"#).unwrap();
    // Toute la passerelle, découpée en modules sous telegram/ (lot G), hors ses tests.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/telegram");
    let mut files = vec![root.with_extension("rs")];
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.ends_with("tests") {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    assert!(files.len() > 20, "{files:?}");
    for path in files.iter().filter(|p| p.exists()) {
        let file = path.display();
        let src = std::fs::read_to_string(path).unwrap();
        let code = src.split("#[cfg(test)]\nmod tests {").next().unwrap();
        let lines: Vec<&str> = code.lines().collect();
        // Une valeur seule sur sa ligne, que ne suit aucun accesseur (`.as_str()`…).
        let raw: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| {
                re.is_match(l)
                    && !lines
                        .get(i + 1)
                        .is_some_and(|next| next.trim_start().starts_with('.'))
            })
            .map(|(_, l)| *l)
            .collect();
        assert!(
            raw.is_empty(),
            "{file} : valeurs JSON brutes formatées : {raw:?}"
        );
    }

    let (_d, g, t, _p) = gateway().await;
    for (i, command) in [
        "/status",
        "/config",
        "/models",
        "/model",
        "/mcp",
        "/sessions",
        "/policies",
        "/wf",
        "/mode",
        "/approvals",
        "/schedules",
        "/budget",
        "/doctor",
    ]
    .iter()
    .enumerate()
    {
        g.process_update(&updates::text_message(
            800 + i as i64,
            OWNER,
            OWNER,
            command,
        ))
        .await
        .unwrap();
    }
    settle(&g).await;
    g.flush_outbox().await.unwrap();
    let mut bubbles = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    bubbles.extend(texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await));
    assert!(bubbles.len() >= 10, "{bubbles:?}");
    for b in &bubbles {
        assert!(!b.contains("null") && !b.contains("undefined"), "{b}");
    }
}

/// #121 : l'indicateur d'activité part dès la mise en file, est renvoyé à intervalle
/// régulier tant que le tour vit, dans son sujet, puis s'arrête ; un échec d'envoi ne
/// touche pas le tour.
#[tokio::test]
async fn the_activity_indicator_lives_as_long_as_the_turn() {
    let (_d, g, t, p) = gateway().await;
    g.set_activity_every(Duration::from_millis(40));
    let chat: i64 = -1_001_234_567_890;
    g.daemon
        .publish_config("test", move |c| {
            c.telegram.allowed_chats = vec![chat];
            c.models.routing.classifier = false;
            Ok(vec!["telegram.allowed_chats".into()])
        })
        .unwrap();
    let loops = tokio::spawn(g.clone().draft_loop());
    p.slow(Duration::from_millis(500));
    p.reply("C'est fait.");
    let mut u = updates::in_topic(updates::text_message(930, chat, OWNER, "vérifie la PR"), 21);
    u["message"]["chat"] = json!({"id": chat, "type": "supergroup", "title": "Chantiers"});
    g.process_update(&u).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !t.calls_to(tg::SEND_CHAT_ACTION).await.is_empty(),
        "signe de vie dès la mise en file"
    );
    t.fail_transport(2, "réseau coupé").await;
    drain(&g).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let actions = t.calls_to(tg::SEND_CHAT_ACTION).await;
    assert!(actions.len() >= 6, "{} actions", actions.len());
    assert!(
        actions
            .iter()
            .all(|a| a["message_thread_id"] == 21 && a["chat_id"] == chat)
    );
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|x| x == "C'est fait."),
        "le tour aboutit : {sent:?}"
    );
    let after = actions.len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        t.calls_to(tg::SEND_CHAT_ACTION).await.len(),
        after,
        "plus rien une fois le tour fini"
    );
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loops).await;
}

#[tokio::test]
async fn html_rejected_by_telegram_falls_back_to_plain_text() {
    let (_d, g, t, _p) = gateway().await;
    t.fail_once(400, "Bad Request: can't parse entities", None)
        .await;
    g.reply(OWNER, None, None, "a **b** c").await.unwrap();
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["text"], "a b c");
    assert!(sent[1].get("parse_mode").is_none());
}

/// #148 : un échec bavard ne part pas en cinq bulles. Le message est coupé et renvoie
/// au journal ; une réponse ordinaire, même longue, n'est pas touchée.
#[test]
fn a_long_failure_is_cut_and_points_at_the_log() {
    let long = format!("❌ Connexion abandonnée : {}", "détail ".repeat(400));
    let cut = shorten_failure(&long);
    assert!(
        cut.chars().count() < 600,
        "{} caractères",
        cut.chars().count()
    );
    assert!(cut.starts_with("❌ Connexion abandonnée"), "{cut}");
    assert!(cut.contains("penelope logs"), "{cut}");

    let court = "❌ Modèle inconnu";
    assert_eq!(shorten_failure(court), court);

    // Une réponse du modèle n'est pas un échec : elle passe entière.
    let reponse = "Voici le plan détaillé. ".repeat(400);
    assert_eq!(shorten_failure(&reponse), reponse);
}

#[tokio::test]
async fn long_answers_are_split_in_order() {
    let (_d, g, t, _p) = gateway().await;
    let long = "paragraphe assez long pour déborder\n\n".repeat(300);
    g.reply(OWNER, None, Some(9), &long).await.unwrap();
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert!(sent.len() >= 3, "{}", sent.len());
    assert!(
        sent.iter()
            .all(|s| s["text"].as_str().unwrap().chars().count() <= 4096)
    );
    assert!(sent[0].get("reply_parameters").is_some());
    assert!(sent[1].get("reply_parameters").is_none());
}

#[test]
fn values_render_compactly() {
    let v = json!([{"id": "s1", "title": "refonte", "state": "active"}]);
    assert_eq!(
        render_value(&v),
        "- id : s1 · title : refonte · state : active\n"
    );
    assert_eq!(render_value(&json!([])), "(vide)");
    assert!(render_value(&json!({"version": "0.1.0"})).contains("**version** : 0.1.0"));
}

#[test]
fn model_ids_default_to_openrouter() {
    assert_eq!(
        normalise_model_id("z-ai/glm-5.3"),
        "openrouter:z-ai/glm-5.3"
    );
    assert_eq!(normalise_model_id("local:llama"), "local:llama");
}
