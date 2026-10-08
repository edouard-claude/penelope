//! Longues charges (#337) : plafond d'appels par étape et tour de reprise, budget de run
//! juste (temps arrêté exclu) et visible.

use super::*;
use penelope_workflow::Control;

fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

fn note(id: &str) -> Scripted {
    Scripted::ToolCalls(
        String::new(),
        vec![call(
            id,
            "session_metadata",
            json!({"op": "set", "key": "lu", "entry": id}),
        )],
    )
}

fn spec_step(max_turns: u32) -> Value {
    wf(
        "spec",
        "specifier",
        json!([
            {"id": "specifier", "type": "agent", "prompt": "Spécifie la story.",
             "maxCalls": 2, "maxTurns": max_turns,
             "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "completed"}},
                {"goto": "$blocked"}
             ]}
        ]),
    )
}

/// Une étape qui dépasse `maxCalls` sans `step_done()` n'échoue pas d'emblée : l'appel
/// resté en attente est fermé, un tour de reprise demande le point, puis l'étape conclut.
#[tokio::test]
async fn a_step_over_its_call_cap_concludes_in_a_recovery_turn() {
    let e = env().await;
    install(&e.d, spec_step(1)).await;
    e.p.push(note("a1"));
    e.p.push(note("a2"));
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("a3", "step_done", json!({}))],
    ));
    e.p.reply("Spécification posée.");
    let run = start_run(&e.d, "spec", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);

    let history =
        e.d.services
            .context
            .history
            .load(&run.session_id, 0)
            .await
            .unwrap();
    let texts: Vec<String> = history.iter().map(|h| h.message.text()).collect();
    let at = texts
        .iter()
        .position(|t| t.starts_with("[reprise du workflow, tour 1/1]"))
        .unwrap_or_else(|| panic!("{texts:?}"));
    assert!(texts[at].contains("`session_notes`"), "{}", texts[at]);
    assert!(
        texts[at - 1].starts_with("non exécuté : plafond"),
        "l'appel en attente est fermé avant la consigne : {texts:?}"
    );
    let meta = session_metadata(&e.d.services, &run.session_id).await;
    assert_eq!(meta["lu"], "a1", "{meta}");
}

/// Sans tour de reprise restant, l'étape échoue en disant pourquoi.
#[tokio::test]
async fn a_step_without_recovery_turns_left_fails_with_its_reason() {
    let e = env().await;
    install(&e.d, spec_step(0)).await;
    for i in 0..4 {
        e.p.push(note(&format!("b{i}")));
    }
    let run = start_run(&e.d, "spec", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let r = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    let why = r.step_outputs["specifier"]["error"]
        .as_str()
        .unwrap_or_default();
    assert!(why.contains("épuisé ses 2 appels"), "{why}");
}

/// Un run bloqué trois heures puis repris ne meurt pas de « durée maximale atteinte » :
/// le temps d'arrêt est exclu ; la carte, l'écran et la liste montrent chaque plafond.
#[tokio::test]
async fn a_run_blocked_three_hours_resumes_and_shows_its_budget() {
    let e = env().await;
    let s = &e.d.services;
    let mut raw = wf(
        "long",
        "un",
        json!([
            {"id": "un", "type": "shell", "command": "echo un",
             "transitions": [{"goto": "$done"}]}
        ]),
    );
    raw["settings"]["budget"] = json!({"maxUsd": 5.0, "maxTokens": 1000, "maxWallMs": 7_200_000});
    install(&e.d, raw).await;
    let run = start_run(&e.d, "long", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    s.budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(run.session_id.clone()),
            run_id: Some(run.id.clone()),
            model: "m".into(),
            provider: "p".into(),
            prompt: 5_000,
            cached: 4_000,
            completion: 500,
            cost_usd: 0.01,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let blocked = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(
        (blocked.spent_tokens, blocked.spent_cached_tokens),
        (1_500, 4_000),
        "le cache est compté à part"
    );

    e.clock.advance_hours(3);
    let err = control(&e.d, &run.id, &Control::Resume).await.unwrap_err();
    assert!(err.to_string().contains("reprise impossible"), "{err}");
    raise_budget(&e.d, &run.id, &json!({"tokens": 10_000}))
        .await
        .unwrap();
    assert_eq!(
        control(&e.d, &run.id, &Control::Resume).await.unwrap(),
        RunState::Running
    );
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let done = s.runs.get(&run.id).await.unwrap().unwrap();
    assert!(done.held_ms >= 3 * 3_600_000, "{}", done.held_ms);

    let line = budget_view(s, &done).await;
    assert!(
        line.starts_with("durée 0/120 min · tokens 1 k/10 k · cache 4 k"),
        "{line}"
    );
    let listing = runs_listing(s, 10).await.unwrap();
    assert_eq!(listing[0]["budget"], json!(line));
    let card = e.r.cards();
    let last = card.last().map(|c| c.1.clone()).unwrap_or_default();
    assert!(last.contains("Budget : durée"), "{last}");
}

/// Une reprise impossible le dit, au lieu d'annoncer `running` : itérations épuisées,
/// puis relevées pour ce run seul.
#[tokio::test]
async fn an_impossible_resume_says_why() {
    let e = env().await;
    let mut raw = wf(
        "court",
        "un",
        json!([
            {"id": "un", "type": "shell", "command": "echo un", "transitions": [{"goto": "deux"}]},
            {"id": "deux", "type": "shell", "command": "echo deux", "transitions": [{"goto": "$done"}]}
        ]),
    );
    raw["settings"]["maxIterations"] = json!(1);
    install(&e.d, raw).await;
    let run = start_run(&e.d, "court", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let err = control(&e.d, &run.id, &Control::Resume).await.unwrap_err();
    assert!(err.to_string().contains("itérations épuisées"), "{err}");
    assert!(err.to_string().contains("--iterations"), "{err}");
    raise_budget(&e.d, &run.id, &json!({"iterations": 5}))
        .await
        .unwrap();
    assert_eq!(
        control(&e.d, &run.id, &Control::Resume).await.unwrap(),
        RunState::Running
    );
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
}

/// Un sous-agent au plafond a son tour de reprise sur le même échange.
#[tokio::test]
async fn a_sub_agent_over_its_call_cap_gets_a_recovery_turn() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "revue",
            "relire",
            json!([
                {"id": "relire", "type": "sub_agent", "prompt": "Relis le diff.",
                 "maxCalls": 1, "maxTurns": 1,
                 "transitions": [
                    {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                    {"goto": "$blocked"}
                 ]}
            ]),
        ),
    )
    .await;
    e.p.push(note("s1"));
    e.p.reply("Rien à signaler.");
    let run = start_run(&e.d, "revue", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let last = e.p.requests().pop().unwrap();
    let texts: Vec<String> = last.messages.iter().map(|m| m.text()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("[reprise du workflow, tour 1/1]")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.starts_with("non exécuté : plafond")),
        "{texts:?}"
    );
}
