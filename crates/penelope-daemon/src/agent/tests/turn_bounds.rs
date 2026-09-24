//! T4 (épopée #208) : chaque sortie de la boucle ferme son tour dans le journal, avec la
//! raison, sous le même `turn_id` que son ouverture.

use super::*;

fn meta() -> TurnMeta {
    TurnMeta {
        turn_id: "q_1".into(),
        kind: "message".into(),
        attempt: 2,
        ..Default::default()
    }
}

/// Joue un tour de la file et rend son issue avec ses deux bornes.
async fn bounded(
    s: &Arc<Services>,
    p: &Arc<MockProvider>,
    spec: &TurnSpec,
) -> (TurnOutcome, Value, Value) {
    let conv = MemoryConversation::new("Tu es Pénélope.", "fais-le");
    let m = meta();
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation_as(spec, Some(&m), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(m.opened.load(Ordering::SeqCst));
    let (started, finished) = bounds(s, &spec.session_id).await;
    (out, started, finished)
}

/// Le seul `turn.started` et le seul `turn.finished` de la session, dans cet ordre.
async fn bounds(s: &Services, sid: &str) -> (Value, Value) {
    let events = s.events.session_events(sid, 0).await.unwrap();
    let pick = |kind: &str| {
        let found: Vec<_> = events.iter().filter(|e| e.kind == kind).collect();
        assert_eq!(found.len(), 1, "{kind} : {events:?}");
        found[0].clone()
    };
    let (started, finished) = (pick("turn.started"), pick("turn.finished"));
    assert!(started.seq < finished.seq);
    assert_eq!(started.payload["turn_id"], "q_1");
    assert_eq!(finished.payload["turn_id"], "q_1");
    assert_eq!(finished.payload["origin_turn"], "t_test");
    (started.payload, finished.payload)
}

#[tokio::test]
async fn an_answer_closes_the_turn_as_answered() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.reply("fait");
    let (out, started, finished) = bounded(&s, &p, &spec(&sid)).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(started["kind"], "message");
    assert_eq!(started["attempt"], 2);
    assert_eq!(started["origin_turn"], "t_test");
    assert_eq!(started["model"], "mock/model");
    assert_eq!(finished["reason"], "answered");
    assert_eq!(finished["iterations"], 1);
}

#[tokio::test]
async fn an_approval_closes_the_turn_as_awaiting_approval() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "shell_exec", json!({"command": "cargo build"}))],
    ));
    let (out, _, finished) = bounded(&s, &p, &spec(&sid)).await;
    let TurnOutcome::AwaitingApproval { approval_id } = out else {
        panic!("{out:?}")
    };
    assert_eq!(finished["reason"], "awaiting_approval");
    assert_eq!(finished["approval_id"], approval_id.as_str());
}

#[tokio::test]
async fn a_stop_closes_the_turn_as_cancelled() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.reply("jamais lu");
    let spec = spec(&sid);
    spec.cancel.cancel();
    let (out, _, finished) = bounded(&s, &p, &spec).await;
    assert_eq!(out, TurnOutcome::Cancelled);
    assert_eq!(finished["reason"], "cancelled");
}

#[tokio::test]
async fn a_spent_budget_closes_the_turn_as_budget_exceeded() {
    let (_d, s, p) = setup().await;
    s.budget
        .record(penelope_kernel::budget::UsageRecord {
            model: "m".into(),
            provider: "mock".into(),
            cost_usd: 100.0,
            ..Default::default()
        })
        .await
        .unwrap();
    let sid = session(&s).await;
    let (out, _, finished) = bounded(&s, &p, &spec(&sid)).await;
    assert!(matches!(out, TurnOutcome::BudgetExceeded { .. }), "{out:?}");
    assert_eq!(finished["reason"], "budget_exceeded");
    assert!(finished["scope"].is_string());
}

#[tokio::test]
async fn a_loop_closes_the_turn_as_loop_aborted() {
    let (_d, s, p) = setup().await;
    for i in 0..8 {
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call(&format!("c{i}"), "fs_read", json!({"path": "a.rs"}))],
        ));
    }
    let sid = session(&s).await;
    let (out, _, finished) = bounded(&s, &p, &spec(&sid)).await;
    assert!(matches!(out, TurnOutcome::LoopAborted { .. }), "{out:?}");
    assert_eq!(finished["reason"], "loop_aborted");
}

#[tokio::test]
async fn a_model_error_closes_the_turn_as_failed() {
    let (_d, s, p) = setup().await;
    p.push(Scripted::Error(LlmErrorKind::Auth, "clé refusée".into()));
    let sid = session(&s).await;
    let (out, _, finished) = bounded(&s, &p, &spec(&sid)).await;
    assert!(matches!(out, TurnOutcome::Failed { .. }), "{out:?}");
    assert_eq!(finished["reason"], "failed");
    assert!(finished["error"].is_string());
}

#[tokio::test]
async fn the_call_cap_closes_the_turn_as_calls_exhausted() {
    let (_d, s, p) = setup().await;
    for u in spent("t_test", 30, 0.0) {
        s.budget.record(u).await.unwrap();
    }
    let sid = session(&s).await;
    let (out, _, finished) = bounded(&s, &p, &spec(&sid)).await;
    assert!(
        matches!(&out, TurnOutcome::Failed { error } if error.starts_with(CALLS_EXHAUSTED)),
        "{out:?}"
    );
    assert_eq!(finished["reason"], "calls_exhausted");
}

/// Un tour tombé avant sa boucle (fournisseur indisponible) est ouvert et fermé par
/// l'appelant ; un tour que la boucle a ouvert ne l'est pas deux fois.
#[tokio::test]
async fn a_turn_failing_before_its_loop_is_still_bounded() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let m = meta();
    let early: anyhow::Result<TurnOutcome> = Err(anyhow::anyhow!("fournisseur indisponible"));
    close_unopened(&s, &sid, &m, &early).await;
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let marks: Vec<_> = events
        .iter()
        .filter(|e| e.kind.starts_with("turn."))
        .collect();
    let kinds: Vec<_> = marks.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["turn.started", "turn.finished"]);
    assert_eq!(marks[0].payload["turn_id"], "q_1");
    assert_eq!(marks[1].payload["reason"], "failed");
    assert_eq!(marks[1].payload["error"], "fournisseur indisponible");

    let sid = session(&s).await;
    p.reply("fait");
    let (_, _, _) = bounded(&s, &p, &spec(&sid)).await;
    let opened = TurnMeta {
        opened: Arc::new(true.into()),
        ..meta()
    };
    close_unopened(&s, &sid, &opened, &early).await;
    bounds(&s, &sid).await;
}
