use super::*;
use crate::mock::MockTransport;
use penelope_kernel::clock::TestClock;

fn bot(mock: Arc<MockTransport>) -> Bot {
    Bot::new(mock, 1000.0, Arc::new(TestClock::default()))
}

/// #70 : les aperçus (brouillon, réaction, « écrit… ») ne prennent pas les créneaux
/// des messages : une réponse finale ne fait plus la queue derrière eux.
#[tokio::test]
async fn previews_do_not_queue_in_front_of_messages() {
    use penelope_kernel::clock::Clock;
    assert!(is_preview(method::SEND_MESSAGE_DRAFT));
    assert!(is_preview(method::SET_MESSAGE_REACTION));
    assert!(is_preview(method::SEND_CHAT_ACTION));
    assert!(!is_preview(method::SEND_MESSAGE));

    let m = MockTransport::new();
    let clock = Arc::new(TestClock::default());
    let now = clock.now_ms();
    // Cadence élevée pour ne pas dormir pendant le test ; ce qui compte est le seau.
    let b = Bot::new(m.clone(), 100.0, clock.clone());
    let chat = 42;
    for i in 0..5 {
        b.send_draft(chat, None, 1, &format!("brouillon {i}"))
            .await
            .unwrap();
    }
    let _ = b
        .call(
            method::SET_MESSAGE_REACTION,
            Some(chat),
            json!({"chat_id": chat, "message_id": 1}),
        )
        .await;

    assert_eq!(
        b.limiter.delay_for(chat, now).await,
        0,
        "le premier message part tout de suite, pas après les aperçus"
    );
    assert!(
        b.preview_limiter.delay_for(chat, now).await > 0,
        "les aperçus gardent leur propre cadence"
    );
}

#[tokio::test]
async fn send_text_builds_the_expected_body() {
    let m = MockTransport::new();
    let b = bot(m.clone());
    b.send_text(42, Some(7), "<b>bonjour</b>", None, Some(99))
        .await
        .unwrap();
    let calls = m.calls().await;
    assert_eq!(calls[0].0, method::SEND_MESSAGE);
    let body = &calls[0].1;
    assert_eq!(body["chat_id"], 42);
    assert_eq!(body["parse_mode"], "HTML");
    assert_eq!(body["message_thread_id"], 7);
    assert_eq!(body["reply_parameters"]["message_id"], 99);
    assert_eq!(body["link_preview_options"]["is_disabled"], true);
}

#[tokio::test]
async fn get_updates_declares_allowed_updates() {
    let m = MockTransport::new();
    m.reply(method::GET_UPDATES, json!([])).await;
    let b = bot(m.clone());
    b.get_updates(100, 50).await.unwrap();
    let body = &m.calls().await[0].1;
    assert_eq!(body["timeout"], 50);
    assert_eq!(body["offset"], 100);
    assert!(body["allowed_updates"].as_array().unwrap().len() >= 4);
}

/// CA 14 : 429 simulé, aucune perte, respect de `retry_after`.
#[tokio::test]
async fn ca_14_3_rate_limit_is_respected_without_loss() {
    let m = MockTransport::new();
    m.fail_once(429, "Too Many Requests: retry after 1", Some(1))
        .await;
    m.reply(method::SEND_MESSAGE, json!({"message_id": 5}))
        .await;
    let b = bot(m.clone());

    let started = std::time::Instant::now();
    let r = b.send_text(42, None, "texte", None, None).await.unwrap();
    assert_eq!(r["message_id"], 5, "le message finit par partir");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(900),
        "le délai imposé doit être respecté"
    );
    assert_eq!(m.calls().await.len(), 2, "une seule reprise");
}

#[tokio::test]
async fn server_errors_are_retried_with_backoff() {
    let m = MockTransport::new();
    m.fail_once(500, "Internal Server Error", None).await;
    m.reply(method::SEND_MESSAGE, json!({"message_id": 1}))
        .await;
    let b = bot(m.clone());
    assert!(b.send_text(1, None, "x", None, None).await.is_ok());
    assert_eq!(m.calls().await.len(), 2);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let m = MockTransport::new();
    m.always_fail(400, "Bad Request: chat not found").await;
    let b = bot(m.clone());
    let e = b.send_text(1, None, "x", None, None).await.unwrap_err();
    assert!(matches!(e, TgError::Api { code: 400, .. }));
    assert_eq!(
        m.calls().await.len(),
        1,
        "aucune reprise sur une erreur 4xx"
    );
}

#[tokio::test]
async fn rate_limiter_spaces_messages_per_chat() {
    let l = RateLimiter::new(1.0);
    assert_eq!(l.delay_for(1, 0).await, 0);
    assert_eq!(
        l.delay_for(1, 0).await,
        1000,
        "second message : 1 s d'attente"
    );
    assert_eq!(
        l.delay_for(2, 0).await,
        0,
        "un autre chat n'est pas pénalisé"
    );
}

/// Issue #26 : une panne réseau ne met pas l'URL, donc le jeton, dans l'erreur.
#[tokio::test]
async fn transport_errors_never_carry_the_token() {
    let token = "5123456789:AAHno_token_in_errors_abcdefghijklmnop";
    let t = HttpTransport::new("http://127.0.0.1:9", token).unwrap();
    let err = t
        .call("getUpdates", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("AAHno_token"), "{err}");
    assert!(!err.contains("127.0.0.1:9/bot"), "{err}");
}

#[test]
fn backoff_grows_and_caps() {
    assert_eq!(backoff_ms(1), 1000);
    assert_eq!(backoff_ms(3), 4000);
    assert_eq!(backoff_ms(20), 32_000);
}

#[test]
fn html_fallback_is_triggered_by_capability_errors() {
    assert!(should_fall_back_to_html(&TgError::Api {
        code: 400,
        description: "Bad Request: unknown method sendRichMessage".into()
    }));
    assert!(should_fall_back_to_html(&TgError::Api {
        code: 400,
        description: "Bad Request: RICH_BLOCK_INVALID".into()
    }));
    assert!(!should_fall_back_to_html(&TgError::Api {
        code: 400,
        description: "Bad Request: chat not found".into()
    }));
    assert!(!should_fall_back_to_html(&TgError::Transport(
        "coupure".into()
    )));
}

#[test]
fn inline_keyboard_shape() {
    let rows = vec![vec![
        crate::render::ButtonSpec::callback("Oui", "a:1", "success"),
        crate::render::ButtonSpec::url("Doc", "https://x"),
        crate::render::ButtonSpec::copy_text("Écrire", "/retiens "),
    ]];
    let k = inline_keyboard(&rows);
    assert_eq!(k["inline_keyboard"][0][0]["callback_data"], "a:1");
    assert_eq!(k["inline_keyboard"][0][1]["url"], "https://x");
    assert_eq!(k["inline_keyboard"][0][2]["copy_text"]["text"], "/retiens ");
    assert_eq!(
        crate::render::deep_link("@penelope_bot", "runs:stuck"),
        "https://t.me/penelope_bot?start=runs_stuck"
    );
}

#[tokio::test]
async fn reactions_use_the_prd_emojis() {
    let m = MockTransport::new();
    let b = bot(m.clone());
    b.set_reaction(1, 2, reaction::WORKING).await.unwrap();
    let body = &m.calls().await[0].1;
    assert_eq!(body["reaction"][0]["emoji"], "⚙️");
}
