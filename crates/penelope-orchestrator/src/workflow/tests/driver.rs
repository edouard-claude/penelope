//! Pilote : admission des runs en file, relances d'une étape en échec.

use super::*;
use penelope_workflow::Control;

fn waiting(id: &str, admission: &str) -> Value {
    let mut raw = wf(
        id,
        "choisir",
        json!([
            {"id": "choisir", "name": "On y va ?", "type": "user",
             "template": "question", "choices": ["Oui"],
             "transitions": [{"goto": "$done"}]}
        ]),
    );
    raw["settings"]["concurrency"] = json!({"maxConcurrent": 1, "admission": admission});
    raw
}

/// `hold` : le second run attend en pause ; le premier annulé, un passage du pilote
/// l'admet et le fait tourner.
#[tokio::test]
async fn a_held_run_is_admitted_once_a_place_frees_up() {
    let e = env().await;
    let s = &e.d.services;
    install(&e.d, waiting("file", "hold")).await;
    let first = start_run(&e.d, "file", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    let second = start_run(&e.d, "file", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(second.state, RunState::Paused);

    drive_all(&e.d).await.unwrap();
    let state = |id: String| async move { s.runs.get(&id).await.unwrap().unwrap().state };
    assert_eq!(
        state(second.id.clone()).await,
        RunState::Paused,
        "la place est prise"
    );

    control(&e.d, &first.id, &Control::Cancel).await.unwrap();
    drive_all(&e.d).await.unwrap();
    assert_eq!(state(second.id.clone()).await, RunState::Running);
    assert!(
        s.kv_get(&format!("wf.held.{}", second.id))
            .await
            .unwrap()
            .is_none(),
        "la marque de file tombe à l'admission"
    );

    // Une pause du propriétaire n'est pas une file : le pilote ne la relance pas.
    control(&e.d, &second.id, &Control::Pause).await.unwrap();
    drive_all(&e.d).await.unwrap();
    assert_eq!(state(second.id.clone()).await, RunState::Paused);
}

/// `coalesce` rend le run déjà actif, `drop` refuse la demande.
#[tokio::test]
async fn coalesce_reuses_the_active_run_and_drop_refuses() {
    let e = env().await;
    install(&e.d, waiting("fusion", "coalesce")).await;
    install(&e.d, waiting("unique", "drop")).await;
    let a = start_run(&e.d, "fusion", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    let b = start_run(&e.d, "fusion", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(a.id, b.id);

    start_run(&e.d, "unique", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    let err = start_run(&e.d, "unique", json!({}), &owner(), None, 0)
        .await
        .unwrap_err();
    assert!(err.contains("admission `drop`"), "{err}");
    let err = start_run(&e.d, "fusion", json!(3), &owner(), None, 0)
        .await
        .unwrap_err();
    assert!(err.contains("attendus en objet"), "{err}");
}

/// `retry` relance une étape en échec, tentative comptée, avant que l'échec ne choisisse
/// la transition.
#[tokio::test]
async fn a_failing_step_is_retried_before_its_failure_counts() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "relance",
            "essayer",
            json!([
                {"id": "essayer", "type": "shell", "command": "echo essai >> essais.txt; exit 3",
                 "retry": {"max": 2, "backoffMs": 1},
                 "transitions": [
                    {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                    {"goto": "$blocked"}
                 ]}
            ]),
        ),
    )
    .await;
    let run = start_run(&e.d, "relance", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let done = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    let tries = std::fs::read_to_string(
        std::path::Path::new(done.workdir.as_deref().unwrap()).join("essais.txt"),
    )
    .unwrap();
    assert_eq!(tries.lines().count(), 3, "un essai et deux relances");
    assert_eq!(done.step_outputs["essayer"]["exitCode"], 3);
    // #220 : la raison du blocage est aussi celle du run, pas seulement de l'événement.
    let why = done.error.clone().unwrap_or_default();
    assert!(why.contains("au résultat `failure`"), "{why}");
    let finished =
        e.d.services
            .events
            .range(0, 1000)
            .await
            .unwrap()
            .into_iter()
            .find(|ev| ev.kind == "workflow.finished")
            .unwrap();
    assert_eq!(finished.payload["state"], "blocked");
    assert!(
        finished.payload["reason"]
            .as_str()
            .unwrap()
            .contains("au résultat `failure`"),
        "{}",
        finished.payload
    );
}

/// #228 : un run piloté tient l'assertion anti-veille pendant ses étapes et la relâche
/// quand il finit, bloqué compris.
#[tokio::test]
async fn a_driven_run_keeps_the_machine_awake_until_it_stops() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "eveil",
            "analyser",
            json!([
                {"id": "analyser", "type": "agent", "prompt": "Analyse.",
                 "transitions": [{"goto": "echouer"}]},
                {"id": "echouer", "type": "shell", "command": "exit 3",
                 "transitions": [
                    {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                    {"goto": "$blocked"}
                 ]}
            ]),
        ),
    )
    .await;
    let power = e.d.services.platform.clone();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (during, log) = (power.clone(), seen.clone());
    e.p.set_responder(Some(Arc::new(move |_| {
        log.lock().unwrap().push(during.power.active());
        Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "step_done".into(),
                arguments: json!({}),
            }],
        )
    })));
    let run = start_run(&e.d, "eveil", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    assert_eq!(
        seen.lock().unwrap().first(),
        Some(&1),
        "l'étape travaille sous l'assertion"
    );
    assert_eq!(power.power.active(), 0, "relâchée quand le run s'arrête");
}

/// La borne de durée mesure le travail du run, pas l'attente du propriétaire (#193) : une
/// question laissée deux heures ne bloque rien ; le temps qui suit la réponse compte.
#[tokio::test]
async fn the_wall_clock_bound_does_not_count_the_owner_wait() {
    let e = env().await;
    let mut raw = wf(
        "patient",
        "choisir",
        json!([
            {"id": "choisir", "name": "On y va ?", "type": "user",
             "template": "question", "choices": ["Oui"],
             "transitions": [{"goto": "attendre"}]},
            {"id": "attendre", "type": "wait", "on": {"event": "jamais.vu"},
             "transitions": [{"goto": "$done"}]}
        ]),
    );
    raw["settings"]["budget"]["maxWallMs"] = json!(600_000);
    install(&e.d, raw).await;
    let run = start_run(&e.d, "patient", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    e.clock.advance_ms(2 * 3_600_000);
    assert_eq!(
        drive(&e.d, &run.id).await.unwrap(),
        RunState::Running,
        "deux heures de réflexion ne sont pas du travail"
    );
    let r = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    let visit = format!("choisir.{}", r.iterations);
    answer(&e.d, &run.id, &visit, "Oui", None).await.unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    e.clock.advance_ms(5 * 60_000);
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    e.clock.advance_ms(6 * 60_000);
    assert_eq!(
        drive(&e.d, &run.id).await.unwrap(),
        RunState::Blocked,
        "onze minutes d'attente d'un événement dépassent la borne de dix"
    );
    let r = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert!(
        r.error
            .unwrap_or_default()
            .contains("durée maximale atteinte"),
        "la borne tient toujours pour le travail"
    );
}
