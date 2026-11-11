//! #425, décision 0025 : `backlog` confie chaque tâche à une mission par `story`. Les
//! missions partent une à une, dans l'ordre ; une mission qui ne finit pas arrête la
//! liste ; annuler le run arrête sa mission ; un `tache` d'instance est signalé.

use super::*;
use penelope_app::mission::{self, State};

/// Un dépôt dans le workspace par défaut, où une mission a le droit de travailler.
fn depot(e: &Env) -> std::path::PathBuf {
    let dir =
        e.d.services
            .platform
            .dirs
            .data()
            .join("workspace")
            .join("depot");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Deux tâches de la même épique, déroulées par `story`.
async fn start_list(e: &Env) -> Run {
    let repo = depot(e);
    let mut raw = wf(
        "deux-taches",
        "liste",
        json!([{
            "id": "liste", "type": "foreach", "workflowId": "story",
            "items": [{"title": "écrire le parseur", "epic": "E1"},
                      {"title": "brancher le parseur", "epic": "E1"}],
            "params": {"depot": repo.to_string_lossy(), "tracker": "fichier"},
            "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                {"goto": "$blocked"}]
        }]),
    );
    raw["settings"]["budget"]["maxWallMs"] = json!(0);
    raw["settings"]["budget"]["maxUsd"] = json!(100.0);
    install(&e.d, raw).await;
    start_run(&e.d, "deux-taches", json!({}), &owner(), None, 0)
        .await
        .unwrap()
}

async fn pump(d: &Context) {
    for _ in 0..12 {
        for r in d
            .services
            .runs
            .list(Some(RunState::Running), 50)
            .await
            .unwrap()
        {
            drive(d, &r.id).await.unwrap();
        }
    }
}

/// Les missions lancées, dans l'ordre : (session, objectif, état).
async fn missions(s: &Services) -> Vec<(String, String, State)> {
    let rows: Vec<String> = s
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT v FROM kv WHERE k LIKE 'mission.%' ORDER BY ts, k")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .await
        .unwrap();
    rows.iter()
        .filter_map(|r| serde_json::from_str::<mission::Mission>(r).ok())
        .map(|m| (m.session_id.clone(), m.goal.clone(), m.state))
        .collect()
}

async fn finish(s: &Services, session: &str, state: State) {
    let mut m = mission::load(s, session).await.unwrap();
    m.state = state;
    mission::save(s, &m).await.unwrap();
}

#[tokio::test]
async fn the_backlog_runs_one_mission_per_task_in_order() {
    let e = env().await;
    let s = &e.d.services;
    let run = start_list(&e).await;
    pump(&e.d).await;
    let first = missions(s).await;
    assert_eq!(first.len(), 1, "une mission à la fois : {first:?}");
    assert!(first[0].1.contains("écrire le parseur"), "{}", first[0].1);
    assert_eq!(first[0].2, State::Running);
    // La mission a son premier tour en file, sur sa propre session.
    assert_eq!(s.turns.queued_for(&first[0].0).await.unwrap(), 1);

    finish(s, &first[0].0, State::Done).await;
    pump(&e.d).await;
    let both = missions(s).await;
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(both[1].1.contains("brancher le parseur"));
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Running
    );

    finish(s, &both[1].0, State::Done).await;
    pump(&e.d).await;
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Done
    );
}

#[tokio::test]
async fn a_mission_that_does_not_finish_stops_the_list() {
    let e = env().await;
    let s = &e.d.services;
    let run = start_list(&e).await;
    pump(&e.d).await;
    let first = missions(s).await;
    finish(s, &first[0].0, State::Blocked).await;
    pump(&e.d).await;
    assert_eq!(missions(s).await.len(), 1, "la deuxième tâche ne part pas");
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Blocked
    );
}

#[tokio::test]
async fn cancelling_the_backlog_stops_the_mission_of_its_story() {
    let e = env().await;
    let s = &e.d.services;
    let run = start_list(&e).await;
    pump(&e.d).await;
    let first = missions(s).await;
    control(&e.d, &run.id, &penelope_workflow::Control::Cancel)
        .await
        .unwrap();
    assert_eq!(missions(s).await[0].2, State::Stopped);
    assert_eq!(s.turns.queued_for(&first[0].0).await.unwrap(), 0);
}

/// Un run d'une seule étape `mission`, budget du run à 1 $, celui de la mission à 10 $.
async fn start_one(e: &Env) -> Run {
    let repo = depot(e);
    let mut raw = wf(
        "une-mission",
        "m",
        json!([{
            "id": "m", "type": "mission", "prompt": "écrire le parseur",
            "cwd": repo.to_string_lossy(), "budget": {"maxUsd": 10.0},
            "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                {"goto": "$blocked"}]
        }]),
    );
    raw["settings"]["budget"] = json!({"maxUsd": 1.0, "maxTokens": 0, "maxWallMs": 0});
    install(&e.d, raw).await;
    start_run(&e.d, "une-mission", json!({}), &owner(), None, 0)
        .await
        .unwrap()
}

/// Deux dollars dépensés par les tours de la mission : dans sa session, sans `run_id`.
async fn spend(s: &Services, session: &str) {
    s.budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(session.to_string()),
            model: "m".into(),
            provider: "p".into(),
            prompt: 1_000,
            completion: 100,
            cost_usd: 2.0,
            ..Default::default()
        })
        .await
        .unwrap();
}

async fn state_of(s: &Services, run: &Run) -> RunState {
    s.runs.get(&run.id).await.unwrap().unwrap().state
}

/// #456 : la liste en pause, la mission de sa story codait et dépensait encore. Elle
/// attend avec le run, sans être arrêtée, et la reprise la relance dans la même session.
#[tokio::test]
async fn pausing_the_list_parks_its_mission_and_resuming_relaunches_it() {
    use penelope_workflow::Control;
    let e = env().await;
    let s = &e.d.services;
    let run = start_list(&e).await;
    pump(&e.d).await;
    let session = missions(s).await[0].0.clone();
    control(&e.d, &run.id, &Control::Pause).await.unwrap();
    // Aucun tour en cours : la relance en file est annulée, la mission rangée.
    assert!(mission::load(s, &session).await.unwrap().parked());
    assert_eq!(s.turns.queued_for(&session).await.unwrap(), 0);
    // La story, elle, est toujours pilotée : elle ne relance rien tant que la liste attend.
    pump(&e.d).await;
    assert!(mission::load(s, &session).await.unwrap().parked());
    assert_eq!(s.turns.queued_for(&session).await.unwrap(), 0);

    control(&e.d, &run.id, &Control::Resume).await.unwrap();
    pump(&e.d).await;
    let resumed = mission::load(s, &session).await.unwrap();
    assert_eq!(resumed.state, State::Running);
    assert!(resumed.hold.is_none());
    assert_eq!(s.turns.queued_for(&session).await.unwrap(), 1);
    assert_eq!(missions(s).await.len(), 1, "la même mission, pas une autre");

    finish(s, &session, State::Done).await;
    pump(&e.d).await;
    assert_eq!(missions(s).await.len(), 2, "la liste continue");
}

/// #456 : les tours de la mission tournent dans sa session et le run ne les comptait pas.
/// Ils entrent dans sa dépense ; au-delà de son budget il se bloque, sa mission attend et
/// le motif la nomme. Le budget relevé, la reprise la relance.
#[tokio::test]
async fn a_run_pays_for_its_mission_and_blocks_with_it() {
    use penelope_workflow::Control;
    let e = env().await;
    let s = &e.d.services;
    let run = start_one(&e).await;
    pump(&e.d).await;
    let session = missions(s).await[0].0.clone();
    assert_eq!(state_of(s, &run).await, RunState::Running);
    spend(s, &session).await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let blocked = s.runs.get(&run.id).await.unwrap().unwrap();
    assert!(
        (blocked.spent_usd - 2.0).abs() < 1e-9,
        "{}",
        blocked.spent_usd
    );
    let why = blocked.error.clone().unwrap_or_default();
    assert!(
        why.contains("budget de 1.00 $") && why.contains(&session),
        "{why}"
    );
    let parked = mission::load(s, &session).await.unwrap();
    assert!(parked.parked(), "bloquée avec son run, pas arrêtée");
    assert_eq!(s.turns.queued_for(&session).await.unwrap(), 0);

    assert!(control(&e.d, &run.id, &Control::Resume).await.is_err());
    raise_budget(&e.d, &run.id, &json!({"usd": 5.0}))
        .await
        .unwrap();
    control(&e.d, &run.id, &Control::Resume).await.unwrap();
    pump(&e.d).await;
    assert_eq!(state_of(s, &run).await, RunState::Running);
    assert_eq!(
        mission::load(s, &session).await.unwrap().state,
        State::Running
    );
    assert_eq!(s.turns.queued_for(&session).await.unwrap(), 1);
}

/// Le tour en cours finit avant le blocage : le run attend la mission, et si elle a fini
/// pendant ce tour, son étape rend le résultat au lieu de laisser un run bloqué sur une
/// tâche terminée.
#[tokio::test]
async fn a_run_over_budget_lets_the_turn_end_and_concludes_a_finished_mission() {
    let e = env().await;
    let s = &e.d.services;
    let run = start_one(&e).await;
    pump(&e.d).await;
    let session = missions(s).await[0].0.clone();
    assert!(s.turns.claim("essai").await.unwrap().is_some());
    spend(s, &session).await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    let asked = mission::load(s, &session).await.unwrap();
    assert_eq!(asked.state, State::Running, "son tour finit");
    assert!(asked.hold.is_some(), "puis elle attendra");

    finish(s, &session, State::Done).await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
}

/// Annuler un run en pause arrête la mission rangée ; passer l'étape `mission` aussi.
#[tokio::test]
async fn cancelling_or_skipping_stops_a_parked_mission() {
    use penelope_workflow::Control;
    for last in [Control::Cancel, Control::SkipStep] {
        let e = env().await;
        let s = &e.d.services;
        let run = start_one(&e).await;
        pump(&e.d).await;
        let session = missions(s).await[0].0.clone();
        control(&e.d, &run.id, &Control::Pause).await.unwrap();
        assert!(mission::load(s, &session).await.unwrap().parked());
        control(&e.d, &run.id, &last).await.unwrap();
        let stopped = mission::load(s, &session).await.unwrap();
        assert_eq!(stopped.state, State::Stopped, "{last:?}");
        assert!(stopped.hold.is_none(), "{last:?}");
    }
}

#[tokio::test]
async fn an_instance_tache_still_loads_and_is_reported() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "tache",
            "un",
            json!([{"id": "un", "type": "shell", "command": "true",
                    "transitions": [{"goto": "$done"}]}]),
        ),
    )
    .await;
    assert!(e.d.services.workflows.get("tache").is_some());
    let report = super::super::step_mission::retired_report(&e.d.services).await;
    assert_eq!(report.len(), 1, "{report:?}");
    assert!(report[0].contains("`tache`") && report[0].contains("0025"));
}
