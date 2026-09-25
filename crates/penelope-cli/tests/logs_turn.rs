//! #206 : `penelope logs --turn <id>` montre les tentatives d'un tour raté, dans l'ordre,
//! avec le modèle, la cause et le début du texte reçu.
//!
//! Binaire de test à part, comme `penelope-daemon/tests/log_spans.rs` : l'abonné JSON
//! en mémoire doit être le seul du processus.

use penelope_app::bus::Origin;
use penelope_app::services::Services;
use penelope_daemon::Daemon;
use penelope_llm::mock::{MockProvider, Scripted};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn logs_of_a_turn_show_its_attempts() {
    let (dispatch, buf) = penelope_observe::capture_json();
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Arc::new(
        Services::for_tests(data.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let p = Arc::new(MockProvider::new());
    p.reply(r#"{"complexity":"low"}"#);
    p.push(Scripted::MidStreamError(
        "Voici le début de la réponse".into(),
        "connexion perdue".into(),
    ));
    d.set_provider_override(p.clone());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.enqueue_message(&sid, "explique-moi le plan", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = s.turns.claim("runner-0").await.unwrap().unwrap();
    penelope_daemon::runner::process(&d, turn.clone(), Duration::from_secs(30)).await;

    let logs = home.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let today = chrono::Utc::now().format("%Y-%m-%d");
    std::fs::write(
        logs.join(format!("penelope-{today}.jsonl")),
        buf.lock().unwrap().clone(),
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_penelope"))
        .arg("--home")
        .arg(home.path())
        .args(["logs", "--turn", turn.id.as_str()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let attempts: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &serde_json::Value| v["fields"]["message"] == "tentative sans réponse")
        .collect();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    let a = &attempts[0]["fields"];
    assert_eq!(a["cause"], "stream_cut");
    assert!(a["model"].is_string());
    assert_eq!(a["text"], "Voici le début de la réponse");
    assert_eq!(attempts[0]["span"]["turn"], turn.id.to_string());
}
