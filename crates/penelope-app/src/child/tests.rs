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
        format!(
            "{{\"type\":\"init\",\"protocol\":2,\"version\":\"{VERSION}\",\"run_id\":\"r1\",\"kind\":\"probe\",\"payload\":{{\"echo\":\"x\"}}}}\n"
        )
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
    supervise(&host, spec, init, &limits, cancel, None).await
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
        r#"echo '{"type":"ready","protocol":2,"pid":0}'; echo '{"type":"progress","text":"a"}'; echo '{"type":"done","result":{"ok":true}}'"#,
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

/// Un appel MCP de l'enfant traverse le protocole, et sa réponse le réveille (#395).
#[tokio::test]
async fn an_mcp_call_from_the_child_is_answered_by_the_supervisor() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (mut to_child, child_in) = tokio::io::duplex(64 * 1024);
    let (child_out, from_child) = tokio::io::duplex(64 * 1024);
    let serve = tokio::spawn(serve_io(child_in, child_out, std::sync::Arc::new(Probe)));
    let init = ToChild::init(
        "r1",
        "probe",
        serde_json::json!({"mcp": "notes__chercher", "args": {"q": "x"}}),
    );
    to_child.write_all(line(&init).as_bytes()).await.unwrap();
    let mut lines = BufReader::new(from_child).lines();
    let ready: FromChild =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(matches!(
        ready,
        FromChild::Ready {
            protocol: PROTOCOL,
            ..
        }
    ));
    let call: FromChild = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    let FromChild::McpCall {
        id,
        op,
        qualified,
        args,
        ..
    } = call
    else {
        panic!("appel MCP attendu : {call:?}");
    };
    assert_eq!(
        (op.as_str(), qualified.as_str()),
        ("call_tool", "notes__chercher")
    );
    assert_eq!(args["q"], "x");
    let reply = ToChild::McpResult {
        id,
        ok: Some(serde_json::json!({"trouvé": 2})),
        error: None,
    };
    to_child.write_all(line(&reply).as_bytes()).await.unwrap();
    let done: FromChild = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        done,
        FromChild::Done {
            result: serde_json::json!({"mcp": {"trouvé": 2}})
        }
    );
    serve.await.unwrap().unwrap();
}

/// Un enfant d'une autre version refuse le run : il migrerait la base sous le daemon.
#[tokio::test]
async fn a_child_of_another_version_refuses_the_run() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (mut to_child, child_in) = tokio::io::duplex(4096);
    let (child_out, from_child) = tokio::io::duplex(4096);
    let serve = tokio::spawn(serve_io(child_in, child_out, std::sync::Arc::new(Probe)));
    let init = ToChild::Init {
        protocol: PROTOCOL,
        version: "0.0.1".into(),
        run_id: "r1".into(),
        kind: "probe".into(),
        payload: serde_json::Value::Null,
    };
    to_child.write_all(line(&init).as_bytes()).await.unwrap();
    let mut lines = BufReader::new(from_child).lines();
    let failed: FromChild =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(
        matches!(&failed, FromChild::Failed { error } if error.contains("même version")),
        "{failed:?}"
    );
    assert!(serve.await.unwrap().is_err());
}

/// #395, reprise : le superviseur meurt en plein run (redémarrage du daemon), l'enfant
/// continue ; un second superviseur se rattache par `resume` et reçoit le résultat, y
/// compris l'appel MCP resté sans réponse, renvoyé.
#[tokio::test]
async fn a_child_survives_its_supervisor_and_a_new_one_reattaches() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("c1.sock");
    let serving = {
        let sock = sock.clone();
        tokio::spawn(
            async move { serve_socket(&sock, Arc::new(Probe), Duration::from_secs(30)).await },
        )
    };
    let limits = fast();
    let pid = std::process::id();
    // Premier superviseur : son host MCP ne répond jamais, il part pendant l'appel.
    struct Stuck;
    #[async_trait::async_trait]
    impl crate::ports::McpGateway for Stuck {
        async fn call_tool(
            &self,
            _: &str,
            _: &serde_json::Value,
            _: crate::elicitation::Destination,
        ) -> Result<serde_json::Value, String> {
            std::future::pending().await
        }
        async fn server_lines(&self) -> Vec<String> {
            Vec::new()
        }
    }
    let first = {
        let sock = sock.clone();
        let limits = limits.clone();
        tokio::spawn(async move {
            let init = ToChild::init("r1", "probe", serde_json::json!({"mcp": "notes__chercher"}));
            supervise_socket(
                pid,
                &sock,
                init,
                &limits,
                &CancelToken::new(),
                Some(Arc::new(Stuck)),
                Instant::now(),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    first.abort();
    // Second superviseur, avec un host MCP cette fois.
    struct Fake;
    #[async_trait::async_trait]
    impl crate::ports::McpGateway for Fake {
        async fn call_tool(
            &self,
            q: &str,
            _: &serde_json::Value,
            _: crate::elicitation::Destination,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({"servi": q}))
        }
        async fn server_lines(&self) -> Vec<String> {
            Vec::new()
        }
    }
    let r = supervise_socket(
        pid,
        &sock,
        ToChild::Resume,
        &limits,
        &CancelToken::new(),
        Some(Arc::new(Fake)),
        Instant::now(),
    )
    .await;
    assert_eq!(r.cause, Cause::Done, "{r:?}");
    assert_eq!(
        r.result,
        Some(serde_json::json!({"mcp": {"servi": "notes__chercher"}}))
    );
    serving.await.unwrap().unwrap();
    // Le résultat reste lisible après la fin, pour un daemon qui reviendrait plus tard.
    assert!(matches!(read_result(&sock), Some(FromChild::Done { .. })));
}
