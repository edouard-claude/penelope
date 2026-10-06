//! Heures calmes, fusion par planification (#318) : une veille à la minute ne rejoue pas
//! ses occurrences de la nuit en autant de tours ; les tours relâchés partent en série.

use super::*;
use penelope_mcp_host::testing::{FakeConnector, declare};

/// `22:00-07:00` à La Réunion, l'horloge de test étant à 4 h du matin.
fn quiet_on(d: &Harness) {
    d.services
        .publish_config("test", |c| {
            c.telegram.quiet_hours = "22:00-07:00".into();
            Ok(vec!["telegram.quiet_hours".into()])
        })
        .unwrap();
}

/// Un serveur MCP `chat` dont l'outil en lecture `recent` rend `messages`.
async fn serve_messages(d: &Harness, messages: Arc<Mutex<Vec<Value>>>) {
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "chat",
        Arc::new(move |m, _| match m {
            "initialize" => Ok(json!({"protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}}, "serverInfo": {"name": "chat"}})),
            "tools/list" => Ok(json!({"tools": [
                {"name": "recent", "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": true}}
            ]})),
            "tools/call" => Ok(json!({
                "content": [{"type": "text", "text": "ok"}],
                "structuredContent": {"messages": messages.lock().unwrap().clone()}
            })),
            "server/discover" => Err(penelope_mcp::McpError::Rpc {
                code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                message: "Method not found".into(),
                data: None,
            }),
            _ => Ok(json!({})),
        }),
    );
    let sup = penelope_mcp_host::testing::supervisor(d.services.clone(), fake);
    declare(&sup, "chat", "");
    sup.reload().await;
    d.set_mcp(sup);
}

/// Une veille `mcp_poll` à la minute, cible `prompt`, voit dix messages pendant la plage :
/// rien ne part la nuit, sa ligne de la file est complétée (une seule entrée), et à 7 h
/// un seul tour reçoit les dix messages, dans l'ordre, avec l'heure de chacun.
#[tokio::test]
async fn a_minute_watch_with_ten_night_items_gives_one_turn_with_all_ten() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let ports = d.scheduler();
    let messages = Arc::new(Mutex::new(vec![json!({"id": 1, "text": "d'hier"})]));
    serve_messages(&d, messages.clone()).await;
    let sched = s
        .schedules
        .create(
            TriggerKind::McpPoll,
            json!({"server": "chat", "tool": "recent", "args": {}, "every_ms": 60_000,
                   "item_path": "$.messages", "id_path": "id"}),
            json!({"type": "prompt", "prompt": "Résume les nouveaux messages",
                   "label": "Veille du sujet"}),
            json!({}),
        )
        .await
        .unwrap();
    clock.advance_ms(61_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(s.turns.pending_count().await.unwrap(), 0, "amorce");

    quiet_on(&d);
    for i in 1..=10 {
        messages
            .lock()
            .unwrap()
            .push(json!({"id": 100 + i, "text": format!("message {i}")}));
        clock.advance_ms(61_000);
        let report = tick(&d, &ports).await.unwrap();
        assert_eq!(report.held, vec![sched.id.clone()], "{report:?}");
    }
    assert!(rec.texts().is_empty(), "rien ne part la nuit");
    assert_eq!(s.turns.pending_count().await.unwrap(), 0);
    assert_eq!(
        penelope_app::quiet::held_count(s).await.unwrap(),
        1,
        "une seule entrée pour la planification"
    );
    let (_, night) = penelope_app::quiet::accumulated(s, &sched.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(night.len(), 10);
    let events = s.events.range(0, 1000).await.unwrap();
    assert_eq!(
        events.iter().filter(|e| e.kind == "schedule.held").count(),
        1,
        "un créneau retenu, une fois"
    );

    // 7h : un tir, un tour, les dix messages.
    clock.advance_ms(3 * 3_600_000);
    let report = tick(&d, &ports).await.unwrap();
    assert_eq!(report.fired, vec![sched.id.clone()], "{report:?}");
    assert_eq!(s.turns.pending_count().await.unwrap(), 1, "un seul tour");
    let turn = s.turns.claim("t").await.unwrap().expect("le tour");
    let text = turn.payload["text"].as_str().unwrap();
    let mut last = 0;
    for i in 1..=10 {
        let at = text
            .find(&format!("\"message {i}\""))
            .unwrap_or_else(|| panic!("message {i} absent : {text}"));
        assert!(at > last, "dans l'ordre : {text}");
        last = at;
    }
    assert!(!text.contains("d'hier"), "{text}");
    assert!(
        text.contains("pendant les heures calmes, dans l'ordre")
            && text.contains("\"observed_at\": \"4h02\"")
            && text.contains("\"observed_at\": \"4h11\""),
        "{text}"
    );
    assert_eq!(turn.payload["lane"], "heures_calmes");
    let grouped = rec.texts();
    assert_eq!(grouped.len(), 1, "{grouped:?}");
    assert_eq!(
        grouped[0].matches("Veille du sujet").count(),
        1,
        "{grouped:?}"
    );
    assert_eq!(penelope_app::quiet::held_count(s).await.unwrap(), 0);

    // Le passage suivant ne rejoue rien.
    clock.advance_ms(61_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(s.turns.pending_count().await.unwrap(), 1);
}

/// Deux planifications `prompt` retenues (un `cron` au quart d'heure, un `interval` à la
/// demi-heure) : à 7 h, une exécution chacune, qui annonce ses occurrences manquées, et
/// deux tours lancés l'un après l'autre.
#[tokio::test]
async fn two_released_schedules_give_two_turns_one_after_the_other() {
    let (d, clock, _rec) = harness().await;
    quiet_on(&d);
    let s = &d.services;
    let ports = d.scheduler();
    let bilan = s
        .schedules
        .create(
            TriggerKind::Cron,
            json!({"expr": "*/15 * * * *"}),
            json!({"type": "prompt", "prompt": "Fais le bilan"}),
            json!({}),
        )
        .await
        .unwrap();
    let revue = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 1_800_000}),
            json!({"type": "prompt", "prompt": "Prépare la revue"}),
            json!({}),
        )
        .await
        .unwrap();
    clock.advance_ms(3_600_000);
    let report = tick(&d, &ports).await.unwrap();
    assert_eq!(report.held.len(), 2, "{report:?}");
    assert_eq!(s.turns.pending_count().await.unwrap(), 0);

    clock.advance_ms(2 * 3_600_000 + 30_000);
    let report = tick(&d, &ports).await.unwrap();
    assert_eq!(report.fired, vec![bilan.id.clone(), revue.id.clone()]);
    assert_eq!(s.turns.pending_count().await.unwrap(), 2);

    let first = s.turns.claim("r1").await.unwrap().expect("premier tour");
    assert_eq!(
        first.payload["text"],
        "🌙 Pendant les heures calmes : prévue à 4h15, retenue jusqu'à 7h00. Les 12 \
         occurrences manquées partent en une seule exécution.\n\nFais le bilan"
    );
    assert!(
        s.turns.claim("r2").await.unwrap().is_none(),
        "le second attend la fin du premier"
    );
    s.turns.complete(&first).await.unwrap();
    let second = s.turns.claim("r2").await.unwrap().expect("second tour");
    assert!(
        second.payload["text"]
            .as_str()
            .unwrap()
            .contains("Les 6 occurrences manquées partent en une seule exécution."),
        "{}",
        second.payload
    );
}

/// Un `event` à cible `prompt`, retenu : les trois événements de la nuit partent en un
/// seul tour à 7 h, chacun avec son heure.
#[tokio::test]
async fn night_events_of_one_schedule_leave_in_one_turn() {
    let (d, clock, _rec) = harness().await;
    let s = &d.services;
    let ports = d.scheduler();
    s.schedules
        .create(
            TriggerKind::Event,
            json!({"event": "run.done"}),
            json!({"type": "prompt", "prompt": "Commente les runs"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &ports).await.unwrap();
    quiet_on(&d);
    for run in ["r1", "r2", "r3"] {
        s.events
            .append(penelope_kernel::event::EventDraft::new(
                "run.done",
                json!({"run": run}),
            ))
            .await
            .unwrap();
        clock.advance_ms(60_000);
        tick(&d, &ports).await.unwrap();
    }
    assert_eq!(s.turns.pending_count().await.unwrap(), 0);
    clock.advance_ms(3 * 3_600_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(s.turns.pending_count().await.unwrap(), 1, "un seul tour");
    let turn = s.turns.claim("t").await.unwrap().unwrap();
    let text = turn.payload["text"].as_str().unwrap();
    assert!(
        ["r1", "r2", "r3"].iter().all(|r| text.contains(r)) && text.contains("observed_at"),
        "{text}"
    );
    assert_eq!(turn.payload["lane"], "heures_calmes");
}
