//! Livraison du cœur au canal : arrêt, plafond atteint, alerte de planification,
//! rafale proposée, titre d'une nouvelle session.

use super::*;
use penelope_app::bus::ChannelDelivery;

async fn focused(g: &TelegramGateway) -> (String, Origin) {
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: Some(10),
    };
    (g.daemon.chat_session_for(&origin).await.unwrap(), origin)
}

#[tokio::test]
async fn cancelled_and_over_budget_turns_are_said() {
    let (_d, g, t, _p) = gateway().await;
    let (sid, origin) = focused(&g).await;
    g.deliver("t1", &sid, &origin, &penelope_agent::TurnOutcome::Cancelled)
        .await;
    g.deliver(
        "t2",
        &sid,
        &origin,
        &penelope_agent::TurnOutcome::BudgetExceeded {
            scope: "session".into(),
            spent_usd: 2.5,
            limit_usd: 2.0,
        },
    )
    .await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(sent[0], "⏹ Génération arrêtée.");
    assert!(
        sent[1].contains("2.50") || sent[1].contains("2,50"),
        "{sent:?}"
    );
}

/// #39 : une planification en échec prévient avec « Relancer » et « Voir » ; « Voir »
/// ouvre l'écran des planifications.
#[tokio::test]
async fn a_schedule_alert_offers_rerun_and_view() {
    let (_d, g, t, _p) = gateway().await;
    let (_, origin) = focused(&g).await;
    g.schedule_alert(&origin, "sch_1", "⚠️ La veille a échoué.")
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    assert!(
        card["text"]
            .as_str()
            .unwrap()
            .contains("La veille a échoué")
    );
    let labels: Vec<String> = inline_buttons(&card).into_iter().map(|(l, _)| l).collect();
    assert_eq!(
        labels,
        ["🔁 Relancer maintenant", "📅 Voir la planification"]
    );
    press(&g, &button(&t, "📅 Voir la planification").await).await;
    let edited = texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
    assert!(!edited.is_empty(), "écran des planifications affiché");
}

/// #49 : une rafale remise par le cœur devient la carte de choix ; « Un seul document »
/// la remet en un tour.
#[tokio::test]
async fn an_offered_burst_is_handled_as_one_document() {
    let (_d, g, t, _p) = gateway().await;
    let (sid, origin) = focused(&g).await;
    g.offer_burst(
        &sid,
        &origin,
        vec!["partie un".into(), "partie deux".into()],
    )
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let buttons = inline_buttons(&card);
    assert!(buttons.len() >= 2, "{card}");
    press(&g, &buttons[0].1).await;
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 1);
    press(&g, &buttons[0].1).await;
    assert_eq!(
        g.daemon.services.turns.pending_count().await.unwrap(),
        1,
        "une rafale ne se traite qu'une fois"
    );
}

/// Le message « nouvelle session » est réécrit avec le titre trouvé ensuite.
#[tokio::test]
async fn a_new_session_message_gets_its_title() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    s.kv_set("tg.new_session.s_1", &format!("{OWNER}:77"))
        .await
        .unwrap();
    g.session_titled("s_1", "Devis ACME").await;
    let edited = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert_eq!(edited.len(), 1);
    assert_eq!(edited[0]["message_id"], 77);
    assert!(
        edited[0]["text"]
            .as_str()
            .unwrap()
            .contains("« Devis ACME »")
    );
    assert!(s.kv_get("tg.new_session.s_1").await.unwrap().is_none());
    g.session_titled("s_1", "Autre").await;
    assert_eq!(
        t.calls_to(tg::EDIT_MESSAGE_TEXT).await.len(),
        1,
        "une seule fois"
    );
}
