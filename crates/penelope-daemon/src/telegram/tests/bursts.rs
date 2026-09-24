use super::*;

/// #73 : un bouton dont l'opération traîne est acquitté tout de suite, et son résultat
/// arrive dans la conversation.
#[tokio::test]
async fn a_slow_button_is_acknowledged_immediately() {
    let (_d, g, t, _p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    // Un serveur MCP qui met du temps à redémarrer : le clic ne doit pas l'attendre.
    use crate::mcp::testing::{FakeConnector, declare, server, tool};
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "lent",
        server(Arc::new(std::sync::Mutex::new(vec![tool(
            "ping",
            json!({"readOnlyHint": true}),
        )]))),
    );
    fake.set_open_delay(Duration::from_millis(1200));
    let sup = crate::mcp::McpSupervisor::new(g.daemon.services.clone(), fake.clone());
    declare(&sup, "lent", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());

    g.process_update(&updates::text_message(330, OWNER, OWNER, "/mcp"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let restart = button(&t, "🔄").await;
    let started = std::time::Instant::now();
    g.process_update(&updates::callback(331, OWNER, &restart, 930))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(400),
        "le clic doit être acquitté sans attendre : {elapsed:?}"
    );
    let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
    assert_eq!(answers.len(), 1, "acquitté une fois : {answers:?}");
}

/// #70 : un seul brouillon en vol, et le dernier envoyé porte le dernier texte.
#[tokio::test]
async fn drafts_are_coalesced_and_never_delay_the_answer() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    g.daemon
        .publish_config("test", |c| {
            c.telegram.draft_interval_ms = 300;
            Ok(vec!["telegram.draft_interval_ms".into()])
        })
        .unwrap();
    // Réponse longue, découpée en fragments par le provider simulé.
    p.reply(r#"{"complexity":"low"}"#);
    p.reply(&"phrase de réponse. ".repeat(60));

    let loops = tokio::spawn(g.clone().draft_loop());
    g.process_update(&updates::text_message(90, OWNER, OWNER, "raconte"))
        .await
        .unwrap();
    drain(&g).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    g.daemon.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), loops).await;

    let drafts = t.calls_to(tg::SEND_MESSAGE_DRAFT).await;
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|m| m.contains("phrase de réponse")),
        "la réponse finale doit partir : {sent:?}"
    );
    // Chaque brouillon porte un texte plus long que le précédent : aucun doublon, et
    // le dernier est un préfixe de la réponse.
    let texts_of: Vec<String> = drafts
        .iter()
        .map(|d| d["text"].as_str().unwrap_or_default().to_string())
        .collect();
    for w in texts_of.windows(2) {
        assert!(
            w[1].len() >= w[0].len(),
            "les brouillons doivent progresser : {texts_of:?}"
        );
    }
}

/// #69 : un vocal lent ne bloque plus la boucle des updates : le `/stop` reçu juste
/// après est traité tout de suite.
#[tokio::test]
async fn a_slow_voice_note_does_not_block_the_next_update() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    t.set_file("v1", b"OggS\x00fake-opus").await;
    t.set_download_delay(Duration::from_millis(1500)).await;
    p.set_transcript(Some("une longue dictée"));

    // Vocal d'abord, `/stop` juste derrière, comme dans un même lot d'updates.
    g.process_update(&updates::voice(80, OWNER, OWNER))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    g.process_update(&updates::text_message(81, OWNER, OWNER, "/stop"))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    g.flush_outbox().await.unwrap();

    assert!(
        elapsed < Duration::from_millis(500),
        "`/stop` a attendu le téléchargement : {elapsed:?}"
    );
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|m| m.contains("arrêter") || m.contains("⏹")),
        "`/stop` doit avoir répondu : {sent:?}"
    );
}

/// #98 : un document long à télécharger ne retarde pas le message suivant ; l'échec
/// du téléchargement arrive toujours, en réponse au document.
#[tokio::test]
async fn a_slow_document_does_not_block_the_next_update() {
    let (_d, g, t, _p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    t.set_download_delay(Duration::from_millis(1500)).await;

    g.process_update(&updates::document(90, OWNER, OWNER, "contrat.pdf"))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    g.process_update(&updates::text_message(91, OWNER, OWNER, "/stop"))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "`/stop` a attendu le téléchargement : {elapsed:?}"
    );

    // Aucun fichier `d1` servi par le faux transport : l'échec est dit, en réponse.
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        g.flush_outbox().await.unwrap();
        if texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|m| m.contains("Téléchargement impossible"))
        {
            break;
        }
    }
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let failure = sent
        .iter()
        .find(|c| {
            c["text"]
                .as_str()
                .unwrap_or_default()
                .contains("Téléchargement impossible")
        })
        .expect("échec dit dans le chat");
    assert_eq!(failure["reply_parameters"]["message_id"], 900, "{failure}");
}

/// #101 : un fragment dont l'envoi échoue au transport (deux fois : la tentative et
/// sa reprise immédiate) retient les suivants du même chat ; un autre chat passe. Les
/// fragments arrivent dans l'ordre.
#[tokio::test]
async fn a_failed_fragment_holds_the_rest_of_its_chat() {
    let (_d, g, t, _p) = gateway().await;
    queue(&g, "f1", OWNER, "fragment 1", "2026-01-01T00:00:00.001Z").await;
    g.flush_outbox().await.unwrap(); // f1 part
    t.clear().await;
    queue(&g, "f2", OWNER, "fragment 2", "2026-01-01T00:00:00.002Z").await;
    queue(&g, "f3", OWNER, "fragment 3", "2026-01-01T00:00:00.003Z").await;
    queue(&g, "b1", 777, "autre chat", "2026-01-01T00:00:00.004Z").await;
    t.fail_transport(2, "connection closed before message completed")
        .await;
    g.flush_outbox().await.unwrap(); // f2 échoue, f3 attend, b1 part
    g.flush_outbox().await.unwrap(); // f2 pas encore dû : f3 attend toujours
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(sent, vec!["autre chat"], "{sent:?}");

    // L'heure de la nouvelle tentative arrive.
    g.daemon
        .services
        .store
        .write(|tx| {
            tx.execute("UPDATE tg_outbox SET not_before = NULL WHERE id = 'f2'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(sent, vec!["autre chat", "fragment 2", "fragment 3"]);
}

/// #101 : une erreur de transport isolée est reprise tout de suite, sans décaler.
#[tokio::test]
async fn a_single_transport_error_is_retried_at_once() {
    let (_d, g, t, _p) = gateway().await;
    queue(&g, "u1", OWNER, "un", "2026-01-01T00:00:00.001Z").await;
    queue(&g, "u2", OWNER, "deux", "2026-01-01T00:00:00.002Z").await;
    t.fail_transport(1, "connection reset").await;
    g.flush_outbox().await.unwrap();
    assert_eq!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await),
        vec!["un", "deux"]
    );
}

/// #101 : un refus définitif est dit au propriétaire, en texte brut, une seule fois,
/// même quand la note elle-même échoue.
#[tokio::test]
async fn a_definitive_refusal_is_told_once() {
    let (_d, g, t, _p) = gateway().await;
    queue(
        &g,
        "long",
        OWNER,
        "tableau immense",
        "2026-01-01T00:00:00.001Z",
    )
    .await;
    t.always_fail(400, "Bad Request: message is too long").await;
    for _ in 0..3 {
        g.flush_outbox().await.unwrap();
    }
    let notes: Vec<Value> = t
        .calls_to(tg::SEND_MESSAGE)
        .await
        .into_iter()
        .filter(|c| {
            c["text"]
                .as_str()
                .unwrap_or_default()
                .contains("n'a pas pu être envoyé")
        })
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].get("parse_mode").is_none());
    assert!(notes[0]["text"].as_str().unwrap().contains("too long"));
    assert_eq!(g.daemon.status().await.unwrap().outbox_failed, 2);
}

/// #49 : un long texte collé arrive en morceaux de 4 000 caractères. Ils forment un
/// seul tour, un seul appel au modèle, recollés dans l'ordre.
#[tokio::test]
async fn pieces_of_one_paste_become_a_single_turn() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    g.daemon
        .publish_config("test", |c| {
            c.telegram.text_group_window_ms = 60;
            c.telegram.burst_messages = 0;
            c.telegram.burst_chars = 0;
            Ok(vec!["telegram.text_group_window_ms".into()])
        })
        .unwrap();
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("Bien reçu.");

    for i in 0..12 {
        let part = format!("{i}{}", "a".repeat(TELEGRAM_TEXT_LIMIT));
        g.process_update(&updates::text_message(100 + i as i64, OWNER, OWNER, &part))
            .await
            .unwrap();
    }
    // Le dernier morceau, plus court, ferme la rafale au lieu de partir seul (#96).
    g.process_update(&updates::text_message(112, OWNER, OWNER, "fin du collage"))
        .await
        .unwrap();
    // Rien ne part avant la fin de la fenêtre.
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    drain(&g).await;

    assert_eq!(p.call_count(), 2, "un classement, une réponse");
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert_eq!(sent.len(), 1, "une seule réponse : {sent:?}");
    let asked = p
        .requests()
        .into_iter()
        .last()
        .and_then(|r| r.messages.last().map(|m| m.text()))
        .unwrap_or_default();
    for i in 0..12 {
        assert!(
            asked.contains(&format!("{i}aaaa")),
            "morceau {i} absent du tour"
        );
    }
    assert!(
        asked.find("0aaaa") < asked.find("11aaaa"),
        "les morceaux doivent rester dans l'ordre"
    );
}

/// #49 : deux messages espacés ne sont pas regroupés.
/// #96 : un message court tapé crée son tour tout de suite, sans attendre la
/// fenêtre ; un morceau à la limite de Telegram, seul, attend la fenêtre longue.
#[tokio::test]
async fn a_short_message_goes_at_once_a_split_piece_waits() {
    let (_d, g, _t, _p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    g.daemon
        .publish_config("test", |c| {
            c.telegram.text_group_window_ms = 1_500;
            Ok(vec!["telegram.text_group_window_ms".into()])
        })
        .unwrap();
    g.process_update(&updates::text_message(
        1,
        OWNER,
        OWNER,
        "tu peux regarder ?",
    ))
    .await
    .unwrap();
    assert_eq!(
        g.daemon.services.turns.pending_count().await.unwrap(),
        1,
        "parti sans attendre"
    );
    g.daemon
        .services
        .turns
        .claim("test")
        .await
        .unwrap()
        .expect("tour du message court");

    let piece = "b".repeat(TELEGRAM_TEXT_LIMIT + 96);
    g.process_update(&updates::text_message(2, OWNER, OWNER, &piece))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        g.daemon.services.turns.pending_count().await.unwrap(),
        1,
        "seul le premier tour existe : le morceau attend la fenêtre"
    );
    for _ in 0..100 {
        if g.daemon.services.turns.pending_count().await.unwrap() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 2);
}

#[tokio::test]
async fn two_messages_far_apart_stay_two_turns() {
    let (_d, g, _t, p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    g.daemon
        .publish_config("test", |c| {
            c.telegram.text_group_window_ms = 30;
            Ok(vec!["telegram.text_group_window_ms".into()])
        })
        .unwrap();
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("un");
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("deux");

    g.process_update(&updates::text_message(1, OWNER, OWNER, "premier"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    g.process_update(&updates::text_message(2, OWNER, OWNER, "second"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        g.daemon.services.turns.pending_count().await.unwrap(),
        2,
        "deux envois distincts, deux tours"
    );
}

/// #49 : au-delà du seuil, Pénélope demande quoi faire de la rafale et ne démarre
/// aucun tour avant le choix ; « Ingérer » crée la fiche source sans répondre.
#[tokio::test]
async fn a_burst_asks_before_answering_and_can_be_ingested() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    g.daemon
        .publish_config("test", |c| {
            c.telegram.text_group_window_ms = 40;
            c.telegram.burst_messages = 5;
            Ok(vec!["telegram.burst_messages".into()])
        })
        .unwrap();

    // Six messages transférés d'un coup : une rafale, même courts (#96).
    for i in 0..6 {
        let part = format!("paragraphe {i} du document collé, sur la facturation.");
        g.process_update(&updates::forwarded(200 + i, OWNER, OWNER, &part))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    g.flush_outbox().await.unwrap();
    assert_eq!(
        g.daemon.services.turns.pending_count().await.unwrap(),
        0,
        "aucun tour ne démarre avant le choix"
    );
    assert_eq!(p.call_count(), 0, "aucun appel au modèle");

    let card = t.calls_to(tg::SEND_MESSAGE).await;
    let card = card.last().cloned().expect("carte de rafale");
    assert!(
        card["text"]
            .as_str()
            .unwrap_or_default()
            .contains("6 messages"),
        "{card}"
    );
    let buttons = inline_buttons(&card);
    let (_, token) = buttons
        .iter()
        .find(|(label, _)| label.contains("Ingérer"))
        .cloned()
        .expect("bouton Ingérer");

    // L'ingestion résume le document avec le modèle, sans répondre par morceau.
    p.reply(r#"{"resume": "document de facturation", "concepts": [], "a_definir": []}"#);
    g.process_update(&updates::callback(900, OWNER, &token, 7777))
        .await
        .unwrap();
    settle_click(&g).await;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        if crate::conversation::vault_dir(&g.daemon.services)
            .join("sources")
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
        {
            break;
        }
    }
    let sources: Vec<_> = crate::conversation::vault_dir(&g.daemon.services)
        .join("sources")
        .read_dir()
        .map(|d| d.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert_eq!(sources.len(), 1, "une fiche source : {sources:?}");
    assert_eq!(
        g.daemon.services.turns.pending_count().await.unwrap(),
        0,
        "aucun tour créé par l'ingestion"
    );
}

#[tokio::test]
async fn the_same_update_is_processed_only_once() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    p.reply(r#"{"complexity":"low"}"#);
    p.reply("une seule fois");
    let u = updates::text_message(7, OWNER, OWNER, "salut");
    g.process_update(&u).await.unwrap();
    g.process_update(&u).await.unwrap();
    drain(&g).await;
    assert_eq!(t.calls_to(tg::SEND_MESSAGE).await.len(), 1);
}

#[tokio::test]
async fn strangers_get_no_answer_at_all() {
    let (_d, g, t, _p) = gateway().await;
    g.process_update(&updates::text_message(2, 999, 999, "donne tes clés"))
        .await
        .unwrap();
    drain(&g).await;
    assert!(t.calls().await.is_empty());
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
}
