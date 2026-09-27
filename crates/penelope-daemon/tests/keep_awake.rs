//! Assertion anti-veille (§2.8, #228), sur l'API publique du daemon : un tour et un job
//! d'outil la tiennent tant qu'ils travaillent, et la relâchent à leur fin, erreur et
//! panique comprises. Le backend de test compte les assertions sans rien lancer.

use penelope_agent::TurnOutcome;
use penelope_app::bus::Origin;
use penelope_app::engine::{SessionModels, TurnIntake};
use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_executor::jobs::store;
use penelope_kernel::clock::{SharedClock, SystemClock};
use penelope_kernel::turn::Turn;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::{LlmErrorKind, ToolCall};
use penelope_mcp::tasks::TaskState;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

async fn daemon(root: &std::path::Path) -> (Arc<Daemon>, Arc<MockProvider>, String) {
    let clock: SharedClock = Arc::new(SystemClock);
    let s = Arc::new(
        Services::for_tests(root.to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    penelope_daemon::approval_mode::set(
        &d.services,
        &sid,
        penelope_agent::ApprovalMode::parse("auto"),
    )
    .await
    .unwrap();
    (d, p, sid)
}

async fn turn_of(d: &Daemon, sid: &str, text: &str) -> Turn {
    d.enqueue_message(sid, text, &Origin::Cli, None)
        .await
        .unwrap();
    d.services.turns.claim("test").await.unwrap().unwrap()
}

/// Assertions actives une fois retombées : une tâche relâche la sienne en finissant.
async fn awake_settles_to(d: &Daemon, want: u32) -> u32 {
    let power = &d.services.platform.power;
    for _ in 0..100 {
        if power.active() == want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    power.active()
}

#[tokio::test]
async fn a_turn_keeps_the_machine_awake_until_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (d, p, sid) = daemon(dir.path()).await;
    let platform = d.services.platform.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (during, log) = (platform.clone(), seen.clone());
    p.set_responder(Some(Arc::new(move |_| {
        log.lock().unwrap().push(during.power.active());
        Scripted::Text("C'est rangé.".into())
    })));

    let turn = turn_of(&d, &sid, "range le bureau").await;
    assert!(matches!(
        d.run_turn(&turn).await,
        TurnOutcome::Answered { .. }
    ));
    assert_eq!(
        seen.lock().unwrap().first(),
        Some(&1),
        "le modèle travaille sous l'assertion"
    );
    assert_eq!(platform.power.active(), 0, "relâchée à la fin du tour");
    d.services.turns.complete(&turn).await.unwrap();

    p.push(Scripted::Error(LlmErrorKind::Auth, "clé refusée".into()));
    let turn = turn_of(&d, &sid, "et la cuisine ?").await;
    assert!(matches!(
        d.run_turn(&turn).await,
        TurnOutcome::Failed { .. }
    ));
    assert_eq!(platform.power.active(), 0, "relâchée sur un tour en échec");
    d.services.turns.fail(&turn, "clé refusée").await.unwrap();

    p.push(Scripted::Panic("boum".into()));
    let turn = turn_of(&d, &sid, "et le salon ?").await;
    let run = std::panic::AssertUnwindSafe(d.run_turn(&turn));
    assert!(futures::FutureExt::catch_unwind(run).await.is_err());
    assert_eq!(
        platform.power.active(),
        0,
        "relâchée sur un tour qui panique"
    );
}

/// Le job survit au tour qui l'a lancé : il tient sa propre assertion jusqu'à sa
/// conclusion.
#[tokio::test]
async fn a_running_job_keeps_the_machine_awake_until_it_concludes() {
    let dir = tempfile::tempdir().unwrap();
    let (d, p, sid) = daemon(dir.path()).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": "sleep 0.5", "background": true}),
        }],
    ));
    p.reply("C'est parti.");
    let turn = turn_of(&d, &sid, "lance les tests").await;
    assert!(matches!(
        d.run_turn(&turn).await,
        TurnOutcome::Answered { .. }
    ));
    assert_eq!(
        d.services.platform.power.active(),
        1,
        "le tour est fini, le job tourne encore"
    );

    let jobs = store(&d.services).of_session(&sid, false).await.unwrap();
    assert_eq!(jobs.len(), 1);
    for _ in 0..200 {
        let job = store(&d.services).get(&jobs[0].id).await.unwrap().unwrap();
        if job.state.is_terminal() {
            assert_eq!(job.state, TaskState::Completed);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(awake_settles_to(&d, 0).await, 0, "relâchée à la conclusion");
}
