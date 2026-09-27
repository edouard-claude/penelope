//! Photos, documents et vocaux trop lourds ou impossibles à télécharger : la raison est
//! dite, rien ne part en tour.

use super::*;

async fn replies_to(g: &Arc<TelegramGateway>, t: &MockTransport, update: Value) -> Vec<String> {
    g.process_update(&update).await.unwrap();
    let done = eventually(|| async {
        let _ = g.flush_outbox().await;
        !t.calls_to(tg::SEND_MESSAGE).await.is_empty()
    })
    .await;
    assert!(done, "une réponse est envoyée");
    texts(&t.calls_to(tg::SEND_MESSAGE).await)
}

#[tokio::test]
async fn oversized_media_are_refused_with_the_limit() {
    for (update, expected) in [
        {
            let mut u = updates::photo(7_001, OWNER, OWNER, None);
            u["message"]["photo"][0]["file_size"] = json!(30_000_000);
            (u, "Photo trop lourde (10 Mo au plus).")
        },
        {
            let mut u = updates::document(7_002, OWNER, OWNER, "archive.zip");
            u["message"]["document"]["file_size"] = json!(30_000_000);
            (u, "Fichier trop gros")
        },
        {
            let mut u = updates::voice(7_003, OWNER, OWNER);
            u["message"]["voice"]["file_size"] = json!(30_000_000);
            (u, "🎙️ Fichier trop gros")
        },
    ] {
        let (_d, g, t, _p) = gateway().await;
        let sent = replies_to(&g, &t, update).await;
        assert!(sent[0].contains(expected), "{sent:?}");
        assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
    }
}

/// Le fichier introuvable côté Telegram : téléchargement impossible, dit tel quel.
#[tokio::test]
async fn an_undownloadable_media_says_so() {
    for (update, expected) in [
        (
            updates::photo(7_101, OWNER, OWNER, None),
            "📷 Photo ignorée : téléchargement impossible",
        ),
        (
            updates::document(7_102, OWNER, OWNER, "notes.md"),
            "📄 Téléchargement impossible",
        ),
        (
            updates::voice(7_103, OWNER, OWNER),
            "🎙️ Téléchargement du vocal impossible",
        ),
    ] {
        let (_d, g, t, _p) = gateway().await;
        let sent = replies_to(&g, &t, update).await;
        assert!(sent[0].starts_with(expected), "{sent:?}");
        assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
    }
}

/// #242 : une photo de 12 Mo n'est plus refusée quand Telegram en propose une taille plus
/// légère ; c'est elle qui est téléchargée et part en tour. Aucune taille sous la limite
/// (ci-dessus) : le refus reste.
#[tokio::test]
async fn a_heavy_photo_falls_back_on_a_lighter_telegram_size() {
    let (_d, g, t, _p) = gateway().await;
    let mut u = updates::photo(7_201, OWNER, OWNER, None);
    u["message"]["photo"] = json!([
        {"file_id": "p-s", "width": 320, "height": 240, "file_size": 20_000},
        {"file_id": "p-m", "width": 1280, "height": 960, "file_size": 3_000_000},
        {"file_id": "p-x", "width": 4000, "height": 3000, "file_size": 12_000_000},
    ]);
    t.set_file("p-m", JPEG).await;
    g.process_update(&u).await.unwrap();
    let queued =
        eventually(|| async { g.daemon.services.turns.pending_count().await.unwrap() == 1 }).await;
    assert!(queued, "la photo part en tour");
    let fetched: Vec<_> = t
        .calls_to(tg::GET_FILE)
        .await
        .iter()
        .map(|c| c["file_id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(fetched, vec!["p-m"], "la plus grande taille sous 10 Mo");
    let _ = g.flush_outbox().await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.iter().all(|m| !m.contains("trop lourde")), "{sent:?}");
}
