//! `/secrets <projet>` (#350) : un bloc de credentials collé est rangé au magasin sans
//! qu'aucun modèle ne le voie, et rien n'en reste en base ni à l'écran.

use super::*;

const VALUE: &str = "sk-dev+valeur/tres=secrete";

fn block() -> String {
    format!(
        "DEV :\n  Bucket : app-mentaleco-dev\n  Access Key : AKDEVXXXXXXXXXXXXXXX\n  Secret Key : \
         {VALUE}\n\nQA :\n  Access Key : AKQAXXXXXXXXXXXXXXXX\nune ligne illisible"
    )
}

/// Le nombre de lignes, dans les tables où un message entre, qui contiennent `needle`.
async fn traces(g: &TelegramGateway, needle: &str) -> i64 {
    let needle = format!("%{needle}%");
    g.daemon
        .services
        .store
        .read(move |c| {
            let mut n = 0;
            for t in ["tg_updates", "turn_queue", "messages", "events", "kv"] {
                let col = match t {
                    "tg_updates" | "turn_queue" | "events" => "payload",
                    "messages" => "content",
                    _ => "v",
                };
                n += c.query_row(
                    &format!("SELECT COUNT(*) FROM {t} WHERE {col} LIKE ?1"),
                    [&needle],
                    |r| r.get::<_, i64>(0),
                )?;
            }
            Ok(n)
        })
        .await
        .unwrap()
}

async fn send(g: &Arc<TelegramGateway>, id: i64, text: &str) {
    g.process_update(&updates::text_message(id, OWNER, OWNER, text))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
}

#[tokio::test]
async fn a_pasted_block_is_stored_without_trace_or_turn() {
    let (_d, g, t, _p) = gateway().await;
    send(&g, 900, "/secrets MentalEco").await;
    let armed = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(armed.join("\n").contains("Colle le bloc"), "{armed:?}");
    let before = t.calls_to(tg::SEND_MESSAGE).await.len();

    send(&g, 901, &block()).await;

    let secrets = &g.daemon.services.platform.secrets;
    assert_eq!(
        secrets.get("mentaleco.dev.secret_key").unwrap().as_deref(),
        Some(VALUE)
    );
    assert!(secrets.get("mentaleco.qa.access_key").unwrap().is_some());
    // Rien du bloc en base, aucun tour, le message effacé.
    assert_eq!(traces(&g, VALUE).await, 0);
    assert_eq!(traces(&g, "AKQAXXXX").await, 0);
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
    let deleted = t.calls_to("deleteMessage").await;
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0]["message_id"], 9010);
    // La réponse donne les noms et la ligne illisible, jamais une valeur.
    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await[before..]).join("\n");
    assert!(out.contains("mentaleco.dev.secret_key"), "{out}");
    assert!(
        out.contains("non reconnues (rien n'en est cité) : 8"),
        "{out}"
    );
    assert!(!out.contains(VALUE) && !out.contains("AKDEV"), "{out}");

    // Désarmé : le message suivant redevient une conversation.
    send(&g, 902, "Merci").await;
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 1);
}

/// Le bloc peut suivre la commande dans le même message.
#[tokio::test]
async fn a_block_after_the_command_is_stored_too() {
    let (_d, g, t, _p) = gateway().await;
    send(&g, 910, &format!("/secrets app\nTOKEN={VALUE}")).await;
    assert_eq!(
        g.daemon
            .services
            .platform
            .secrets
            .get("app.token")
            .unwrap()
            .as_deref(),
        Some(VALUE)
    );
    assert_eq!(traces(&g, VALUE).await, 0);
    assert_eq!(t.calls_to("deleteMessage").await.len(), 1);
}

/// Passé le délai, ou venu d'un autre que le propriétaire, un message n'est pas capturé.
#[tokio::test]
async fn an_expired_wait_or_another_sender_is_not_captured() {
    let (_d, g, _t, _p) = gateway().await;
    let s = &g.daemon.services;
    s.kv_set(&format!("tg.await_secrets.{OWNER}.0"), "app 0")
        .await
        .unwrap();
    s.clock.as_ref();
    send(&g, 920, "K=v après le délai").await;
    assert!(s.platform.secrets.get("app.k").unwrap().is_none());

    send(&g, 921, "/secrets app").await;
    g.process_update(&updates::text_message(922, OWNER, 7, "K=v"))
        .await
        .unwrap();
    assert!(s.platform.secrets.get("app.k").unwrap().is_none());
}
