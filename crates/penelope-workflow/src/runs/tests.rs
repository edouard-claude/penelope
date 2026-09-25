use super::*;
use crate::model::{Metadata, Settings, Step, Transition};
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

fn workflow() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "demo".into(),
            ..Default::default()
        },
        entry_step: "un".into(),
        settings: Settings::default(),
        start_condition: json!({"type":"always"}),
        steps: vec![
            Step {
                id: "un".into(),
                kind: "shell".into(),
                command: json!("echo un"),
                transitions: vec![Transition::always("deux")],
                ..Default::default()
            },
            Step {
                id: "deux".into(),
                kind: "shell".into(),
                command: json!("echo deux"),
                transitions: vec![Transition::always(DONE)],
                ..Default::default()
            },
        ],
    }
}

async fn runs(clock: TestClock) -> RunStore {
    let store = Store::open_memory().unwrap();
    store
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES('s1','workflow_run','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    RunStore::new(store, Arc::new(clock))
}

#[tokio::test]
async fn create_advance_and_finish() {
    let rs = runs(TestClock::default()).await;
    let w = workflow();
    let run = rs
        .create(&w, "s1", json!({"x":1}), Some("/tmp/r1"), None, 0)
        .await
        .unwrap();
    assert_eq!(run.current_step.as_deref(), Some("un"));
    assert_eq!(run.state, RunState::Running);

    let r = rs
        .advance(
            &run.id,
            "un",
            &StepResult::Success,
            json!({"exitCode":0}),
            "deux",
            Some("build"),
        )
        .await
        .unwrap();
    assert_eq!(r.current_step.as_deref(), Some("deux"));
    assert_eq!(r.iterations, 1);
    assert_eq!(r.step_outputs["un"]["exitCode"], 0);
    assert_eq!(r.step_outputs["__last"]["exitCode"], 0);

    let r = rs
        .advance(&run.id, "deux", &StepResult::Success, json!({}), DONE, None)
        .await
        .unwrap();
    assert_eq!(r.state, RunState::Done);
    assert!(r.finished_at.is_some());
    assert!(r.current_step.is_none());
}

#[tokio::test]
async fn blocked_runs_can_be_resumed() {
    let rs = runs(TestClock::default()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();
    let r = rs
        .advance(
            &run.id,
            "un",
            &StepResult::Failure,
            json!({}),
            BLOCKED,
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.state, RunState::Blocked);

    rs.control(&run.id, &Control::Resume).await.unwrap();
    assert_eq!(
        rs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Running
    );
}

/// CA 12 : `kill -9` à chaque étape ⇒ reprise sans double effet.
#[tokio::test]
async fn ca_12_2_runs_are_recovered_at_their_current_step() {
    let clock = TestClock::default();
    let store = Store::open_memory().unwrap();
    store
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES('s1','workflow_run','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let rs = RunStore::new(store.clone(), Arc::new(clock.clone()));
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();
    rs.advance(
        &run.id,
        "un",
        &StepResult::Success,
        json!({"a":1}),
        "deux",
        None,
    )
    .await
    .unwrap();

    // « kill -9 » : nouvel exécuteur sur la même base.
    let rs2 = RunStore::new(store, Arc::new(clock));
    let recovered = rs2.recover_on_boot().await.unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, run.id);
    assert_eq!(
        recovered[0].current_step.as_deref(),
        Some("deux"),
        "la reprise repart de l'étape courante, pas du début"
    );
    assert_eq!(
        recovered[0].step_outputs["un"]["a"], 1,
        "les sorties déjà produites sont conservées"
    );
    assert_eq!(
        recovered[0].iterations, 1,
        "l'itération n'est pas recomptée"
    );
}

#[tokio::test]
async fn control_operations() {
    let rs = runs(TestClock::default()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();

    assert_eq!(
        rs.control(&run.id, &Control::Pause).await.unwrap(),
        RunState::Paused
    );
    assert!(
        !rs.get(&run.id)
            .await
            .unwrap()
            .unwrap()
            .state
            .is_advanceable()
    );
    assert_eq!(
        rs.control(&run.id, &Control::Resume).await.unwrap(),
        RunState::Running
    );

    rs.control(&run.id, &Control::Goto("deux".into()))
        .await
        .unwrap();
    assert_eq!(
        rs.get(&run.id)
            .await
            .unwrap()
            .unwrap()
            .current_step
            .as_deref(),
        Some("deux")
    );

    rs.control(&run.id, &Control::Cancel).await.unwrap();
    assert!(rs.get(&run.id).await.unwrap().unwrap().state.is_terminal());
}

#[test]
fn control_parsing_and_approval() {
    assert_eq!(Control::parse("pause"), Some(Control::Pause));
    assert_eq!(Control::parse("retry-step"), Some(Control::RetryStep));
    assert_eq!(
        Control::parse("goto:etape"),
        Some(Control::Goto("etape".into()))
    );
    assert!(Control::parse("inventé").is_none());
    assert!(Control::SkipStep.requires_approval());
    assert!(!Control::Pause.requires_approval());
}

#[tokio::test]
async fn trace_records_every_step() {
    let rs = runs(TestClock::default()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();
    rs.advance(
        &run.id,
        "un",
        &StepResult::Success,
        json!({"a":1}),
        "deux",
        None,
    )
    .await
    .unwrap();
    rs.advance(
        &run.id,
        "deux",
        &StepResult::Failure,
        json!({}),
        BLOCKED,
        None,
    )
    .await
    .unwrap();
    let t = rs.trace(&run.id).await.unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t[0]["step"], "un");
    assert_eq!(t[1]["result"], "failure");
}

#[tokio::test]
async fn admission_policies() {
    let rs = runs(TestClock::default()).await;
    let w = workflow();
    rs.create(&w, "s1", json!({}), None, None, 0).await.unwrap();

    assert_eq!(rs.admit("demo", 2, "hold").await.unwrap(), Admission::Start);
    assert_eq!(rs.admit("demo", 1, "hold").await.unwrap(), Admission::Hold);
    assert_eq!(rs.admit("demo", 1, "drop").await.unwrap(), Admission::Drop);
    assert_eq!(
        rs.admit("demo", 1, "coalesce").await.unwrap(),
        Admission::Coalesce
    );
    assert_eq!(
        rs.admit("demo", 1, "parallel").await.unwrap(),
        Admission::Start
    );
    assert_eq!(
        rs.admit("autre", 1, "hold").await.unwrap(),
        Admission::Start
    );
}

#[tokio::test]
async fn expired_workspaces_are_listed() {
    let clock = TestClock::default();
    let rs = runs(clock.clone()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), Some("/tmp/r1"), None, 0)
        .await
        .unwrap();
    rs.advance(&run.id, "un", &StepResult::Success, json!({}), DONE, None)
        .await
        .unwrap();
    assert!(rs.expired_workspaces(7).await.unwrap().is_empty());
    clock.advance_days(8);
    let expired = rs.expired_workspaces(7).await.unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].1, "/tmp/r1");
}

/// #177 : un run en pause conserve son workspace et doit être visible dans
/// l'inventaire de capacité, même si la rétention ne le sélectionne pas.
#[tokio::test]
async fn paused_workspaces_are_in_capacity_inventory() {
    let rs = runs(TestClock::default()).await;
    let paused = rs
        .create(&workflow(), "s1", json!({}), Some("/tmp/paused"), None, 0)
        .await
        .unwrap();
    rs.set_state(&paused.id, RunState::Paused, None)
        .await
        .unwrap();
    let done = rs
        .create(&workflow(), "s2", json!({}), Some("/tmp/done"), None, 0)
        .await
        .unwrap();
    rs.set_state(&done.id, RunState::Done, None).await.unwrap();

    let inventory = rs.active_workspaces().await.unwrap();
    assert_eq!(
        inventory,
        vec![(paused.id, RunState::Paused, "/tmp/paused".into())]
    );
}

#[test]
fn limits_are_checked() {
    let budget = crate::model::Budget {
        max_usd: 5.0,
        max_tokens: 1000,
        max_wall_ms: 60_000,
    };
    let base = Run {
        id: "r".into(),
        workflow_id: "w".into(),
        session_id: "s".into(),
        params: json!({}),
        state: RunState::Running,
        current_step: Some("un".into()),
        phase: None,
        iterations: 0,
        max_iterations: 40,
        step_outputs: json!({}),
        workdir: None,
        spent_usd: 0.0,
        spent_tokens: 0,
        started_at: "2026-01-01T00:00:00Z".into(),
        finished_at: None,
        result: None,
        error: None,
        parent_run: None,
        depth: 0,
    };
    let t0 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(check_limits(&base, &budget, t0), Limit::Ok);
    assert_eq!(
        check_limits(
            &Run {
                iterations: 40,
                ..base.clone()
            },
            &budget,
            t0
        ),
        Limit::IterationsExhausted
    );
    assert_eq!(
        check_limits(
            &Run {
                spent_usd: 6.0,
                ..base.clone()
            },
            &budget,
            t0
        ),
        Limit::BudgetUsd
    );
    assert_eq!(
        check_limits(
            &Run {
                spent_tokens: 2000,
                ..base.clone()
            },
            &budget,
            t0
        ),
        Limit::BudgetTokens
    );
    assert_eq!(check_limits(&base, &budget, t0 + 120_000), Limit::WallClock);
}
