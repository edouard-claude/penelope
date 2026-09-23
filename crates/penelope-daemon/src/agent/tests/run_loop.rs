use super::*;

#[tokio::test]
async fn a_plain_answer_finishes_in_one_iteration() {
    let (_d, s, p) = setup().await;
    p.reply("voici la réponse");
    let sid = session(&s).await;
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    let e = exec(false);
    match loop_.run(request(&sid), &e).await.unwrap() {
        TurnOutcome::Answered {
            text, iterations, ..
        } => {
            assert_eq!(text, "voici la réponse");
            assert_eq!(iterations, 1);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(e.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn read_tools_run_without_approval() {
    let (_d, s, p) = setup().await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("j'ai lu le fichier");
    let sid = session(&s).await;
    let e = exec(false);
    let out = AgentLoop::new(s.clone(), p.clone())
        .run(request(&sid), &e)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(e.calls.load(Ordering::SeqCst), 1);
}

/// #85 : trois lectures demandées ensemble partent ensemble ; une en échec n'annule
/// pas les autres, et les résultats sont enregistrés dans l'ordre des appels.
#[tokio::test]
async fn reads_requested_together_run_together_and_keep_their_order() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("c1", "fs_read", json!({"path":"a.rs"})),
            call("c2", "fs_read", json!({"path":"casse.rs"})),
            call("c3", "fs_read", json!({"path":"c.rs"})),
        ],
    ));
    p.reply("lus");
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis les trois");
    let e = TimedExecutor::new(300);
    let t = std::time::Instant::now();
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    // En série : 900 ms au moins.
    assert!(
        t.elapsed() < std::time::Duration::from_millis(600),
        "{:?}",
        t.elapsed()
    );
    let (a0, _) = e.span("a.rs");
    let (c0, _) = e.span("c.rs");
    assert!(
        c0.duration_since(a0) < std::time::Duration::from_millis(150),
        "partis ensemble"
    );
    let ids: Vec<String> = conv
        .messages()
        .iter()
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    assert_eq!(ids, vec!["c1", "c2", "c3"]);
    assert!(conv.messages()[3].text().contains("illisible"));
    assert!(conv.messages()[4].text().contains("c.rs"));
}

/// #85 : une écriture entre deux lectures est une barrière : la lecture d'avant a
/// fini quand elle part, celle d'après part quand elle a fini.
#[tokio::test]
async fn a_write_between_reads_is_a_barrier() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    s.policies
        .create_rule(
            penelope_hitl::RuleScope::Tool,
            Some("shell_exec"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("c1", "fs_read", json!({"path":"avant.rs"})),
            call("c2", "shell_exec", json!({"command":"cargo fmt"})),
            call("c3", "fs_read", json!({"path":"apres.rs"})),
        ],
    ));
    p.reply("fait");
    let conv = MemoryConversation::new("Tu es Pénélope.", "formate");
    let e = TimedExecutor::new(100);
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    let (_, avant_fin) = e.span("avant.rs");
    let (w0, w1) = e.span("cargo fmt");
    let (apres0, _) = e.span("apres.rs");
    assert!(
        avant_fin <= w0,
        "la lecture d'avant a fini avant l'écriture"
    );
    assert!(w1 <= apres0, "la lecture d'après attend l'écriture");
}

/// #85 : `/stop` pendant un lot de lectures les interrompt toutes, vite.
#[tokio::test]
async fn stop_interrupts_a_whole_batch_of_reads() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("c1", "fs_read", json!({"path":"a.rs"})),
            call("c2", "fs_read", json!({"path":"b.rs"})),
            call("c3", "fs_read", json!({"path":"c.rs"})),
        ],
    ));
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis");
    let e = TimedExecutor::new(10_000);
    let sp = spec(&sid);
    let cancel = sp.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let t = std::time::Instant::now();
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(
        t.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(out, TurnOutcome::Cancelled);
    assert_eq!(e.log.lock().unwrap().len(), 3, "les trois interrompus");
}

#[tokio::test]
async fn tool_errors_go_back_to_the_model() {
    let (_d, s, p) = setup().await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("je vois l'erreur, je change d'approche");
    let sid = session(&s).await;
    let e = exec(true);
    let out = AgentLoop::new(s.clone(), p.clone())
        .run(request(&sid), &e)
        .await
        .unwrap();
    match out {
        TurnOutcome::Answered { text, .. } => assert!(text.contains("change d'approche")),
        other => panic!("le tour doit continuer malgré l'erreur : {other:?}"),
    }
    assert_eq!(
        s.effects
            .count_by_state(penelope_kernel::effects::EffectState::Failed)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn loop_detector_aborts_the_turn() {
    let (_d, s, p) = setup().await;
    for i in 0..8 {
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call(&format!("c{i}"), "fs_read", json!({"path":"a.rs"}))],
        ));
    }
    let sid = session(&s).await;
    let e = exec(false);
    let out = AgentLoop::new(s.clone(), p.clone())
        .run(request(&sid), &e)
        .await
        .unwrap();
    match out {
        TurnOutcome::LoopAborted {
            report, choices, ..
        } => {
            assert!(report.contains("fs_read"));
            assert!(report.contains("Appels du tour"));
            assert_eq!(choices.len(), 3, "suites par défaut");
        }
        other => panic!("{other:?}"),
    }
}

/// Issue #31 : un outil qui renvoie toujours la même erreur, appelé en boucle. Le tour
/// répond quand même en citant l'erreur et en proposant des suites, la conversation
/// garde le résultat et la note d'arrêt, et le message suivant ne relance rien.
#[tokio::test]
async fn a_stopped_loop_still_answers_with_the_real_error_and_choices() {
    let (_d, s, p) = setup().await;
    for i in 0..4 {
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call(&format!("c{i}"), "fs_read", json!({"path": "mails"}))],
        ));
    }
    p.reply(
        "J'ai lu la boîte trois fois, l'outil répond « disque plein ».\n\
             CHOIX : Chercher autrement | Je te précise le compte | Laisser tomber",
    );
    let sid = session(&s).await;
    let e = exec(true);
    let conv = MemoryConversation::new("système", "regarde mes mails");
    let spec = TurnSpec {
        session_id: sid.clone(),
        run_id: None,
        turn_id: None,
        model_id: "mock/model".into(),
        fallback_models: Vec::new(),
        tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
        allowed_tools: Vec::new(),
        cancel: CancelToken::new(),
    };
    let agent = AgentLoop::new(s.clone(), p.clone());
    let out = agent
        .run_conversation(&spec, &conv, &e, &NullSink)
        .await
        .unwrap();
    let TurnOutcome::LoopAborted {
        answer, choices, ..
    } = out
    else {
        panic!("{out:?}");
    };
    assert!(answer.contains("disque plein"), "{answer}");
    assert!(!answer.contains("CHOIX"));
    assert_eq!(
        choices,
        vec![
            "Chercher autrement",
            "Je te précise le compte",
            "Laisser tomber"
        ]
    );
    let wrap_up = p.requests().last().unwrap().clone();
    assert_eq!(
        wrap_up.tool_choice,
        Some(ToolChoice::None),
        "dernière réponse sans outil"
    );

    let history = conv.messages();
    let stopped = history
        .iter()
        .find(|m| m.role == Role::Tool && m.text().contains(LOOP_STOP_NOTE))
        .expect("note d'arrêt dans la conversation");
    assert!(
        stopped.text().contains("disque plein"),
        "{}",
        stopped.text()
    );
    assert_eq!(history.last().unwrap().text(), answer, "réponse gardée");
    assert!(pending_calls(&history).is_empty(), "plus rien à relancer");

    // « Donc ? » : le modèle voit la note, aucun outil n'est rejoué d'office.
    let calls_before = e.calls.load(Ordering::SeqCst);
    conv.record(&ChatMessage::user("Donc ?"), false)
        .await
        .unwrap();
    p.reply("Je n'insiste pas avec la même lecture : précise le compte.");
    let next = agent
        .run_conversation(&spec, &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(next, TurnOutcome::Answered { .. }), "{next:?}");
    assert_eq!(e.calls.load(Ordering::SeqCst), calls_before);
    let seen = p.requests().last().unwrap().clone();
    assert!(
        seen.messages
            .iter()
            .any(|m| m.text().contains(LOOP_STOP_NOTE)),
        "la note d'arrêt est dans l'historique envoyé au modèle"
    );
}

#[test]
fn choices_are_split_from_the_answer() {
    let (answer, choices) =
        split_choices("Erreur : 401.\n\n**CHOIX :** Réessayer | « Autre compte »  |");
    assert_eq!(answer, "Erreur : 401.");
    assert_eq!(choices, vec!["Réessayer", "Autre compte"]);
    let (answer, choices) = split_choices("Rien à proposer.");
    assert_eq!(answer, "Rien à proposer.");
    assert!(choices.is_empty());
}

#[tokio::test]
async fn cancellation_stops_the_turn() {
    let (_d, s, p) = setup().await;
    p.reply("réponse");
    let sid = session(&s).await;
    let req = request(&sid);
    req.cancel.cancel();
    let e = exec(false);
    assert_eq!(
        AgentLoop::new(s.clone(), p.clone())
            .run(req, &e)
            .await
            .unwrap(),
        TurnOutcome::Cancelled
    );
}

#[tokio::test]
async fn exceeded_budget_stops_before_calling_the_model() {
    let (_d, s, p) = setup().await;
    s.budget
        .record(penelope_kernel::budget::UsageRecord {
            model: "m".into(),
            provider: "mock".into(),
            cost_usd: 100.0,
            ..Default::default()
        })
        .await
        .unwrap();
    let sid = session(&s).await;
    let e = exec(false);
    let out = AgentLoop::new(s.clone(), p.clone())
        .run(request(&sid), &e)
        .await
        .unwrap();
    match &out {
        TurnOutcome::BudgetExceeded {
            spent_usd,
            limit_usd,
            ..
        } => assert!(spent_usd > limit_usd, "{out:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(p.call_count(), 0, "le modèle n'est pas appelé");
    assert_eq!(s.approvals.pending(10).await.unwrap().len(), 1);
}

/// Issue #19 : un tour qui a coûté 1 $ (reprises comprises) demande s'il continue ;
/// accepté, il reprend jusqu'au palier suivant et sa réponse dit ce qu'il a coûté ;
/// refusé, il s'arrête.
#[tokio::test]
async fn a_costly_turn_asks_before_going_on() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    for u in spent("t_test", 8, 0.13) {
        s.budget.record(u).await.unwrap();
    }
    let conv = MemoryConversation::new("Tu es Pénélope.", "enquête");
    let e = exec(false);
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    let sink = RecordingSink::default();
    let id = match loop_
        .run_conversation(&spec(&sid), &conv, &e, &sink)
        .await
        .unwrap()
    {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    assert_eq!(p.call_count(), 0, "rien n'est dépensé avant la réponse");
    let a = s.approvals.get(&id).await.unwrap().unwrap();
    assert_eq!(a.payload["checkpoint"], true);
    assert!(
        a.payload["reason"]
            .as_str()
            .unwrap()
            .contains("Ce tour a coûté 1,04 $ (8 appels au modèle)"),
        "{}",
        a.payload
    );
    assert!(
        sink.events()
            .iter()
            .any(|ev| matches!(ev, TurnEvent::Approval { .. }))
    );

    decide_approval(&s, &id, &Decision::approve_once("test"))
        .await
        .unwrap();
    p.reply("conclusion");
    match loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::Answered { text, .. } => {
            assert!(text.starts_with("conclusion"), "{text}");
            assert!(
                text.contains("_Coût de ce tour : 1,04 $ (9 appels au modèle)._"),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }

    // Palier suivant, refusé : le tour s'arrête sans appeler le modèle.
    for u in spent("t_test", 1, 1.0) {
        s.budget.record(u).await.unwrap();
    }
    let id = match loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    decide_approval(&s, &id, &Decision::deny("test", None))
        .await
        .unwrap();
    let calls = p.call_count();
    match loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::Answered { text, .. } => {
            assert!(text.contains("Tour arrêté à ta demande"), "{text}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(p.call_count(), calls);
}

/// Issue #19 : le plafond d'appels compte tout le tour, reprises comprises, et le
/// dixième appel rappelle de regrouper ou de déléguer.
#[tokio::test]
async fn the_call_cap_spans_resumptions_and_suggests_delegating() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    for u in spent("t_test", 9, 0.0) {
        s.budget.record(u).await.unwrap();
    }
    let conv = MemoryConversation::new("Tu es Pénélope.", "enquête");
    let e = exec(false);
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c9", "fs_read", json!({"path": "src/lib.rs"}))],
    ));
    p.reply("fini");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    let tail = conv.tail().await.unwrap();
    let result = tail.iter().find(|m| m.role == Role::Tool).unwrap();
    assert!(
        result.text().contains("10 appels au modèle dans ce tour"),
        "{}",
        result.text()
    );
    assert!(result.text().contains("sub_agent_spawn"));

    for u in spent("t_test", 20, 0.0) {
        s.budget.record(u).await.unwrap();
    }
    let calls = p.call_count();
    match loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::Failed { error } => {
            assert!(
                error.contains("reprises après approbation comprises"),
                "{error}"
            );
            // #139 : la passerelle reconnaît le plafond à ce préfixe pour proposer
            // « Continuer » plutôt que « Réessayer ».
            assert!(error.starts_with(CALLS_EXHAUSTED), "{error}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(p.call_count(), calls);
}

#[test]
fn the_budget_message_names_the_key_of_the_scope_reached() {
    let session = budget_exceeded_text("session", 5.12, 5.0);
    assert!(
        session.contains("5.12 $ dépensés pour un plafond de 5.00 $"),
        "{session}"
    );
    assert!(session.contains("budget.session_usd 10"), "{session}");
    assert!(session.contains("/new"), "{session}");
    assert!(!session.contains("daily_usd"), "{session}");
    assert!(session.contains("/compact"), "{session}");

    let day = budget_exceeded_text("jour", 20.4, 20.0);
    assert!(day.contains("budget.daily_usd 40"), "{day}");
    assert!(!day.contains("/new"), "{day}");
    assert!(budget_exceeded_text("run", 6.0, 5.0).contains("budget.run_usd 10"));
}

#[tokio::test]
async fn tools_outside_the_allowlist_are_refused() {
    let (_d, s, p) = setup().await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "shell_exec", json!({"command":"x"}))],
    ));
    p.reply("compris");
    let sid = session(&s).await;
    let mut req = request(&sid);
    req.allowed_tools = vec!["fs_*".into()];
    let e = exec(false);
    let out = AgentLoop::new(s.clone(), p.clone())
        .run(req, &e)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(e.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        s.approvals.pending(10).await.unwrap().len(),
        0,
        "un outil hors liste blanche ne demande même pas d'approbation"
    );
}

#[tokio::test]
async fn deltas_are_streamed_to_the_sink() {
    let (_d, s, p) = setup().await;
    p.reply("bonjour à toi");
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let sink = RecordingSink::default();
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &sink)
        .await
        .unwrap();
    let streamed: String = sink
        .events()
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Delta(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "bonjour à toi");
    assert_eq!(conv.messages().last().unwrap().text(), "bonjour à toi");
}

#[test]
fn blind_models_get_a_mention_instead_of_images() {
    let catalog = penelope_llm::catalog::Catalog::new();
    let mut seeing = penelope_llm::catalog::ModelInfo::minimal("v/voit", "v", 32_000);
    seeing.input_modalities = vec!["text".into(), "image".into()];
    catalog.upsert(vec![
        seeing,
        penelope_llm::catalog::ModelInfo::minimal("t/texte", "t", 32_000),
    ]);
    let photo = ChatMessage {
        content: vec![
            Content::text("regarde"),
            Content::ImageUrl {
                url: "data:image/jpeg;base64,AA".into(),
                detail: None,
            },
        ],
        ..ChatMessage::user("")
    };
    let msgs = vec![photo];
    let blind = fit_modalities(&msgs, &catalog, "openrouter:t/texte");
    assert!(
        matches!(&blind[0].content[1], Content::Text { text } if text.contains("image non transmise"))
    );
    let seen = fit_modalities(&msgs, &catalog, "openrouter:v/voit");
    assert!(matches!(seen[0].content[1], Content::ImageUrl { .. }));
    let unknown = fit_modalities(&msgs, &catalog, "openrouter:inconnu/x");
    assert!(
        matches!(unknown[0].content[1], Content::ImageUrl { .. }),
        "sans catalogue, on ne retire rien"
    );
}
