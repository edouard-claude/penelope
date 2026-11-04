//! #422 : un sous-agent en arrière-plan devant une carte l'attend au lieu d'échouer. La
//! carte dit qu'elle vient de lui, son job passe `input_required`, aucun tour du parent ;
//! accordée, il reprend et finit ; refusée, il conclut sans l'outil.

use super::*;
use penelope_executor::jobs::{self, NewJob, live};
use penelope_mcp::tasks::TaskState;

struct Bench {
    e: Env,
    sid: String,
    job: String,
    ws: std::path::PathBuf,
}

async fn bench() -> Bench {
    let e = env().await;
    let s = &e.d.services;
    let sid = s
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    let job = jobs::store(s)
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
    Bench { e, sid, job, ws }
}

/// Le sous-agent tourne dans son job ; `decide` tranche la carte quand elle arrive, après
/// avoir vérifié ce que voit le propriétaire.
async fn run_and_decide(b: &Bench, decision: penelope_hitl::Decision) -> Result<String, String> {
    let s = &b.e.d.services;
    let tools = ["shell_exec".to_string()];
    let (state, cancel) = (s.platform.dirs.state(), CancelToken::new());
    let sub = live::scoped(
        &state,
        &b.job,
        run_sub_agent(
            &b.e.d,
            SubAgentTask {
                session_id: &b.sid,
                run_id: None,
                kind: "general",
                prompt: "pose un fichier dans l'espace",
                model_id: "mock:model",
                tools: &tools,
                workspaces: vec![b.ws.clone()],
                limits: None,
            },
            &cancel,
        ),
    );
    let owner = async {
        let a = loop {
            if let Some(a) = s.approvals.pending(10).await.unwrap().into_iter().next() {
                break a;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        // La carte a été envoyée et dit qu'elle vient du sous-agent, avec sa tâche.
        for _ in 0..40 {
            if !b.e.r.approvals().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(b.e.r.approvals(), vec![a.id.0.clone()]);
        let a = s.approvals.get(a.id.as_str()).await.unwrap().unwrap();
        assert_eq!(a.payload["why_from"], "enfant");
        let why = a.payload["why"].as_str().unwrap();
        assert!(
            why.contains("sous-agent `general`") && why.contains("pose un fichier"),
            "{why}"
        );
        let job = jobs::store(s).get(&b.job).await.unwrap().unwrap();
        assert_eq!(job.state, TaskState::InputRequired);
        assert!(
            s.kv_get(&jobs::child_approval_key(a.id.as_str()))
                .await
                .unwrap()
                .is_some(),
            "sa décision ne relance pas de tour du parent"
        );
        s.approvals.decide(a.id.as_str(), &decision).await.unwrap();
    };
    let (out, ()) = tokio::join!(sub, owner);
    out
}

#[tokio::test]
async fn an_approved_card_lets_the_background_sub_agent_finish() {
    let b = bench().await;
    b.e.p.reply("fichier posé");
    let out = run_and_decide(&b, penelope_hitl::Decision::approve_once("test")).await;
    assert_eq!(out.unwrap(), "fichier posé");
    assert!(
        b.ws.join("fait.txt").exists(),
        "l'outil a tourné après l'accord"
    );
    let job = jobs::store(&b.e.d.services)
        .get(&b.job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.state, TaskState::Working, "il a repris");
}

#[tokio::test]
async fn a_refused_card_makes_the_sub_agent_conclude_without_the_tool() {
    let b = bench().await;
    b.e.p.reply("je n'ai rien posé : refusé");
    let out = run_and_decide(&b, penelope_hitl::Decision::deny("test", None)).await;
    assert_eq!(out.unwrap(), "je n'ai rien posé : refusé");
    assert!(!b.ws.join("fait.txt").exists());
}

#[tokio::test]
async fn a_restart_during_the_wait_fails_the_job_in_plain_words() {
    let b = bench().await;
    let store = jobs::store(&b.e.d.services);
    store.set_waiting(&b.job, true).await.unwrap();
    let lost = store.recover_on_boot().await.unwrap();
    assert_eq!(lost.len(), 1);
    let job = store.get(&b.job).await.unwrap().unwrap();
    assert_eq!(job.state, TaskState::Failed);
    assert_eq!(job.result.unwrap()["error"], jobs::LOST_WAITING_ON_BOOT);
}
