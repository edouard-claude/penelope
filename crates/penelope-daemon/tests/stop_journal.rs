//! #225 : un redémarrage demandé par la socket (ici au nom de Telegram) écrit son origine
//! dans le fichier de journal, avant toute sortie du processus.
//!
//! Binaire de test à part : le journal sur fichier s'installe une fois par processus.

use penelope_app::services::Services;
use penelope_daemon::rpc::Rpc;
use penelope_daemon::runtime::Daemon;
use serde_json::{Value, json};
use std::sync::Arc;

#[tokio::test]
async fn a_restart_requested_over_rpc_leaves_its_origin_in_the_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    penelope_observe::init(&logs, "info", 7, false);
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Arc::new(
        Services::for_tests(dir.path().join("home"), clock)
            .await
            .unwrap(),
    );
    let d = Daemon::from_services(s);
    let rpc = Rpc::new(d.core.clone());
    rpc.call(
        "restart",
        json!({"by": "telegram", "why": "/restart, confirmé"}),
    )
    .await
    .unwrap();
    assert!(d.handle.is_shutting_down() && d.handle.wants_restart());

    // Lu tout de suite, sans rien relâcher : l'écriture du journal n'est pas différée.
    let file = std::fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("penelope-"))
        .expect("fichier de journal du jour");
    let text = std::fs::read_to_string(file.path()).unwrap();
    let stop: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &Value| v["fields"]["message"] == "arrêt demandé")
        .collect();
    assert_eq!(stop.len(), 1, "{text}");
    assert_eq!(stop[0]["level"], "INFO");
    assert_eq!(stop[0]["fields"]["par"], "telegram");
    assert_eq!(stop[0]["fields"]["motif"], "/restart, confirmé");
    assert_eq!(stop[0]["fields"]["redemarrage"], true);
    assert!(stop[0]["timestamp"].is_string(), "{text}");
}
