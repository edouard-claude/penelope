//! Bout en bout sur la vraie socket : `chat.stream` puis `chat.send`, comme la CLI.

use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
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
    tokio::spawn(penelope_daemon::rpc::serve(d.core.clone()));
    // Attendre que la socket accepte : le fichier existe entre `bind` et `listen`, une
    // connexion à ce moment-là est refusée.
    let sock = d.services.platform.dirs.socket_path();
    for _ in 0..250 {
        if penelope_platform::ipc::connect(&sock).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (dir, d, p)
}

/// Comme la CLI : le jeton de session du daemon accompagne la requête (#91).
async fn exchange(d: &Daemon, req: RpcRequest) -> Vec<Value> {
    let sock = d.services.platform.dirs.socket_path();
    let req = req.with_auth(penelope_platform::ipc::read_token(&sock));
    exchange_raw(d, req).await
}

async fn exchange_raw(d: &Daemon, req: RpcRequest) -> Vec<Value> {
    let sock = d.services.platform.dirs.socket_path();
    let stream = penelope_platform::ipc::connect(&sock).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut body = serde_json::to_string(&req).unwrap();
    body.push('\n');
    write.write_all(body.as_bytes()).await.unwrap();
    let mut lines = BufReader::new(read).lines();
    let mut out = Vec::new();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
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
        RpcRequest::new(
            7,
            method::CHAT_STREAM,
            json!({"text": "bonjour, fais le point"}),
        ),
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
    assert!(
        seen.iter()
            .any(|t| t.ends_with("\n\nbonjour, fais le point")),
        "{seen:?}"
    );

    d.handle.shutdown();
}

/// #91 : sans jeton, ou avec un mauvais, la socket refuse et n'exécute rien ; le jeton
/// n'apparaît pas dans `doctor`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_without_the_session_token_is_refused() {
    let (_dir, d, _p) = start().await;
    let before = d.services.config.config().sandbox.default_profile.clone();
    let set = |auth: Option<&str>| {
        RpcRequest::new(
            3,
            method::CONFIG_SET,
            json!({"path": "sandbox.default_profile", "value": "readonly"}),
        )
        .with_auth(auth.map(String::from))
    };
    for auth in [None, Some("0000"), Some("")] {
        let lines = exchange_raw(&d, set(auth)).await;
        let err = lines[0]["error"]["message"].as_str().unwrap_or_default();
        assert!(err.starts_with("unauthorized"), "{auth:?} : {lines:?}");
    }
    assert_eq!(
        d.services.config.config().sandbox.default_profile,
        before,
        "rien n'a été écrit"
    );

    let sock = d.services.platform.dirs.socket_path();
    let token = penelope_platform::ipc::read_token(&sock).expect("jeton écrit");
    let lines = exchange(&d, RpcRequest::new(4, method::DOCTOR, json!({}))).await;
    assert!(lines[0]["result"].is_array(), "{lines:?}");
    assert!(
        !lines[0].to_string().contains(&token),
        "le jeton ne sort pas"
    );
    d.handle.shutdown();
}

/// #100 : un client de flux qui ferme sa connexion (Ctrl-C, terminal fermé, SSH coupé)
/// arrête son tour : issue `cancelled` en moins de deux secondes, plus aucun appel au
/// fournisseur ensuite.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_client_that_leaves_cancels_its_turn() {
    use penelope_llm::mock::Scripted;
    use penelope_llm::types::ToolCall;
    let (_dir, d, p) = start().await;
    p.slow(Duration::from_millis(300));
    p.reply(r#"{"complexity":"low"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "time_now".into(),
            arguments: json!({}),
        }],
    ));
    p.reply("jamais lu");

    let sock = d.services.platform.dirs.socket_path();
    let stream = penelope_platform::ipc::connect(&sock).await.unwrap();
    let (_read, mut write) = stream.into_split();
    let req = RpcRequest::new(
        9,
        method::CHAT_STREAM,
        json!({"text": "liste tout et résume chaque fichier en détail"}),
    )
    .with_auth(penelope_platform::ipc::read_token(&sock));
    let mut body = serde_json::to_string(&req).unwrap();
    body.push('\n');
    write.write_all(body.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let turn_id: String = d
        .services
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT id FROM turn_queue ORDER BY enqueued_at DESC, rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let outcome = d.bus.wait_for(&turn_id);
    let left = std::time::Instant::now();
    drop(write);
    drop(_read);

    let out = tokio::time::timeout(Duration::from_secs(5), outcome)
        .await
        .expect("le tour s'arrête")
        .unwrap();
    assert_eq!(out, penelope_agent::TurnOutcome::Cancelled);
    assert!(
        left.elapsed() < Duration::from_secs(2),
        "{:?}",
        left.elapsed()
    );
    let calls = p.call_count();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(p.call_count(), calls, "plus d'appel au fournisseur");
    d.handle.shutdown();
}
