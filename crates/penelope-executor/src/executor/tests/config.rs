use super::*;

struct TestConfigAdmin {
    config: Arc<penelope_kernel::config::ConfigStore>,
}

#[async_trait::async_trait]
impl crate::selfknow::Admin for TestConfigAdmin {
    fn uptime_s(&self) -> u64 {
        0
    }

    async fn set_config(&self, path: &str, value: Value) -> Result<u64, String> {
        let generation = self
            .config
            .mutate("test", |cfg| {
                match path {
                    "sandbox.workspaces" => {
                        cfg.sandbox.workspaces = serde_json::from_value(value)
                            .map_err(penelope_kernel::KernelError::Json)?;
                    }
                    "models.aliases.main" => {
                        cfg.models.aliases.insert(
                            "main".into(),
                            value
                                .as_str()
                                .ok_or_else(|| {
                                    penelope_kernel::KernelError::config(
                                        "models.aliases.main attend une chaîne",
                                    )
                                })?
                                .to_string(),
                        );
                    }
                    _ => {
                        return Err(penelope_kernel::KernelError::config(format!(
                            "chemin de test inconnu : {path}"
                        )));
                    }
                }
                Ok(vec![path.to_string()])
            })
            .map_err(|e| e.to_string())?;
        Ok(generation.generation)
    }
}

fn with_config_admin(x: &mut NativeToolExecutor) {
    x.admin = Some(Arc::new(TestConfigAdmin {
        config: x.services.config.clone(),
    }));
}

/// #163 : une modification des workspaces prend effet pour les outils suivants du même
/// tour, y compris la normalisation du shell ; un nouveau tour n'a rien à recharger.
#[tokio::test]
async fn config_set_workspaces_applies_to_the_next_tool_call_and_turn() {
    let (dir, mut x) = executor().await;
    with_config_admin(&mut x);
    let added = dir.path().join("added-workspace");
    std::fs::create_dir_all(&added).unwrap();
    std::fs::write(added.join("visible.txt"), "ok").unwrap();

    let changed = x
        .execute(
            "config_set",
            &json!({
                "path": "sandbox.workspaces",
                "value": json!([added.to_string_lossy()]).to_string(),
            }),
        )
        .await
        .unwrap();
    assert_eq!(changed.value["applied"], "à chaud, dès le prochain appel");

    let listed = x
        .execute("fs_list", &json!({"path": added}))
        .await
        .expect("le nouvel espace est visible dans le même tour");
    assert!(listed.text.contains("visible.txt"), "{}", listed.text);
    let shell = x
        .execute("shell_exec", &json!({"command": "pwd", "cwd": added}))
        .await
        .expect("le nouvel espace est accepté comme cwd dans le même tour");
    assert!(shell.text.contains("added-workspace"), "{}", shell.text);

    let line = format!("cd {} && pwd", added.display());
    assert!(
        x.normalise_call("shell_exec", &json!({"command": line}))
            .is_some(),
        "la normalisation relit les workspaces vivants"
    );

    let next = NativeToolExecutor::new(
        x.services.clone(),
        ToolEnv {
            session_id: "s2".into(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: default_workspaces(&x.services),
            in_workflow: false,
            turn_model: None,
        },
    );
    next.execute("fs_read", &json!({"path": added.join("visible.txt")}))
        .await
        .expect("le tour suivant voit le workspace sans autre action");
}

#[tokio::test]
async fn config_set_reports_canonical_and_missing_workspaces() {
    let (dir, mut x) = executor().await;
    with_config_admin(&mut x);
    let actual = dir.path().join("penelope");
    let alias = dir.path().join("Penelope");
    std::fs::create_dir(&actual).unwrap();
    if std::fs::canonicalize(&alias).is_err() {
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
    }
    let changed = x
        .execute(
            "config_set",
            &json!({"path": "sandbox.workspaces",
                    "value": json!([alias.to_string_lossy()]).to_string()}),
        )
        .await
        .unwrap();
    assert_eq!(
        changed.value["value"][0],
        actual.canonicalize().unwrap().to_string_lossy().to_string()
    );
    assert!(
        changed.value["avertissements"]
            .to_string()
            .contains("forme réelle")
    );

    let missing = dir.path().join("missing");
    let changed = x
        .execute(
            "config_set",
            &json!({"path": "sandbox.workspaces",
                    "value": json!([missing.to_string_lossy()]).to_string()}),
        )
        .await
        .unwrap();
    assert!(
        changed.value["avertissements"]
            .to_string()
            .contains("n'existe pas")
    );
}

#[tokio::test]
async fn every_native_tool_execution_is_in_the_runtime_log() {
    let (dir, x) = executor().await;
    let path = dir.path().join("ws").join("secret.txt");
    let secret = "sk_test_FauxSecret1234567890";
    std::fs::write(&path, secret).unwrap();
    x.execute("fs_read", &json!({"path": path})).await.unwrap();
    let events = x.services.events.range(0, 100).await.unwrap();
    let tool = events.iter().find(|e| e.kind == "runtime.tool").unwrap();
    assert_eq!(tool.payload["tool"], "fs_read");
    assert!(tool.payload["duration_ms"].is_number());
    assert!(!tool.payload.to_string().contains(secret));
}

/// #163 : un refus décrit les racines réellement lues, pas le snapshot du début du tour.
#[tokio::test]
async fn a_workspace_refusal_lists_the_live_roots() {
    let (dir, x) = executor().await;
    let old = x.env.workspaces[0].clone();
    let live = dir.path().join("live-workspace");
    std::fs::create_dir_all(&live).unwrap();
    x.services
        .config
        .mutate("test", |cfg| {
            cfg.sandbox.workspaces = vec![live.to_string_lossy().into_owned()];
            Ok(vec!["sandbox.workspaces".into()])
        })
        .unwrap();

    let error = x
        .execute("fs_read", &json!({"path": dir.path().join("outside.txt")}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&live.canonicalize().unwrap().to_string_lossy().to_string()),
        "{error}"
    );
    assert!(
        !error.contains(&old.to_string_lossy().to_string()),
        "{error}"
    );

    let restricted = dir.path().join("restricted-agent");
    std::fs::create_dir_all(&restricted).unwrap();
    let restricted_executor = NativeToolExecutor::new(
        x.services.clone(),
        ToolEnv {
            session_id: "restricted".into(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: vec![restricted],
            in_workflow: false,
            turn_model: None,
        },
    );
    restricted_executor
        .execute("fs_list", &json!({"path": live}))
        .await
        .expect_err("une liste restreinte ne s'élargit pas aux workspaces généraux");
}

/// #163 : les réglages réellement dynamiques gardent leur promesse au prochain appel.
#[tokio::test]
async fn config_set_reports_the_actual_application_time() {
    let (_dir, mut x) = executor().await;
    with_config_admin(&mut x);
    let changed = x
        .execute(
            "config_set",
            &json!({
                "path": "models.aliases.main",
                "value": "openrouter:z-ai/glm-5.3",
            }),
        )
        .await
        .unwrap();
    assert_eq!(changed.value["applied"], "à chaud, dès le prochain appel");
    assert_eq!(
        config_application_time("models.routing.classifier"),
        (
            "au prochain tour",
            Some("les outils de ce tour gardent l'ancienne valeur")
        )
    );
    assert_eq!(
        config_application_time("telegram.token").0,
        "au redémarrage"
    );
}
