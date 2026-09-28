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
