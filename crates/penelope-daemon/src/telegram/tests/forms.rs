use super::*;

/// #113 : un groupe à sujets autorisé par son identifiant ; l'administrateur anonyme y
/// ouvre une session par sujet, deux sujets travaillent en parallèle ; un groupe non
/// listé est ignoré mais son identifiant est gardé pour `doctor` ; un tiers est ignoré.
/// #149 : un formulaire ouvert dans un sujet n'avale pas le texte tapé dans un autre,
/// et ses phrases reviennent dans **son** sujet. Le 20/09, « re test » tapé ailleurs
/// est parti remplir un formulaire Redmine, et l'erreur de validation est arrivée dans
/// Général.
#[tokio::test]
async fn a_form_lives_in_its_topic_and_swallows_nothing_from_another() {
    let (_d, g, t, _p) = gateway().await;
    let chat: i64 = -1_001_234_567_890;
    let group = |update_id: i64, topic: i64, text: &str| {
        let mut u = updates::in_topic(updates::text_message(update_id, 0, OWNER, text), topic);
        u["message"]["chat"] = json!({
            "id": chat, "type": "supergroup", "title": "Chantiers", "is_forum": true
        });
        u
    };
    g.daemon
        .publish_config("test", |c| {
            c.telegram.allowed_chats = vec![chat];
            Ok(vec!["telegram.allowed_chats".into()])
        })
        .unwrap();

    // Un formulaire à un champ booléen, ouvert dans le sujet 3.
    let schema = json!({
        "type": "object",
        "properties": {"publier": {"type": "boolean", "title": "Publier ?"}},
        "required": ["publier"],
    });
    let state = penelope_telegram::forms::FormState::new("redmine", schema).unwrap();
    let pending = json!({"choice": "redmine · commentaire", "state": state, "topic": 3});
    g.daemon
        .kv_set(&form_key(chat, Some(3)), &pending.to_string())
        .await
        .unwrap();

    // Un texte dans le sujet 552 : le formulaire du sujet 3 n'y touche pas.
    g.process_update(&group(600, 552, "re test")).await.unwrap();
    settle(&g).await;
    let raw = g
        .daemon
        .kv_get(&form_key(chat, Some(3)))
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(
        raw.contains("\"cursor\":0"),
        "le formulaire du sujet 3 a bougé : {raw}"
    );
    assert!(
        g.daemon
            .kv_get(&form_key(chat, Some(552)))
            .await
            .unwrap()
            .unwrap_or_default()
            .is_empty(),
        "aucun formulaire n'a été ouvert dans le sujet 552"
    );

    // Un texte qui ne vaut pas un booléen, tapé dans le sujet du formulaire :
    // l'avertissement revient dans le sujet 3, jamais dans Général.
    g.process_update(&group(601, 3, "re test")).await.unwrap();
    settle(&g).await;
    let _ = g.flush_outbox().await;
    let warned: Vec<Option<i64>> = t
        .calls_to("sendMessage")
        .await
        .into_iter()
        .filter(|p| p["text"].as_str().is_some_and(|x| x.contains("booléen")))
        .map(|p| p["message_thread_id"].as_i64())
        .collect();
    assert_eq!(
        warned,
        [Some(3)],
        "l'erreur de validation reste dans le sujet du formulaire"
    );
}

#[tokio::test]
async fn a_topic_group_listed_by_id_accepts_the_anonymous_admin() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let chat: i64 = -1_001_234_567_890;
    let group_message = |update_id: i64, chat_id: i64, from: i64, topic: i64, text: &str| {
        let mut u = updates::in_topic(updates::text_message(update_id, 0, from, text), topic);
        u["message"]["chat"] =
            json!({"id": chat_id, "type": "supergroup", "title": "Chantiers", "is_forum": true});
        if from == penelope_telegram::ANONYMOUS_ADMIN_ID {
            u["message"]["sender_chat"] = json!({"id": chat_id, "type": "supergroup"});
        }
        u
    };
    let anon = penelope_telegram::ANONYMOUS_ADMIN_ID;

    // Pas encore listé : silence, identifiant gardé.
    g.process_update(&group_message(500, chat, anon, 7, "bonjour"))
        .await
        .unwrap();
    settle(&g).await;
    assert_eq!(s.turns.pending_count().await.unwrap(), 0);
    assert!(
        t.calls_to(tg::SEND_MESSAGE).await.is_empty(),
        "rien dans le groupe"
    );
    let seen = seen_chats(&s).await;
    assert_eq!(seen[0]["id"], chat);
    assert_eq!(seen[0]["type"], "supergroup");
    let check = crate::doctor::telegram_chats_check(&s).await;
    assert!(check.detail.contains("-1001234567890"), "{}", check.detail);

    g.daemon
        .publish_config("test", |c| {
            c.telegram.allowed_chats = vec![chat];
            Ok(vec!["telegram.allowed_chats".into()])
        })
        .unwrap();
    g.process_update(&group_message(501, chat, anon, 7, "corrige le ticket 12"))
        .await
        .unwrap();
    g.process_update(&group_message(
        502,
        chat,
        anon,
        8,
        "rédige la note de version",
    ))
    .await
    .unwrap();
    g.process_update(&group_message(503, chat, 999, 7, "je passe par là"))
        .await
        .unwrap();
    settle(&g).await;
    let a = s
        .sessions
        .find_by_topic(chat, Some(7))
        .await
        .unwrap()
        .expect("sujet 7");
    let b = s
        .sessions
        .find_by_topic(chat, Some(8))
        .await
        .unwrap()
        .expect("sujet 8");
    assert_ne!(a.id, b.id, "une session par sujet");
    let first = s.turns.claim("r1").await.unwrap().expect("tour du sujet 7");
    let second = s
        .turns
        .claim("r2")
        .await
        .unwrap()
        .expect("tour du sujet 8 en parallèle");
    assert_ne!(first.session_id, second.session_id);
    assert!(
        s.turns.claim("r3").await.unwrap().is_none(),
        "le tiers n'a rien lancé"
    );
}

#[tokio::test]
async fn a_workflow_question_uses_telegram_buttons_and_typed_input() {
    let (_d, g, t, _p) = gateway().await;
    let s = &g.daemon.services;
    let raw = json!({
        "metadata": {"id": "validation", "name": "Validation", "parameters": [
            {"id": "sujet", "label": "Sujet", "type": "string", "required": true}
        ]},
        "entryStep": "decider",
        "settings": {"budget": {"maxUsd": 1.0, "maxTokens": 1000, "maxWallMs": 60000}},
        "steps": [
            {"id": "decider", "name": "Valider {{sujet}} ?", "type": "user",
             "template": "question", "choices": ["Valider", "Réviser"], "input": "text",
             "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "Valider"}},
                {"goto": "$blocked", "condition": {"type": "step_result", "result": "Réviser"}}
             ]}
        ]
    });
    let wf = penelope_workflow::Workflow::from_json(&raw.to_string()).unwrap();
    let known = crate::runtime::workflow_known(&s.config.config(), &s.mcp_tools).await;
    let dir = s.platform.dirs.workflows();
    std::fs::create_dir_all(&dir).unwrap();
    s.workflows.write(&dir, &wf, &known).unwrap();
    s.workflows
        .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);

    // La commande prépare la conversation de plan sans lancer le run.
    g.process_update(&updates::text_message(120, OWNER, OWNER, "/run validation"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        !sent.iter().any(|m| m.contains("Sujet")),
        "aucun champ demandé d'office : {sent:?}"
    );
    let turn = s
        .turns
        .claim("test")
        .await
        .unwrap()
        .expect("tour de conversation");
    let asked = turn.payload["text"].as_str().unwrap_or_default();
    assert!(
        asked.contains("`validation`") && asked.contains("sujet (Sujet)"),
        "{asked}"
    );
    assert!(s.runs.list(None, 5).await.unwrap().is_empty());
    // L'ancien scénario valide ensuite les boutons d'une étape `user` sur un run
    // technique créé directement, indépendamment du nouveau gate de plan.
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let run = crate::workflow::start_run(
        &g.daemon,
        "validation",
        json!({"sujet":"devis-42"}),
        &origin,
        None,
        0,
    )
    .await
    .unwrap();
    assert_eq!(run.params["sujet"], "devis-42");

    crate::workflow::drive(&g.daemon, &run.id).await.unwrap();
    let calls = t.calls_to(tg::SEND_MESSAGE).await;
    let question = calls
        .iter()
        .find(|c| {
            c["text"]
                .as_str()
                .is_some_and(|x| x.contains("Valider devis-42"))
        })
        .expect("question avec boutons");
    let token = question["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
        .as_str()
        .unwrap()
        .to_string();

    // « Valider » demande une précision : le message suivant la donne.
    g.process_update(&updates::callback(122, OWNER, &token, 700))
        .await
        .unwrap();
    settle_click(&g).await;
    g.process_update(&updates::text_message(
        123,
        OWNER,
        OWNER,
        "ok pour la version 2",
    ))
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    assert!(
        s.turns.claim("test").await.unwrap().is_none(),
        "la saisie n'est pas un message de conversation"
    );
    assert_eq!(
        crate::workflow::drive(&g.daemon, &run.id).await.unwrap(),
        penelope_workflow::RunState::Done
    );
    let done = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(done.step_outputs["decider"]["choice"], "Valider");
    assert_eq!(
        done.step_outputs["decider"]["input"],
        "ok pour la version 2"
    );

    // La carte du run a été éditée sur place, pas renvoyée à chaque étape.
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert!(
        edits
            .iter()
            .any(|e| e["text"].as_str().is_some_and(|x| x.contains("terminé"))),
        "{edits:?}"
    );
}

/// Issue #21 : une séance d'accueil complète sur Telegram écrit `profil.md` et
/// `memoire.md`, chaque directive reliée à sa réponse ; rejouer une partie montre ce
/// qui change et remplace l'ancienne réponse.
#[tokio::test]
async fn onboarding_writes_the_profile_from_the_answers() {
    let (_d, g, t, _p) = gateway().await;
    let d = g.daemon.clone();
    let vault = crate::conversation::vault_dir(&d.services);
    let last = || {
        let t = t.clone();
        async move { t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone() }
    };
    let click = |label: &'static str, update: i64| {
        let (g, last) = (g.clone(), last);
        async move {
            let msg = last().await;
            let token = inline_buttons(&msg)
                .into_iter()
                .find(|(l, _)| l.contains(label))
                .unwrap_or_else(|| panic!("pas de bouton « {label} » : {msg}"))
                .1;
            g.process_update(&updates::callback(update, OWNER, &token, 4000))
                .await
                .unwrap();
            settle_click(&g).await;
        }
    };
    let say = |text: &'static str, update: i64| {
        let g = g.clone();
        async move {
            g.process_update(&updates::text_message(update, OWNER, OWNER, text))
                .await
                .unwrap();
        }
    };

    say("/accueil", 400).await;
    assert!(
        last().await["text"]
            .as_str()
            .unwrap()
            .contains("Accueil · 1/9")
    );
    say("développeur indépendant", 401).await;
    let file = std::fs::read_dir(vault.join("accueil"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let raw = std::fs::read_to_string(&file).unwrap();
    assert!(
        raw.contains("## 1. Quel est ton rôle ou ton métier ?\n\ndéveloppeur indépendant"),
        "{raw}"
    );
    assert!(
        raw.contains("## 9. Et ce que je dois toujours faire"),
        "questions écrites d'avance"
    );
    say("Squirrel et ses clients", 402).await;
    say("Pénélope\nSite vitrine", 403).await;
    click("Passer", 404).await;
    click("Tutoiement", 405).await;
    click("Courtes", 406).await;
    say("français", 407).await;
    say("écrire en mon nom\nsupprimer sans demander", 408).await;
    say("éviter le jargon", 409).await;
    let recap = last().await["text"].as_str().unwrap().to_string();
    assert!(
        recap.contains("+ Toujours tutoyer le propriétaire"),
        "{recap}"
    );
    assert!(
        recap.contains("+ Jamais supprimer sans demander"),
        "{recap}"
    );
    assert!(
        recap.contains("+ Projet en cours du propriétaire : Site vitrine"),
        "{recap}"
    );
    assert!(!recap.contains("Outils"), "question passée : {recap}");
    click("Écrire", 410).await;

    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    for directive in [
        "Toujours tutoyer le propriétaire",
        "Préférer des réponses courtes",
        "Toujours répondre en français",
        "Jamais écrire en mon nom",
        "Éviter le jargon",
    ] {
        assert!(profil.contains(directive), "{directive} absent : {profil}");
    }
    assert!(
        std::fs::read_to_string(vault.join("memoire.md"))
            .unwrap()
            .contains("Rôle du propriétaire : développeur indépendant")
    );
    let rel = format!("accueil/{}", file.file_name().unwrap().to_string_lossy());
    let source: String = d
        .services
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT p.source_ref FROM mem_entries e JOIN mem_provenance p ON p.uid = e.uid
                     WHERE e.text = 'Toujours tutoyer le propriétaire'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(source, format!("{rel}#q5"));

    // Rejouer le style : l'ancienne réponse est remplacée, le reste gardé.
    say("/accueil style", 420).await;
    click("Vouvoiement", 421).await;
    click("Courtes", 422).await;
    click("Français", 423).await;
    let recap = last().await["text"].as_str().unwrap().to_string();
    assert!(
        recap.contains("- Toujours tutoyer le propriétaire\n+ Toujours vouvoyer le propriétaire"),
        "{recap}"
    );
    assert!(
        recap.contains("= Préférer des réponses courtes (déjà retenu)"),
        "{recap}"
    );
    click("Écrire", 424).await;
    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert!(profil.contains("Toujours vouvoyer le propriétaire"));
    assert!(
        !profil.contains("Toujours tutoyer le propriétaire"),
        "{profil}"
    );
}

/// Issue #21 : un premier message sur un profil vide propose l'accueil, une fois.
#[tokio::test]
async fn an_empty_profile_proposes_onboarding_once() {
    let (_d, g, t, p) = gateway().await;
    for i in 0..2 {
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("Bonjour !");
        g.process_update(&updates::text_message(430 + i, OWNER, OWNER, "salut"))
            .await
            .unwrap();
        drain(&g).await;
    }
    let proposals = texts(&t.calls_to(tg::SEND_MESSAGE).await)
        .into_iter()
        .filter(|x| x.contains("Ton profil est encore vide"))
        .count();
    assert_eq!(proposals, 1);
}

/// #143 : sans foyer, les avis sans session vont au chat privé ; avec `telegram.home`,
/// au sujet du groupe. Une session, elle, garde toujours sa conversation.
#[tokio::test]
async fn notices_without_a_session_go_to_the_home_topic() {
    let (_d, g, t, _p) = gateway().await;
    let internal = Origin::Internal {
        source: "budget".into(),
    };
    // Sans foyer : le chat privé, comme avant.
    assert_eq!(g.home_chat(), (OWNER, None));
    g.send_text(&internal, "alerte").await.unwrap();
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await.pop().expect("envoi");
    assert_eq!(sent["chat_id"], OWNER);
    assert!(sent.get("message_thread_id").is_none_or(|v| v.is_null()));

    // `/home` depuis un sujet de groupe : le foyer suit.
    g.daemon
        .publish_config("test", |c| {
            c.telegram.home = penelope_kernel::config::TelegramHome {
                chat: -100_394,
                topic: 17,
            };
            Ok(vec!["telegram.home".into()])
        })
        .unwrap();
    assert_eq!(g.home_chat(), (-100_394, Some(17)));
    t.clear().await;
    g.send_text(&internal, "alerte").await.unwrap();
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await.pop().expect("envoi");
    assert_eq!(sent["chat_id"], -100_394);
    assert_eq!(sent["message_thread_id"], 17);

    // Une session garde son chat et son sujet : le foyer ne les remplace pas.
    let with_session = Origin::Telegram {
        chat_id: -100_999,
        topic_id: Some(5),
        message_id: None,
    };
    t.clear().await;
    g.send_text(&with_session, "réponse").await.unwrap();
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await.pop().expect("envoi");
    assert_eq!(sent["chat_id"], -100_999);
    assert_eq!(sent["message_thread_id"], 5);

    // `doctor` ne réclame un foyer que si des groupes sont autorisés.
    let check = crate::doctor::home_check(&g.daemon.services);
    assert!(check.ok, "{check:?}");
    g.daemon
        .publish_config("test", |c| {
            c.telegram.home = Default::default();
            c.telegram.allowed_chats = vec![-100_394];
            Ok(vec!["telegram.home".into()])
        })
        .unwrap();
    let check = crate::doctor::home_check(&g.daemon.services);
    assert!(!check.ok, "{check:?}");
    assert!(check.detail.contains("chat privé"), "{check:?}");
}

/// Étape `user` avec `input: "form:<id>"` : le choix ouvre le formulaire, un champ par
/// écran (boutons ou message), récapitulatif, puis la saisie validée part au workflow.
#[tokio::test]
async fn a_workflow_form_is_filled_field_by_field() {
    let (_d, g, t, _p) = gateway().await;
    let d = g.daemon.clone();
    let raw = json!({
        "metadata": {"id": "deploiement-form", "name": "Déploiement", "parameters": []},
        "entryStep": "parametres",
        "settings": {
            "maxIterations": 5,
            "budget": {"maxUsd": 1.0, "maxTokens": 1000, "maxWallMs": 60000},
            "forms": {"deploy": {
                "type": "object",
                "required": ["environnement", "version"],
                "properties": {
                    "environnement": {"type": "string", "title": "Environnement",
                        "enum": ["prod", "staging"], "enumNames": ["Production", "Pré-production"]},
                    "version": {"type": "string", "title": "Version", "minLength": 1},
                    "notifier": {"type": "boolean", "title": "Prévenir l'équipe"}
                }
            }}
        },
        "steps": [
            {"id": "parametres", "type": "user", "template": "question",
             "choices": ["Déployer", "Annuler"], "input": "form:deploy",
             "transitions": [{"goto": "$done"}]}
        ]
    });
    let s = &d.services;
    let wf = penelope_workflow::model::Workflow::from_json(&raw.to_string()).unwrap();
    let known =
        crate::runtime::workflow_known_with(&s.config.config(), &s.mcp_tools, &s.workflows).await;
    let dir = s.platform.dirs.workflows();
    std::fs::create_dir_all(&dir).unwrap();
    s.workflows
        .write(&dir, &wf, &known)
        .expect("workflow valide");
    s.workflows
        .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);

    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let run = crate::workflow::start_run(&d, "deploiement-form", json!({}), &origin, None, 0)
        .await
        .unwrap();
    crate::workflow::drive(&d, &run.id).await.unwrap();

    let button = |calls: &[Value], label: &str| -> String {
        calls
            .iter()
            .rev()
            .find_map(|c| {
                c["reply_markup"]["inline_keyboard"]
                    .as_array()?
                    .iter()
                    .find_map(|row| {
                        row.as_array()?.iter().find_map(|b| {
                            (b["text"].as_str()? == label)
                                .then(|| b["callback_data"].as_str().map(String::from))
                                .flatten()
                        })
                    })
            })
            .unwrap_or_else(|| panic!("bouton « {label} » absent"))
    };
    let mut update = 500;
    let mut click = |label: &str, calls: Vec<Value>| {
        update += 1;
        (update, button(&calls, label))
    };

    let (u, token) = click("Déployer", t.calls_to(tg::SEND_MESSAGE).await);
    g.process_update(&updates::callback(u, OWNER, &token, 900))
        .await
        .unwrap();
    settle_click(&g).await;
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert!(
        sent.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("Environnement"),
        "{sent:?}"
    );

    let (u, token) = click("Production", sent);
    g.process_update(&updates::callback(u, OWNER, &token, 901))
        .await
        .unwrap();
    settle_click(&g).await;
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert!(
        sent.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("Version")
    );

    g.process_update(&updates::text_message(600, OWNER, OWNER, "1.4.2"))
        .await
        .unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert!(
        sent.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("Prévenir")
    );

    let (u, token) = click("Oui", sent);
    g.process_update(&updates::callback(u, OWNER, &token, 902))
        .await
        .unwrap();
    settle_click(&g).await;
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let summary = sent.last().unwrap()["text"].as_str().unwrap().to_string();
    assert!(
        summary.contains("Récapitulatif") && summary.contains("1.4.2"),
        "{summary}"
    );

    let (u, token) = click("✅ Envoyer", sent);
    g.process_update(&updates::callback(u, OWNER, &token, 903))
        .await
        .unwrap();
    settle_click(&g).await;
    g.flush_outbox().await.unwrap();
    assert_eq!(
        crate::workflow::drive(&d, &run.id).await.unwrap(),
        penelope_workflow::runs::RunState::Done
    );
    let done = d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(
        done.step_outputs["parametres"]["input"],
        json!({"environnement": "prod", "version": "1.4.2", "notifier": true})
    );
    assert_eq!(done.step_outputs["parametres"]["choice"], "Déployer");
}
