//! Tests de l'exécuteur qui ont besoin du daemon : `self_status` par son `Admin` (T24).
//! Celui de `schedule_move` est dans l'ordonnanceur (`penelope-orchestrator`, T36).

use super::*;
use crate::agent::ToolExecutor;
use crate::bus::Origin;
use crate::runtime::Services;
use penelope_kernel::clock::TestClock;
use serde_json::json;
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
