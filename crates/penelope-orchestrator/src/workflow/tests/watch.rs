//! Suivre un run de loin (#346) : autonomie héritée par les sous-runs, état lisible avec
//! la liste et ce qui retient, destination déplacée avec les sous-runs.

use super::*;
use penelope_app::autonomy;

/// Une liste de trois éléments dont chaque sous-run attend une réponse du propriétaire :
/// le premier reste en cours.
async fn waiting_list(e: &Env, autonomous: bool) -> Run {
    install(
        &e.d,
        json!({
            "metadata": {"id": "attente", "name": "Attente",
                         "parameters": [{"id": "item", "type": "object", "required": true}]},
            "entryStep": "choisir",
            "settings": {"maxIterations": 5, "budget": {"maxUsd": 1.0}},
            "steps": [{"id": "choisir", "name": "On y va ?", "type": "user",
                       "template": "question", "choices": ["Oui"],
                       "transitions": [{"goto": "$done"}]}]
        }),
    )
    .await;
    let mut list = wf(
        "serie",
        "liste",
        json!([{
            "id": "liste", "type": "foreach", "workflowId": "attente",
            "items": [{"title": "1.1 Socle"}, {"title": "1.2 PWA"}, {"title": "1.3 Écran"}],
            "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                {"goto": "$blocked"}]
        }]),
    );
    list["settings"]["autonomous"] = json!(autonomous);
    list["settings"]["budget"]["maxWallMs"] = json!(0);
    install(&e.d, list).await;
    let run = start_run(&e.d, "serie", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    for _ in 0..5 {
        for r in
            e.d.services
                .runs
                .list(Some(RunState::Running), 50)
                .await
                .unwrap()
        {
            drive(&e.d, &r.id).await.unwrap();
        }
    }
    run
}

async fn child_of(s: &Services, run: &Run) -> Run {
    s.runs
        .list(None, 50)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.parent_run.as_deref() == Some(run.id.as_str()))
        .unwrap()
}

/// Un workflow déclaré autonome l'est avec ses sous-runs ; le propriétaire peut le
/// remettre sous supervision, ou rendre autonome un run qui ne l'était pas, sous-runs
/// compris, sans relance.
#[tokio::test]
async fn autonomy_comes_from_the_workflow_or_the_owner_and_reaches_sub_runs() {
    let e = env().await;
    let s = &e.d.services;
    let run = waiting_list(&e, true).await;
    let child = child_of(s, &run).await;
    assert!(autonomy::is_autonomous(s, &run.id).await);
    assert!(autonomy::is_autonomous(s, &child.id).await);
    autonomy::set(s, &run.id, false).await.unwrap();
    assert!(!autonomy::is_autonomous(s, &child.id).await);

    let e = env().await;
    let s = &e.d.services;
    let run = waiting_list(&e, false).await;
    let child = child_of(s, &run).await;
    assert!(!autonomy::is_autonomous(s, &child.id).await);
    autonomy::set(s, &run.id, true).await.unwrap();
    assert!(autonomy::is_autonomous(s, &child.id).await);
    assert_eq!(
        pending_of_autonomous(s).await.unwrap(),
        Vec::<String>::new()
    );
}

/// `workflow_status` d'une liste en cours : sa place, le décompte, l'autonomie, et
/// l'approbation qu'attend le sous-run, avec sa durée ; celle-ci est à lever d'office
/// quand le run est autonome.
#[tokio::test]
async fn the_status_says_where_the_list_is_and_what_holds_it() {
    let e = env().await;
    let s = &e.d.services;
    let run = waiting_list(&e, false).await;
    let child = child_of(s, &run).await;

    let v = run_status(s, &run.id).await.unwrap();
    assert_eq!(
        v["progress"], "élément 1/3 · 1.1 Socle · étape choisir",
        "{v}"
    );
    assert_eq!(v["list"]["total"], 3, "{v}");
    assert_eq!(v["list"]["done"], 0);
    assert_eq!(v["list"]["todo"], 3);
    assert_eq!(v["autonomous"], false);
    assert!(v["waiting"].is_null(), "{v}");

    let a = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec"}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(child.session_id.as_str()),
            Some(child.id.as_str()),
            false,
        )
        .await
        .unwrap();
    e.clock.advance_ms(100 * 60 * 1000);
    let v = run_status(s, &run.id).await.unwrap();
    let waiting = v["waiting"].as_str().unwrap();
    assert!(
        waiting.starts_with("bloqué : en attente d'approbation `shell_exec` depuis 1 h 40"),
        "{waiting}"
    );
    assert!(waiting.contains(child.id.as_str()), "{waiting}");

    autonomy::set(s, &run.id, true).await.unwrap();
    assert_eq!(
        pending_of_autonomous(s).await.unwrap(),
        vec![a.id.to_string()]
    );
    assert_eq!(run_status(s, &run.id).await.unwrap()["autonomous"], true);
}

/// Un run lancé hors de Telegram parle dans le fil privé ; `move` le déplace dans un
/// sujet, avec son sous-run en cours.
#[tokio::test]
async fn moving_a_run_moves_its_sub_runs() {
    let e = env().await;
    let s = &e.d.services;
    let run = waiting_list(&e, false).await;
    let child = child_of(s, &run).await;
    let topic = Origin::Telegram {
        chat_id: 42,
        topic_id: Some(5869),
        message_id: None,
    };
    let moved = retarget(s, &run.id, &topic).await.unwrap();
    assert_eq!(moved, vec![run.id.clone(), child.id.clone()]);
    for id in [&run.id, &child.id] {
        assert_eq!(origin_of(s, id).await.to_value(), topic.to_value(), "{id}");
    }
}

/// #358 : un sous-agent lancé depuis l'étape d'un run autonome (`sub_agent_spawn`) travaille
/// pour ce run : une écriture du shell ne demande rien. Hors run, elle demande.
#[tokio::test]
async fn a_sub_agent_spawned_by_an_autonomous_run_asks_nothing() {
    let e = env().await;
    let s = &e.d.services;
    let run = waiting_list(&e, true).await;
    // « Tout demander » : hors run autonome, l'écriture attend forcément le propriétaire.
    s.config
        .mutate("test", |c| {
            c.tools.approval_mode = "ask".into();
            Ok(vec!["tools.approval_mode".into()])
        })
        .unwrap();
    let o = WorkflowOrchestrator {
        context: e.d.cx.clone(),
    };
    let write = || {
        Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "w1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command": "touch essai-sous-agent", "pourquoi": "essai"}),
            }],
        )
    };
    e.p.push(write());
    e.p.reply("Fait.");
    penelope_executor::executor::Orchestrator::spawn_sub_agent(
        &o,
        run.session_id.as_str(),
        "crée le fichier d'essai",
        None,
        vec!["shell_exec".into()],
        &owner(),
        &penelope_llm::CancelToken::new(),
    )
    .await
    .unwrap();
    assert!(
        s.approvals.pending(10).await.unwrap().is_empty(),
        "{:?}",
        s.approvals.pending(10).await.unwrap()
    );

    let chat = s
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap();
    e.p.push(write());
    e.p.reply("Je demande.");
    let _ = penelope_executor::executor::Orchestrator::spawn_sub_agent(
        &o,
        chat.id.as_str(),
        "crée le fichier d'essai",
        None,
        vec!["shell_exec".into()],
        &owner(),
        &penelope_llm::CancelToken::new(),
    )
    .await;
    assert_eq!(s.approvals.pending(10).await.unwrap().len(), 1);
}
