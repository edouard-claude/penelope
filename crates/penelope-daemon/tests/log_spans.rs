//! #103 : chaque ligne écrite pendant un tour porte son identifiant et sa session.
//!
//! Binaire de test à part : l'intérêt des points de journalisation est mis en cache par
//! processus, et des tests voisins sans abonné le figeraient à « jamais ».

use penelope_app::bus::Origin;
use penelope_app::services::Services;
use penelope_daemon::Daemon;
use penelope_llm::mock::MockProvider;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn every_log_line_of_a_turn_carries_the_turn() {
    let (dispatch, buf) = penelope_observe::capture_json();
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let p = Arc::new(MockProvider::new());
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("fait");
    d.set_provider_override(p.clone());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.enqueue_message(&sid, "fais le point sur la semaine", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = s.turns.claim("runner-0").await.unwrap().unwrap();
    penelope_daemon::runner::process(&d, turn.clone(), Duration::from_secs(30)).await;

    let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let chosen: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &serde_json::Value| v["fields"]["message"] == "modèle choisi")
        .collect();
    assert_eq!(chosen.len(), 1, "{text}");
    assert_eq!(chosen[0]["span"]["turn"], turn.id.to_string());
    assert_eq!(chosen[0]["span"]["session"], sid);
}
