//! `penelope run --stdio` derrière le superviseur (#395) : le vrai binaire, sans daemon ni
//! réseau, avec le run d'essai.

use penelope_app::child::{Cause, Limits, ToChild, supervise};
use penelope_llm::CancelToken;
use penelope_platform::process::process_exists;
use penelope_platform::{ProcessSpec, UnixProcessHost};
use std::time::Duration;

async fn run(
    payload: serde_json::Value,
    limits: Limits,
    cancel: &CancelToken,
) -> penelope_app::child::Report {
    let host = UnixProcessHost::new(std::env::temp_dir().join("penelope-child-run"));
    let spec = ProcessSpec::new(env!("CARGO_BIN_EXE_penelope"))
        .arg("run")
        .arg("--stdio");
    supervise(
        &host,
        spec,
        ToChild::init("r1", "probe", payload),
        &limits,
        cancel,
    )
    .await
}

fn limits() -> Limits {
    Limits {
        max_rss_kb: None,
        max_duration: Duration::from_secs(30),
        grace: Duration::from_secs(2),
        poll: Duration::from_millis(50),
    }
}

#[tokio::test]
async fn a_run_in_a_child_process_returns_its_result() {
    let r = run(
        serde_json::json!({"echo": "bonjour", "progress": ["un", "deux"]}),
        limits(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!(r.cause, Cause::Done, "{r:?}");
    assert_eq!(r.result.unwrap()["echo"], "bonjour");
    assert_eq!(r.progress, ["un", "deux"]);
}

#[tokio::test]
async fn stop_reaches_the_child_which_stops_by_itself() {
    let cancel = CancelToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c.cancel();
    });
    let r = run(serde_json::json!({"sleep_ms": 30_000}), limits(), &cancel).await;
    assert_eq!(r.cause, Cause::Interrupted);
    assert!(!process_exists(r.pid));
}

#[tokio::test]
async fn a_crashing_child_reports_its_cause() {
    let r = run(
        serde_json::json!({"abort": true}),
        limits(),
        &CancelToken::new(),
    )
    .await;
    assert!(
        matches!(
            r.cause,
            Cause::Exit {
                signal: Some(6),
                ..
            }
        ),
        "{r:?}"
    );
    let r = run(
        serde_json::json!({"exit": 3}),
        limits(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!(
        r.cause,
        Cause::Exit {
            code: Some(3),
            signal: None
        }
    );
}

#[tokio::test]
async fn a_child_that_swells_is_killed_for_memory() {
    let l = Limits {
        max_rss_kb: Some(200 * 1024),
        ..limits()
    };
    let r = run(
        serde_json::json!({"alloc_mb": 400, "sleep_ms": 30_000}),
        l,
        &CancelToken::new(),
    )
    .await;
    assert!(matches!(r.cause, Cause::Oom { .. }), "{r:?}");
    assert!(!process_exists(r.pid));
}
