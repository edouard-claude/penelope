//! #464 : une demande d'accord dont le demandeur a disparu est retirée. Elle passe
//! `expired` avec sa raison, la clé de son job est oubliée, le canal est prévenu, et la
//! trancher après coup rend la raison au lieu d'un « approuvé » sans suite.

use super::*;
use crate::withdrawn::{gone, nothing_to_decide, sweep};
use penelope_executor::jobs::{self, NewJob};
use penelope_hitl::{ApprovalKind, ApprovalRequest, ApprovalState, Decision};
use penelope_kernel::session::SessionKind;

async fn ask(
    s: &Services,
    kind: ApprovalKind,
    session: Option<&str>,
    run: Option<&str>,
) -> ApprovalRequest {
    s.approvals
        .create(
            kind,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec"}),
            vec!["Autoriser".into(), "Refuser".into()],
            session,
            run,
            false,
        )
        .await
        .unwrap()
}

async fn chat(s: &Services) -> String {
    let session = s.sessions.create(SessionKind::Chat, None).await.unwrap();
    session.id.to_string()
}

async fn state_of(s: &Services, a: &ApprovalRequest) -> ApprovalState {
    let now = s.approvals.get(a.id.as_str()).await.unwrap().unwrap();
    now.state
}

/// Le daemon redémarre pendant qu'un sous-agent d'arrière-plan attend un accord : son job
/// est clos au démarrage, et la demande part avec lui. Tant que le job est seulement en
/// attente (il n'a pas encore repris), rien n'est retiré.
#[tokio::test]
async fn a_request_whose_job_was_lost_on_boot_is_withdrawn() {
    let e = env().await;
    let s = &e.d.services;
    let sid = chat(s).await;
    let store = jobs::store(s);
    let job = store
        .create(NewJob {
            session_id: sid.clone(),
            run_id: None,
            turn_id: None,
            call_id: None,
            tool: "sub_agent_spawn".into(),
            request: json!({"prompt": "pose un fichier"}),
            effect_id: None,
        })
        .await
        .unwrap()
        .id;
    let a = ask(s, ApprovalKind::ToolCall, Some(&sid), None).await;
    let key = jobs::child_approval_key(a.id.as_str());
    s.kv_set(&key, &job).await.unwrap();
    store.set_waiting(&job, true).await.unwrap();

    // Le job attend : sa carte aussi, même si la session du parent venait à se clore.
    s.sessions.set_state(&sid, "closed").await.unwrap();
    assert!(sweep(s, Some(&*e.r)).await.unwrap().is_empty());
    assert_eq!(state_of(s, &a).await, ApprovalState::Pending);

    // Redémarrage : le sous-agent vivait dans le daemon, son job est perdu.
    assert_eq!(store.recover_on_boot().await.unwrap().len(), 1);
    let withdrawn = sweep(s, Some(&*e.r)).await.unwrap();
    assert_eq!(withdrawn, vec![a.id.to_string()]);

    let after = s.approvals.get(a.id.as_str()).await.unwrap().unwrap();
    assert_eq!(after.state, ApprovalState::Expired);
    let why = after.reason.clone().unwrap();
    assert!(why.contains("sous-agent") && why.contains(&job), "{why}");
    assert!(s.kv_get(&key).await.unwrap().is_none(), "clé retirée");
    assert_eq!(e.r.withdrawn(), vec![(a.id.to_string(), why.clone())]);
    let events = s.events.range(0, 1000).await.unwrap();
    let said = events.iter().find(|ev| ev.kind == "approval.withdrawn");
    assert_eq!(said.unwrap().payload["approval_id"], a.id.as_str());

    // Un clic après coup : la raison, et aucune décision n'est prise.
    let told = nothing_to_decide(s, Some(&*e.r), &after).await.unwrap();
    assert_eq!(told.as_deref(), Some(why.as_str()));
    let late = Decision::approve_once("test");
    assert!(s.approvals.decide(a.id.as_str(), &late).await.is_err());
    // Rien à refaire au passage suivant.
    assert!(sweep(s, Some(&*e.r)).await.unwrap().is_empty());
    assert_eq!(e.r.withdrawn().len(), 1);
}

/// Un clic arrive avant le passage périodique : la demande est retirée à ce moment-là.
/// Un job annulé hors de ce processus (`job_cancel` après un redémarrage) compte comme
/// un demandeur disparu.
#[tokio::test]
async fn deciding_a_request_whose_job_was_cancelled_withdraws_it() {
    let e = env().await;
    let s = &e.d.services;
    let sid = chat(s).await;
    let store = jobs::store(s);
    let job = store
        .create(NewJob {
            session_id: sid.clone(),
            run_id: None,
            turn_id: None,
            call_id: None,
            tool: "sub_agent_spawn".into(),
            request: json!({}),
            effect_id: None,
        })
        .await
        .unwrap()
        .id;
    let a = ask(s, ApprovalKind::ToolCall, Some(&sid), None).await;
    let key = jobs::child_approval_key(a.id.as_str());
    s.kv_set(&key, &job).await.unwrap();
    store.set_waiting(&job, true).await.unwrap();
    assert!(nothing_to_decide(s, None, &a).await.unwrap().is_none());

    let cancelled = penelope_mcp::tasks::TaskState::Cancelled;
    store.finish(&job, cancelled, None).await.unwrap();
    let why = nothing_to_decide(s, None, &a).await.unwrap().unwrap();
    assert!(why.contains("cancelled"), "{why}");
    assert_eq!(state_of(s, &a).await, ApprovalState::Expired);
    assert!(s.kv_get(&key).await.unwrap().is_none());
}

/// Seule une fin constatée retire une demande : un run terminé, une session close. Un
/// run qui attend, un run introuvable, une session ouverte gardent la leur ; un effet
/// incertain se tranche sans son demandeur.
#[tokio::test]
async fn only_a_finished_requester_loses_its_request() {
    let e = env().await;
    let s = &e.d.services;
    let step = json!([{"id": "choisir", "name": "On y va ?", "type": "user",
                       "template": "question", "choices": ["Oui"],
                       "transitions": [{"goto": "$done"}]}]);
    install(&e.d, wf("attente", "choisir", step)).await;
    let run = start_run(&e.d, "attente", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    let (open, closing) = (chat(s).await, chat(s).await);

    let gate = ApprovalKind::WorkflowGate;
    let of_run = ask(s, gate, Some(&run.session_id), Some(&run.id)).await;
    let of_chat = ask(s, ApprovalKind::ToolCall, Some(&closing), None).await;
    let kept = [
        ask(s, ApprovalKind::ToolCall, Some(&open), None).await,
        ask(s, ApprovalKind::ToolCall, None, Some("r_inconnu")).await,
        ask(s, ApprovalKind::ToolCall, Some("s_inconnue"), None).await,
        ask(s, ApprovalKind::EffectUnknown, Some(&closing), None).await,
    ];
    assert!(sweep(s, None).await.unwrap().is_empty(), "tous attendent");

    s.runs
        .set_state(&run.id, RunState::Cancelled, None)
        .await
        .unwrap();
    s.sessions.set_state(&closing, "closed").await.unwrap();
    let why = gone(s, &of_run).await.unwrap().unwrap();
    assert!(why.contains("run") && why.contains(&run.id), "{why}");
    let mut withdrawn = sweep(s, None).await.unwrap();
    withdrawn.sort();
    let mut expected = vec![of_run.id.to_string(), of_chat.id.to_string()];
    expected.sort();
    assert_eq!(withdrawn, expected);
    for a in &kept {
        assert_eq!(state_of(s, a).await, ApprovalState::Pending, "{:?}", a.kind);
    }
}
