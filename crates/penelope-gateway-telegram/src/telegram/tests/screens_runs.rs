//! Écrans des runs : liste, filtre des runs arrêtés, détail, pause, reprise, arrêt.

use super::*;

#[tokio::test]
async fn a_run_is_paused_resumed_and_stopped_from_its_screens() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let (text, _) = screen_of(&g, "runs", json!({})).await;
    assert!(text.ends_with("Aucun run."), "{text}");
    let run = penelope_orchestrator::workflow::start_run(
        &penelope_daemon::workflow::context_of(&g.daemon),
        "build-verify",
        json!({"objectif": "corriger le dépôt"}),
        &Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        },
        None,
        0,
    )
    .await
    .unwrap();
    let state = |id: String| {
        let s = s.clone();
        async move { s.runs.get(&id).await.unwrap().unwrap().state }
    };

    let (text, buttons) = screen_of(&g, "runs", json!({})).await;
    assert!(text.starts_with("**Runs** (1)"), "{text}");
    assert!(
        text.contains(&format!("🏃 **build-verify** · running")),
        "{text}"
    );
    token_of(&buttons, "🔎 build-verify");
    press(&g, &token_of(&buttons, "⏸")).await;
    assert_eq!(
        state(run.id.clone()).await,
        penelope_workflow::RunState::Paused
    );

    let (text, buttons) = screen_of(&g, "runs", json!({"filter": "stuck"})).await;
    assert!(
        text.starts_with("**Runs en pause ou bloqués** (1)"),
        "{text}"
    );
    token_of(&buttons, "Tous les runs");

    let (text, buttons) = screen_of(&g, "run.detail", json!({"run": run.id})).await;
    assert!(
        text.contains("Paramètres : `{\"objectif\":\"corriger le dépôt\"}`"),
        "{text}"
    );
    press(&g, &token_of(&buttons, "▶️ Reprendre")).await;
    assert_eq!(
        state(run.id.clone()).await,
        penelope_workflow::RunState::Running
    );

    let (_, buttons) = screen_of(&g, "run.detail", json!({"run": run.id})).await;
    token_of(&buttons, "⏸ Pause");
    confirm(&g, &token_of(&buttons, "⏹ Arrêter")).await;
    assert_eq!(
        state(run.id.clone()).await,
        penelope_workflow::RunState::Cancelled
    );
    let (text, buttons) = screen_of(&g, "run.detail", json!({"run": run.id})).await;
    assert!(text.starts_with("⏹ **build-verify** · cancelled"), "{text}");
    assert_eq!(buttons.len(), 1, "seul le retour reste : {buttons:?}");
    let (text, _) = screen_of(&g, "runs", json!({"filter": "stuck"})).await;
    assert!(text.ends_with("Aucun run à reprendre."), "{text}");
    assert!(
        g.build_screen(OWNER, None, "run.detail", &json!({"run": "r_absent"}))
            .await
            .is_err()
    );
}

/// Un run bloqué se reprend depuis la liste ; son erreur est dite dans le détail.
#[tokio::test]
async fn a_blocked_run_shows_its_error() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let run = penelope_orchestrator::workflow::start_run(
        &penelope_daemon::workflow::context_of(&g.daemon),
        "build-verify",
        json!({"objectif": "x"}),
        &Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        },
        None,
        0,
    )
    .await
    .unwrap();
    s.runs
        .set_state(
            &run.id,
            penelope_workflow::RunState::Blocked,
            Some("budget atteint"),
        )
        .await
        .unwrap();
    let (text, buttons) = screen_of(&g, "runs", json!({})).await;
    assert!(text.contains("⛔ **build-verify** · blocked"), "{text}");
    token_of(&buttons, "▶️");
    let (text, _) = screen_of(&g, "run.detail", json!({"run": run.id})).await;
    assert!(text.contains("Erreur : budget atteint"), "{text}");
}
