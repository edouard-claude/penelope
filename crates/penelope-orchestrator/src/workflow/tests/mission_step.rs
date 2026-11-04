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
