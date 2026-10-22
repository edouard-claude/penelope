//! #395, point 4 : une mission `--host` dont les fichiers, le shell et la vérification
//! tournent sur l'hôte, par le même protocole que l'enfant local. L'hôte `@local` est le
//! vrai binaire, sans `ssh` : la suite rejouable que l'issue demande.

use penelope_agent::ToolExecutor;
use penelope_app::bus::Origin;
use penelope_app::mission::{self, Request};
use penelope_app::services::Services;
use serde_json::json;
use std::sync::Arc;

async fn services(home: &std::path::Path, workspace: &std::path::Path) -> Arc<Services> {
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Arc::new(
        Services::for_tests(home.to_path_buf(), clock)
            .await
            .unwrap(),
    );
    s.publish_config("test", |c| {
        c.runners.local_executor = env!("CARGO_BIN_EXE_penelope").into();
        c.runners.remote_full_for = vec!["@local".into()];
        c.sandbox.workspaces = vec![workspace.to_string_lossy().into_owned()];
        Ok(Vec::new())
    })
    .unwrap();
    s
}

#[tokio::test]
async fn a_host_mission_works_on_the_host_through_the_protocol() {
    let home = tempfile::tempdir().unwrap();
    let hote = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(hote.path()).unwrap();
    let s = services(home.path(), &root).await;
    let req = Request {
        goal: "écrire et tester".into(),
        check: Some("test -f src/a.txt".into()),
        cwd: Some(root.to_string_lossy().to_string()),
        host: Some("@local".into()),
        ..Default::default()
    };
    let (m, _) = mission::start(&s, "s_hote", req, &Origin::Cli)
        .await
        .unwrap();
    assert_eq!(m.host.as_deref(), Some("@local"));

    let exec = penelope_executor::executor::NativeToolExecutor::new(
        s.clone(),
        penelope_executor::executor::ToolEnv {
            session_id: "s_hote".into(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: Vec::new(),
            in_workflow: false,
            turn_model: None,
        },
    );
    exec.execute(
        "fs_write",
        &json!({"path": "src/a.txt", "content": "sur l'hôte\n"}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
        "sur l'hôte\n",
        "le fichier est écrit sur l'hôte, dans la racine de la mission"
    );
    let out = exec
        .execute("shell_exec", &json!({"command": "cat src/a.txt && pwd"}))
        .await
        .unwrap();
    let text = out.value.to_string();
    assert!(
        text.contains("sur l'hôte") && text.contains(root.to_str().unwrap()),
        "{text}"
    );
    let refused = exec
        .execute("fs_read", &json!({"path": "/etc/hosts"}))
        .await;
    assert!(refused.is_err(), "rien ne sort de la racine sur l'hôte");

    mission::run_check_remote(&s, "@local", "test -f src/a.txt", m.cwd.as_deref())
        .await
        .unwrap();
    let red = mission::run_check_remote(&s, "@local", "test -f absent", m.cwd.as_deref()).await;
    assert!(red.unwrap_err().starts_with("code 1"));
}

#[tokio::test]
async fn a_host_mission_refuses_what_it_cannot_honour() {
    let home = tempfile::tempdir().unwrap();
    let s = services(home.path(), home.path()).await;
    let base = || Request {
        goal: "x".into(),
        cwd: Some("/srv/depot".into()),
        host: Some("lab".into()),
        ..Default::default()
    };
    for (req, why) in [
        (
            Request {
                host: Some("-oProxyCommand=sh".into()),
                ..base()
            },
            "refusé",
        ),
        (
            Request {
                worktree: true,
                ..base()
            },
            "worktree",
        ),
        (
            Request {
                cwd: Some("relatif".into()),
                ..base()
            },
            "absolu",
        ),
        (
            Request {
                hook: Some("cargo fmt".into()),
                ..base()
            },
            "hook",
        ),
    ] {
        let e = mission::start(&s, "s_refus", req, &Origin::Cli)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{why} : {e}");
    }
    let parsed =
        mission::parse_request("corriger --host lab --cwd /srv/depot --check cargo test").unwrap();
    assert_eq!(parsed.host.as_deref(), Some("lab"));
}
