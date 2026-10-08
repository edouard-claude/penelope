//! Crédits épuisés pendant un run (#339) : pause au point sûr, message, reprise.

use super::*;
use penelope_workflow::Control;

fn note(id: &str) -> Scripted {
    Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: id.into(),
            name: "session_metadata".into(),
            arguments: json!({"op": "set", "key": "lu", "entry": id}),
        }],
    )
}

fn step_done(id: &str) -> Scripted {
    Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: id.into(),
            name: "step_done".into(),
            arguments: json!({}),
        }],
    )
}

/// Le choix du propriétaire : rester sur son modèle, sans repli.
async fn dev_workflow(e: &Env) {
    e.d.services
        .config
        .mutate("test", |c| {
            let name = c.models.active_name().to_string();
            if let Some(p) = c.models.materialize(&name) {
                p.fallback.clear();
            }
            Ok(vec![])
        })
        .unwrap();
    install(
        &e.d,
        wf(
            "dev",
            "dev",
            json!([
                {"id": "dev", "name": "dev", "type": "agent", "prompt": "Développe la story.",
                 "transitions": [{"goto": "$done"}]}
            ]),
        ),
    )
    .await;
}

/// Quota Codex épuisé en pleine étape : le run passe `paused` sans perdre l'outil déjà
/// exécuté, le propriétaire est prévenu une fois avec le point d'arrêt et l'heure de
/// retour ; au retour du quota (horloge de test), le run reprend seul et finit.
#[tokio::test]
async fn a_spent_quota_pauses_the_run_and_resumes_it_when_the_quota_returns() {
    let e = env().await;
    let s = &e.d.services;
    dev_workflow(&e).await;
    e.p.push(note("a1"));
    e.p.push(Scripted::UsageLimit(Some(3_600)));
    let run = start_run(&e.d, "dev", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    let paused = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(paused.state, RunState::Paused, "{}", paused.step_outputs);
    assert_eq!(e.p.call_count(), 2, "pas de relance contre un quota épuisé");
    let said: Vec<String> =
        e.r.texts()
            .into_iter()
            .filter(|t| t.starts_with("⏸ Je me suis arrêtée là"))
            .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("crédits Codex épuisés"), "{}", said[0]);
    assert!(said[0].contains("Dernier point : étape dev"), "{}", said[0]);
    assert!(said[0].contains("Reprise prévue à"), "{}", said[0]);

    // Rien ne bouge avant l'heure ; une visite rejouée ne redit rien.
    assert_eq!(crate::workflow::credits::resume_due(&e.d).await.unwrap(), 0);
    e.clock.advance_secs(3_601);
    e.p.push(step_done("a2"));
    e.p.reply("Story développée.");
    assert_eq!(crate::workflow::credits::resume_due(&e.d).await.unwrap(), 1);
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let meta = session_metadata(s, &run.session_id).await;
    assert_eq!(
        meta["lu"], "a1",
        "l'outil fait avant la pause n'est pas perdu"
    );
    assert!(
        e.r.texts().iter().any(|t| t.starts_with("▶️ Je reprends")),
        "{:?}",
        e.r.texts()
    );
    let done = s.runs.get(&run.id).await.unwrap().unwrap();
    assert!(done.held_ms >= 3_600_000, "la pause sort de la durée");
}

/// 402 d'OpenRouter : pas d'heure de retour, pas de reprise seule ; « Reprendre » relance.
#[tokio::test]
async fn spent_openrouter_credits_wait_for_resume() {
    let e = env().await;
    let s = &e.d.services;
    dev_workflow(&e).await;
    e.p.push(Scripted::Error(
        penelope_llm::types::LlmErrorKind::PaymentRequired,
        "Insufficient credits".into(),
    ));
    let run = start_run(&e.d, "dev", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Paused
    );
    let said = e.r.texts().join("\n");
    assert!(said.contains("à la recharge des crédits"), "{said}");
    assert!(said.contains(&format!("/resume {}", run.id)), "{said}");
    e.clock.advance_hours(24);
    assert_eq!(crate::workflow::credits::resume_due(&e.d).await.unwrap(), 0);

    e.p.push(step_done("b1"));
    assert_eq!(
        control(&e.d, &run.id, &Control::Resume).await.unwrap(),
        RunState::Running
    );
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
}

/// Budget journalier atteint : pause jusqu'au minuit du propriétaire.
#[tokio::test]
async fn the_daily_budget_pauses_until_midnight() {
    let e = env().await;
    let s = &e.d.services;
    dev_workflow(&e).await;
    s.config
        .mutate("test", |c| {
            c.budget.daily_usd = 0.5;
            Ok(vec![])
        })
        .unwrap();
    s.budget
        .record(penelope_kernel::budget::UsageRecord {
            model: "m".into(),
            provider: "p".into(),
            cost_usd: 1.0,
            ..Default::default()
        })
        .await
        .unwrap();
    let run = start_run(&e.d, "dev", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Paused
    );
    assert!(
        e.r.texts().join("\n").contains("budget journalier atteint"),
        "{:?}",
        e.r.texts()
    );
    e.clock.advance_days(1);
    assert_eq!(crate::workflow::credits::resume_due(&e.d).await.unwrap(), 1);
}
