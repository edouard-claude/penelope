//! #459 : la session d'un tir ou d'un run fini se ferme, pas celle qui attend encore.

use super::*;
use crate::scheduler::closing::GRACE_MS;
use penelope_executor::jobs::NewJob;
use penelope_hitl::policy::schedule_key;
use penelope_mcp::tasks::TaskState;
use penelope_workflow::RunState;

async fn open(s: &Services, kind: SessionKind) -> String {
    s.sessions.create(kind, None).await.unwrap().id.to_string()
}

async fn state_of(s: &Services, id: &str) -> String {
    s.sessions.get(id).await.unwrap().unwrap().state
}

/// Les clés `kv` qui portent le nom de la session.
async fn keys_of(s: &Services, id: &str) -> Vec<String> {
    let like = format!("%{id}");
    s.store
        .read(move |c| {
            let mut st = c.prepare("SELECT k FROM kv WHERE k LIKE ?1 ORDER BY k")?;
            let rows = st.query_map([like], |r| r.get(0))?;
            Ok(rows.collect::<Result<Vec<String>, _>>()?)
        })
        .await
        .unwrap()
}

/// Dix tirs d'une planification : chacun ouvre sa session, le suivant ferme la
/// précédente. Il en reste une ouverte au plus, puis aucune ; le transcript d'une session
/// fermée se relit, ses clés de travail sont parties, son lien à la planification reste.
#[tokio::test]
async fn ten_firings_leave_one_open_session_at_most() {
    let (d, clock, _rec) = harness().await;
    let s = &d.services;
    let ports = d.scheduler();
    let ws = penelope_executor::executor::default_workspaces(s)[0].clone();
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("vus.json"), "[]").unwrap();
    let sched = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "prompt", "prompt": "Fais le point"}),
            json!({}),
        )
        .await
        .unwrap();

    let mut fired = Vec::new();
    for n in 0..10 {
        clock.advance_ms(3_600_500);
        tick(&d, &ports).await.unwrap();
        let turn = s.turns.claim("t").await.unwrap().expect("un tour par tir");
        let sid = turn.session_id.clone();
        // Le tour travaille : un message, et les clés que posent les quatre crates.
        let answer = penelope_llm::types::ChatMessage::assistant(format!("Point {n}."));
        s.context
            .history
            .append(&sid, &answer, 3, 0, false, None)
            .await
            .unwrap();
        penelope_app::frozen_tools::freeze(s, &sid, &[]).await;
        penelope_executor::tools_on_demand::touch(s, &sid, "schedule_create").await;
        penelope_app::activity::commit(s, &sid, &Default::default())
            .await
            .unwrap();
        // Un état gardé avant un tour qui n'a jamais rendu son issue.
        save_state(&d, &sid, "vus.json").await;
        assert_eq!(
            keys_of(s, &sid).await.len(),
            5,
            "{:?}",
            keys_of(s, &sid).await
        );

        s.turns.complete(&turn).await.unwrap();
        fired.push(sid);
        let open: Vec<_> = s
            .sessions
            .active()
            .await
            .unwrap()
            .into_iter()
            .filter(|x| x.kind == SessionKind::Scheduled)
            .collect();
        assert_eq!(open.len(), 1, "tir {n} : seule la session du tir en cours");
    }
    assert_eq!(
        s.sessions
            .list(Some(SessionKind::Scheduled), 100)
            .await
            .unwrap()
            .len(),
        10,
        "les sessions finies restent lisibles"
    );

    clock.advance_ms(GRACE_MS + 1_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(s.sessions.count_active().await.unwrap(), 0);
    for sid in &fired {
        let session = s.sessions.get(sid).await.unwrap().unwrap();
        assert_eq!(session.state, "closed");
        assert!(session.closed_at.is_some());
        assert_eq!(
            s.context.history.load(sid, 0).await.unwrap().len(),
            1,
            "le transcript reste"
        );
        assert_eq!(
            keys_of(s, sid).await,
            vec![schedule_key(sid)],
            "seul le lien à la planification reste"
        );
    }
    assert_eq!(
        s.kv_get(&schedule_key(&fired[0])).await.unwrap().as_deref(),
        Some(sched.id.as_str())
    );
}

/// Une session qui attend n'est pas fermée : une carte posée, un job en cours ou à
/// livrer, un run ouvert (et la session d'étape qui en dépend). Elle l'est une fois
/// l'attente levée et le délai passé, pas avant : le tour de reprise n'est pas encore en
/// file quand la carte vient d'être tranchée (#379, #399).
#[tokio::test]
async fn a_session_waiting_on_a_card_a_job_or_a_run_stays_open() {
    let (d, clock, _rec) = harness().await;
    let s = &d.services;
    let ports = d.scheduler();
    let idle = open(s, SessionKind::Scheduled).await;
    let card = open(s, SessionKind::Scheduled).await;
    let job = open(s, SessionKind::Scheduled).await;
    let run = open(s, SessionKind::WorkflowRun).await;
    let step = s
        .sessions
        .create_with(SessionKind::WorkflowRun, None, Some(run.clone()), None)
        .await
        .unwrap()
        .id
        .to_string();

    let approval = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec"}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(card.as_str()),
            None,
            false,
        )
        .await
        .unwrap();
    let jobs = penelope_executor::jobs::store(s);
    let background = jobs
        .create(NewJob {
            session_id: job.clone(),
            run_id: None,
            turn_id: None,
            call_id: None,
            tool: "shell_exec".into(),
            request: json!({}),
            effect_id: None,
        })
        .await
        .unwrap();
    let wf = penelope_workflow::model::Workflow::from_json(
        &json!({
            "metadata": {"id": "attente", "name": "Attente", "parameters": []},
            "entryStep": "patienter",
            "settings": {"maxIterations": 10},
            "steps": [{"id": "patienter", "type": "wait", "on": {"duration_ms": 86_400_000},
                       "transitions": [{"goto": "$done"}]}],
        })
        .to_string(),
    )
    .unwrap();
    let waiting = s
        .runs
        .create(&wf, &run, json!({}), None, None, 0)
        .await
        .unwrap();

    clock.advance_ms(3_600_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(state_of(s, &idle).await, "closed", "rien ne la retient");
    for (name, sid) in [
        ("carte", &card),
        ("job", &job),
        ("run", &run),
        ("étape", &step),
    ] {
        assert_eq!(state_of(s, sid).await, "active", "{name} en attente");
    }

    // Les attentes se lèvent : rien ne ferme encore, la suite peut arriver.
    s.approvals
        .decide(
            approval.id.as_str(),
            &penelope_hitl::Decision::approve_once("cli"),
        )
        .await
        .unwrap();
    jobs.finish(&background.id, TaskState::Completed, Some(json!("ok")))
        .await
        .unwrap();
    s.runs
        .set_state(&waiting.id, RunState::Done, None)
        .await
        .unwrap();
    tick(&d, &ports).await.unwrap();
    for (name, sid) in [("carte", &card), ("run", &run), ("étape", &step)] {
        assert_eq!(state_of(s, sid).await, "active", "{name} : à l'instant");
    }
    clock.advance_ms(GRACE_MS + 1_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(
        state_of(s, &card).await,
        "closed",
        "carte tranchée, délai passé"
    );
    assert_eq!(state_of(s, &run).await, "closed", "run fini, délai passé");
    assert_eq!(state_of(s, &step).await, "closed");
    assert_eq!(
        state_of(s, &job).await,
        "active",
        "résultat pas encore livré"
    );

    assert!(jobs.mark_delivered(&background.id).await.unwrap());
    clock.advance_ms(GRACE_MS - 1_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(state_of(s, &job).await, "active", "livré à l'instant");
    clock.advance_ms(2_000);
    tick(&d, &ports).await.unwrap();
    assert_eq!(state_of(s, &job).await, "closed");
}

/// Une session de tir où le propriétaire a écrit, ou qu'il a rouverte, est à lui : elle
/// ne se ferme plus toute seule. Une conversation non plus.
#[tokio::test]
async fn a_session_the_owner_took_over_is_left_to_them() {
    let (d, clock, _rec) = harness().await;
    let s = &d.services;
    let written = open(s, SessionKind::Scheduled).await;
    let reopened = open(s, SessionKind::Scheduled).await;
    let chat = open(s, SessionKind::Chat).await;

    s.turns
        .enqueue(
            &written,
            TurnKind::Message,
            json!({"text": "et la suite ?"}),
            None,
            0,
        )
        .await
        .unwrap();
    let turn = s.turns.claim("t").await.unwrap().unwrap();
    s.turns.complete(&turn).await.unwrap();
    s.sessions.set_state(&reopened, "closed").await.unwrap();
    s.sessions.set_state(&reopened, "active").await.unwrap();

    clock.advance_days(3);
    tick(&d, &d.scheduler()).await.unwrap();
    for (name, sid) in [
        ("écrite", &written),
        ("rouverte", &reopened),
        ("conversation", &chat),
    ] {
        assert_eq!(state_of(s, sid).await, "active", "{name}");
    }
}
