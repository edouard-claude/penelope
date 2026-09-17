//! Bout en bout sur la vraie socket : `chat.stream` puis `chat.send`, comme la CLI.

use penelope_daemon::runtime::{Daemon, Services};
use penelope_kernel::api::{RpcRequest, method};
use penelope_kernel::clock::SystemClock;
use penelope_llm::mock::MockProvider;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn start() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
    // Chemin court : une socket Unix est limitée à ~104 octets.
    let dir = tempfile::Builder::new()
        .prefix("pen")
        .tempdir_in("/tmp")
        .unwrap();
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), Arc::new(SystemClock))
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    tokio::spawn(penelope_daemon::runner::run_pool(d.clone()));
    tokio::spawn(penelope_daemon::rpc::serve(d.clone()));
    // Laisser la socket apparaître.
    let sock = d.services.platform.dirs.socket_path();
    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (dir, d, p)
}

async fn exchange(d: &Daemon, req: RpcRequest) -> Vec<Value> {
    let sock = d.services.platform.dirs.socket_path();
    let stream = penelope_platform::ipc::connect(&sock).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut body = serde_json::to_string(&req).unwrap();
    body.push('\n');
    write.write_all(body.as_bytes()).await.unwrap();
    let mut lines = BufReader::new(read).lines();
    let mut out = Vec::new();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("le daemon doit répondre")
            .unwrap()
            .expect("connexion fermée trop tôt");
        let v: Value = serde_json::from_str(&line).unwrap();
        let is_final = !v["id"].is_null();
        out.push(v);
        if is_final {
            return out;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_stream_sends_deltas_then_the_final_answer() {
    let (_dir, d, p) = start().await;
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("Bonjour depuis la socket.");

    let lines = exchange(
        &d,
        RpcRequest::new(7, method::CHAT_STREAM, json!({"text": "bonjour"})),
    )
    .await;

    let deltas: String = lines
        .iter()
        .filter(|l| l["params"]["type"] == "delta")
        .filter_map(|l| l["params"]["text"].as_str())
        .collect();
    assert_eq!(deltas, "Bonjour depuis la socket.");

    let last = lines.last().unwrap();
    assert_eq!(last["id"], 7);
    assert_eq!(last["result"]["outcome"], "answered");
    assert_eq!(last["result"]["text"], "Bonjour depuis la socket.");

    // `chat.send` sur la même session courante : l'historique suit. Le premier message
    // était « simple », rien n'est collant : le second repasse par le classifieur.
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Toujours là.");
    let lines = exchange(
        &d,
        RpcRequest::new(8, method::CHAT_SEND, json!({"text": "tu es là ?"})),
    )
    .await;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["result"]["text"], "Toujours là.");
    let seen: Vec<String> = p
        .requests()
        .last()
        .unwrap()
        .messages
        .iter()
        .map(|m| m.text())
        .collect();
    assert!(seen.iter().any(|t| t.ends_with("\n\nbonjour")), "{seen:?}");

    d.handle.shutdown();
}
