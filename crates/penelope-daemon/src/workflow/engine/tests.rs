use super::harness::Harness;
use super::*;
use crate::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ToolCall;

struct Env {
    d: Harness,
    p: Arc<MockProvider>,
    clock: TestClock,
    r: Arc<RecordingMessenger>,
}

async fn env() -> Env {
    let clock = TestClock::new(1_789_516_800_000);
    let d = Harness::new(Arc::new(clock.clone())).await;
    let p = d.provider.clone();
    let r = RecordingMessenger::with_cards();
    d.set_messenger(r.clone());
    Env { d, p, clock, r }
}

/// Installe un workflow de test, validé comme un fichier déposé par l'utilisateur.
async fn install(d: &Context, raw: Value) {
    let s = &d.services;
    let wf = Workflow::from_json(&raw.to_string()).expect("JSON de workflow");
    let known =
        penelope_app::services::workflow_known_with(&s.config.config(), &s.mcp_tools, &s.workflows)
            .await;
    let dir = s.platform.dirs.workflows();
    std::fs::create_dir_all(&dir).unwrap();
    s.workflows
        .write(&dir, &wf, &known)
        .expect("workflow valide");
    s.workflows
        .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);
}

fn wf(id: &str, entry: &str, steps: Value) -> Value {
    json!({
        "metadata": {"id": id, "name": format!("Essai {id}"), "parameters": []},
        "entryStep": entry,
        "settings": {"maxIterations": 10, "budget": {"maxUsd": 1.0, "maxTokens": 100000, "maxWallMs": 3600000}},
        "steps": steps,
    })
}

fn owner() -> Origin {
    Origin::Telegram {
        chat_id: 42,
        topic_id: None,
        message_id: None,
    }
}

/// `wait` sur une tâche MCP longue : suivie dans `mcp_tasks`, sondée jusqu'à la fin,
/// son résultat passe dans la sortie de l'étape.
#[tokio::test]
async fn a_long_mcp_task_is_awaited_until_it_completes() {
    use crate::mcp::testing::{FakeConnector, server, tool};
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
    let sup = crate::mcp::testing::supervisor(e.d.services.clone(), fake.clone());
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
    let ws = crate::executor::default_workspaces(s)[0].clone();
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
    let project = crate::executor::default_workspaces(&e.d.services)[0].clone();
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

/// #136 : le budget de tokens d'un run compte les tokens facturés (entrée hors cache
/// et sortie) ; la borne atteinte se dit avec ses chiffres ; `Resume` sur un run
/// toujours au-dessus répond sans changer l'état ; `budget` relève le plafond du run et
/// le rend reprenable.
#[tokio::test]
async fn a_run_budget_counts_billed_tokens_and_can_be_raised() {
    let e = env().await;
    let s = &e.d.services;
    let raw = json!({
        "metadata": {"id": "cache", "name": "Cache", "parameters": []},
        "entryStep": "choisir",
        "settings": {"maxIterations": 10,
                     "budget": {"maxUsd": 5.0, "maxTokens": 300000, "maxWallMs": 3600000}},
        "steps": [{"id": "choisir", "name": "On continue ?", "type": "user",
                   "template": "question", "choices": ["Oui", "Non"],
                   "transitions": [{"goto": "$done"}]}]
    });
    install(&e.d, raw).await;
    let run = start_run(&e.d, "cache", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    let spend = |n: usize| {
        let (s, run) = (s.clone(), run.clone());
        async move {
            for _ in 0..n {
                s.budget
                    .record(penelope_kernel::budget::UsageRecord {
                        session_id: Some(run.session_id.clone()),
                        run_id: Some(run.id.clone()),
                        model: "m".into(),
                        provider: "p".into(),
                        prompt: 44_000,
                        cached: 40_000,
                        completion: 400,
                        cost_usd: 0.01,
                        ..Default::default()
                    })
                    .await
                    .unwrap();
            }
        }
    };
    spend(50).await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    let now = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(
        now.spent_tokens, 220_000,
        "90 % servis par le cache ne comptent pas"
    );

    spend(50).await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let blocked = s.runs.get(&run.id).await.unwrap().unwrap();
    let why = blocked.error.clone().unwrap_or_default();
    assert!(why.contains("440000 tokens facturés sur 300000"), "{why}");
    assert!(why.contains("wf control"), "{why}");

    let err = control(&e.d, &run.id, &penelope_workflow::Control::Resume)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("toujours bloqué"), "{err}");
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Blocked
    );

    let raised = raise_budget(&e.d, &run.id, None, Some(1_000_000))
        .await
        .unwrap();
    assert!(raised["still_blocked"].is_null(), "{raised}");
    assert_eq!(
        control(&e.d, &run.id, &penelope_workflow::Control::Resume)
            .await
            .unwrap(),
        RunState::Running
    );
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
}

#[tokio::test]
async fn parameters_are_checked_before_anything_starts() {
    let e = env().await;
    let raw = json!({
        "metadata": {"id": "avec-params", "name": "Paramètres", "parameters": [
            {"id": "ticket", "label": "Ticket", "type": "string", "required": true},
            {"id": "branche", "label": "Branche", "type": "string", "default": "main"}
        ]},
        "entryStep": "fin",
        "settings": {"budget": {"maxUsd": 1.0, "maxTokens": 1000, "maxWallMs": 60000}},
        "steps": [{"id": "fin", "type": "wait", "on": {"duration_ms": 0},
                   "transitions": [{"goto": "$done"}]}]
    });
    install(&e.d, raw).await;
    let err = start_run(&e.d, "avec-params", json!({}), &owner(), None, 0)
        .await
        .unwrap_err();
    assert!(err.contains("ticket"), "{err}");
    let err = start_run(
        &e.d,
        "avec-params",
        json!({"ticket": 1, "inconnu": 2}),
        &owner(),
        None,
        0,
    )
    .await
    .unwrap_err();
    assert!(err.contains("inconnu"), "{err}");
    let run = start_run(&e.d, "avec-params", json!({"ticket": 7}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(run.params, json!({"ticket": 7, "branche": "main"}));
    assert!(
        start_run(&e.d, "absent", json!({}), &owner(), None, 0)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_question_waits_for_the_owner_then_follows_the_choice() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "question",
            "choisir",
            json!([
                {"id": "choisir", "name": "On déploie ?", "type": "user",
                 "template": "question", "choices": ["Oui", "Non"],
                 "transitions": [
                    {"goto": "$done", "condition": {"type": "step_result", "result": "Oui"}},
                    {"goto": "$blocked", "condition": {"type": "step_result", "result": "Non"}}
                 ]}
            ]),
        ),
    )
    .await;
    let run = start_run(&e.d, "question", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    assert_eq!(
        drive(&e.d, &run.id).await.unwrap(),
        RunState::Running,
        "repasser ne repose pas la question"
    );
    let questions = e.r.questions();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].0, format!("{}|choisir.0", run.id));
    assert!(questions[0].1.contains("On déploie ?"));
    assert_eq!(questions[0].2, vec!["Oui", "Non"]);

    assert!(
        answer(&e.d, &run.id, "choisir.0", "Peut-être", None)
            .await
            .is_err()
    );
    assert!(
        answer(&e.d, &run.id, "choisir.9", "Oui", None)
            .await
            .is_err(),
        "question périmée"
    );
    answer(&e.d, &run.id, "choisir.0", "Oui", None)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let done = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(done.step_outputs["choisir"]["choice"], "Oui");
    let cards = e.r.cards();
    assert!(cards.iter().all(|(k, _)| *k == format!("run.{}", run.id)));
    assert!(cards.last().unwrap().1.contains("terminé"), "{cards:?}");
}

#[tokio::test]
async fn an_agent_step_ends_with_step_done_after_a_nudge() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "agent",
            "analyser",
            json!([
                {"id": "analyser", "type": "agent", "prompt": "Analyse {{run.id}}.",
                 "transitions": [{"goto": "$done"}]}
            ]),
        ),
    )
    .await;
    // Premier tour : une réponse sans `step_done()` ; relance ; puis la fin d'étape.
    e.p.reply("Je regarde.");
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            ToolCall {
                id: "c1".into(),
                name: "return_value".into(),
                arguments: json!({"result": "success", "content": "cause trouvée"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "step_done".into(),
                arguments: json!({}),
            },
        ],
    ));
    e.p.reply("Étape terminée.");
    let run = start_run(&e.d, "agent", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let done = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(done.step_outputs["analyser"]["content"], "cause trouvée");
    let requests = e.p.requests();
    assert!(requests[0].tools.iter().any(|t| t.name == "step_done"));
    let history =
        e.d.services
            .context
            .history
            .load(&run.session_id, 0)
            .await
            .unwrap();
    let texts: Vec<String> = history.iter().map(|h| h.message.text()).collect();
    assert!(
        texts[0].contains(&format!("Analyse {}.", run.id)),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.starts_with("[relance du workflow]")));
}

/// Issue #35 : le brief d'un lancement en conversation précède la consigne de la
/// première étape `agent` ou `sub_agent`, pas des suivantes, et paraît sur la carte.
#[tokio::test]
async fn a_brief_reaches_the_first_agent_step_and_the_progress_card() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "brief",
            "tri",
            json!([
                {"id": "tri", "type": "sub_agent", "prompt": "Trie le ticket.",
                 "transitions": [{"goto": "analyser"}]},
                {"id": "analyser", "type": "agent", "prompt": "Analyse le ticket.",
                 "transitions": [{"goto": "$done"}]}
            ]),
        ),
    )
    .await;
    e.p.reply("Trié.");
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "step_done".into(),
            arguments: json!({}),
        }],
    ));
    e.p.reply("Fait.");
    let brief = "Ticket #7647 : le cache ne se vide pas. Vérifier Redis d'abord.";
    let o = WorkflowOrchestrator {
        context: e.d.cx.clone(),
    };
    let started = crate::executor::Orchestrator::start_workflow(
        &o,
        "brief",
        json!({}),
        Some(brief),
        &owner(),
    )
    .await
    .unwrap();
    let run_id = started["run_id"].as_str().unwrap().to_string();
    assert_eq!(brief_of(&e.d.services, &run_id).await, brief);
    assert_eq!(drive(&e.d, &run_id).await.unwrap(), RunState::Done);

    let requests = e.p.requests();
    let prompt_of = |i: usize| {
        requests[i]
            .messages
            .iter()
            .map(|m| m.text())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        prompt_of(0).contains(brief),
        "sous-agent : {}",
        prompt_of(0)
    );
    let run = e.d.services.runs.get(&run_id).await.unwrap().unwrap();
    let history =
        e.d.services
            .context
            .history
            .load(&run.session_id, 0)
            .await
            .unwrap();
    assert!(
        !history[0].message.text().contains(brief),
        "seule la première étape reçoit le brief"
    );
    let cards = e.r.cards();
    assert!(
        cards
            .iter()
            .any(|(_, c)| c.contains("Brief : Ticket #7647")),
        "{cards:?}"
    );
}

#[tokio::test]
async fn a_tool_step_waits_for_approval_and_runs_once() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "ecriture",
            "ecrire",
            json!([
                {"id": "ecrire", "type": "tool", "tool": "fs_write",
                 "args": {"path": "{{workdir}}/note.txt", "content": "run {{run.id}}"},
                 "transitions": [
                    {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                    {"goto": "$blocked"}
                 ]}
            ]),
        ),
    )
    .await;
    let run = start_run(&e.d, "ecriture", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    let approvals = e.r.approvals();
    assert_eq!(approvals.len(), 1, "une carte d'approbation");
    drive(&e.d, &run.id).await.unwrap();
    assert_eq!(e.r.approvals().len(), 1, "une seule carte");

    penelope_agent::decide_approval(
        &e.d.agent,
        &approvals[0],
        &penelope_hitl::Decision::approve_once("test"),
    )
    .await
    .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let run = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    let note = std::path::Path::new(run.workdir.as_deref().unwrap()).join("note.txt");
    assert_eq!(
        std::fs::read_to_string(note).unwrap(),
        format!("run {}", run.id)
    );
}

#[tokio::test]
async fn shell_steps_are_replayed_from_the_ledger_after_a_crash() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "shell",
            "compter",
            json!([
                {"id": "compter", "type": "shell", "command": "echo passage >> passages.txt && wc -l < passages.txt",
                 "transitions": [{"goto": "$done"}]}
            ]),
        ),
    )
    .await;
    let run = start_run(&e.d, "shell", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    let wf = e.d.services.workflows.get("shell").unwrap();
    let step = wf.step("compter").unwrap().clone();
    let cancel = CancelToken::new();
    let ctx = StepCtx {
        d: &e.d,
        run: &run,
        wf: &wf,
        step: &step,
        attempt: 0,
        cancel: &cancel,
    };
    // L'étape s'exécute, puis « crash » avant l'avancement du run.
    let first = execute_step(&ctx).await.unwrap();
    let again = execute_step(&ctx).await.unwrap();
    assert_eq!(first, again, "rejoué depuis le ledger");
    let out = match first {
        StepOutcome::Done { result, output } => {
            assert_eq!(result, StepResult::Success, "{output}");
            output
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(out["stdout"].as_str().unwrap().trim(), "1");
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let workdir =
        e.d.services
            .runs
            .get(&run.id)
            .await
            .unwrap()
            .unwrap()
            .workdir
            .unwrap();
    let lines =
        std::fs::read_to_string(std::path::Path::new(&workdir).join("passages.txt")).unwrap();
    assert_eq!(
        lines.lines().count(),
        1,
        "la commande n'a tourné qu'une fois"
    );
}

#[tokio::test]
async fn waits_parallel_children_and_sub_workflows_compose() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "enfant",
            "pause",
            json!([
                {"id": "pause", "type": "wait", "on": {"duration_ms": 60000},
                 "transitions": [{"goto": "$done"}]}
            ]),
        ),
    )
    .await;
    install(
        &e.d,
        wf(
            "parent",
            "ensemble",
            json!([
                {"id": "ensemble", "type": "parallel", "children": [
                    {"id": "heure", "type": "tool", "tool": "time_now", "transitions": [{"goto": "$done"}]},
                    {"id": "encore", "type": "tool", "tool": "time_now", "transitions": [{"goto": "$done"}]}
                 ],
                 "transitions": [
                    {"goto": "sous", "condition": {"type": "step_result", "result": "success"}},
                    {"goto": "$blocked"}
                 ]},
                {"id": "sous", "type": "workflow", "workflowId": "enfant",
                 "transitions": [
                    {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                    {"goto": "$blocked"}
                 ]}
            ]),
        ),
    )
    .await;
    let run = start_run(&e.d, "parent", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    let parent = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(parent.current_step.as_deref(), Some("sous"));
    assert!(parent.step_outputs["ensemble"]["heure"]["result"] == "success");

    let child =
        e.d.services
            .runs
            .list(None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.parent_run.as_deref() == Some(run.id.as_str()))
            .expect("sous-run");
    assert_eq!(child.depth, 1);
    assert_eq!(
        drive(&e.d, &child.id).await.unwrap(),
        RunState::Running,
        "attente"
    );
    e.clock.advance_secs(61);
    assert_eq!(drive(&e.d, &child.id).await.unwrap(), RunState::Done);
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let parent = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(parent.step_outputs["sous"]["run"], child.id);
}

#[tokio::test]
async fn loops_stop_at_the_iteration_limit_and_control_works() {
    let e = env().await;
    let mut raw = wf(
        "boucle",
        "tour",
        json!([
            {"id": "tour", "type": "tool", "tool": "time_now", "transitions": [{"goto": "tour"}]}
        ]),
    );
    raw["settings"]["maxIterations"] = json!(3);
    // `$done` doit rester atteignable : une sortie jamais prise suffit à la validation.
    raw["steps"][0]["transitions"] = json!([
        {"goto": "$done", "condition": {"type": "step_result", "result": "jamais"}},
        {"goto": "tour"}
    ]);
    install(&e.d, raw).await;
    let run = start_run(&e.d, "boucle", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let blocked = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(blocked.iterations, 3);
    assert!(blocked.error.unwrap().contains("itérations"));
    assert!(
        control(&e.d, &run.id, &penelope_workflow::Control::Cancel)
            .await
            .is_ok()
    );
    assert_eq!(
        e.d.services.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Cancelled
    );
    assert!(
        control(&e.d, &run.id, &penelope_workflow::Control::Resume)
            .await
            .is_err(),
        "un run terminé ne reprend pas"
    );
}

/// T11 : un sous-agent n'a personne à qui demander une approbation (contexte neuf, pas
/// de carte) : un outil soumis à approbation termine le sous-agent en erreur, sans que
/// l'outil tourne.
#[tokio::test]
async fn a_sub_agent_that_needs_an_approval_fails_instead_of_waiting() {
    let e = env().await;
    let sid =
        e.d.services
            .sessions
            .create(penelope_kernel::session::SessionKind::Chat, None)
            .await
            .unwrap()
            .id
            .to_string();
    let ws = e.d.dir.path().join("espace");
    std::fs::create_dir_all(&ws).unwrap();
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "s1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": "touch fait.txt", "cwd": ws.display().to_string()}),
        }],
    ));
    e.p.reply("ne devrait pas arriver");
    let err = run_sub_agent(
        &e.d,
        SubAgentTask {
            session_id: &sid,
            run_id: None,
            kind: "general",
            prompt: "pose un fichier",
            model_id: "mock:model",
            tools: &["shell_exec".to_string()],
            workspaces: vec![ws.clone()],
        },
        &CancelToken::new(),
    )
    .await
    .expect_err("une approbation arrête le sous-agent");
    assert!(err.contains("soumis à approbation"), "{err}");
    assert!(!ws.join("fait.txt").exists(), "l'outil n'a pas tourné");
}

/// #57 : `/stop` pendant un sous-agent l'arrête : aucun appel au modèle après l'arrêt,
/// et le tour parent n'est pas touché par l'arrêt du sous-agent.
#[tokio::test]
async fn a_cancelled_turn_stops_its_sub_agent() {
    let e = env().await;
    e.p.reply("le sous-agent ne devrait pas répondre");
    let orchestrator: Arc<dyn crate::executor::Orchestrator> = Arc::new(WorkflowOrchestrator {
        context: e.d.cx.clone(),
    });
    let sid =
        e.d.services
            .sessions
            .create(penelope_kernel::session::SessionKind::Chat, None)
            .await
            .unwrap()
            .id
            .to_string();

    // Le tour parent est déjà arrêté quand le sous-agent démarre.
    let parent = CancelToken::new();
    parent.cancel();
    let _ = orchestrator
        .spawn_sub_agent(
            &sid,
            "cherche la cause",
            None,
            vec![],
            &Origin::Cli,
            &parent,
        )
        .await;
    assert_eq!(e.p.call_count(), 0, "aucun appel après l'arrêt");

    // Le sous-agent s'arrête tout seul (son délai) : le parent continue.
    let vivant = CancelToken::new();
    let enfant = vivant.child();
    enfant.cancel();
    assert!(!vivant.is_cancelled(), "le tour parent n'est pas touché");
}

/// #56 : une étape qui dépasse son `timeoutMs` enregistre son résultat et suit sa
/// transition, au lieu d'annuler le run et de repartir à chaque passage.
#[tokio::test]
async fn a_step_that_times_out_records_its_result_and_moves_on() {
    let e = env().await;
    e.p.slow(std::time::Duration::from_secs(5));
    e.p.reply("trop tard");
    let mut raw = wf(
        "delai",
        "reflechir",
        json!([
            {"id": "reflechir", "type": "agent", "prompt": "réfléchis", "timeoutMs": 200,
             "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "timeout"}},
                {"goto": "reflechir"}
             ]}
        ]),
    );
    raw["settings"]["maxIterations"] = json!(3);
    install(&e.d, raw).await;
    let run = start_run(&e.d, "delai", json!({}), &owner(), None, 0)
        .await
        .unwrap();

    // Un seul passage suffit : le résultat `timeout` mène à `$done`.
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let after = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(after.state, RunState::Done);
    let log = e.d.services.runs.trace(&run.id).await.unwrap();
    assert!(
        log.iter().any(|l| l["result"] == "timeout"),
        "le résultat doit être enregistré : {log:?}"
    );
    assert_eq!(e.p.call_count(), 1, "un seul appel au modèle");
}

/// #56 : dans un `parallel`, un enfant qui expire n'emporte pas ses frères.
#[tokio::test]
async fn a_timed_out_child_does_not_cancel_its_siblings() {
    let e = env().await;
    let mut raw = wf(
        "para",
        "groupe",
        json!([
            {"id": "groupe", "type": "parallel", "children": [
                {"id": "lent", "type": "shell", "command": "sleep 5", "timeoutMs": 200},
                {"id": "rapide", "type": "shell", "command": "echo bonjour"}
            ],
             "transitions": [{"goto": "$done"}]}
        ]),
    );
    raw["settings"]["maxIterations"] = json!(2);
    install(&e.d, raw).await;
    let run = start_run(&e.d, "para", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let log = e.d.services.runs.trace(&run.id).await.unwrap();
    let groupe = log
        .iter()
        .find(|l| l["step"] == "groupe")
        .expect("étape parallèle journalisée");
    let output = groupe["output"].clone();
    assert_eq!(
        output["rapide"]["result"], "success",
        "le frère rapide doit aboutir : {output}"
    );
    assert_eq!(output["lent"]["result"], "timeout", "{output}");
}

#[tokio::test]
async fn the_orchestrator_starts_runs_for_tools_and_schedules() {
    let e = env().await;
    install(
        &e.d,
        wf(
            "rapide",
            "fin",
            json!([{"id": "fin", "type": "wait", "on": {"duration_ms": 0}, "transitions": [{"goto": "$done"}]}]),
        ),
    )
    .await;
    let o = e.d.orchestrator().unwrap();
    let v = o
        .start_workflow("rapide", json!({}), None, &owner())
        .await
        .unwrap();
    let run_id = v["run_id"].as_str().unwrap().to_string();
    assert!(
        o.control_run(&run_id, "skip-step").await.is_err(),
        "approbation requise"
    );
    drive_all(&e.d).await.unwrap();
    for _ in 0..100 {
        let r = e.d.services.runs.get(&run_id).await.unwrap().unwrap();
        if r.state == RunState::Done {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("le run lancé par l'orchestrateur n'a pas abouti");
}
