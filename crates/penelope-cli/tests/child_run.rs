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
        None,
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

/// Un faux endpoint OpenAI-compatible sur la boucle locale : chaque `chat/completions`
/// répond en flux par `reply`, `GET /models` par une liste vide.
async fn fake_llm(reply: &'static str) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 256 * 1024];
                let mut got = 0;
                // L'en-tête, puis le corps annoncé.
                loop {
                    let n = sock.read(&mut buf[got..]).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if got >= end + 4 + len {
                            break;
                        }
                    }
                }
                let head = String::from_utf8_lossy(&buf[..got]).to_string();
                let resp = if head.starts_with("POST") && head.contains("/chat/completions") {
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let chunk = serde_json::json!({"id":"c1","object":"chat.completion.chunk","model":"essai",
                        "choices":[{"index":0,"delta":{"role":"assistant","content":reply},"finish_reason":null}]});
                    let end = serde_json::json!({"id":"c1","object":"chat.completion.chunk","model":"essai",
                        "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}});
                    let body = format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n");
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    let body = r#"{"object":"list","data":[]}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (base, calls)
}

/// #395 : un vrai sous-agent dans le vrai binaire enfant, sur un modèle local simulé : il
/// ouvre la base et la configuration de la maison du daemon, et rend sa réponse.
#[tokio::test]
async fn a_real_sub_agent_runs_in_the_child_process() {
    let home = tempfile::tempdir().unwrap();
    let (base, calls) = fake_llm("Trois fichiers lus, rien à signaler.").await;
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_penelope"))
        .args(["--home", home.path().to_str().unwrap(), "--json", "paths"])
        .output()
        .unwrap();
    let paths: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let config = std::path::PathBuf::from(paths["config"].as_str().unwrap());
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[providers.openrouter]\nenabled = false\n\n[providers.local]\nenabled = true\nbase_url = \"{base}\"\n"
        ),
    )
    .unwrap();
    let job = penelope_orchestrator::workflow::child::SubAgentJob {
        session_id: "s_essai".into(),
        run_id: None,
        kind: "explore".into(),
        prompt: "Relis le dépôt et dis ce qui cloche.".into(),
        model_id: "local:essai".into(),
        tools: Vec::new(),
        workspaces: vec![home.path().to_path_buf()],
        limits: None,
    };
    let host = UnixProcessHost::new(home.path().join("pids"));
    let spec = ProcessSpec::new(env!("CARGO_BIN_EXE_penelope"))
        .arg("run")
        .arg("--stdio")
        .env("PENELOPE_HOME", home.path().to_str().unwrap())
        // Sous Linux, le magasin de secrets est un fichier chiffré : sa phrase de passe.
        .env("PENELOPE_PASSPHRASE", "phrase-d-essai");
    let r = supervise(
        &host,
        spec,
        ToChild::init("s_essai", "sub_agent", serde_json::to_value(&job).unwrap()),
        &limits(),
        &CancelToken::new(),
        None,
    )
    .await;
    assert_eq!(r.cause, Cause::Done, "{r:?}");
    assert_eq!(
        r.result.unwrap()["text"],
        "Trois fichiers lus, rien à signaler."
    );
    assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
}
