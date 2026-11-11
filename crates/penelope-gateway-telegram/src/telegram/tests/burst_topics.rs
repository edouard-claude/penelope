//! #462 : une rafale appartient à son sujet, et un arrêt pendant sa fenêtre ne la perd pas.

use super::*;

const CHAT: i64 = -100_462;

/// Un groupe à sujets autorisé, une fenêtre de regroupement, pas de carte de rafale.
fn setup(g: &TelegramGateway, window_ms: u64) {
    g.daemon
        .publish_config("test", move |c| {
            c.telegram.allowed_chats = vec![CHAT];
            c.telegram.text_group_window_ms = window_ms;
            c.telegram.burst_messages = 0;
            c.telegram.burst_chars = 0;
            Ok(vec![
                "telegram.allowed_chats".into(),
                "telegram.text_group_window_ms".into(),
            ])
        })
        .unwrap();
}

fn in_topic(update_id: i64, topic: i64, text: &str) -> Value {
    let mut u = updates::in_topic(updates::text_message(update_id, CHAT, OWNER, text), topic);
    u["message"]["chat"] = json!({"id": CHAT, "type": "supergroup", "title": "Chantiers"});
    u
}

/// Un morceau à la limite de Telegram : il ouvre ou prolonge une rafale.
fn piece(mark: &str) -> String {
    format!("{mark}{}", "a".repeat(TELEGRAM_TEXT_LIMIT))
}

async fn session_of(g: &TelegramGateway, topic: i64) -> String {
    g.daemon
        .chat_session_for(&Origin::Telegram {
            chat_id: CHAT,
            topic_id: Some(topic),
            message_id: None,
        })
        .await
        .unwrap()
}

/// Les tours en file : (session, texte), dans l'ordre de leur mise en file.
async fn queued(g: &TelegramGateway) -> Vec<(String, String)> {
    g.daemon
        .services
        .store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT session_id, json_extract(payload, '$.text') FROM turn_queue
                 ORDER BY enqueued_at, id",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

/// L'état d'un update en base : (`processed`, contenu encore gardé).
async fn update_row(g: &TelegramGateway, update_id: i64) -> (i64, bool) {
    g.daemon
        .services
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT processed, payload <> '{}' FROM tg_updates WHERE update_id = ?1",
                [update_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
}

/// Deux sujets d'un même groupe, à moins d'une fenêtre d'écart : le message court du
/// sujet 5 ne rejoint pas la rafale ouverte dans le sujet 3, et les deux rafales restent
/// distinctes. Avant #462, la rafale était rangée par conversation : le second message
/// partait dans la session du premier.
#[tokio::test]
async fn a_message_from_another_topic_does_not_join_the_burst() {
    let (_d, g, _t, _p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    setup(&g, 150);
    let (three, five) = (session_of(&g, 3).await, session_of(&g, 5).await);
    assert_ne!(three, five);

    g.process_update(&in_topic(900, 3, &piece("trois-")))
        .await
        .unwrap();
    // Un message court tapé dans l'autre sujet : il part tout de suite, chez lui.
    g.process_update(&in_topic(901, 5, "question du sujet cinq"))
        .await
        .unwrap();
    assert_eq!(
        queued(&g).await,
        vec![(five.clone(), "question du sujet cinq".to_string())]
    );
    // Un collage dans le sujet 5 pendant que celui du sujet 3 attend : deux rafales.
    g.process_update(&in_topic(902, 5, &piece("cinq-")))
        .await
        .unwrap();
    g.process_update(&in_topic(903, 3, "fin du trois"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let turns = queued(&g).await;
    assert_eq!(
        turns.len(),
        3,
        "{:?}",
        turns.iter().map(|t| &t.0).collect::<Vec<_>>()
    );
    let of = |session: &str| -> Vec<&str> {
        turns
            .iter()
            .filter(|(s, _)| s == session)
            .map(|(_, t)| t.as_str())
            .collect()
    };
    let (in_three, in_five) = (of(&three), of(&five));
    assert_eq!(in_three.len(), 1);
    assert!(in_three[0].starts_with("trois-") && in_three[0].ends_with("fin du trois"));
    assert!(!in_three[0].contains("cinq"));
    assert_eq!(in_five.len(), 2);
    assert!(in_five[1].starts_with("cinq-") && !in_five[1].contains("trois"));
}

/// `/stop` dans un sujet vide la rafale de ce sujet, pas celle du voisin ; les updates
/// abandonnés sont marqués traités, ils ne reviendront pas au redémarrage.
#[tokio::test]
async fn stop_drops_the_burst_of_its_topic_only() {
    let (_d, g, _t, _p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    setup(&g, 200);
    let five = session_of(&g, 5).await;

    g.process_update(&in_topic(910, 3, &piece("trois-")))
        .await
        .unwrap();
    g.process_update(&in_topic(911, 5, &piece("cinq-")))
        .await
        .unwrap();
    assert_eq!(
        update_row(&g, 910).await,
        (2, true),
        "retenu, contenu gardé"
    );
    g.process_update(&in_topic(912, 3, "/stop")).await.unwrap();
    assert_eq!(update_row(&g, 910).await, (1, false), "abandonné");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let turns = queued(&g).await;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].0, five);
    assert_eq!(update_row(&g, 911).await, (1, false), "parti en file");
    assert_eq!(g.resume_held_updates().await.unwrap(), 0);
}

/// Le daemon s'arrête pendant la fenêtre d'une rafale : ses updates, déjà acquittés
/// auprès de Telegram, sont restés en base avec leur contenu. La passerelle suivante les
/// rejoue, la rafale se reforme et part en un tour. Rejoués une seconde fois (arrêt entre
/// la mise en file et le marquage), ils ne font pas un second tour.
#[tokio::test]
async fn a_burst_cut_by_a_restart_is_delivered_once() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    s.kv_set("tg.onboard.proposed", "test").await.unwrap();
    // Une fenêtre que cette vie-là ne verra pas finir.
    setup(&g, 60_000);
    let (first, last) = (
        in_topic(920, 3, &piece("collage-")),
        in_topic(921, 3, "et la fin"),
    );
    g.process_update(&first).await.unwrap();
    g.process_update(&last).await.unwrap();
    assert!(queued(&g).await.is_empty(), "rien n'est encore en file");
    assert_eq!(update_row(&g, 920).await, (2, true));
    assert_eq!(update_row(&g, 921).await, (2, true));
    // Telegram renvoie un update retenu (offset non écrit) : il n'entre pas deux fois.
    g.process_update(&last).await.unwrap();
    let parts = g.bursts.lock().unwrap()[&(CHAT, Some(3))].parts.len();
    assert_eq!(parts, 2, "le renvoi n'ajoute pas un morceau");

    // Redémarrage : une passerelle neuve sur la même base, sa mémoire vide.
    setup(&g, 60);
    let g2 = TelegramGateway::with_transport(g.daemon.clone(), t.clone());
    g2.register();
    assert_eq!(g2.resume_held_updates().await.unwrap(), 2);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let turns = queued(&g2).await;
    assert_eq!(turns.len(), 1, "un seul tour pour la rafale");
    assert_eq!(turns[0].0, session_of(&g2, 3).await);
    assert!(turns[0].1.starts_with("collage-") && turns[0].1.ends_with("et la fin"));
    assert_eq!(turns[0].1.matches("et la fin").count(), 1);
    assert_eq!(
        update_row(&g2, 920).await,
        (1, false),
        "traité, contenu effacé"
    );
    assert_eq!(update_row(&g2, 921).await, (1, false));
    assert_eq!(g2.resume_held_updates().await.unwrap(), 0);

    // Arrêt entre la mise en file et le marquage : les updates sont encore retenus.
    let (a, b) = (first.to_string(), last.to_string());
    s.store
        .write(move |tx| {
            for (id, raw) in [(920, &a), (921, &b)] {
                tx.execute(
                    "UPDATE tg_updates SET processed = 2, payload = ?2 WHERE update_id = ?1",
                    params![id, raw],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let g3 = TelegramGateway::with_transport(g.daemon.clone(), t.clone());
    g3.register();
    assert_eq!(g3.resume_held_updates().await.unwrap(), 2);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(queued(&g3).await.len(), 1, "la file dédoublonne");
    assert_eq!(update_row(&g3, 920).await, (1, false));
}
