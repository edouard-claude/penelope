//! Contrôle d'un run : refaire ou passer l'étape courante, réponses hors de propos.

use super::*;
use penelope_workflow::Control;

fn question(id: &str) -> Value {
    wf(
        id,
        "choisir",
        json!([
            {"id": "choisir", "name": "On déploie ?", "type": "user",
             "template": "question", "choices": ["Oui", "Non"],
             "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "skipped"}},
                {"goto": "$blocked"}
             ]}
        ]),
    )
}

/// `retry-step` efface les marqueurs de la visite et compte la tentative : la question
/// repart au passage suivant du pilote, une seule fois.
#[tokio::test]
async fn retrying_a_step_asks_its_question_again() {
    let e = env().await;
    install(&e.d, question("refaire")).await;
    let run = start_run(&e.d, "refaire", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    assert_eq!(e.r.questions().len(), 1);

    for attempt in 1..=2 {
        assert_eq!(
            control(&e.d, &run.id, &Control::RetryStep).await.unwrap(),
            RunState::Running
        );
        let key = format!("wf.attempt.{}.choisir.0", run.id);
        assert_eq!(
            e.d.services.kv_get(&key).await.unwrap().as_deref(),
            Some(attempt.to_string().as_str()),
            "chaque reprise compte une tentative de plus"
        );
        drive(&e.d, &run.id).await.unwrap();
        drive(&e.d, &run.id).await.unwrap();
        assert_eq!(
            e.r.questions().len(),
            1 + attempt,
            "une question par reprise"
        );
    }
    let controls: Vec<Value> =
        e.d.services
            .events
            .range(0, 1000)
            .await
            .unwrap()
            .into_iter()
            .filter(|ev| ev.kind == "workflow.control")
            .map(|ev| ev.payload)
            .collect();
    assert_eq!(controls.len(), 2);
    assert_eq!(controls[0]["op"], "RetryStep");
    assert_eq!(controls[0]["state"], "running");
}

/// `skip-step` clôt l'étape courante sur le résultat `skipped` et suit la transition
/// qui l'attend, sans réponse du propriétaire.
#[tokio::test]
async fn skipping_a_step_follows_the_skipped_transition() {
    let e = env().await;
    install(&e.d, question("passer")).await;
    let run = start_run(&e.d, "passer", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    control(&e.d, &run.id, &Control::SkipStep).await.unwrap();
    let after = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(after.step_outputs["choisir"], json!({"skipped": true}));
    assert_eq!(
        after.current_step, None,
        "la transition `skipped` mène à `$done`"
    );
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    assert!(
        answer(&e.d, &run.id, "choisir.0", "Oui", None)
            .await
            .is_err(),
        "une réponse après coup ne rouvre rien"
    );
}

/// Une réponse ne vaut que pour une étape `user` ; un formulaire n'existe que pour la
/// visite en cours.
#[tokio::test]
async fn only_a_user_step_takes_an_answer() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "attente",
            "patienter",
            json!([
                {"id": "patienter", "type": "wait", "on": {"duration_ms": 3_600_000},
                 "transitions": [{"goto": "$done"}]}
            ]),
        ),
    )
    .await;
    let run = start_run(&e.d, "attente", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    let err = answer(&e.d, &run.id, "patienter.0", "Oui", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("n'attend pas de réponse"), "{err}");
    assert!(
        form_of(&e.d.services, &run.id, "patienter.0")
            .await
            .is_none()
    );
    assert!(
        form_of(&e.d.services, &run.id, "patienter.3")
            .await
            .is_none()
    );
    assert!(
        form_of(&e.d.services, "inconnu", "patienter.0")
            .await
            .is_none()
    );
    assert!(
        control(&e.d, "inconnu", &Control::Pause)
            .await
            .unwrap_err()
            .to_string()
            .contains("introuvable")
    );
    assert_eq!(
        control(&e.d, &run.id, &Control::Pause).await.unwrap(),
        RunState::Paused
    );
    assert_eq!(
        control(&e.d, &run.id, &Control::Resume).await.unwrap(),
        RunState::Running,
        "un run en pause reprend sans examen de budget"
    );
}
