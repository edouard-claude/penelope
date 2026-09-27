use super::*;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

fn schedules(clock: TestClock) -> ScheduleStore {
    ScheduleStore::new(
        Store::open_memory().unwrap(),
        Arc::new(clock),
        "Indian/Reunion",
    )
}

fn poll_spec() -> Value {
    json!({
        "server":"redmine",
        "tool":"mcp__redmine__search_issues",
        "args":{"assigned_to":"me"},
        "every_ms":900000,
        "item_path":"issues",
        "id_path":"id"
    })
}

fn workflow_target() -> Value {
    json!({"type":"workflow","workflowId":"ticket-to-deploy","params":{}})
}

#[tokio::test]
async fn cron_schedules_compute_their_next_run() {
    let clock = TestClock::new(
        chrono::DateTime::parse_from_rfc3339("2026-09-16T10:00:00Z")
            .unwrap()
            .timestamp_millis(),
    );
    let s = schedules(clock);
    let sched = s
        .create(
            TriggerKind::Cron,
            json!({"expr":"30 3 * * *"}),
            json!({"type":"prompt","prompt":"consolide"}),
            json!({}),
        )
        .await
        .unwrap();
    // 03:30 à La Réunion = 23:30 UTC la veille.
    assert_eq!(sched.next_run.as_deref(), Some("2026-09-16T23:30:00.000Z"));
}

#[tokio::test]
async fn due_returns_only_ripe_schedules() {
    let clock = TestClock::default();
    let s = schedules(clock.clone());
    s.create(
        TriggerKind::Interval,
        json!({"every_ms": 3600000}),
        json!({"type":"notify","template":"heartbeat"}),
        json!({}),
    )
    .await
    .unwrap();
    assert!(s.due().await.unwrap().is_empty());
    clock.advance_hours(2);
    assert_eq!(s.due().await.unwrap().len(), 1);
}

#[tokio::test]
async fn validation_rejects_bad_specs() {
    let s = schedules(TestClock::default());
    assert!(
        s.create(
            TriggerKind::Cron,
            json!({"expr":"pas du cron"}),
            json!({"type":"prompt","prompt":"x"}),
            json!({})
        )
        .await
        .is_err()
    );
    assert!(
        s.create(
            TriggerKind::Interval,
            json!({"every_ms": 10}),
            json!({"type":"prompt","prompt":"x"}),
            json!({})
        )
        .await
        .unwrap_err()
        .contains("1000 ms")
    );
    assert!(
        s.create(
            TriggerKind::McpPoll,
            json!({"server":"x"}),
            workflow_target(),
            json!({})
        )
        .await
        .is_err()
    );
    assert!(
        s.create(
            TriggerKind::Cron,
            json!({"expr":"0 8 * * *"}),
            json!({"type":"workflow"}),
            json!({})
        )
        .await
        .unwrap_err()
        .contains("workflowId")
    );
}

#[test]
fn items_are_extracted_with_stable_fingerprints() {
    let result = json!({"issues":[
        {"id": 4312, "subject":"TVA", "updated_on":"2026-09-16"},
        {"id": 4313, "subject":"Facture"}
    ]});
    let items = extract_items(&result, "issues", "id");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "4312");

    // La même donnée donne la même empreinte, l'ordre des clés ne compte pas.
    let reordered = json!({"issues":[
        {"subject":"TVA","id": 4312,"updated_on":"2026-09-16"}
    ]});
    assert_eq!(
        extract_items(&reordered, "issues", "id")[0].fingerprint,
        items[0].fingerprint
    );

    // Un changement de contenu change l'empreinte.
    let changed = json!({"issues":[{"id":4312,"subject":"TVA corrigée"}]});
    assert_ne!(
        extract_items(&changed, "issues", "id")[0].fingerprint,
        items[0].fingerprint
    );
}

#[test]
fn nested_item_paths_and_filters() {
    let result = json!({"data":{"items":[{"ticket":{"id":"T-1"},"priority":"haute"}]}});
    let items = extract_items(&result, "data.items", "ticket.id");
    assert_eq!(items[0].id, "T-1");
    assert!(passes_filter(
        &items[0].value,
        Some(&json!({"priority":"haute"}))
    ));
    assert!(!passes_filter(
        &items[0].value,
        Some(&json!({"priority":"basse"}))
    ));
    assert!(passes_filter(&items[0].value, None));
}

/// CA 12 : le schedule `mcp_poll` ne déclenche qu'une fois par ticket, et à nouveau
/// si le ticket est modifié lorsque `retrigger_on_change = true`.
#[tokio::test]
async fn ca_12_4_poll_fires_once_per_item() {
    let s = schedules(TestClock::default());
    let sched = s
        .create(
            TriggerKind::McpPoll,
            poll_spec(),
            workflow_target(),
            json!({"retrigger_on_change": true}),
        )
        .await
        .unwrap();

    let first = extract_items(
        &json!({"issues":[{"id":4312,"subject":"TVA"},{"id":4313,"subject":"Facture"}]}),
        "issues",
        "id",
    );
    let fired = s.new_or_changed(&sched.id, &first, true).await.unwrap();
    assert_eq!(fired.len(), 2, "premier passage : les deux tickets");

    // Deuxième passage, rien n'a changé.
    let fired = s.new_or_changed(&sched.id, &first, true).await.unwrap();
    assert!(fired.is_empty(), "aucun redéclenchement sans changement");

    // Le ticket 4312 est modifié.
    let changed = extract_items(
        &json!({"issues":[{"id":4312,"subject":"TVA corrigée"},{"id":4313,"subject":"Facture"}]}),
        "issues",
        "id",
    );
    let fired = s.new_or_changed(&sched.id, &changed, true).await.unwrap();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].id, "4312");

    // Sans `retrigger_on_change`, une modification ne redéclenche pas.
    let changed2 = extract_items(
        &json!({"issues":[{"id":4312,"subject":"encore autre chose"}]}),
        "issues",
        "id",
    );
    assert!(
        s.new_or_changed(&sched.id, &changed2, false)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn seeding_marks_existing_items_without_firing() {
    let s = schedules(TestClock::default());
    let sched = s
        .create(
            TriggerKind::McpPoll,
            poll_spec(),
            workflow_target(),
            json!({}),
        )
        .await
        .unwrap();
    let existing = extract_items(
        &json!({"issues":[{"id":1},{"id":2},{"id":3}]}),
        "issues",
        "id",
    );
    assert_eq!(s.seed(&sched.id, &existing, false).await.unwrap(), 3);
    assert!(
        s.new_or_changed(&sched.id, &existing, true)
            .await
            .unwrap()
            .is_empty(),
        "les éléments amorcés ne déclenchent pas"
    );

    // Avec `backfill`, rien n'est amorcé : tout déclenche.
    let sched2 = s
        .create(
            TriggerKind::McpPoll,
            poll_spec(),
            workflow_target(),
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(s.seed(&sched2.id, &existing, true).await.unwrap(), 0);
    assert_eq!(
        s.new_or_changed(&sched2.id, &existing, true)
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn coalescing_groups_notifications_only() {
    let s = schedules(TestClock::default());
    let notify = s
        .create(
            TriggerKind::McpPoll,
            poll_spec(),
            json!({"type":"notify","template":"ticket_detected"}),
            json!({}),
        )
        .await
        .unwrap();
    let wf = s
        .create(
            TriggerKind::McpPoll,
            poll_spec(),
            workflow_target(),
            json!({}),
        )
        .await
        .unwrap();
    let items = extract_items(&json!({"issues":[{"id":1},{"id":2}]}), "issues", "id");

    assert_eq!(
        s.coalesce(&notify, &items).len(),
        1,
        "une seule notification"
    );
    assert_eq!(s.coalesce(&wf, &items).len(), 2, "un run par élément");
    assert!(s.coalesce(&notify, &[]).is_empty());
}

#[tokio::test]
async fn pause_and_resume() {
    let clock = TestClock::default();
    let s = schedules(clock.clone());
    let sched = s
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 60000}),
            json!({"type":"notify","template":"heartbeat"}),
            json!({}),
        )
        .await
        .unwrap();
    clock.advance_ms(120_000);
    assert_eq!(s.due().await.unwrap().len(), 1);

    s.set_state(&sched.id, "paused").await.unwrap();
    assert!(s.due().await.unwrap().is_empty());
    s.set_state(&sched.id, "active").await.unwrap();
    assert_eq!(s.due().await.unwrap().len(), 1);
}

#[tokio::test]
async fn mark_run_schedules_the_next_occurrence() {
    let clock = TestClock::default();
    let s = schedules(clock.clone());
    let sched = s
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 60000}),
            json!({"type":"notify","template":"heartbeat"}),
            json!({}),
        )
        .await
        .unwrap();
    clock.advance_ms(120_000);
    s.mark_run(&sched.id, None).await.unwrap();
    let after = s.get(&sched.id).await.unwrap().unwrap();
    assert_eq!(after.runs, 1);
    assert!(after.last_run.is_some());
    assert!(s.due().await.unwrap().is_empty(), "reprogrammé plus tard");
}

#[test]
fn event_and_watch_triggers_have_no_schedule() {
    let s = Schedule {
        id: "x".into(),
        kind: TriggerKind::Event,
        spec: json!({"event":"run.done"}),
        target: json!({"type":"notify","template":"run_done"}),
        dedup: json!({}),
        state: "active".into(),
        last_run: None,
        next_run: None,
        runs: 0,
        last_error: None,
    };
    assert!(s.next_after(0, "UTC").is_none());
    s.validate("UTC").unwrap();
}
