//! Tests de l'exécuteur qui ont besoin du daemon : la planification passe par
//! l'orchestrateur (`WorkflowOrchestrator`, port `Orchestrator`, T24).

use super::*;
use crate::agent::ToolExecutor;
use crate::bus::Origin;
use crate::runtime::Services;
use penelope_kernel::clock::TestClock;
use serde_json::{Value, json};
use std::sync::Arc;

async fn executor() -> (tempfile::TempDir, NativeToolExecutor) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let env = ToolEnv {
        session_id: "s1".into(),
        run_id: None,
        origin: Origin::Cli,
        workspaces: vec![],
        in_workflow: false,
        turn_model: None,
    };
    (dir, NativeToolExecutor::new(s, env))
}

/// #124 : `schedule_move` déplace une planification vers la conversation de l'appel
/// (sujet compris) ou vers la conversation privée ; `schedule_list` dit où livre
/// chacune ; hors Telegram, `here` n'a pas de sens.
#[tokio::test]
async fn schedule_move_sends_a_schedule_here_or_home() {
    let (_dir, mut x) = executor().await;
    let s = x.services.clone();
    // La planification passe par l'orchestrateur du daemon (port `Orchestrator`, T24).
    x.orchestrator = Some(Arc::new(crate::workflow::WorkflowOrchestrator {
        daemon: Arc::new(crate::runtime::Daemon::from_services(s.clone())),
    }));
    s.config
        .mutate("test", |c| {
            c.owner.telegram_user_id = 42;
            c.telegram.allowed_chats = vec![-100_777];
            Ok(vec!["telegram.allowed_chats".into()])
        })
        .unwrap();
    let sched = s
        .schedules
        .create(
            penelope_workflow::TriggerKind::Cron,
            json!({"expr": "0 9 * * *"}),
            json!({"type": "notify", "template": "🧭 Veille"}),
            json!({}),
        )
        .await
        .unwrap();
    let err = x
        .execute("schedule_move", &json!({"id": sched.id, "to": "here"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("private"), "{err}");

    x.env.origin = Origin::Telegram {
        chat_id: -100_777,
        topic_id: Some(12),
        message_id: Some(5),
    };
    let moved = x
        .execute("schedule_move", &json!({"id": sched.id, "to": "here"}))
        .await
        .unwrap();
    assert_eq!(moved.value["destination"], "sujet 12, groupe -100777");
    let listed = x.execute("schedule_list", &json!({})).await.unwrap();
    assert_eq!(listed.value[0]["destination"], "sujet 12, groupe -100777");
    assert_eq!(
        listed.value[0]["target"]["origin"]["message_id"],
        Value::Null
    );

    let home = x
        .execute("schedule_move", &json!({"id": sched.id, "to": "private"}))
        .await
        .unwrap();
    assert_eq!(home.value["destination"], "conversation privée");
}

/// T24 : en production, l'exécuteur d'un tour reçoit le daemon comme `Admin`
/// (`engine.rs`) ; l'outil `self_status` rend alors la mémoire du processus, l'état Codex
/// et la taille du contexte, que l'exécuteur ne lit plus lui-même.
#[tokio::test]
async fn self_status_through_the_daemon_admin_keeps_memory_codex_and_context() {
    let (_dir, mut x) = executor().await;
    let d = Arc::new(crate::runtime::Daemon::from_services(x.services.clone()));
    x.admin = Some(d.clone() as Arc<dyn crate::selfknow::Admin>);
    x.env.turn_model = Some(crate::selfknow::TurnModel {
        alias: "main".into(),
        model_id: "openrouter:z-ai/glm-5.3".into(),
    });
    let v = x.execute("self_status", &json!({})).await.unwrap().value;
    assert!(
        v["penelope"]["rss_mb"].as_f64().is_some_and(|mb| mb > 0.0),
        "{v}"
    );
    let codex = &v["config"]["providers"]["codex"];
    assert!(codex.is_object(), "{v}");
    assert_eq!(codex["enabled"], json!(false));
    let context = &v["costs"]["context"];
    assert!(context.is_object(), "{v}");
    assert_eq!(
        *context,
        crate::compaction::context_view(&x.services, "s1", Some("openrouter:z-ai/glm-5.3"))
            .await
            .unwrap()
    );
}
