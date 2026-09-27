//! Étapes `shell` et `tool` : les issues autres que le succès.

use super::*;

async fn run_one(e: &Env, id: &str, step: Value) -> Run {
    install(&e.d, wf(id, "etape", json!([step]))).await;
    let run = start_run(&e.d, id, json!({}), &owner(), None, 0)
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    e.d.services.runs.get(&run.id).await.unwrap().unwrap()
}

fn outcomes() -> Value {
    json!([
        {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
        {"goto": "$blocked"}
    ])
}

#[tokio::test]
async fn a_shell_step_without_a_command_for_this_os_is_an_error() {
    let e = env().await;
    let run = run_one(
        &e,
        "ailleurs",
        json!({"id": "etape", "type": "shell", "command": {"plan9": "true"},
               "transitions": outcomes()}),
    )
    .await;
    assert_eq!(run.state, RunState::Blocked);
    let err = run.step_outputs["etape"]["error"].as_str().unwrap();
    assert!(err.starts_with("pas de commande pour"), "{err}");
}

/// #106 : sous un profil qui coupe le réseau, un échec qui ressemble à une panne réseau
/// le dit ; sans bac à sable (profil `full` des tests hors macOS), rien n'est ajouté.
#[tokio::test]
async fn a_network_failure_under_the_sandbox_says_so() {
    let e = env().await;
    let run = run_one(
        &e,
        "reseau",
        json!({"id": "etape", "type": "shell",
               "command": "echo 'curl: (6) Could not resolve host: exemple.invalid' >&2; exit 6",
               "transitions": outcomes()}),
    )
    .await;
    let out = &run.step_outputs["etape"];
    assert_eq!(out["exitCode"], 6);
    assert!(
        out["stderr"]
            .as_str()
            .unwrap()
            .contains("Could not resolve")
    );
    let sandboxed = e.d.services.config.config().sandbox.default_profile != "full";
    assert_eq!(
        out["note"]
            .as_str()
            .is_some_and(|n| n.contains("network: true")),
        sandboxed,
        "{out}"
    );
}

/// Un refus du propriétaire est un échec de l'étape, et l'outil ne tourne pas.
#[tokio::test]
async fn a_denied_tool_step_fails_without_running() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "refus",
            "ecrire",
            json!([
                {"id": "ecrire", "type": "tool", "tool": "fs_write",
                 "args": {"path": "{{workdir}}/note.txt", "content": "non"},
                 "transitions": outcomes()}
            ]),
        ),
    )
    .await;
    let run = start_run(&e.d, "refus", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    drive(&e.d, &run.id).await.unwrap();
    let approvals = e.r.approvals();
    penelope_agent::decide_approval(
        &e.d.agent,
        &approvals[0],
        &penelope_hitl::Decision::deny("test", None),
    )
    .await
    .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let run = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(
        run.step_outputs["ecrire"]["error"],
        "appel refusé par le propriétaire"
    );
    assert!(
        !std::path::Path::new(run.workdir.as_deref().unwrap())
            .join("note.txt")
            .exists()
    );
}

/// Arguments invalides : l'étape échoue avant toute carte (#117) ; un outil qui répond
/// en erreur donne un échec qui porte son texte.
#[tokio::test]
async fn tool_step_errors_are_reported_in_the_step_output() {
    let e = env().await;
    let run = run_one(
        &e,
        "sans-args",
        json!({"id": "etape", "type": "tool", "tool": "fs_read", "args": {},
               "transitions": outcomes()}),
    )
    .await;
    assert_eq!(run.state, RunState::Blocked);
    assert!(run.step_outputs["etape"]["error"].is_string());
    assert!(
        e.r.approvals().is_empty(),
        "aucune carte pour un appel invalide"
    );

    let run = run_one(
        &e,
        "absent",
        json!({"id": "etape", "type": "tool", "tool": "fs_read",
               "args": {"path": "{{workdir}}/absent.txt"},
               "transitions": outcomes()}),
    )
    .await;
    assert_eq!(run.state, RunState::Blocked, "{:?}", run.step_outputs);
    let out = &run.step_outputs["etape"];
    assert_eq!(out["error"], out["text"], "{out}");
    assert!(!out["text"].as_str().unwrap().is_empty());
}
