//! #313 : `config reload` reconstruit les fournisseurs, comme `config set` ; sans cela, un
//! `[providers.codex]` édité à la main n'était lu qu'au redémarrage.

use penelope_app::services::Services;
use penelope_daemon::rpc::Rpc;
use penelope_daemon::runtime::Daemon;
use penelope_kernel::api::{RpcRequest, method};
use penelope_kernel::clock::TestClock;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn config_reload_rebuilds_the_providers() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let core = Daemon::from_services(s.clone()).core;
    let rpc = Rpc::new(core.clone());
    s.platform
        .secrets
        .set("openrouter_api_key", "sk-or-test-0123456789")
        .unwrap();
    assert!(core.provider_for("openrouter:a/b").await.is_ok());

    let mut edited = (*s.config.config()).clone();
    edited.providers.openrouter.enabled = false;
    std::fs::write(s.config.path(), edited.to_toml().unwrap()).unwrap();
    // Tant que rien ne relit le fichier, les fournisseurs construits restent.
    assert!(core.provider_for("openrouter:a/b").await.is_ok());

    let reloaded = rpc
        .handle(RpcRequest::new(1, method::CONFIG_RELOAD, json!({})))
        .await;
    assert!(reloaded.error.is_none(), "{:?}", reloaded.error);
    assert!(core.provider_for("openrouter:a/b").await.is_err());
}
