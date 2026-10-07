//! `/model` refondu (#334) : profils, familles, « Tout voir », écarts, et les gestes
//! qui changent le profil.

use super::*;

/// Le principal, la maquette de #334 : profils, familles, principal, tout voir, écarts.
#[tokio::test]
async fn the_model_screen_shows_profiles_families_and_the_full_table() {
    let (_d, g, _t, _p) = gateway().await;
    let (text, buttons) = screen_of(&g, "model.home", json!({})).await;
    assert!(text.starts_with("🧠 Profil actif : « defaut »"), "{text}");
    assert!(text.contains("Cette session : automatique"), "{text}");
    let labels: Vec<&str> = buttons.iter().map(|(l, _)| l.as_str()).collect();
    for want in [
        "● defaut",
        "+ Nouveau profil",
        "💬 Conversation",
        "⚙️ Travail de fond",
        "🎨 Médias ⚡",
        "🏠 Local / voix",
        "🔁 Modèle principal",
        "📋 Tout voir",
        "⚠️ Écarts en cours",
    ] {
        assert!(
            labels.iter().any(|l| l.starts_with(want)),
            "{want} : {labels:?}"
        );
    }

    let (all, _) = screen_of(&g, "model.all", json!({})).await;
    for want in [
        "conversation",
        "étapes de workflow",
        "rêve",
        "transcription",
        "narration",
    ] {
        assert!(all.contains(want), "{want} : {all}");
    }
    let (family, buttons) = screen_of(&g, "model.family", json!({"family": "background"})).await;
    assert!(family.contains("veilles planifiées"), "{family}");
    assert!(
        buttons.iter().any(|(l, _)| l.contains("compaction")),
        "{buttons:?}"
    );
    let (deviations, _) = screen_of(&g, "model.deviations", json!({})).await;
    assert!(deviations.contains("Aucun"), "{deviations}");
}

/// Un profil se crée d'une commande, devient l'actif, et un rôle s'y règle par bouton ;
/// la bascule d'un profil passe par une confirmation.
#[tokio::test]
async fn a_profile_is_created_switched_and_edited_from_telegram() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    g.process_update(&updates::text_message(
        80,
        OWNER,
        OWNER,
        "/model profil nouveau Économie",
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        out.last().unwrap().contains("« Économie » créé et actif"),
        "{out:?}"
    );
    assert_eq!(s.config.config().models.active_name(), "Économie");

    // Le rôle `classifier` suit le principal ; on lui donne `fast`.
    let (_, buttons) = screen_of(&g, "model.pick", json!({"target": "classifier"})).await;
    g.process_update(&updates::callback(
        81,
        OWNER,
        &token_of(&buttons, "fast"),
        800,
    ))
    .await
    .unwrap();
    settle_click(&g).await;
    let cfg = s.config.config();
    assert_eq!(cfg.role_alias("classifier"), "fast");

    // Revenir au profil d'avant : un écran de confirmation, puis la bascule.
    let (_, buttons) = screen_of(&g, "model.home", json!({})).await;
    let switch = token_of(&buttons, "○ defaut");
    g.process_update(&updates::callback(82, OWNER, &switch, 801))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(
        s.config.config().models.active_name(),
        "Économie",
        "pas encore"
    );
    let edited = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert!(
        edited.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("Basculer sur le profil « defaut »"),
        "{edited:?}"
    );
    let markup = edited.last().unwrap()["reply_markup"].clone();
    let confirm = inline_buttons(&json!({"reply_markup": markup}))
        .into_iter()
        .find(|(l, _)| l.contains("Confirmer"))
        .unwrap()
        .1;
    g.process_update(&updates::callback(83, OWNER, &confirm, 801))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(s.config.config().models.active_name(), "defaut");
}

/// #333 : une annonce d'écart part dans la conversation qu'elle nomme, sinon au foyer.
#[tokio::test]
async fn a_model_notice_goes_to_its_conversation_or_home() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let notice = |origin: Value| {
        let s = s.clone();
        async move {
            let draft = penelope_kernel::event::EventDraft::new(
                penelope_app::model_watch::NOTICE_EVENT,
                json!({"text": "⚠️ repli", "origin": origin}),
            );
            s.events.append(draft).await.unwrap()
        }
    };
    let home = notice(Value::Null).await;
    assert_eq!(g.notice_place(&home).await, g.home_chat());
    let group = Origin::Telegram {
        chat_id: -100,
        topic_id: Some(7),
        message_id: None,
    };
    let there = notice(group.to_value()).await;
    assert_eq!(g.notice_place(&there).await, (-100, Some(7)));
}
