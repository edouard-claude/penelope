//! Session locale reproductible pour éprouver le relais Pathlayer (#162).

use penelope_agent::ToolExecutor;
use penelope_app::bus::Origin;
use penelope_app::services::Services;
use penelope_daemon::runtime_events::{StreamConsumer, serve_connection};
use penelope_executor::executor::{NativeToolExecutor, ToolEnv};
use penelope_kernel::session::SessionKind;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let token = std::env::var("PENELOPE_EVENTS_TOKEN")?;
    anyhow::ensure!(token.len() >= 32, "jeton de 32 caractères minimum");
    let home = tempfile::tempdir()?;
    let services = Arc::new(
        Services::for_tests(
            home.path().to_path_buf(),
            penelope_kernel::clock::system_clock(),
        )
        .await?,
    );
    let workspace = home.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("boucle.txt"), "même contenu\n")?;
    services.config.mutate("démo", |config| {
        config.sandbox.workspaces = vec![workspace.to_string_lossy().into_owned()];
        Ok(vec!["sandbox.workspaces".into()])
    })?;
    let session = services
        .sessions
        .create(SessionKind::Chat, Some("démo Pathlayer".into()))
        .await?;
    let executor = NativeToolExecutor::new(
        services.clone(),
        ToolEnv {
            session_id: session.id.to_string(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: vec![workspace.clone()],
            in_workflow: false,
            turn_model: None,
        },
    );
    for _ in 0..4 {
        executor
            .execute("fs_read", &json!({"path": "boucle.txt"}))
            .await?;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:9466").await?;
    println!("session={} ws://127.0.0.1:9466/events", session.id);
    loop {
        let (socket, _) = listener.accept().await?;
        let log = Arc::new(services.events.clone());
        let consumer = StreamConsumer {
            token: token.clone(),
            kinds: vec!["runtime.tool".into()],
        };
        tokio::spawn(async move {
            let _ = serve_connection(socket, log, vec![consumer]).await;
        });
    }
}
