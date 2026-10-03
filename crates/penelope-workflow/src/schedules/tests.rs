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
        failures_in_a_row: 0,
        alerted_reason: None,
    };
    assert!(s.next_after(0, "UTC").is_none());
    s.validate("UTC").unwrap();
}

/// #229 : chaque chemin qui enregistre un échec allonge la série (déclenchement raté,
/// exécution ratée, tour raté) ; un déclenchement réussi l'attend ; un succès la clôt et
/// ne rend sa longueur que si une alerte en était partie.
#[tokio::test]
async fn every_failure_path_extends_the_streak_and_a_success_closes_it() {
    let clock = TestClock::default();
    let s = schedules(clock.clone());
    let sched = s
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 60000}),
            json!({"type":"prompt","prompt":"veille"}),
            json!({}),
        )
        .await
        .unwrap();
    let streak = |s: ScheduleStore, id: String| async move {
        s.get(&id).await.unwrap().unwrap().failures_in_a_row
    };
    s.advance(&sched.id, Some("canal injoignable"))
        .await
        .unwrap();
    s.mark_run(&sched.id, Some("serveur absent")).await.unwrap();
    s.record_outcome(&sched.id, Some("modèle en panne"))
        .await
        .unwrap();
    assert_eq!(streak(s.clone(), sched.id.clone()).await, 3);
    s.advance(&sched.id, None).await.unwrap();
    assert_eq!(
        streak(s.clone(), sched.id.clone()).await,
        3,
        "un tour lancé n'est pas encore un succès"
    );
    assert_eq!(
        s.record_outcome(&sched.id, None).await.unwrap(),
        None,
        "aucune alerte partie : rien à annoncer"
    );
    assert_eq!(streak(s.clone(), sched.id.clone()).await, 0);

    s.record_outcome(&sched.id, Some("modèle en panne"))
        .await
        .unwrap();
    assert_eq!(
        s.alert_due(&sched.id, "modèle en panne").await.unwrap(),
        Some(1)
    );
    s.record_outcome(&sched.id, Some("modèle en panne"))
        .await
        .unwrap();
    assert_eq!(
        s.alert_due(&sched.id, "modèle en panne").await.unwrap(),
        None
    );
    assert_eq!(s.mark_run(&sched.id, None).await.unwrap(), Some(2));
    let after = s.get(&sched.id).await.unwrap().unwrap();
    assert_eq!((after.failures_in_a_row, after.alerted_reason), (0, None));
}

/// #229 : ce qui alerte dans une série au même motif (premier échec, paliers), et ce qui
/// fait un motif : sans identifiant ni horodatage, mais avec son code.
#[test]
fn failure_steps_and_motifs() {
    let steps: Vec<u32> = (1..=400).filter(|n| failure_step(*n)).collect();
    assert_eq!(steps, vec![5, 20, 100, 200, 300, 400]);
    assert_eq!(
        failure_motif("fournisseur indisponible (requête gen-1759000001-a1b2c3)"),
        failure_motif("fournisseur indisponible (requête gen-1759000002-a1b2c3)")
    );
    assert_eq!(
        failure_motif("tour annulé le 2026-09-27T08:00:00Z (session s_01K5N0Q5T3B9V8X2M4R7C6A1E0)"),
        "tour annulé le # (session #)"
    );
    assert_ne!(failure_motif("HTTP 429"), failure_motif("HTTP 500"));
    assert_ne!(failure_motif("quota épuisé"), failure_motif("clé refusée"));
}

/// #294 : un webhook porte un chemin `/hook/<jeton>` et le nom de son secret, jamais sa
/// valeur ; il n'a pas de prochain passage, c'est la requête qui le pousse.
#[tokio::test]
async fn a_webhook_carries_a_path_and_a_secret_name_and_never_a_next_run() {
    let s = schedules(TestClock::default());
    let spec =
        json!({"path": "/hook/abcdefghijklmnop1234", "secret_ref": "webhook_abcdefghijklmnop1234"});
    let sched = s
        .create(
            TriggerKind::Webhook,
            spec.clone(),
            json!({"type":"notify","template":"🔔 {{payload}}"}),
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(sched.next_run, None);
    assert_eq!(sched.webhook_path(), Some("/hook/abcdefghijklmnop1234"));
    assert!(sched.state == "active");
    assert_eq!(sched.next_after(0, "Indian/Reunion"), None);

    let refused = |spec: Value| {
        let s = &s;
        async move {
            s.create(
                TriggerKind::Webhook,
                spec,
                json!({"type":"notify","template":"x"}),
                json!({}),
            )
            .await
            .expect_err("refusé")
        }
    };
    assert!(refused(json!({})).await.contains("`path`"));
    assert!(
        refused(json!({"path": "/hook/court", "secret_ref": "webhook_court"}))
            .await
            .contains("<jeton>")
    );
    assert!(
        refused(json!({"path": "/autre/abcdefghijklmnop1234", "secret_ref": "x"}))
            .await
            .contains("<jeton>")
    );
    assert!(
        refused(json!({"path": "/hook/abcdefghijklmnop1234"}))
            .await
            .contains("`secret_ref`")
    );
    assert!(
        refused(json!({"path": "/hook/abcdefghijklmnop1234", "secret_ref": "a b"}))
            .await
            .contains("nom de secret invalide")
    );
    assert!(
        refused(
            json!({"path": "/hook/abcdefghijklmnop1234", "secret_ref": "n", "secret": "s3cret"})
        )
        .await
        .contains("magasin de secrets")
    );
    assert!(
        refused(json!({"path": "/hook/abcdefghijklmnop1234", "secret_ref": "n", "filter": 3}))
            .await
            .contains("`filter`")
    );
    assert_eq!(TriggerKind::parse("webhook"), Some(TriggerKind::Webhook));
    assert_eq!(TriggerKind::Webhook.as_str(), "webhook");
    assert!(sched.webhook_path().is_some());
    assert!(
        Schedule {
            kind: TriggerKind::Cron,
            ..sched
        }
        .webhook_path()
        .is_none()
    );
}

/// #293 : un `mcp_subscribe` exige `server` et `uri`, borne ses réglages, n'a pas de
/// prochain passage (il est poussé par les notifications) et se relit par son nom.
#[tokio::test]
async fn mcp_subscribe_is_validated_and_pushed_not_due() {
    let s = schedules(TestClock::default());
    assert_eq!(
        TriggerKind::parse("mcp_subscribe"),
        Some(TriggerKind::McpSubscribe)
    );
    assert_eq!(TriggerKind::McpSubscribe.as_str(), "mcp_subscribe");
    let target = json!({"type":"notify","template":"ticket_detected"});
    let err = s
        .create(
            TriggerKind::McpSubscribe,
            json!({"server":"pont"}),
            target.clone(),
            json!({}),
        )
        .await
        .unwrap_err();
    assert!(err.contains("`uri`"), "{err}");
    for (key, bad) in [("every_ms", 5_000), ("window_ms", 10), ("max_per_hour", 0)] {
        let err = s
            .create(
                TriggerKind::McpSubscribe,
                json!({"server":"pont","uri":"mail://inbox", key: bad}),
                target.clone(),
                json!({}),
            )
            .await
            .unwrap_err();
        assert!(err.contains(key), "{err}");
    }
    let ok = s
        .create(
            TriggerKind::McpSubscribe,
            json!({"server":"pont","uri":"mail://inbox","id_path":"id","every_ms":60_000}),
            target,
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(ok.next_run, None, "poussé, pas programmé");
    assert!(s.due().await.unwrap().is_empty());
    let again = s.get(&ok.id).await.unwrap().unwrap();
    assert_eq!(again.kind, TriggerKind::McpSubscribe);
    assert_eq!(again.spec["uri"], "mail://inbox");
}
