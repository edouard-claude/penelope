//! Trace des outils sur Telegram (issue #222) : une bulle par tour, créée par la file
//! d'envoi avant la réponse, modifiée en place, close au démarrage suivant si le daemon
//! est tombé en plein tour.

use super::*;

fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

async fn quiet(g: &TelegramGateway) {
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    g.daemon
        .publish_config("test", |c| {
            c.models.routing.classifier = false;
            Ok(vec!["models.routing.classifier".into()])
        })
        .unwrap();
}

/// Quatre `mem_search` sur la même requête (le garde de boucle refuse un appel identique
/// répété : la limite change), puis un `time_now` : un tour de six réponses du modèle.
fn script_tools(p: &MockProvider) {
    for i in 0..4 {
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call(
                &format!("c{i}"),
                "mem_search",
                json!({"query": "facturation", "limit": i + 1}),
            )],
        ));
    }
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("t", "time_now", json!({}))],
    ));
    p.reply("Voilà.");
}

/// Joue le tour en file, vide la file, attend que la bulle soit close.
async fn play(g: &TelegramGateway) {
    drain(g).await;
    for _ in 0..300 {
        g.flush_outbox().await.unwrap();
        if g.traces_idle() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("la bulle de trace n'a pas été close");
}

/// Les envois qui portent la trace (création et modifications).
async fn trace_calls(t: &MockTransport) -> Vec<(String, Value)> {
    t.calls()
        .await
        .into_iter()
        .filter(|(m, b)| {
            (m == tg::SEND_MESSAGE || m == tg::EDIT_MESSAGE_TEXT)
                && b["text"]
                    .as_str()
                    .is_some_and(|x| x.contains("🧠 mem_search"))
        })
        .collect()
}

/// Une bulle, créée avant la réponse, modifiée en place au fil du tour à la cadence
/// voulue, et qui finit sur l'état final : `(×4) ✅`.
#[tokio::test]
async fn one_bubble_is_created_before_the_answer_and_edited_in_place() {
    let (_d, g, t, p) = gateway().await;
    quiet(&g).await;
    script_tools(&p);
    p.slow(Duration::from_millis(500));
    let loops = (g.spawn_trace(), tokio::spawn(g.clone().outbox_loop()));
    let started = Instant::now();
    g.process_update(&updates::text_message(900, OWNER, OWNER, "on en est où ?"))
        .await
        .unwrap();
    play(&g).await;
    let elapsed = started.elapsed();
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loops.0).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), loops.1).await;

    let calls = t.calls().await;
    let trace = trace_calls(&t).await;
    let created: Vec<&Value> = trace
        .iter()
        .filter(|(m, _)| m == tg::SEND_MESSAGE)
        .map(|(_, b)| b)
        .collect();
    assert_eq!(created.len(), 1, "une seule bulle : {trace:?}");
    assert_eq!(created[0]["disable_notification"], true);
    let edits: Vec<&Value> = trace
        .iter()
        .filter(|(m, _)| m == tg::EDIT_MESSAGE_TEXT)
        .map(|(_, b)| b)
        .collect();
    assert!(edits.len() >= 2, "modifiée au fil du tour : {trace:?}");
    assert!(
        edits.len() as u128 <= elapsed.as_millis() / 1_500 + 2,
        "{} modifications en {elapsed:?}",
        edits.len()
    );
    let message_id = edits[0]["message_id"].clone();
    assert!(edits.iter().all(|e| e["message_id"] == message_id));
    assert_eq!(
        edits.last().unwrap()["text"],
        "🧠 mem_search · <code>facturation</code> (×4) ✅\n🗓 time_now ✅"
    );
    let pos = |pred: &dyn Fn(&Value) -> bool| {
        calls
            .iter()
            .position(|(m, b)| m == tg::SEND_MESSAGE && pred(b))
            .unwrap()
    };
    let bubble = pos(&|b| b["text"].as_str().unwrap_or("").contains("mem_search"));
    let answer = pos(&|b| b["text"] == "Voilà.");
    assert!(bubble < answer, "la trace précède la réponse");
    assert!(
        g.daemon
            .services
            .kv_get(crate::telegram::trace::live::OPEN_KEY)
            .await
            .unwrap()
            .is_none(),
        "la bulle close n'est plus inscrite"
    );
    // Aucune note d'échec : un `not modified` n'est pas un envoi raté.
    assert!(
        !texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.contains("n'a pas pu être envoyé"))
    );
}

/// Un tour sans outil ne pose aucune bulle ; `off` n'en pose jamais, et le mode relu à
/// chaque tour s'applique à chaud.
#[tokio::test]
async fn no_tool_no_bubble_and_the_mode_is_read_at_each_turn() {
    let (_d, g, t, p) = gateway().await;
    quiet(&g).await;
    let loop_ = g.spawn_trace();
    p.reply("Bonjour.");
    g.process_update(&updates::text_message(901, OWNER, OWNER, "salut"))
        .await
        .unwrap();
    play(&g).await;
    assert_eq!(t.calls_to(tg::SEND_MESSAGE).await.len(), 1);

    g.daemon
        .publish_config("test", |c| {
            c.telegram.tool_trace = penelope_kernel::config::ToolTrace::Off;
            Ok(vec!["telegram.tool_trace".into()])
        })
        .unwrap();
    script_tools(&p);
    g.process_update(&updates::text_message(902, OWNER, OWNER, "et là ?"))
        .await
        .unwrap();
    play(&g).await;
    assert!(trace_calls(&t).await.is_empty(), "off : aucune bulle");

    g.daemon
        .publish_config("test", |c| {
            c.telegram.tool_trace = penelope_kernel::config::ToolTrace::Full;
            Ok(vec!["telegram.tool_trace".into()])
        })
        .unwrap();
    script_tools(&p);
    g.process_update(&updates::text_message(903, OWNER, OWNER, "encore"))
        .await
        .unwrap();
    play(&g).await;
    let trace = trace_calls(&t).await;
    assert!(
        trace.last().unwrap().1["text"]
            .as_str()
            .unwrap()
            .contains("↳"),
        "full, rechargé à chaud : {trace:?}"
    );
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_).await;
}

/// Dans un sujet de groupe : la bulle y part, sans argument en `compact`.
#[tokio::test]
async fn in_a_group_topic_the_bubble_goes_to_the_topic_without_arguments() {
    let (_d, g, t, p) = gateway().await;
    quiet(&g).await;
    let chat: i64 = -1_001_234_567_890;
    g.daemon
        .publish_config("test", move |c| {
            c.telegram.allowed_chats = vec![chat];
            Ok(vec!["telegram.allowed_chats".into()])
        })
        .unwrap();
    script_tools(&p);
    let loop_ = g.spawn_trace();
    let mut u = updates::in_topic(updates::text_message(910, chat, OWNER, "point"), 21);
    u["message"]["chat"] = json!({"id": chat, "type": "supergroup", "title": "Chantiers"});
    g.process_update(&u).await.unwrap();
    play(&g).await;
    let trace = trace_calls(&t).await;
    let created = &trace[0].1;
    assert_eq!(created["message_thread_id"], 21);
    assert_eq!(created["chat_id"], chat);
    let last = trace.last().unwrap().1["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(last, "🧠 mem_search (×4) ✅\n🗓 time_now ✅");
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_).await;
}

/// #10 : une session en arrière-plan ne pose pas de bulle.
#[tokio::test]
async fn a_background_session_gets_no_bubble() {
    let (_d, g, t, p) = gateway().await;
    quiet(&g).await;
    let d = g.daemon.clone();
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let first = d.chat_session_for(&chat).await.unwrap();
    d.enqueue_message(&first, "longue question", &chat, None)
        .await
        .unwrap();
    let running = d.services.turns.claim("test").await.unwrap().unwrap();
    g.process_update(&updates::text_message(920, OWNER, OWNER, "/fork"))
        .await
        .unwrap();
    let loop_ = g.spawn_trace();
    script_tools(&p);
    penelope_daemon::runner::process(&daemon_of(&g), running, Duration::from_secs(30)).await;
    play(&g).await;
    assert!(trace_calls(&t).await.is_empty());
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_).await;
}

/// Une bulle laissée ouverte par une vie précédente est close au démarrage, une fois.
#[tokio::test]
async fn an_orphan_bubble_is_closed_at_start() {
    let (_d, g, t, _p) = gateway().await;
    let mut tr = crate::telegram::trace::Trace::default();
    tr.call("shell_exec", &json!({"command": "cargo build"}));
    let open = json!({"t1": {
        "chat_id": OWNER, "outbox_id": "o_x", "message_id": 1234,
        "trace": tr, "mode": "compact", "group": false,
    }});
    g.daemon
        .services
        .kv_set(crate::telegram::trace::live::OPEN_KEY, &open.to_string())
        .await
        .unwrap();
    assert_eq!(g.close_orphan_traces().await.unwrap(), 1);
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["message_id"], 1234);
    assert_eq!(
        edits[0]["text"],
        "💻 shell_exec · <code>cargo build</code> ❔\n⏹ interrompu par un redémarrage"
    );
    assert_eq!(g.close_orphan_traces().await.unwrap(), 0);
    assert_eq!(t.calls_to(tg::EDIT_MESSAGE_TEXT).await.len(), 1);
}

/// La course forcée : la réponse est prête alors que la boucle de trace n'a pas encore lu
/// l'appel d'outil du tour (aucun point de suspension entre l'appel et `deliver`). La
/// réponse attend que la boucle ait rattrapé le bus : la bulle est enfilée avant elle.
#[tokio::test]
async fn the_answer_waits_for_its_bubble_even_when_the_trace_lags() {
    let (_d, g, t, _p) = gateway().await;
    let loop_ = g.spawn_trace();
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: Some(5),
    };
    let bus = &g.daemon.bus;
    let event = |kind| penelope_app::bus::BusEvent {
        turn_id: "t-course".into(),
        session_id: "s-course".into(),
        origin: origin.clone(),
        kind,
    };
    bus.publish(event(BusKind::Started));
    bus.publish(event(BusKind::Event(TurnEvent::ToolCall {
        name: "fs_read".into(),
        args: json!({"path": "notes.txt"}),
    })));
    let outcome = TurnOutcome::Answered {
        text: "Réponse prête.".into(),
        iterations: 2,
        cost_usd: 0.0,
    };
    g.deliver("t-course", "s-course", &origin, &outcome).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].starts_with("📄 fs_read"), "{sent:?}");
    assert!(sent[1].contains("Réponse prête."), "{sent:?}");
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_).await;
}

// --- Modes `resume` et `narre` (#273) --------------------------------------------------

const LOCAL_MODEL: &str = "local:mlx-community/Qwen3-1.7B-4bit";

/// Passe la trace en `narre` avec un alias `local:` de texte, sans `models.roles.trace` :
/// c'est l'alias par défaut du rôle.
fn narrated(g: &TelegramGateway, mode: penelope_kernel::config::ToolTrace) {
    g.daemon
        .publish_config("test", move |c| {
            c.telegram.tool_trace = mode;
            c.models.aliases.insert("local".into(), LOCAL_MODEL.into());
            Ok(vec![
                "telegram.tool_trace".into(),
                "models.aliases.local".into(),
            ])
        })
        .unwrap();
}

/// Le répondeur du mock : le script du tour pour le modèle de conversation, `trace` pour
/// les appels du rôle `trace` (reconnus à leur modèle `local:`).
fn route(p: &MockProvider, trace: Scripted) {
    let main: Arc<std::sync::Mutex<std::collections::VecDeque<Scripted>>> = Arc::default();
    {
        let mut q = main.lock().unwrap();
        for i in 0..4 {
            q.push_back(Scripted::ToolCalls(
                String::new(),
                vec![call(
                    &format!("c{i}"),
                    "mem_search",
                    json!({"query": "facturation", "limit": i + 1}),
                )],
            ));
        }
        q.push_back(Scripted::ToolCalls(
            String::new(),
            vec![call("t", "time_now", json!({}))],
        ));
        q.push_back(Scripted::Text("Voilà.".into()));
    }
    p.set_responder(Some(Arc::new(
        move |req: &penelope_llm::types::ChatRequest| {
            if req.model.starts_with("local:") {
                return trace.clone();
            }
            main.lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Scripted::Text("script épuisé".into()))
        },
    )));
}

/// Les requêtes reçues par le rôle `trace`, dans l'ordre.
fn trace_requests(p: &MockProvider) -> Vec<penelope_llm::types::ChatRequest> {
    p.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.model == LOCAL_MODEL)
        .cloned()
        .collect()
}

async fn narrated_events(g: &TelegramGateway) -> Vec<Value> {
    g.daemon
        .services
        .events
        .range(0, 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == crate::telegram::trace::narrate::EVENT)
        .map(|e| e.payload)
        .collect()
}

/// `resume` : la bulle tient en une ligne comptée, sans appel au modèle, du premier appel
/// à l'état final.
#[tokio::test]
async fn in_resume_mode_the_bubble_is_one_counted_line_without_any_model_call() {
    let (_d, g, t, p) = gateway().await;
    quiet(&g).await;
    narrated(&g, penelope_kernel::config::ToolTrace::Resume);
    route(&p, Scripted::Text("📄 jamais appelé".into()));
    let loop_ = g.spawn_trace();
    g.process_update(&updates::text_message(930, OWNER, OWNER, "point"))
        .await
        .unwrap();
    play(&g).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(sent[0], "🧠 1 rappel en cours", "{sent:?}");
    let edits = texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
    assert_eq!(
        edits.last().map(String::as_str),
        Some("🧠 4 rappels · 🗓 1 heure · ✅"),
        "{edits:?}"
    );
    assert!(
        trace_requests(&p).is_empty(),
        "resume n'appelle aucun modèle"
    );
    assert!(narrated_events(&g).await.is_empty());
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_).await;
}

/// `narre` : la bulle est créée avec la ligne `resume` (le modèle n'est pas sur le chemin
/// de la réponse), puis chaque modification porte la phrase du rôle `trace`. Le modèle
/// reçoit la phrase précédente et, quand il la renvoie, la bulle n'est pas retouchée.
#[tokio::test]
async fn in_narre_mode_the_phrase_replaces_the_line_and_is_kept_when_returned() {
    let (_d, g, t, p) = gateway().await;
    quiet(&g).await;
    narrated(&g, penelope_kernel::config::ToolTrace::Narre);
    route(
        &p,
        Scripted::Text("🧠 Relecture de la facturation en cours 🚀".into()),
    );
    // Un tour de six réponses à 350 ms : plus long que la cadence d'édition (1,5 s), et
    // chaque narration reste sous les 500 ms de budget.
    p.slow(Duration::from_millis(350));
    let loops = (g.spawn_trace(), tokio::spawn(g.clone().outbox_loop()));
    g.process_update(&updates::text_message(931, OWNER, OWNER, "on en est où ?"))
        .await
        .unwrap();
    play(&g).await;
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loops.0).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), loops.1).await;

    let calls = t.calls().await;
    let bubble = calls
        .iter()
        .position(|(m, b)| m == tg::SEND_MESSAGE && b["text"] == "🧠 1 rappel en cours")
        .expect("la bulle est créée avec la ligne resume");
    let answer = calls
        .iter()
        .position(|(m, b)| m == tg::SEND_MESSAGE && b["text"] == "Voilà.")
        .unwrap();
    assert!(bubble < answer, "la bulle précède la réponse");
    let edits = texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
    assert_eq!(
        edits,
        vec!["🧠 Relecture de la facturation en cours".to_string()],
        "une seule modification : la phrase, gardée ensuite, sans le 🚀 hors liste"
    );
    let asked = trace_requests(&p);
    assert!(!asked.is_empty(), "le rôle trace a été appelé");
    for (i, r) in asked.iter().enumerate() {
        let user = r.messages[1].text();
        assert!(!user.contains("Voilà"), "jamais un résultat : {user}");
        assert!(user.contains("mem_search facturation"), "{user}");
        if i > 0 {
            assert!(
                user.starts_with("Phrase précédente : 🧠 Relecture de la facturation en cours\n"),
                "appel {i} : {user}"
            );
        } else {
            assert!(user.starts_with("Phrase précédente : aucune\n"), "{user}");
        }
        assert_eq!(r.max_tokens, Some(60));
    }
    let events = narrated_events(&g).await;
    assert_eq!(events.len(), asked.len(), "{events:?}");
    assert!(events.iter().all(|e| e["fallback"] == false), "{events:?}");
    assert_eq!(events[0]["alias"], "local");
    assert_eq!(events[0]["cost_usd"], 0.0, "local : coût nul");
    assert_eq!(events.last().unwrap()["kept"], asked.len() > 1);
    // L'usage est compté au rôle `trace`, à zéro dollar.
    let (n, cost): (i64, f64) = g
        .daemon
        .services
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(cost_usd), 0) FROM usage WHERE role = 'trace'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n as usize, asked.len());
    assert_eq!(cost, 0.0);
}

/// Le modèle du rôle `trace` échoue : la bulle finit sur la ligne `resume`, sans un mot
/// d'erreur, la réponse n'attend rien, et l'événement dit le repli.
#[tokio::test]
async fn when_the_trace_model_fails_the_bubble_falls_back_to_resume() {
    let (_d, g, t, p) = gateway().await;
    quiet(&g).await;
    narrated(&g, penelope_kernel::config::ToolTrace::Narre);
    route(
        &p,
        Scripted::Error(
            penelope_llm::types::LlmErrorKind::Transient,
            "connexion refusée (127.0.0.1:8080)".into(),
        ),
    );
    let loop_ = g.spawn_trace();
    g.process_update(&updates::text_message(932, OWNER, OWNER, "point"))
        .await
        .unwrap();
    play(&g).await;
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(
        sent,
        vec!["🧠 1 rappel en cours".to_string(), "Voilà.".to_string()]
    );
    let edits = texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
    assert_eq!(edits, vec!["🧠 4 rappels · 🗓 1 heure · ✅".to_string()]);
    let events = narrated_events(&g).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["fallback"], true);
    assert_eq!(events[0]["reason"], "erreur");
    assert!(
        events[0]["detail"]
            .as_str()
            .unwrap()
            .contains("connexion refusée")
    );
}

/// Serveur du rôle arrêté (rien n'écoute) : la narration rend la main sous le budget, sans
/// phrase, par le vrai fournisseur OpenAI-compatible.
#[tokio::test]
async fn a_stopped_local_server_makes_narration_fall_back_within_budget() {
    struct Dead(String);
    #[async_trait::async_trait]
    impl penelope_app::ports::ProviderSource for Dead {
        async fn provider_for(&self, _: &str) -> Result<Arc<dyn penelope_llm::Provider>, String> {
            Ok(Arc::new(
                penelope_llm::OpenAiCompatProvider::new(
                    self.0.clone(),
                    "",
                    penelope_llm::Catalog::new(),
                )
                .map_err(|e| e.to_string())?,
            ))
        }
        fn provider_override_active(&self) -> Option<Arc<dyn penelope_llm::Provider>> {
            None
        }
    }
    let (_d, g, _t, _p) = gateway().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    let mut tr = crate::telegram::trace::Trace::default();
    tr.call("fs_read", &json!({"path": "notes.txt"}));
    let n = crate::telegram::trace::narrate::Narrator {
        alias: "local".into(),
        model: LOCAL_MODEL.into(),
    };
    let started = Instant::now();
    let phrase = crate::telegram::trace::narrate::narrate(
        &g.daemon.services,
        &Dead(base),
        &n,
        &tr,
        crate::telegram::trace::narrate::Ask {
            session_id: "s-dead",
            turn_id: "t-dead",
            previous: None,
        },
    )
    .await;
    assert_eq!(phrase, None);
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    let events = narrated_events(&g).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["fallback"], true);
    assert!(
        matches!(events[0]["reason"].as_str(), Some("erreur" | "delai")),
        "{events:?}"
    );
}

/// `doctor` : rien à dire hors `narre` ; en `narre` sans modèle, un échec qui nomme le
/// repli ; avec un alias local dont le serveur est arrêté, un échec qui le dit.
#[tokio::test]
async fn doctor_names_a_trace_role_without_a_reachable_model() {
    use penelope_app::bus::ChannelDelivery;
    let (_d, g, _t, _p) = gateway().await;
    assert!(
        g.doctor_checks().await.is_empty(),
        "compact : rien à contrôler"
    );
    g.daemon
        .publish_config("test", |c| {
            c.telegram.tool_trace = penelope_kernel::config::ToolTrace::Narre;
            Ok(vec!["telegram.tool_trace".into()])
        })
        .unwrap();
    let checks = g.doctor_checks().await;
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].id, "telegram.trace");
    assert!(
        !checks[0].ok && checks[0].detail.contains("retombe sur `resume`"),
        "{checks:?}"
    );
    assert!(checks[0].fix.is_some());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    g.daemon
        .publish_config("test", move |c| {
            c.providers.local.enabled = true;
            c.providers.local.base_url = base;
            c.models.aliases.insert("local".into(), LOCAL_MODEL.into());
            Ok(vec!["providers.local".into()])
        })
        .unwrap();
    let checks = g.doctor_checks().await;
    assert!(
        !checks[0].ok && checks[0].detail.contains("injoignable"),
        "{checks:?}"
    );
    assert!(checks[0].detail.contains("alias `local`"), "{checks:?}");

    g.daemon
        .publish_config("test", |c| {
            c.models.roles.insert("trace".into(), "fast".into());
            Ok(vec!["models.roles.trace".into()])
        })
        .unwrap();
    let checks = g.doctor_checks().await;
    assert!(
        checks[0].ok && checks[0].detail.contains("facturée"),
        "{checks:?}"
    );
}
