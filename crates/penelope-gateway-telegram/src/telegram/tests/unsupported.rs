//! #461 : un message sans texte ne lance jamais un tour vide. Un message de service est
//! ignoré, un contenu sans fichier est dit en une ligne, une vidéo suit le chemin d'un
//! fichier, et le reste est dit au propriétaire sans tour.

use super::*;

/// Un message du propriétaire sans champ `text`, avec ce contenu.
fn other_update(update_id: i64, content: Value) -> Value {
    let mut u = updates::text_message(update_id, OWNER, OWNER, "");
    let msg = u["message"].as_object_mut().unwrap();
    msg.remove("text");
    for (k, v) in content.as_object().unwrap() {
        msg.insert(k.clone(), v.clone());
    }
    u
}

async fn pending(g: &TelegramGateway) -> i64 {
    g.daemon.services.turns.pending_count().await.unwrap()
}

/// Le texte du tour en file.
async fn queued_text(g: &TelegramGateway) -> String {
    let turn = g.daemon.services.turns.claim("test").await.unwrap();
    let turn = turn.expect("un tour en file");
    turn.payload["text"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_service_message_starts_no_turn_and_gets_no_answer() {
    let (_d, g, t, _p) = gateway().await;
    for (n, content) in [
        json!({"forum_topic_created": {"name": "Projet", "icon_color": 7322096},
               "message_thread_id": 12, "is_topic_message": true}),
        json!({"forum_topic_edited": {"name": "Projet bis"}, "message_thread_id": 12}),
        json!({"pinned_message": {"message_id": 3, "text": "à garder"}}),
    ]
    .into_iter()
    .enumerate()
    {
        g.process_update(&other_update(8_000 + n as i64, content))
            .await
            .unwrap();
    }
    settle_click(&g).await;
    assert_eq!(pending(&g).await, 0);
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.is_empty(), "{sent:?}");
}

#[tokio::test]
async fn a_sticker_and_a_location_start_a_turn_that_names_them() {
    for (content, expected) in [
        (
            json!({"sticker": {"file_id": "s1", "emoji": "😀"}}),
            "[sticker 😀]",
        ),
        (
            json!({"location": {"latitude": 48.85, "longitude": 2.35},
                   "caption": "c'est loin ?"}),
            "[localisation 48.85, 2.35]\nc'est loin ?",
        ),
    ] {
        let (_d, g, _t, _p) = gateway().await;
        g.process_update(&other_update(8_100, content))
            .await
            .unwrap();
        assert!(eventually(|| async { pending(&g).await == 1 }).await);
        assert_eq!(queued_text(&g).await, expected);
    }
}

#[tokio::test]
async fn an_unknown_type_is_answered_without_a_turn() {
    let (_d, g, t, _p) = gateway().await;
    g.process_update(&other_update(8_200, json!({"hologram": {"id": 1}})))
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            let _ = g.flush_outbox().await;
            !t.calls_to(tg::SEND_MESSAGE).await.is_empty()
        })
        .await
    );
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].contains("Je ne sais pas lire"), "{sent:?}");
    assert!(sent[0].contains("hologram"), "{sent:?}");
    assert_eq!(pending(&g).await, 0);
}

/// Une vidéo est un fichier : déposée dans le workspace, et sa légende part en tour avec
/// le chemin. Sans légende, elle est rangée sans tour, comme tout fichier.
#[tokio::test]
async fn a_video_is_stored_and_its_caption_is_the_request() {
    let (_d, g, t, _p) = gateway().await;
    t.set_file("vid1", &[0u8, 0, 0, 24, 102, 116, 121, 112, 0, 255])
        .await;
    let video = json!({"file_id": "vid1", "duration": 3, "file_size": 10});
    g.process_update(&other_update(8_300, json!({"video": video.clone()})))
        .await
        .unwrap();
    let workspace = penelope_executor::executor::default_workspaces(&g.daemon.services)[0].clone();
    assert!(eventually(|| async { workspace.join("telegram/video.mp4").exists() }).await);
    settle_click(&g).await;
    assert_eq!(pending(&g).await, 0, "sans légende : rangée, pas de tour");

    let asked = json!({"video": video, "caption": "extrais le son"});
    g.process_update(&other_update(8_301, asked)).await.unwrap();
    assert!(eventually(|| async { pending(&g).await == 1 }).await);
    let text = queued_text(&g).await;
    assert!(
        text.starts_with("extrais le son\n\n[Fichier joint"),
        "{text}"
    );
    assert!(text.contains("video-2.mp4"), "{text}");
}
