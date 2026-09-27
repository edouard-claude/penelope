//! Étapes : attente MCP, vérification, construction.

use super::*;

/// `wait` sur une tâche MCP longue : suivie dans `mcp_tasks`, sondée jusqu'à la fin,
/// son résultat passe dans la sortie de l'étape.
#[tokio::test]
async fn a_long_mcp_task_is_awaited_until_it_completes() {
    use penelope_mcp_host::testing::{FakeConnector, server, tool};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let e = env().await;
    let fake = Arc::new(FakeConnector::default());
    let base = server(Arc::new(std::sync::Mutex::new(vec![tool(
        "build",
        json!({}),
    )])));
    let polls = Arc::new(AtomicUsize::new(0));
    let seen = polls.clone();
    fake.serve(
        "forge",
        Arc::new(move |m, params| match m {
            "tasks/get" => {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"task": {
                    "taskId": params["taskId"],
                    "status": if n == 0 { "working" } else { "completed" }
                }}))
            }
            "tasks/result" => Ok(json!({
                "content": [{"type": "text", "text": "build vert"}],
                "isError": false
            })),
            other => base(other, params),
        }),
    );
    let sup = penelope_mcp_host::testing::supervisor(e.d.services.clone(), fake.clone());
    e.d.set_mcp(sup.clone());
    sup.add(
        penelope_mcp::config::ServerConfig::stdio("forge", "/opt/mcp/forge", &[]),
        false,
    )
    .await
    .unwrap();

    install(
        &e.d,
        wf(
            "attente-tache",
            "attendre",
            json!([{
                "id": "attendre", "type": "wait", "timeoutMs": 600000,
                "on": {"mcp_task": "forge:task-7"},
                "transitions": [
                    {"goto": "$done", "condition": {"type": "output_match", "path": "status", "equals": "completed"}},
                    {"goto": "$blocked"}
                ]
            }]),
        ),
    )
    .await;
    let run = start_run(&e.d, "attente-tache", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    assert_eq!(
        polls.load(Ordering::SeqCst),
        1,
        "premier sondage : en cours"
    );
    assert_eq!(
        drive(&e.d, &run.id).await.unwrap(),
        RunState::Running,
        "pas de nouveau sondage avant l'échéance"
    );
    assert_eq!(polls.load(Ordering::SeqCst), 1);

    e.clock.advance_secs(3);
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let done = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    let out = &done.step_outputs["attendre"];
    assert_eq!(out["status"], "completed");
    assert_eq!(out["result"]["content"][0]["text"], "build vert");
    let task_id =
        e.d.services
            .kv_get(&format!("wf.mcp_task.{}.attendre.0", run.id))
            .await
            .unwrap();
    assert!(task_id.is_some(), "tâche suivie dans mcp_tasks");
}

/// #137 : le cas du constat, rejoué sur le `build-verify` livré. Le plan déclare le
/// projet et sept critères ; `build` les coche un à un et atteint `verify` en une seule
/// itération ; `verify` lance les tests du projet déclaré, pas `cargo test`.
#[tokio::test]
async fn build_verify_reaches_verify_once_its_criteria_are_ticked() {
    let e = env().await;
    let s = &e.d.services;
    let ws = penelope_executor::executor::default_workspaces(s)[0].clone();
    let project = ws.join("serveur-go");
    std::fs::create_dir_all(&project).unwrap();
    let ids: Vec<String> = (1..=7).map(|i| format!("c{i}")).collect();
    let tool = |id: &str, name: &str, args: Value| ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    };
    let criteria: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "label": format!("critère {id}")}))
        .collect();
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            tool("p1", "session_metadata", json!({"op": "set", "key": "project",
                 "entry": {"dir": project.to_string_lossy(), "test_command": "echo tests-du-projet"}})),
            tool("p2", "session_metadata", json!({"op": "set", "key": "criteria", "entry": criteria})),
            tool("p3", "step_done", json!({})),
        ],
    ));
    e.p.reply("Plan posé.");
    let mut ticks: Vec<ToolCall> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            tool(
                &format!("b{i}"),
                "session_metadata",
                json!({"op": "update", "key": "criteria", "entry": {"id": id, "status": "completed"}}),
            )
        })
        .collect();
    ticks.push(tool("b9", "step_done", json!({})));
    e.p.push(Scripted::ToolCalls(String::new(), ticks));
    e.p.reply("Implémenté.");
    let verdict: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "status": "passed", "note": "vu"}))
        .collect();
    e.p.reply(&json!({"criteria": verdict, "verdict": "passed"}).to_string());

    let run = start_run(
        &e.d,
        "build-verify",
        json!({"objectif": "ajouter un outil"}),
        &owner(),
        None,
        0,
    )
    .await
    .unwrap();
    let mut state = RunState::Running;
    for _ in 0..10 {
        state = drive(&e.d, &run.id).await.unwrap();
        if state != RunState::Running {
            break;
        }
    }
    let done = s.runs.get(&run.id).await.unwrap().unwrap();
    let events = s.events.session_events(&run.session_id, 0).await.unwrap();
    let steps: Vec<String> = events
        .iter()
        .filter(|ev| ev.kind == "workflow.step")
        .map(|ev| {
            format!(
                "{}→{}",
                ev.payload["step"].as_str().unwrap_or(""),
                ev.payload["next"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(state, RunState::Done, "{steps:?} {:?}", done.error);
    assert_eq!(
        steps,
        ["plan→build", "build→verify", "verify→$done"],
        "une seule itération de build"
    );
    let checks = done.step_outputs["verify"]["checks"].to_string();
    assert!(checks.contains("tests-du-projet"), "{checks}");
}

/// #167 : le build peut préciser la commande qui a réellement passé, avec ses
/// prérequis explicites ; verify ne reprend pas la commande périmée du plan.
#[test]
fn verification_contract_overrides_the_planned_test_command() {
    let meta = json!({
        "project": {"dir": "/tmp/projet", "test_command": "cargo test --workspace"},
        "verification": {
            "dir": "/tmp/projet",
            "test_command": "ulimit -n 4096; PATH=/opt/rust/bin:$PATH cargo test --workspace",
            "prerequisites": ["Rust 1.98", "limite de fichiers ouverts 4096"]
        }
    });
    let check = project_test_spec(&meta);
    assert_eq!(check.0, "/tmp/projet");
    assert!(check.1.starts_with("ulimit -n 4096"));
}

#[test]
fn verification_distinguishes_missing_tool_from_red_tests() {
    let absent = json!({"exitCode": 127, "stderr": "zsh: command not found: cargo"});
    let red = json!({"exitCode": 1, "stderr": "test failed"});
    assert_eq!(classify_project_test(&absent), "prerequisite_missing");
    assert_eq!(classify_project_test(&red), "test_failed");
}

#[test]
fn verification_rejects_evidence_from_another_commit() {
    let evidence = json!({"kind": "ci", "ref": "https://example.test/run/42", "sha": "old"});
    assert!(requires_current_sha(&evidence));
    assert!(!evidence_matches_head(&evidence, "new"));
    assert!(evidence_matches_head(&json!({"sha": "new"}), "new"));
    assert!(!requires_current_sha(
        &json!({"kind": "tdd_red", "sha": "old"})
    ));
    assert!(requires_current_sha(
        &json!({"kind": "tdd_green", "sha": "new"})
    ));
}

#[test]
fn verifier_receives_repository_release_rules_and_conditional_criteria() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("AGENTS.md"),
        "Une issue fermée exige version et notes dans la PR ; le tag vient de la CI.",
    )
    .unwrap();
    let rules = repository_rules(project.path().to_str().unwrap());
    let prompt = verifier_prompt(
        "Livrer le correctif",
        &["Si la CI est verte, vérifier le SHA".into()],
        &json!({"verify-check-1": {"exitCode": 0}}),
        &json!({"head_sha": "abc"}),
        &rules,
        project.path(),
    );
    assert!(prompt.contains("Une issue fermée exige version et notes"));
    assert!(prompt.contains("Si la CI est verte"));
    assert!(prompt.contains("Respecte les clauses conditionnelles"));
    assert!(prompt.contains("ne sont pas une release anticipée"));
}

#[tokio::test]
async fn build_verify_reports_an_unavailable_test_tool_without_a_verdict() {
    let e = env().await;
    let project = penelope_executor::executor::default_workspaces(&e.d.services)[0].clone();
    let call = |id: &str, key: &str, entry: Value| ToolCall {
        id: id.into(),
        name: "session_metadata".into(),
        arguments: json!({"op": "set", "key": key, "entry": entry}),
    };
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call(
                "p1",
                "project",
                json!({"dir": project, "test_command": "false"}),
            ),
            call(
                "p2",
                "criteria",
                json!([{"id":"c1", "text":"le test passe", "status":"pending"}]),
            ),
            ToolCall {
                id: "p3".into(),
                name: "step_done".into(),
                arguments: json!({}),
            },
        ],
    ));
    e.p.reply("Plan posé.");
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("b1", "verification", json!({
                "dir": project,
                "test_command": "outil_introuvable_penelope_167",
                "prerequisites": ["outil requis"],
                "evidence": []
            })),
            ToolCall { id: "b2".into(), name: "session_metadata".into(), arguments:
                json!({"op":"update", "key":"criteria", "entry":{"id":"c1", "status":"completed"}}) },
            ToolCall { id: "b3".into(), name: "step_done".into(), arguments: json!({}) },
        ],
    ));
    e.p.reply("Build terminé.");
    let run = start_run(
        &e.d,
        "build-verify",
        json!({"objectif": "valider les tests"}),
        &owner(),
        None,
        0,
    )
    .await
    .unwrap();
    for _ in 0..3 {
        drive(&e.d, &run.id).await.unwrap();
    }
    let pending = e.d.services.approvals.pending(10).await.unwrap();
    let approval = pending
        .iter()
        .find(|a| a.run_id.as_deref() == Some(run.id.as_str()))
        .expect("verify demande la même approbation shell_exec que le builder");
    e.d.services
        .approvals
        .decide(
            approval.id.as_str(),
            &penelope_hitl::Decision::approve_once("cli"),
        )
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    let observed = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    let verify = &observed.step_outputs["verify"];
    assert_eq!(
        verify["failure_kind"], "prerequisite_missing",
        "{observed:?}"
    );
    assert!(
        verify["verdict"].is_null(),
        "aucun jugement de produit : {verify}"
    );
    assert_eq!(verify["checks"]["verify-check-1"]["data"]["exitCode"], 127);
    let stored = session_metadata(&e.d.services, &run.session_id).await;
    assert_eq!(
        stored["verification"]["test_command"],
        "outil_introuvable_penelope_167"
    );
    let reopened = Services::for_tests(e.d.dir.path().to_path_buf(), Arc::new(e.clock.clone()))
        .await
        .unwrap();
    let recovered = session_metadata(&reopened, &run.session_id).await;
    assert_eq!(recovered["verification"], stored["verification"]);
}
