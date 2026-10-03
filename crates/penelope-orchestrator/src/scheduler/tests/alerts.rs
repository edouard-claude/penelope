//! Alertes d'une planification en échec (#229) : au premier échec, au palier, au motif
//! qui change, et le rétablissement.

use super::*;

/// #229 : une planification cassée n'alerte plus à chaque exécution. Dix échecs au même
/// motif (identifiant de requête mis à part) : deux alertes, la première et le palier 5 ;
/// le succès qui suit dit « rétablie après 10 échecs ». Dans la série suivante, un motif
/// qui change alerte aussitôt, avec la longueur de la série.
#[tokio::test]
async fn a_failing_schedule_alerts_on_first_failure_new_reason_and_steps_only() {
    let (d, _clock, rec) = harness().await;
    let s = &d.services;
    let sched = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "prompt", "label": "Veille du matin", "prompt": "Veille"}),
            json!({}),
        )
        .await
        .unwrap();
    let ports = d.scheduler();
    let failed = |error: String| penelope_agent::TurnOutcome::Failed { error };
    let before = rec.texts().len();
    for i in 1..=10 {
        let error = format!("fournisseur indisponible (requête gen-17590000{i:02}-a1b2c3)");
        trigger_outcome(&d, &ports, &sched.id, &failed(error)).await;
    }
    let alerts: Vec<String> = rec.texts()[before..].to_vec();
    assert_eq!(alerts.len(), 2, "{alerts:?}");
    assert!(
        alerts[0].contains("n'a pas pu s'exécuter : erreur du modèle : fournisseur"),
        "{alerts:?}"
    );
    assert!(alerts[1].contains("(5 échecs de suite)"), "{alerts:?}");
    let after = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert_eq!(after.failures_in_a_row, 10, "{after:?}");
    let doctor = penelope_ops::doctor::schedules_check(s).await;
    assert!(doctor.detail.contains("10 échecs de suite"), "{doctor:?}");

    let answered = penelope_agent::TurnOutcome::Answered {
        text: "Veille faite.".into(),
        iterations: 1,
        cost_usd: 0.0,
    };
    trigger_outcome(&d, &ports, &sched.id, &answered).await;
    let texts = rec.texts();
    assert_eq!(texts.len(), before + 3, "{texts:?}");
    assert!(
        texts
            .last()
            .unwrap()
            .contains("« Veille du matin » est rétablie après 10 échecs"),
        "{texts:?}"
    );
    let after = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert_eq!((after.failures_in_a_row, after.runs), (0, 1), "{after:?}");

    // Un succès hors série ne dit rien.
    trigger_outcome(&d, &ports, &sched.id, &answered).await;
    assert_eq!(rec.texts().len(), before + 3);

    // Nouvelle série : premier échec alerté, le même motif tait, un autre alerte.
    trigger_outcome(&d, &ports, &sched.id, &failed("quota épuisé".into())).await;
    trigger_outcome(&d, &ports, &sched.id, &failed("quota épuisé".into())).await;
    assert_eq!(rec.texts().len(), before + 4);
    trigger_outcome(&d, &ports, &sched.id, &failed("clé refusée".into())).await;
    let texts = rec.texts();
    assert_eq!(texts.len(), before + 5, "{texts:?}");
    assert!(
        texts
            .last()
            .unwrap()
            .contains("(3 échecs de suite) : erreur du modèle : clé refusée"),
        "{texts:?}"
    );
    let events = s.events.range(0, 1000).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "schedule.failed")
            .count(),
        13,
        "chaque échec reste dans le journal, alerté ou non"
    );
}
