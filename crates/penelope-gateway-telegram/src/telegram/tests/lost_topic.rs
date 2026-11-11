//! #463 : un envoi vers un sujet supprimé arrive dans le fil général, et ce qui livrait
//! dans ce sujet y est rattaché.

use super::*;

const CHAT: i64 = -100_463;
const GONE: &str = "Bad Request: message thread not found";

fn place(topic_id: Option<i64>, message_id: Option<i64>) -> Value {
    Origin::Telegram {
        chat_id: CHAT,
        topic_id,
        message_id,
    }
    .to_value()
}

/// Une planification qui livre dans `origin` (aucune : celle du propriétaire).
async fn schedule(g: &TelegramGateway, origin: Option<Value>) -> String {
    let mut target = json!({"type": "notify", "template": "🧭 Veille"});
    if let Some(origin) = origin {
        target["origin"] = origin;
    }
    g.daemon
        .services
        .schedules
        .create(
            penelope_workflow::TriggerKind::Cron,
            json!({"expr": "0 9 * * *"}),
            target,
            json!({}),
        )
        .await
        .unwrap()
        .id
}

async fn origin_of(g: &TelegramGateway, schedule: &str) -> Value {
    let sched = g.daemon.services.schedules.get(schedule).await.unwrap();
    sched.unwrap().target["origin"].clone()
}

/// Les lignes de la file : (état, identifiant commençant par `failnote-`).
async fn rows(g: &TelegramGateway) -> Vec<(String, bool)> {
    g.daemon
        .services
        .store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT state, id LIKE 'failnote-%' FROM tg_outbox ORDER BY created_at, rowid",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

/// Le sujet 21 a été supprimé. La réponse qui lui était destinée arrive dans le fil
/// général, une fois, précédée d'une ligne qui dit d'où elle vient ; la planification qui
/// livrait là est rattachée au fil général, celle du sujet voisin ne bouge pas. Avant
/// #463, la ligne passait `failed` au premier refus et son texte ne partait nulle part.
#[tokio::test]
async fn a_message_for_a_deleted_topic_lands_in_the_general_thread() {
    let (_d, g, t, _p) = gateway().await;
    let s = &g.daemon.services;
    s.kv_set(&topic_name_key(CHAT, 21), "Veille & IA")
        .await
        .unwrap();
    let lost = schedule(&g, Some(place(Some(21), Some(7)))).await;
    let neighbour = schedule(&g, Some(place(Some(22), None))).await;
    let default = schedule(&g, None).await;

    g.reply(CHAT, Some(21), Some(7), "Trois tickets ouverts ce matin.")
        .await
        .unwrap();
    t.fail_once(400, GONE, None).await;
    g.flush_outbox().await.unwrap();

    let calls = t.calls_to(tg::SEND_MESSAGE).await;
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0]["message_thread_id"], 21);
    assert!(calls[1].get("message_thread_id").is_none(), "{}", calls[1]);
    assert!(calls[1].get("reply_parameters").is_none(), "{}", calls[1]);
    assert_eq!(calls[1]["parse_mode"], "HTML");
    let text = calls[1]["text"].as_str().unwrap();
    assert!(
        text.starts_with("↪️ Sujet « Veille &amp; IA » supprimé : ce message lui était destiné."),
        "{text}"
    );
    assert!(text.ends_with("Trois tickets ouverts ce matin."), "{text}");
    assert_eq!(rows(&g).await, vec![("sent".to_string(), false)]);
    // La ligne de la file garde l'envoi rerouté : une nouvelle tentative, si le second
    // essai était tombé sur une panne, n'aurait pas refait le détour.
    let (topic, kept): (Option<i64>, String) = s
        .store
        .read(|c| {
            Ok(
                c.query_row("SELECT topic_id, payload FROM tg_outbox", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(topic, None);
    assert_eq!(serde_json::from_str::<Value>(&kept).unwrap(), calls[1]);

    // Une seule fois : rien ne repart au passage suivant.
    g.flush_outbox().await.unwrap();
    assert_eq!(t.calls_to(tg::SEND_MESSAGE).await.len(), 2);

    assert_eq!(origin_of(&g, &lost).await, place(None, None));
    assert_eq!(origin_of(&g, &neighbour).await, place(Some(22), None));
    assert!(origin_of(&g, &default).await.is_null());
}

/// Un refus d'une autre nature dans un sujet reste un échec définitif, avec sa note, et
/// ne déplace rien. Un second essai refusé aussi : la ligne est `failed`, la note part.
#[tokio::test]
async fn another_refusal_stays_failed_with_its_note() {
    let (_d, g, t, _p) = gateway().await;
    let sched = schedule(&g, Some(place(Some(21), None))).await;

    g.reply(CHAT, Some(21), None, "trop long").await.unwrap();
    t.fail_once(400, "Bad Request: message is too long", None)
        .await;
    g.flush_outbox().await.unwrap();
    assert_eq!(
        rows(&g).await,
        vec![("failed".to_string(), false), ("pending".to_string(), true)]
    );
    assert_eq!(origin_of(&g, &sched).await, place(Some(21), None));
    g.flush_outbox().await.unwrap();
    let said = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(said.contains("n'a pas pu être envoyé"), "{said}");
    assert!(said.contains("too long"), "{said}");

    // Le sujet a disparu, et le fil général refuse aussi : échec dit, une fois.
    t.clear().await;
    g.reply(CHAT, Some(21), None, "perdu").await.unwrap();
    t.fail_once(400, GONE, None).await;
    t.fail_once(400, "Bad Request: chat not found", None).await;
    g.flush_outbox().await.unwrap();
    assert_eq!(t.calls_to(tg::SEND_MESSAGE).await.len(), 2);
    let all = rows(&g).await;
    assert_eq!(all[2], ("failed".to_string(), false), "{all:?}");
    assert_eq!(all[3], ("pending".to_string(), true), "{all:?}");
    // Le sujet n'existe plus pour autant : la planification est rattachée.
    assert_eq!(origin_of(&g, &sched).await, place(None, None));
}

/// Un texte déjà à la limite de Telegram ne peut pas porter la ligne : elle part seule,
/// juste avant, et le texte suit sans son sujet.
#[tokio::test]
async fn a_full_message_is_preceded_by_its_line() {
    let (_d, g, t, _p) = gateway().await;
    let full = "mot ".repeat(1_023).trim_end().to_string();
    let payload = json!({"chat_id": CHAT, "text": full, "message_thread_id": 21});
    g.outbox_push(CHAT, Some(21), tg::SEND_MESSAGE, payload)
        .await
        .unwrap();
    t.fail_once(400, GONE, None).await;
    g.flush_outbox().await.unwrap();

    let calls = t.calls_to(tg::SEND_MESSAGE).await;
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0]["message_thread_id"], 21);
    assert_eq!(
        calls[1],
        json!({"chat_id": CHAT, "text": "↪️ Sujet supprimé : ce message lui était destiné."})
    );
    assert_eq!(calls[2], json!({"chat_id": CHAT, "text": full}));
    assert_eq!(rows(&g).await, vec![("sent".to_string(), false)]);
}
