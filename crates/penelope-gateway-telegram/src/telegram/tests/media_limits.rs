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
