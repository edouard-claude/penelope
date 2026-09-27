//! Étape `wait` : événement, cron, délai d'étape, tâche MCP mal désignée.

use super::*;

async fn waiting(e: &Env, id: &str, step: Value) -> Run {
    install(&e.d, wf(id, "attendre", json!([step]))).await;
    start_run(&e.d, id, json!({}), &owner(), None, 0)
        .await
        .unwrap()
}

fn outcomes() -> Value {
    json!([
        {"goto": "$done", "condition": {"type": "step_result", "result": "fired"}},
        {"goto": "$blocked"}
    ])
}

async fn output(e: &Env, run: &Run) -> Value {
    let run = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    run.step_outputs["attendre"].clone()
}

/// Seul un événement du nom attendu, journalisé après le début de l'attente, déclenche
/// l'étape ; sa charge passe en sortie.
#[tokio::test]
async fn a_wait_step_fires_on_the_named_event() {
    let e = env().await;
    let s = &e.d.services;
    s.events
        .append(EventDraft::new("essai.signal", json!({"n": 0})))
        .await
        .unwrap();
    let run = waiting(
        &e,
        "signal",
        json!({"id": "attendre", "type": "wait", "on": {"event": "essai.signal"},
               "transitions": outcomes()}),
    )
    .await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    s.events
        .append(EventDraft::new("essai.autre", json!({})))
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    s.events
        .append(EventDraft::new("essai.signal", json!({"n": 1})))
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let out = output(&e, &run).await;
    assert_eq!(out["event"], "essai.signal");
    assert_eq!(out["payload"], json!({"n": 1}));
}

#[tokio::test]
async fn a_wait_step_fires_at_its_next_cron_time() {
    let e = env().await;
    let run = waiting(
        &e,
        "horaire",
        json!({"id": "attendre", "type": "wait", "on": {"cron": "*/5 * * * *"},
               "transitions": outcomes()}),
    )
    .await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    // Sous la borne de durée du run (une heure).
    e.clock.advance_ms(10 * 60_000);
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    assert!(output(&e, &run).await["at_ms"].as_i64().unwrap() > 1_789_516_800_000);
}

/// Le délai de l'étape passe avant la condition : l'échéance donne `timeout`.
#[tokio::test]
async fn a_wait_step_times_out_after_its_own_delay() {
    let e = env().await;
    let run = waiting(
        &e,
        "delai",
        json!({"id": "attendre", "type": "wait", "on": {"event": "jamais.vu"},
               "timeoutMs": 60_000, "transitions": outcomes()}),
    )
    .await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    e.clock.advance_ms(61_000);
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    assert!(output(&e, &run).await["waited_ms"].as_i64().unwrap() >= 60_000);
}

#[tokio::test]
async fn a_wait_on_an_unnamed_mcp_task_is_an_error() {
    let e = env().await;
    for (id, spec) in [
        ("tache-texte", json!("sans-deux-points")),
        ("tache-objet", json!({"server": "redmine"})),
        ("tache-nombre", json!(7)),
    ] {
        let run = waiting(
            &e,
            id,
            json!({"id": "attendre", "type": "wait", "on": {"mcp_task": spec},
                   "transitions": outcomes()}),
        )
        .await;
        assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
        let err = output(&e, &run).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(err.contains("serveur ou tâche absent"), "{id} : {err}");
    }
}
