//! Heures calmes (#296) : les planifications attendent la fin de la plage et partent
//! groupées, une fois ; `urgent` passe ; la file persistée part avec elles.

use super::*;
use penelope_kernel::config::TimeRange;

/// Le harnais des autres tests éteint les heures calmes ; ici, `22:00-07:00` à La
/// Réunion, et l'horloge de test est à 4 h du matin.
async fn quiet_harness() -> (Harness, Arc<TestClock>, Arc<RecordingMessenger>) {
    let (d, clock, rec) = harness().await;
    d.services
        .publish_config("test", |c| {
            c.telegram.quiet_hours = "22:00-07:00".into();
            Ok(vec!["telegram.quiet_hours".into()])
        })
        .unwrap();
    (d, clock, rec)
}

fn events_of(events: &[penelope_kernel::event::Event], kind: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.payload.clone())
        .collect()
}

/// Trois planifications de nuit (une récurrente, un rappel daté, un prompt) restent dues
/// jusqu'à 7 h, sans rien envoyer ; une quatrième, `urgent`, part à l'heure. À 7 h, un
/// seul message groupé sous l'en-tête, chaque planification une fois avec ses créneaux
/// fusionnés ; le prompt part avec la mention ; le rappel daté est fini.
#[tokio::test]
async fn night_schedules_wait_and_leave_grouped_once_at_the_end_of_quiet_hours() {
    let (d, clock, rec) = quiet_harness().await;
    let s = &d.services;
    let ports = d.scheduler();
    let create = |kind: TriggerKind, spec: Value, target: Value| {
        let s = s.clone();
        async move {
            s.schedules
                .create(kind, spec, target, json!({}))
                .await
                .unwrap()
        }
    };
    let veille = create(
        TriggerKind::Cron,
        json!({"expr": "0,30 * * * *"}),
        json!({"type": "notify", "template": "📰 Veille"}),
    )
    .await;
    let paul = create(
        TriggerKind::Cron,
        json!({"expr": "0 5 * * *", "once": true}),
        json!({"type": "notify", "template": "⏰ Appeler Paul"}),
    )
    .await;
    let revue = create(
        TriggerKind::Interval,
        json!({"every_ms": 3_600_000}),
        json!({"type": "prompt", "prompt": "Prépare la revue de presse", "label": "Revue de presse"}),
    )
    .await;
    let urgent = create(
        TriggerKind::Interval,
        json!({"every_ms": 1_800_000, "urgent": true}),
        json!({"type": "notify", "template": "💊 Médicament"}),
    )
    .await;

    // 4h30 : seule l'urgente part ; la veille est due et retenue, une fois journalisée.
    clock.advance_ms(30 * 60_000 + 500);
    let report = tick(&d, &ports).await.unwrap();
    assert_eq!(report.fired, vec![urgent.id.clone()], "{report:?}");
    assert_eq!(report.held, vec![veille.id.clone()], "{report:?}");
    assert_eq!(rec.texts(), vec!["💊 Médicament".to_string()]);

    // 5h00 : le rappel et le prompt sont dus aussi ; rien ne part, le créneau de la
    // veille reste celui de 4h30 (dans la base : un redémarrage n'y change rien).
    clock.advance_ms(30 * 60_000);
    let report = tick(&d, &ports).await.unwrap();
    assert_eq!(report.fired, vec![urgent.id.clone()]);
    assert_eq!(
        report.held,
        vec![veille.id.clone(), paul.id.clone(), revue.id.clone()],
        "{report:?}"
    );
    assert_eq!(rec.texts().len(), 2, "{:?}", rec.texts());
    assert!(
        s.schedules
            .get(&veille.id)
            .await
            .unwrap()
            .unwrap()
            .next_run
            .unwrap()
            .ends_with("00:30:00.000Z"),
        "le créneau retenu reste dû"
    );
    assert_eq!(
        s.turns.pending_count().await.unwrap(),
        0,
        "le prompt attend"
    );
    clock.advance_ms(3_600_000);
    tick(&d, &ports).await.unwrap();
    let events = s.events.range(0, 1000).await.unwrap();
    let held = events_of(&events, "schedule.held");
    assert_eq!(held.len(), 3, "une fois par créneau retenu : {held:?}");
    assert_eq!(held[0]["schedule"], veille.id);
    assert_eq!(held[0]["until"], "07:00");

    // 7h00 : tout part, groupé, une fois.
    clock.advance_ms(3_600_000);
    let report = tick(&d, &ports).await.unwrap();
    assert!(report.held.is_empty(), "{report:?}");
    assert_eq!(
        report.fired,
        vec![
            veille.id.clone(),
            paul.id.clone(),
            revue.id.clone(),
            urgent.id.clone()
        ],
        "{report:?}"
    );
    let sent = rec.sent();
    // Quatre « 💊 » (4h30, 5h00, 6h00, 7h00), puis un seul message groupé.
    let grouped: Vec<&(Origin, String)> =
        sent.iter().filter(|(_, t)| t.starts_with("🌙")).collect();
    assert_eq!(grouped.len(), 1, "{sent:?}");
    assert_eq!(sent.len(), 5, "{sent:?}");
    assert!(matches!(grouped[0].0, Origin::Telegram { chat_id: 42, .. }));
    assert_eq!(
        grouped[0].1,
        "🌙 Pendant les heures calmes :\n\n\
         📰 Veille\n(prévue à 4h30 ; les 6 créneaux manqués partent en une seule livraison)\n\n\
         ⏰ Appeler Paul\n(prévue à 5h00)\n\n\
         Revue de presse (prévue à 5h00 ; les 3 créneaux manqués partent en une seule \
         livraison) : elle part maintenant, sa réponse suivra."
    );
    let turn = s.turns.claim("t").await.unwrap().expect("tour du prompt");
    assert_eq!(
        turn.payload["text"],
        "🌙 Pendant les heures calmes : prévue à 5h00, retenue jusqu'à 7h00. Les 3 créneaux \
         manqués partent en une seule exécution.\n\nPrépare la revue de presse"
    );
    assert_eq!(
        s.schedules.get(&paul.id).await.unwrap().unwrap().state,
        "done"
    );
    let events = s.events.range(0, 1000).await.unwrap();
    let fired = events_of(&events, "schedule.fired");
    assert_eq!(
        fired
            .iter()
            .filter(|e| e["quiet"] == true && e["late"].is_string())
            .count(),
        3,
        "{fired:?}"
    );
    let notified = events_of(&events, "schedule.notified");
    assert_eq!(
        notified.iter().filter(|e| e["quiet"] == true).count(),
        2,
        "{notified:?}"
    );
    assert_eq!(events_of(&events, "quiet.delivered")[0]["items"], 3);

    // Le passage suivant n'a plus rien de dû : un créneau manqué de nuit n'est annoncé
    // qu'une fois, et le suivant se compte depuis la fin de la plage.
    let report = tick(&d, &ports).await.unwrap();
    assert!(
        report.fired.is_empty() && report.held.is_empty(),
        "{report:?}"
    );
    assert_eq!(rec.sent().len(), 5);
    assert!(
        s.schedules
            .get(&veille.id)
            .await
            .unwrap()
            .unwrap()
            .next_run
            .unwrap()
            .ends_with("03:30:00.000Z"),
        "prochain créneau : 7h30"
    );
}

/// Une livraison retenue ailleurs (alerte MCP) attend dans la base et part avec la fournée,
/// avec l'heure où elle est née ; seule, elle a quand même son en-tête ; une fois partie,
/// la file est vide.
#[tokio::test]
async fn queued_alerts_leave_with_the_batch_at_the_end_of_quiet_hours() {
    let (d, clock, rec) = quiet_harness().await;
    let s = &d.services;
    let ports = d.scheduler();
    let owner = owner_origin_of(s);
    penelope_app::quiet::hold(s, "mcp_notice", &owner, "🔧 L'outil `jira.update` a changé")
        .await
        .unwrap();
    tick(&d, &ports).await.unwrap();
    clock.advance_ms(2 * 3_600_000);
    tick(&d, &ports).await.unwrap();
    assert!(rec.texts().is_empty(), "rien ne part la nuit");
    assert_eq!(penelope_app::quiet::held_count(s).await.unwrap(), 1);

    clock.advance_ms(3_600_000 + 1_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(
        rec.texts(),
        vec![
            "🌙 Pendant les heures calmes :\n\n🔧 L'outil `jira.update` a changé\n(reçu à 4h00)"
                .to_string()
        ]
    );
    assert_eq!(penelope_app::quiet::held_count(s).await.unwrap(), 0);
    tick(&d, &ports).await.unwrap();
    assert_eq!(rec.texts().len(), 1, "une fois");
}

/// Les déclencheurs poussés (`event`, `watch_file`) sans `urgent` sont passés la nuit, et
/// rattrapent à 7 h ce qui s'est produit pendant la plage : aucun événement du journal
/// n'est perdu, l'empreinte du fichier est comparée à celle d'avant la nuit. Un `event`
/// urgent, lui, part la nuit.
#[tokio::test]
async fn pushed_triggers_catch_up_after_quiet_hours_without_losing_events() {
    let (d, clock, rec) = quiet_harness().await;
    let s = &d.services;
    let ports = d.scheduler();
    let path = d.dir.path().join("rapport.txt");
    std::fs::write(&path, "v1").unwrap();
    tick(&d, &ports).await.unwrap();
    for (spec, template) in [
        (json!({"event": "run.done"}), "✅ run terminé ({{payload}})"),
        (
            json!({"event": "run.done", "urgent": true}),
            "🚨 run terminé ({{payload}})",
        ),
        (
            json!({"path": path.display().to_string()}),
            "📄 {{path}} {{state}}",
        ),
    ] {
        let kind = if spec.get("path").is_some() {
            TriggerKind::WatchFile
        } else {
            TriggerKind::Event
        };
        s.schedules
            .create(
                kind,
                spec,
                json!({"type": "notify", "template": template}),
                json!({}),
            )
            .await
            .unwrap();
    }
    // Première observation du fichier, et le journal est amorcé.
    tick(&d, &ports).await.unwrap();
    assert!(rec.texts().is_empty());

    let run_done = |payload: Value| {
        let s = s.clone();
        async move {
            s.events
                .append(penelope_kernel::event::EventDraft::new("run.done", payload))
                .await
                .unwrap()
        }
    };
    run_done(json!({"run": "r1"})).await;
    clock.advance_ms(60_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(
        rec.texts(),
        vec!["🚨 run terminé ({\"run\":\"r1\"})".to_string()],
        "seule l'urgente parle la nuit"
    );
    assert!(
        s.kv_get("scheduler.quiet.pushed_from")
            .await
            .unwrap()
            .is_some(),
        "le curseur d'avant l'attente est gardé"
    );
    run_done(json!({"run": "r2"})).await;
    std::fs::write(&path, "version 2, plus longue").unwrap();
    clock.advance_ms(60_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(rec.texts().len(), 2, "{:?}", rec.texts());

    // 7h01 : les deux événements de la nuit et le fichier, en un message.
    clock.advance_ms(3 * 3_600_000);
    let report = tick(&d, &ports).await.unwrap();
    assert_eq!(report.fired.len(), 2, "{report:?}");
    let texts = rec.texts();
    assert_eq!(texts.len(), 3, "{texts:?}");
    let grouped = &texts[2];
    assert!(
        grouped.starts_with("🌙 Pendant les heures calmes :\n\n"),
        "{grouped}"
    );
    assert!(
        grouped
            .contains("✅ run terminé ({\"run\":\"r1\"})\n(constaté à la fin des heures calmes)")
            && grouped.contains("✅ run terminé ({\"run\":\"r2\"})")
            && grouped.contains("rapport.txt modifié\n(constaté à la fin des heures calmes)"),
        "{grouped}"
    );
    assert!(
        s.kv_get("scheduler.quiet.pushed_from")
            .await
            .unwrap()
            .is_none()
    );
    tick(&d, &ports).await.unwrap();
    assert_eq!(rec.texts().len(), 3, "rien deux fois");
}

/// La création d'une planification dont le premier passage tombe dans la plage le dit
/// et propose `urgent` ; avec `urgent`, ou hors plage, ou sans heures calmes, rien.
#[tokio::test]
async fn a_schedule_created_for_the_night_is_told_and_urgent_is_proposed() {
    let (d, _clock, _rec) = quiet_harness().await;
    let s = &d.services;
    let made = |spec: Value| {
        let s = s.clone();
        async move {
            create(
                &s,
                TriggerKind::Cron,
                spec,
                json!({"type": "notify", "template": "🚆 Train"}),
                json!({}),
            )
            .await
            .unwrap()
        }
    };
    let v = made(json!({"expr": "30 6 * * *", "once": true})).await;
    let note = v["heures_calmes"].as_str().expect("note");
    assert!(
        note.starts_with(
            "cette planification part à 06:30, pendant les heures calmes (22:00-07:00) : elle \
             sera retenue jusqu'à 07:00"
        ) && note.contains("`\"urgent\": true`"),
        "{note}"
    );
    let v = made(json!({"expr": "30 6 * * *", "once": true, "urgent": true})).await;
    assert!(v.get("heures_calmes").is_none(), "{v}");
    let v = made(json!({"expr": "30 9 * * *"})).await;
    assert!(v.get("heures_calmes").is_none(), "{v}");
    assert_eq!(
        s.schedules
            .create(
                TriggerKind::Cron,
                json!({"expr": "30 6 * * *", "urgent": "oui"}),
                json!({"type": "notify", "template": "x"}),
                json!({})
            )
            .await
            .unwrap_err(),
        "`urgent` est un booléen (vrai : part même pendant les heures calmes)"
    );
    // Le doctor dit la même chose, et ignore la planification urgente.
    let checks = penelope_ops::doctor::coherence_checks(s).await;
    let quiet: Vec<&str> = checks
        .iter()
        .filter(|c| c.detail.contains("heures calmes"))
        .map(|c| c.detail.as_str())
        .collect();
    assert_eq!(quiet.len(), 1, "{checks:?}");
    assert!(
        quiet[0].contains("part à 06:30")
            && quiet[0].contains("retenu jusqu'à 07:00")
            && quiet[0].contains("`\"urgent\": true`"),
        "{}",
        quiet[0]
    );
}

/// `late_of` avec la plage : un créneau qui y tombait est annoncé même deux minutes après,
/// pas un créneau hors plage ; un tir encore dans la plage (urgent) n'est pas « retenu » ;
/// les textes, avec et sans veille.
#[test]
fn a_held_slot_is_always_announced_and_says_so() {
    let sched = Schedule {
        id: "sch_1".into(),
        kind: TriggerKind::Cron,
        spec: json!({"expr": "58 6 * * *"}),
        target: json!({"type": "notify", "template": "x"}),
        dedup: json!({}),
        state: "active".into(),
        last_run: None,
        next_run: Some("2026-01-01T02:58:00.000Z".into()),
        runs: 0,
        last_error: None,
        failures_in_a_row: 0,
        alerted_reason: None,
    };
    let tz = "Indian/Reunion";
    let range = TimeRange::parse("22:00-07:00").unwrap();
    let planned = 1_767_236_280_000; // 2026-01-01T02:58:00Z = 6h58 à La Réunion
    // Deux minutes de retard : rien sans plage, tout avec.
    assert_eq!(late_of(&sched, planned + 2 * 60_000, tz, None, None), None);
    let late = late_of(&sched, planned + 2 * 60_000, tz, None, Some(&range)).unwrap();
    assert!(late.quiet && late.missed == 1, "{late:?}");
    assert_eq!(
        late_text(&late, planned + 2 * 60_000, tz),
        "🌙 Pendant les heures calmes : prévue à 6h58, retenue jusqu'à 7h00."
    );
    assert_eq!(held_text(&late, planned + 2 * 60_000, tz), "prévue à 6h58");
    // Encore dans la plage (un tir urgent) : pas retenu, pas en retard.
    assert_eq!(
        late_of(&sched, planned + 30_000, tz, None, Some(&range)),
        None
    );
    // Retenu, puis une veille qui couvre le créneau : les deux se disent.
    let wake = Wake {
        at_ms: planned + 2 * 3_600_000,
        slept_ms: 3 * 3_600_000,
    };
    let late = late_of(&sched, wake.at_ms, tz, Some(wake), Some(&range)).unwrap();
    assert_eq!(
        late_text(&late, wake.at_ms, tz),
        "🌙 Pendant les heures calmes : prévue à 6h58, retenue jusqu'à 8h58 après une \
         veille de 3 h 00."
    );
    // Le lendemain (daemon arrêté) : la date, et les créneaux fusionnés.
    let next_day = planned + 26 * 3_600_000;
    let late = late_of(&sched, next_day, tz, None, Some(&range)).unwrap();
    assert_eq!(
        late_text(&late, next_day, tz),
        "🌙 Pendant les heures calmes : prévue le 01/01 à 6h58, retenue jusqu'à 8h58. Les 2 \
         créneaux manqués partent en une seule exécution."
    );
    assert_eq!(
        held_text(&late, next_day, tz),
        "prévue le 01/01 à 6h58 ; les 2 créneaux manqués partent en une seule livraison"
    );
}
