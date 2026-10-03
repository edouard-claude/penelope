//! Un sujet Telegram = un projet (#301) : ce que la passerelle tranche (Général, foyer) et
//! déclare au cœur.

use super::*;

const CHAT: i64 = -100_555;

/// Un groupe à sujets autorisé, dont le sujet 7 est le foyer.
fn allow_group(g: &TelegramGateway) {
    g.daemon
        .publish_config("test", |c| {
            c.telegram.allowed_chats = vec![CHAT];
            c.telegram.home.chat = CHAT;
            c.telegram.home.topic = 7;
            Ok(vec![
                "telegram.allowed_chats".into(),
                "telegram.home".into(),
            ])
        })
        .unwrap();
}

fn in_group(mut u: Value) -> Value {
    u["message"]["chat"] = json!({"id": CHAT, "type": "supergroup", "title": "Chantiers"});
    u
}

/// Un message du propriétaire dans un sujet, avec le nom de naissance du sujet en tête,
/// comme Telegram l'envoie.
fn topic_message(update_id: i64, topic: i64, born_as: &str, text: &str) -> Value {
    let mut u = in_group(updates::in_topic(
        updates::text_message(update_id, CHAT, OWNER, text),
        topic,
    ));
    u["message"]["reply_to_message"] =
        json!({"message_id": topic, "forum_topic_created": {"name": born_as}});
    u
}

/// Le message de service d'une création ou d'un renommage de sujet.
fn topic_event(update_id: i64, topic: i64, event: &str, name: &str) -> Value {
    in_group(json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id * 10,
            "date": 1789516800,
            "chat": {"id": CHAT, "type": "supergroup"},
            "from": {"id": OWNER, "is_bot": false, "first_name": "Edouard"},
            "message_thread_id": topic,
            event: {"name": name, "icon_color": 7322096},
        }
    }))
}

async fn topic_session(g: &TelegramGateway, topic: Option<i64>) -> String {
    g.daemon
        .chat_session_for(&Origin::Telegram {
            chat_id: CHAT,
            topic_id: topic,
            message_id: None,
        })
        .await
        .unwrap()
}

async fn project_of(g: &TelegramGateway, sid: &str) -> (Option<String>, Option<String>) {
    penelope_vault::session_project::of_session(&g.daemon.services, sid).await
}

/// #301 : au démarrage, chaque sujet nommé devient un projet et ses sessions y sont
/// rattachées (sans projet, ou projet déduit d'un message), sauf « Général », le foyer et
/// les choix explicites ; un second passage ne change rien ; le port `subject_of` ne nomme
/// que les sujets qui valent projet.
#[tokio::test]
async fn named_topics_become_projects_at_startup_except_general_and_home() {
    let (_d, g, _t, _p) = gateway().await;
    allow_group(&g);
    let s = &g.daemon.services;
    for (topic, name) in [
        (1, "Général"),
        (7, "Pénélope"),
        (21, "Dose"),
        (22, "WestRiders"),
        (23, "Thaïlande"),
    ] {
        s.kv_set(&topic_name_key(CHAT, topic), name).await.unwrap();
    }
    let general = topic_session(&g, None).await;
    let home = topic_session(&g, Some(7)).await;
    let dose = topic_session(&g, Some(21)).await;
    let west = topic_session(&g, Some(22)).await;
    s.kv_set(
        &format!("session.project.{west}"),
        r#"{"project":"helloasso","how":"message"}"#,
    )
    .await
    .unwrap();
    let thai = topic_session(&g, Some(23)).await;
    penelope_vault::session_project::set(s, &thai, Some("voyages")).await;

    let r = g.adopt_subjects().await;
    assert_eq!(
        (r.subjects, r.created, r.attached, r.corrected, r.kept),
        (3, 3, 1, 1, 1),
        "{r:?}"
    );
    let vault = penelope_app::helpers::vault_dir(s);
    assert_eq!(
        penelope_vault::session_project::subjects::notes(&vault),
        vec!["dose", "thailande", "westriders"]
    );
    assert_eq!(
        project_of(&g, &dose).await,
        (Some("dose".into()), Some("sujet".into()))
    );
    assert_eq!(
        project_of(&g, &west).await,
        (Some("westriders".into()), Some("sujet".into()))
    );
    assert_eq!(
        project_of(&g, &thai).await,
        (Some("voyages".into()), Some("explicite".into()))
    );
    assert_eq!(project_of(&g, &home).await, (None, None));
    assert_eq!(project_of(&g, &general).await, (None, None));

    assert_eq!(g.subject_of(&dose).await, Some("Dose".into()));
    assert_eq!(g.subject_of(&thai).await, Some("Thaïlande".into()));
    assert_eq!(
        g.subject_of(&home).await,
        None,
        "le foyer n'est pas un projet"
    );
    assert_eq!(g.subject_of(&general).await, None);

    let again = g.adopt_subjects().await;
    assert!(!again.changed(), "{again:?}");
}

/// #301 : dans un sujet, `/projet` montre le projet du sujet ; le changer ne vaut que pour
/// la session, et la réponse le dit. En conversation privée, rien ne change.
#[tokio::test]
async fn the_project_command_in_a_topic_shows_the_subject_and_a_change_is_session_only() {
    let (_d, g, t, _p) = gateway().await;
    allow_group(&g);
    let s = &g.daemon.services;
    s.kv_set("tg.onboard.proposed", "test").await.unwrap();

    g.process_update(&topic_message(700, 21, "Dose", "/projet"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let said = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        said.contains("<b>dose</b> (le projet du sujet « Dose »)"),
        "{said}"
    );
    // La fiche est née avec le nom du sujet, appris de ce premier message.
    let vault = penelope_app::helpers::vault_dir(s);
    assert!(vault.join("projets/dose.md").exists());

    g.process_update(&topic_message(701, 21, "Dose", "/projet voyages"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let said = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(
        said.contains("<b>voyages</b>")
            && said.contains("ne vaut que pour cette session")
            && said.contains("du sujet « Dose » reviendra à son projet <b>dose</b>"),
        "{said}"
    );
    let sid = topic_session(&g, Some(21)).await;
    assert_eq!(
        project_of(&g, &sid).await,
        (Some("voyages".into()), Some("explicite".into()))
    );
    // Un redémarrage ne reprend pas ce choix.
    let r = g.adopt_subjects().await;
    assert_eq!((r.kept, r.corrected), (1, 0), "{r:?}");

    // En privé : la réponse d'avant, mot pour mot.
    g.process_update(&updates::text_message(
        702,
        OWNER,
        OWNER,
        "/projet fidelatoo",
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let last = texts(&t.calls_to(tg::SEND_MESSAGE).await).pop().unwrap();
    assert_eq!(
        last,
        "📁 Sujet de la session : <b>fidelatoo</b>. La mémoire d'office s'y limite dès le \
         prochain message ; le reste reste au rappel."
    );
}

/// #301 : créer un sujet crée sa fiche ; le renommer déplace la fiche (ancien nom en alias)
/// et rattache ses sessions au nouveau projet ; le nom de naissance répété en tête des
/// messages suivants ne défait pas le renommage. Le foyer, lui, n'a pas de fiche.
#[tokio::test]
async fn creating_a_topic_creates_its_project_and_renaming_moves_it() {
    let (_d, g, _t, _p) = gateway().await;
    allow_group(&g);
    let s = &g.daemon.services;
    s.kv_set("tg.onboard.proposed", "test").await.unwrap();
    let vault = penelope_app::helpers::vault_dir(s);

    g.process_update(&topic_event(800, 30, "forum_topic_created", "Jade"))
        .await
        .unwrap();
    assert!(vault.join("projets/jade.md").exists());
    assert_eq!(
        s.kv_get(&topic_name_key(CHAT, 30))
            .await
            .unwrap()
            .as_deref(),
        Some("Jade")
    );

    g.process_update(&topic_message(801, 30, "Jade", "où en est-on ?"))
        .await
        .unwrap();
    let sid = topic_session(&g, Some(30)).await;
    assert_eq!(g.subject_of(&sid).await, Some("Jade".into()));

    g.process_update(&topic_event(802, 30, "forum_topic_edited", "Jade bijoux"))
        .await
        .unwrap();
    assert!(!vault.join("projets/jade.md").exists());
    let note = std::fs::read_to_string(vault.join("projets/jade-bijoux.md")).unwrap();
    assert!(
        note.contains("nom: Jade bijoux\n") && note.contains("aliases:\n  - Jade\n"),
        "{note}"
    );
    assert_eq!(
        project_of(&g, &sid).await,
        (Some("jade-bijoux".into()), Some("sujet".into()))
    );
    assert_eq!(g.subject_of(&sid).await, Some("Jade bijoux".into()));

    // Le message suivant répète le nom de naissance : il ne fait pas foi.
    g.process_update(&topic_message(803, 30, "Jade", "et maintenant ?"))
        .await
        .unwrap();
    assert_eq!(
        s.kv_get(&topic_name_key(CHAT, 30))
            .await
            .unwrap()
            .as_deref(),
        Some("Jade bijoux")
    );
    assert!(!vault.join("projets/jade.md").exists());
    assert_eq!(
        penelope_vault::session_project::subjects::notes(&vault),
        vec!["jade-bijoux"]
    );

    // Le foyer nommé n'est pas un projet.
    g.process_update(&topic_event(804, 7, "forum_topic_created", "Pénélope"))
        .await
        .unwrap();
    assert!(!vault.join("projets/penelope.md").exists());
}
