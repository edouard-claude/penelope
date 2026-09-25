use super::*;

use penelope_kernel::clock::TestClock;

use penelope_llm::mock::{MockProvider, Scripted};

use penelope_llm::types::ToolCall;

use penelope_telegram::api::method as tg;

use penelope_telegram::mock::{MockTransport, updates};

mod approvals;
mod background;
mod bursts;
mod delivery;
mod elicitation;
mod forms;
mod media;
mod ops;
mod sessions;
mod workflows;

const OWNER: i64 = 42;

async fn gateway() -> (
    tempfile::TempDir,
    Arc<TelegramGateway>,
    Arc<MockTransport>,
    Arc<MockProvider>,
) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    // Le limiteur réel dort une seconde par message : inutile en test.
    d.publish_config("test", |c| {
        c.telegram.rate_per_chat_per_s = 1_000.0;
        Ok(vec!["telegram.rate_per_chat_per_s".into()])
    })
    .unwrap();
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let t = MockTransport::new();
    let g = TelegramGateway::with_transport(d, t.clone());
    g.register();
    (dir, g, t, p)
}

/// Laisse un clic de bouton finir son travail détaché (issue #73).
async fn settle_click(g: &TelegramGateway) {
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;
    }
    let _ = g.flush_outbox().await;
}

/// Laisse les traitements détachés (vocal, photo, export, audit : issue #69) arriver
/// au bout avant d'observer ce qui a été envoyé.
async fn settle(g: &TelegramGateway) {
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if g.daemon.services.turns.pending_count().await.unwrap_or(0) > 0 {
            return;
        }
    }
}

/// Exécute les tours en file, comme le ferait le pool de runners.
async fn drain(g: &TelegramGateway) {
    while let Some(turn) = g.daemon.services.turns.claim("test").await.unwrap() {
        penelope_daemon::runner::process(&g.daemon, turn, Duration::from_secs(30)).await;
    }
    g.flush_outbox().await.unwrap();
}

/// Boutons d'un message envoyé : (libellé, `callback_data` ou URL).
fn inline_buttons(call: &Value) -> Vec<(String, String)> {
    call["reply_markup"]["inline_keyboard"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row.as_array().cloned().unwrap_or_default())
        .map(|b| {
            let target = b["callback_data"].as_str().or(b["url"].as_str());
            (
                b["text"].as_str().unwrap_or_default().to_string(),
                target.unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn texts(calls: &[Value]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect()
}

/// Insère un envoi dans la file, comme `outbox_push`, à une date donnée.
async fn queue(g: &TelegramGateway, id: &str, chat: i64, text: &str, at: &str) {
    let (id, text, at) = (id.to_string(), text.to_string(), at.to_string());
    g.daemon
        .services
        .store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                     VALUES(?1, ?2, 'sendMessage', ?3, 'pending', ?4)",
                params![
                    id,
                    chat,
                    json!({"chat_id": chat, "text": text}).to_string(),
                    at
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Boutons du dernier menu `/model` envoyé : (libellé, jeton).
async fn model_buttons(t: &MockTransport) -> Vec<(String, String)> {
    let menu = t
        .calls_to(tg::SEND_MESSAGE)
        .await
        .into_iter()
        .rev()
        .find(|c| {
            c["text"]
                .as_str()
                .unwrap_or("")
                .contains("Modèle de cette session")
        })
        .expect("menu /model envoyé");
    menu["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row[0]["text"].as_str().unwrap().to_string(),
                row[0]["callback_data"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Octets d'un JPEG : l'en-tête suffit à la reconnaissance.
const JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
];

fn photo_update(
    update_id: i64,
    file_id: &str,
    group: Option<&str>,
    caption: Option<&str>,
) -> Value {
    let mut u = updates::photo(update_id, OWNER, OWNER, group);
    u["message"]["photo"][0]["file_id"] = json!(file_id);
    if let Some(c) = caption {
        u["message"]["caption"] = json!(c);
    }
    u
}

fn document_update(update_id: i64, file_id: &str, name: &str, caption: Option<&str>) -> Value {
    let mut u = updates::document(update_id, OWNER, OWNER, name);
    u["message"]["document"]["file_id"] = json!(file_id);
    if let Some(c) = caption {
        u["message"]["caption"] = json!(c);
    }
    u
}

/// Attend qu'une condition asynchrone devienne vraie (tâches lancées en fond).
async fn eventually<F, Fut>(mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..150 {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Dernier message envoyé ou édité portant des boutons, et le jeton du bouton dont le
/// libellé contient `label`.
async fn button(t: &MockTransport, label: &str) -> String {
    let mut calls = t.calls_to(tg::SEND_MESSAGE).await;
    calls.extend(t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
    calls
        .iter()
        .rev()
        .flat_map(inline_buttons)
        .find(|(l, _)| l.contains(label))
        .unwrap_or_else(|| panic!("pas de bouton « {label} »"))
        .1
}
