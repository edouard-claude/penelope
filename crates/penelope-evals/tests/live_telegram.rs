//! Suite `live-telegram` (§20.1, réseau) : le transport HTTP contre l'API Bot réelle, avec
//! un bot et une conversation de test (jamais le bot de production).
//!
//! ```bash
//! PENELOPE_LIVE_TELEGRAM_TOKEN=… PENELOPE_LIVE_TELEGRAM_CHAT=… penelope eval live-telegram
//! ```
//!
//! La conversation doit avoir été ouverte avec le bot (un `/start` suffit). Les messages
//! envoyés sont supprimés à la fin.

use penelope_evals::live;
use penelope_kernel::clock::{SharedClock, SystemClock};
use penelope_telegram::api::{Bot, HttpTransport, inline_keyboard, method, reaction};
use penelope_telegram::markdown_to_html;
use penelope_telegram::render::ButtonSpec;
use serde_json::json;
use std::sync::Arc;

fn bot() -> (Bot, i64) {
    let v = live::require_env(&[
        "PENELOPE_LIVE_TELEGRAM_TOKEN",
        "PENELOPE_LIVE_TELEGRAM_CHAT",
    ]);
    let chat: i64 = v[1]
        .trim()
        .parse()
        .expect("PENELOPE_LIVE_TELEGRAM_CHAT : identifiant numérique");
    let transport = HttpTransport::new("https://api.telegram.org", v[0].trim()).expect("transport");
    let clock: SharedClock = Arc::new(SystemClock);
    (Bot::new(Arc::new(transport), 1.0, clock), chat)
}

#[tokio::test]
#[ignore = "réseau : PENELOPE_LIVE_TELEGRAM_TOKEN, PENELOPE_LIVE_TELEGRAM_CHAT"]
async fn the_bot_sends_edits_reacts_and_cleans_up() {
    let (bot, chat) = bot();
    let me = bot.get_me().await.expect("getMe");
    assert_eq!(me["is_bot"], true, "{me}");

    // Rendu HTML maison et clavier en ligne.
    let html = markdown_to_html("**Essai Pénélope** : `live-telegram`, _à supprimer_.");
    let keyboard = inline_keyboard(&[vec![ButtonSpec::callback("OK", "a:essai", "")]]);
    let sent = bot
        .send_text(chat, None, &html, Some(keyboard), None)
        .await
        .expect("sendMessage");
    let message_id = sent["message_id"].as_i64().expect("message_id");

    let edited = bot
        .edit_text(chat, message_id, "<b>Essai modifié</b>", None)
        .await
        .expect("editMessageText");
    assert!(edited.to_string().contains("Essai modifié"), "{edited}");
    bot.set_reaction(chat, message_id, reaction::DONE)
        .await
        .expect("setMessageReaction");
    bot.call(
        method::SEND_CHAT_ACTION,
        Some(chat),
        json!({"chat_id": chat, "action": "typing"}),
    )
    .await
    .expect("sendChatAction");

    // Document, puis ménage.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("essai.txt");
    std::fs::write(&file, "fichier de test live-telegram\n").unwrap();
    let doc = bot
        .send_document(chat, None, &file, Some("Essai de document"))
        .await
        .expect("sendDocument");
    for id in [message_id, doc["message_id"].as_i64().unwrap_or(0)] {
        if id != 0 {
            let _ = bot
                .call(
                    method::DELETE_MESSAGE,
                    Some(chat),
                    json!({"chat_id": chat, "message_id": id}),
                )
                .await;
        }
    }
}

#[tokio::test]
#[ignore = "réseau : PENELOPE_LIVE_TELEGRAM_TOKEN, PENELOPE_LIVE_TELEGRAM_CHAT"]
async fn long_messages_are_split_under_the_api_limit() {
    let (bot, chat) = bot();
    let long = "paragraphe de test assez long pour déborder\n\n".repeat(200);
    let mut ids = Vec::new();
    for fragment in penelope_telegram::split_message(&long, 3_500) {
        let sent = bot
            .send_text(chat, None, &markdown_to_html(&fragment), None, None)
            .await
            .expect("fragment accepté par l'API");
        ids.push(sent["message_id"].as_i64().unwrap_or(0));
    }
    assert!(ids.len() >= 2);
    for id in ids {
        let _ = bot
            .call(
                method::DELETE_MESSAGE,
                Some(chat),
                json!({"chat_id": chat, "message_id": id}),
            )
            .await;
    }
}
