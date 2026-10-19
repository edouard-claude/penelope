use super::*;
use penelope_llm::CancelToken;
use penelope_platform::process::process_exists;
use penelope_platform::{ProcessSpec, UnixProcessHost};
use std::time::Duration;

#[test]
fn the_protocol_schema_is_fixed() {
    let init = ToChild::init("r1", "probe", serde_json::json!({"echo": "x"}));
    assert_eq!(
        line(&init),
        "{\"type\":\"init\",\"protocol\":1,\"run_id\":\"r1\",\"kind\":\"probe\",\"payload\":{\"echo\":\"x\"}}\n"
    );
    assert_eq!(line(&ToChild::Interrupt), "{\"type\":\"interrupt\"}\n");
    let done: FromChild = serde_json::from_str(r#"{"type":"done","result":3}"#).unwrap();
    assert_eq!(
        done,
        FromChild::Done {
            result: serde_json::json!(3)
        }
    );
    let failed = FromChild::Failed { error: "e".into() };
    assert_eq!(
        serde_json::from_str::<FromChild>(line(&failed).trim()).unwrap(),
        failed
    );
}

fn report(cause: Cause) -> Report {
    Report {
        cause,
        result: None,
        progress: Vec::new(),
        pid: 1,
        rss_max_kb: 0,
    }
}

#[test]
fn a_dead_child_is_retried_once_and_only_a_dead_one() {
    let dead = report(Cause::Exit {
        code: None,
        signal: Some(9),
    });
    assert!(dead.retry_allowed(0));
    assert!(!dead.retry_allowed(1));
    assert!(report(Cause::Oom { rss_kb: 9 }).retry_allowed(0));
    for c in [
        Cause::Done,
        Cause::Interrupted,
        Cause::Timeout,
        Cause::Failed { error: "x".into() },
    ] {
        assert!(!report(c).retry_allowed(0));
    }
    assert_eq!(
        dead.describe(),
        "mort sans résultat, tué par le signal 9 (SIGKILL)"
    );
}

/// Un faux enfant en shell : il lit `init`, puis fait ce que dit `script`.
async fn run(script: &str, limits: Limits, cancel: &CancelToken) -> Report {
    let dir = std::env::temp_dir().join(format!("penelope-child-{}", std::process::id()));
    let host = UnixProcessHost::new(dir);
    let spec = ProcessSpec::new("/bin/sh")
        .arg("-c")
        .arg(format!("read l; {script}"));
    let init = ToChild::init("r1", "probe", serde_json::Value::Null);
    supervise(&host, spec, init, &limits, cancel).await
}

fn fast() -> Limits {
    Limits {
        max_rss_kb: None,
        max_duration: Duration::from_secs(20),
        grace: Duration::from_millis(300),
        poll: Duration::from_millis(50),
    }
}

#[tokio::test]
async fn a_child_that_answers_gives_its_result_and_progress() {
    let r = run(
        r#"echo '{"type":"ready","protocol":1,"pid":0}'; echo '{"type":"progress","text":"a"}'; echo '{"type":"done","result":{"ok":true}}'"#,
        fast(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!(r.cause, Cause::Done);
    assert_eq!(r.result, Some(serde_json::json!({"ok": true})));
    assert_eq!(r.progress, vec!["a".to_string()]);
}

#[tokio::test]
async fn a_killed_child_leaves_its_signal_not_a_fallen_daemon() {
    let r = run("kill -9 $$", fast(), &CancelToken::new()).await;
    assert_eq!(
        r.cause,
        Cause::Exit {
            code: None,
            signal: Some(9)
        }
    );
    assert!(r.retry_allowed(0));
}

#[tokio::test]
async fn a_child_past_its_duration_is_stopped_with_its_whole_group() {
    let limits = Limits {
        max_duration: Duration::from_millis(300),
        ..fast()
    };
    let r = run("sleep 30 & wait", limits, &CancelToken::new()).await;
    assert_eq!(r.cause, Cause::Timeout);
    assert!(!process_exists(r.pid));
}

#[tokio::test]
async fn a_child_past_its_memory_bound_is_killed() {
    let limits = Limits {
        max_rss_kb: Some(1),
        ..fast()
    };
    let r = run("sleep 30", limits, &CancelToken::new()).await;
    assert!(
        matches!(r.cause, Cause::Oom { rss_kb } if rss_kb > 1),
        "{r:?}"
    );
    assert!(!process_exists(r.pid));
    assert!(r.describe().starts_with("arrêté : mémoire dépassée"));
}

#[tokio::test]
async fn stop_interrupts_a_child_that_ignores_it() {
    let cancel = CancelToken::new();
    cancel.cancel();
    let r = run("sleep 30", fast(), &cancel).await;
    assert_eq!(r.cause, Cause::Interrupted);
    assert!(!process_exists(r.pid));
}
