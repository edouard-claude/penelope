//! Commandes de session et de modèles, relance d'une demande MCP expirée.

use super::*;

async fn say(g: &Arc<TelegramGateway>, t: &MockTransport, text: &str) -> Vec<String> {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(50_000);
    let before = t.calls_to(tg::SEND_MESSAGE).await.len();
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    g.process_update(&updates::text_message(id, OWNER, OWNER, text))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    texts(&t.calls_to(tg::SEND_MESSAGE).await[before..])
}

/// #32 : une nouvelle session sur un sujet proche propose de reprendre les notes de
/// l'ancienne, et le bouton les recopie.
#[tokio::test]
async fn a_new_session_offers_the_notes_of_a_similar_one() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let old = s
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Migration Postgres Atlas".into()),
        )
        .await
        .unwrap()
        .id
        .to_string();
    penelope_vault::session_notes::update(&s, &old, "Décisions", "Garder la réplique", false)
        .await
        .unwrap();
    let out = say(&g, &t, "/new Migration Postgres").await;
    assert!(out[0].contains("Des notes de travail existent"), "{out:?}");
    press(&g, &button(&t, "📓 Reprendre les notes").await).await;
    let new = g
        .daemon
        .chat_session_for(&Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        })
        .await
        .unwrap();
    assert_ne!(new, old);
    let notes = penelope_vault::session_notes::read(&s, &new).await.unwrap();
    assert!(
        notes.is_some_and(|n| n.contains("Garder la réplique")),
        "notes reprises"
    );
}

/// `/rewind n` défait des échanges, et dit pourquoi quand il ne peut pas.
#[tokio::test]
async fn rewind_undoes_exchanges_or_says_why_not() {
    let (_d, g, t, _p) = gateway().await;
    let out = say(&g, &t, "/rewind 2").await;
    assert!(out[0].starts_with("❌"), "{out:?}");
    let sid = g
        .daemon
        .chat_session_for(&Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        })
        .await
        .unwrap();
    let h = &g.daemon.services.context.history;
    for (u, a) in [("un", "1"), ("deux", "2")] {
        h.append(
            &sid,
            &penelope_llm::types::ChatMessage::user(u),
            5,
            0,
            false,
            None,
        )
        .await
        .unwrap();
        h.append(
            &sid,
            &penelope_llm::types::ChatMessage::assistant(a),
            5,
            0,
            false,
            None,
        )
        .await
        .unwrap();
    }
    let out = say(&g, &t, "/rewind 2").await;
    assert!(
        out[0].starts_with("⏪ 2 échange(s) défait(s) (4 messages"),
        "{out:?}"
    );
}

/// `/model auto` bascule le routage ; `/model auth status` et `logout` répondent sans
/// connexion.
#[tokio::test]
async fn model_routing_and_account_subcommands() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let out = say(&g, &t, "/model auto off").await;
    assert!(out[0].contains("Routage fixe"), "{out:?}");
    assert!(!s.config.config().models.routing.classifier);
    let out = say(&g, &t, "/model auto on").await;
    assert!(out[0].contains("Routage adaptatif activé"), "{out:?}");
    assert!(s.config.config().models.routing.classifier);
    let out = say(&g, &t, "/model auto peut-être").await;
    assert!(out[0].starts_with("Usage"), "{out:?}");
    let out = say(&g, &t, "/model auth status").await;
    assert!(out[0].contains("Aucun compte ChatGPT connecté"), "{out:?}");
    let out = say(&g, &t, "/model auth --logout").await;
    assert!(out[0].contains("déconnecté"), "{out:?}");
}

/// #143 : « Relancer » une demande MCP expirée renvoie la consigne dans sa session ;
/// sans session, la relance est impossible et dite.
#[tokio::test]
async fn an_expired_mcp_request_is_retried_in_its_session() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let sid = s
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    let retry = |session: String| {
        let g = g.clone();
        async move {
            g.actions
                .create(
                    penelope_telegram::actions::kind::ELICIT_RETRY,
                    "e1",
                    json!({"session": session, "server": "drive", "what": "Relier le compte"}),
                    3_600_000,
                    true,
                )
                .await
                .unwrap()
                .token
        }
    };
    press(&g, &retry(sid.clone()).await).await;
    let turn = s.turns.claim("t").await.unwrap().expect("relance en file");
    assert_eq!(turn.session_id, sid);
    assert!(
        turn.payload
            .to_string()
            .contains("Relance la demande `drive` qui a expiré (Relier le compte)"),
        "{}",
        turn.payload
    );
    press(&g, &retry(String::new()).await).await;
    g.flush_outbox().await.unwrap();
    assert!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.contains("pas de conversation à relancer"))
    );
}
