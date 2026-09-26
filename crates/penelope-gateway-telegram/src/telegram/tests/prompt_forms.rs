//! Formulaire des arguments d'un prompt MCP : saisie, retour, envoi, abandon.

use super::*;
use penelope_mcp_host::testing::{FakeConnector, declare, server, tool};

async fn with_ticket_prompt(g: &TelegramGateway) {
    let base = server(Arc::new(std::sync::Mutex::new(vec![tool(
        "lire",
        json!({}),
    )])));
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "docs",
        Arc::new(move |m, p| match m {
            "prompts/list" => Ok(json!({"prompts": [
                {"name": "ticket", "description": "Ouvre un ticket",
                 "arguments": [{"name": "titre", "required": true}, {"name": "note"}]}
            ]})),
            "prompts/get" => Ok(json!({"messages": [
                {"role": "user", "content": {"type": "text",
                 "text": format!("Ticket {}", p["arguments"]["titre"])}}
            ]})),
            _ => base(m, p),
        }),
    );
    let sup = penelope_mcp_host::testing::supervisor(g.daemon.services.clone(), fake);
    declare(&sup, "docs", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup);
}

async fn open_form(g: &Arc<TelegramGateway>) {
    let (_, buttons) = screen_of(g, "prompts", json!({"server": "docs"})).await;
    press(g, &token_of(&buttons, "▶️ ticket")).await;
}

/// Un champ se remplit d'un message, un champ facultatif se passe, « Modifier » revient
/// en arrière, et l'envoi remet le prompt rendu au modèle.
#[tokio::test]
async fn a_prompt_form_is_filled_corrected_and_sent() {
    let (_d, g, t, _p) = gateway().await;
    with_ticket_prompt(&g).await;
    open_form(&g).await;
    let first = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(first.iter().any(|x| x.contains("titre")), "{first:?}");
    g.process_update(&updates::text_message(6_001, OWNER, OWNER, "Relire la PR"))
        .await
        .unwrap();
    press(&g, &button(&t, "⏭ Passer").await).await;
    press(&g, &button(&t, "↩️ Modifier").await).await;
    press(&g, &button(&t, "⏭ Passer").await).await;
    press(&g, &button(&t, "Envoyer").await).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|x| x.contains("Prompt <code>ticket</code> : 1 message(s) envoyé(s)")),
        "{sent:?}"
    );
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 1);
    assert_eq!(
        g.daemon
            .services
            .kv_get(&form_key(OWNER, None))
            .await
            .unwrap()
            .as_deref(),
        Some(""),
        "formulaire refermé"
    );
}

/// Abandonné, le formulaire ne lance rien ; un bouton de formulaire sans formulaire en
/// cours le dit.
#[tokio::test]
async fn an_abandoned_prompt_form_launches_nothing() {
    let (_d, g, t, _p) = gateway().await;
    with_ticket_prompt(&g).await;
    open_form(&g).await;
    let abandon = button(&t, "✖️ Abandonner").await;
    press(&g, &abandon).await;
    let stale = g
        .actions
        .create(
            penelope_telegram::actions::kind::FORM_DECLINE,
            "formulaire",
            json!({}),
            3_600_000,
            true,
        )
        .await
        .unwrap();
    press(&g, &stale.token).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|x| x.contains("rien n'est lancé")),
        "{sent:?}"
    );
    assert!(
        sent.iter().any(|x| x.contains("aucun formulaire en cours")),
        "{sent:?}"
    );
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
}
