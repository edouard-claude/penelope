use super::*;

#[tokio::test]
async fn a_photo_is_described_for_a_model_that_cannot_see() {
    let (_d, g, t, p) = gateway().await;
    t.set_file("ph1", JPEG).await;
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Un reçu de pharmacie : total 23,40 €, daté du 12 septembre.");
    p.reply("Tu as dépensé 23,40 € à la pharmacie.");
    g.process_update(&photo_update(70, "ph1", None, Some("combien ?")))
        .await
        .unwrap();
    settle(&g).await;
    drain(&g).await;

    let requests = p.requests();
    let vision = requests
        .iter()
        .find(|r| r.messages[0].text().contains("Tu décris des images"))
        .expect("appel au modèle de vision");
    let cfg = g.daemon.services.config.config();
    assert_eq!(vision.model, cfg.alias_model("vision").unwrap());
    assert!(vision.messages[1].content.iter().any(|c| matches!(
        c,
        penelope_llm::types::Content::ImageUrl { url, .. } if url.starts_with("data:image/jpeg;base64,")
    )));
    let chat = requests.last().unwrap();
    assert!(
        chat.messages.iter().all(|m| m
            .content
            .iter()
            .all(|c| !matches!(c, penelope_llm::types::Content::ImageUrl { .. }))),
        "le modèle de la conversation ne reçoit que du texte"
    );
    assert!(chat.messages.iter().any(|m| {
        let t = m.text();
        // Le contexte volatil (T4) précède le texte du dernier message utilisateur.
        t.contains("combien ?") && t.contains("23,40 €") && t.contains("modèle de vision")
    }));
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|m| m.contains("23,40 € à la pharmacie")),
        "{sent:?}"
    );
    let roles = g
        .daemon
        .services
        .budget
        .report("role", None, None, 10)
        .await
        .unwrap();
    assert!(roles.iter().any(|r| r.key == "image_describe"), "{roles:?}");
}

#[tokio::test]
async fn a_multimodal_model_sees_the_album_in_one_turn() {
    let (_d, g, t, p) = gateway().await;
    let cfg = g.daemon.services.config.config();
    let main = penelope_llm::catalog::strip_provider(cfg.alias_model("main").unwrap()).to_string();
    let mut info = penelope_llm::catalog::ModelInfo::minimal(&main, "deepseek", 128_000);
    info.input_modalities = vec!["text".into(), "image".into()];
    g.daemon.services.catalog.upsert(vec![info]);
    t.set_file("a1", JPEG).await;
    t.set_file("a2", JPEG).await;
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Deux vues du même salon.");

    g.process_update(&photo_update(80, "a1", Some("album-1"), Some("compare")))
        .await
        .unwrap();
    g.process_update(&photo_update(81, "a2", Some("album-1"), None))
        .await
        .unwrap();
    assert!(
        g.daemon
            .services
            .turns
            .claim("test")
            .await
            .unwrap()
            .is_none(),
        "l'album attend ses photos avant de partir"
    );
    tokio::time::sleep(ALBUM_WINDOW).await;
    // Téléchargements détachés : sous charge, on attend le tour plutôt qu'un délai fixe.
    for _ in 0..100 {
        if g.daemon.services.turns.pending_count().await.unwrap() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drain(&g).await;

    let requests = p.requests();
    assert_eq!(
        requests.len(),
        2,
        "un classifieur et une réponse, pas de vision"
    );
    let images: usize = requests[1]
        .messages
        .iter()
        .map(|m| {
            m.content
                .iter()
                .filter(|c| matches!(c, penelope_llm::types::Content::ImageUrl { .. }))
                .count()
        })
        .sum();
    assert_eq!(images, 2, "les deux photos dans le même tour");
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.iter().any(|m| m.contains("même salon")), "{sent:?}");
}

#[tokio::test]
async fn a_document_is_ingested_proposed_and_answered() {
    let (_d, g, t, p) = gateway().await;
    let body = "Contrat de maintenance ACME\n\nLe contrat court jusqu'au 31 mars 2027.\n\n\
                    Carte de test : 4111 1111 1111 1111\n\nContact : Paul Martin.";
    t.set_file("doc1", body.as_bytes()).await;
    p.reply(
        r#"{"resume": "Contrat de maintenance avec ACME jusqu'en mars 2027.",
                "faits": ["Le contrat de maintenance ACME court jusqu'au 31 mars 2027."]}"#,
    );
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Il expire le 31 mars 2027.");
    g.process_update(&document_update(
        90,
        "doc1",
        "Contrat ACME.txt",
        Some("quand expire-t-il ?"),
    ))
    .await
    .unwrap();

    let vault = crate::conversation::vault_dir(&g.daemon.services);
    let source = vault.join("sources/contrat-acme.md");
    assert!(
        eventually(|| async { source.exists() }).await,
        "fiche écrite"
    );
    assert!(
        eventually(|| async {
            g.flush_outbox().await.unwrap();
            texts(&t.calls_to(tg::SEND_MESSAGE).await)
                .iter()
                .any(|m| m.contains("Propositions de mémoire"))
        })
        .await
    );
    drain(&g).await;

    let raw = std::fs::read_to_string(&source).unwrap();
    let parsed = penelope_memory::ingest::parse_source(&raw).unwrap();
    assert_eq!(parsed.origine, penelope_memory::Origin::Untrusted);
    assert!(
        !raw.contains("4111"),
        "un numéro de carte n'entre pas dans le vault"
    );
    assert!(raw.contains("## Résumé"));

    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|m| m.contains("sources/contrat-acme.md")),
        "{sent:?}"
    );
    assert!(sent.iter().any(|m| m.contains("31 mars 2027")), "{sent:?}");
    let chat = p.requests().last().unwrap().clone();
    let asked = chat
        .messages
        .iter()
        .map(|m| m.text())
        .find(|t| t.contains("quand expire-t-il ?"))
        .expect("la légende part en tour");
    assert!(asked.contains("contenu non fiable"), "{asked}");
    assert!(asked.contains("Paul Martin"));

    // Les passages se retrouvent par recherche explicite, jamais par rappel automatique.
    let s = &g.daemon.services;
    let explicit = penelope_memory::SearchFilter::explicit();
    let hits = s.memory.search("ACME", None, &explicit, &[]).await.unwrap();
    assert!(hits.iter().any(|h| h.entry.etype == "source"));
    let auto = s
        .memory
        .search("ACME", None, &penelope_memory::SearchFilter::default(), &[])
        .await
        .unwrap();
    assert!(auto.iter().all(|h| h.entry.etype != "source"));

    // « Tout » : le fait rejoint notes.md, avec le document en provenance.
    let pending = s.approvals.pending(10).await.unwrap();
    let proposal = pending
        .iter()
        .find(|a| a.kind == penelope_hitl::ApprovalKind::MemoryProposal)
        .expect("proposition en attente");
    let token = s
        .actions
        .create(
            k::MEMORY_ACCEPT,
            proposal.id.as_str(),
            json!({}),
            60_000,
            true,
        )
        .await
        .unwrap()
        .token;
    g.process_update(&updates::callback(91, OWNER, &token, 900))
        .await
        .unwrap();
    settle_click(&g).await;
    g.flush_outbox().await.unwrap();
    let notes = std::fs::read_to_string(vault.join("notes.md")).unwrap();
    assert!(notes.contains("31 mars 2027"), "{notes}");
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|m| m.contains("1 fait(s) ajouté(s)")),
        "{sent:?}"
    );
}

#[tokio::test]
async fn mien_marks_a_document_as_written_by_the_owner() {
    let (_d, g, t, p) = gateway().await;
    t.set_file(
        "doc2",
        b"# Mes principes\n\nToujours relire avant d'envoyer.",
    )
    .await;
    p.reply(r#"{"resume": "Principes de travail.", "faits": []}"#);
    g.process_update(&document_update(95, "doc2", "principes.md", Some("/mien")))
        .await
        .unwrap();
    let source = crate::conversation::vault_dir(&g.daemon.services).join("sources/principes.md");
    assert!(eventually(|| async { source.exists() }).await);
    let raw = std::fs::read_to_string(&source).unwrap();
    assert_eq!(
        penelope_memory::ingest::parse_source(&raw).unwrap().origine,
        penelope_memory::Origin::Owner
    );
    assert!(
        g.daemon
            .services
            .turns
            .claim("test")
            .await
            .unwrap()
            .is_none(),
        "`/mien` seul n'est pas une demande"
    );
}

#[tokio::test]
async fn other_files_become_attachments_the_agent_can_reach() {
    let (_d, g, t, _p) = gateway().await;
    t.set_file("csv1", b"date,montant\n2026-09-01,12\n").await;
    t.set_file("bin1", &[0u8, 159, 146, 150, 0, 1]).await;
    g.process_update(&document_update(96, "csv1", "depenses.csv", None))
        .await
        .unwrap();
    g.process_update(&document_update(97, "bin1", "../archive.bin", None))
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            g.flush_outbox().await.unwrap();
            let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
            sent.iter().any(|m| m.contains("artefact"))
                && sent.iter().any(|m| m.contains("archive.bin"))
        })
        .await
    );
    let workspace = crate::executor::default_workspaces(&g.daemon.services)[0].clone();
    assert!(workspace.join("telegram/archive.bin").exists());
}

#[tokio::test]
async fn a_voice_note_is_transcribed_quoted_then_answered() {
    let (_d, g, t, p) = gateway().await;
    t.set_file("v1", b"OggS\x00fake-opus").await;
    p.set_transcript(Some("Rappelle-moi d'appeler Paul demain"));
    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("C'est noté.");
    g.process_update(&updates::voice(60, OWNER, OWNER))
        .await
        .unwrap();
    settle(&g).await;
    drain(&g).await;

    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent[0].contains("Rappelle-moi d'appeler Paul demain"),
        "{sent:?}"
    );
    assert!(sent[0].contains("blockquote"), "citation : {sent:?}");
    assert!(sent.iter().any(|m| m.contains("C'est noté.")), "{sent:?}");

    let (name, size, lang) = p.transcribed.lock().unwrap()[0].clone();
    assert_eq!(name, "audio.ogg");
    assert_eq!(size, 14);
    assert_eq!(lang.as_deref(), Some("fr"));
    let chat = p.requests().last().unwrap().clone();
    assert!(
        chat.messages
            .iter()
            .any(|m| m.text().contains("(message vocal transcrit) Rappelle-moi")),
        "le modèle reçoit le texte transcrit"
    );
    let stt = g
        .daemon
        .services
        .budget
        .report("role", None, None, 10)
        .await
        .unwrap();
    assert!(stt.iter().any(|r| r.key == "stt"), "{stt:?}");
}

#[tokio::test]
async fn a_voice_note_without_local_stt_explains_what_to_configure() {
    let (_d, g, t, _p) = gateway().await;
    // Sans provider imposé : la configuration par défaut vise un serveur local éteint.
    let g = TelegramGateway::with_transport(
        Arc::new(Daemon::from_services(g.daemon.services.clone())),
        t.clone(),
    );
    t.set_file("v1", b"OggS").await;
    g.process_update(&updates::voice(61, OWNER, OWNER))
        .await
        .unwrap();
    // Traitement détaché : on attend le message d'explication (issue #69).
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        g.flush_outbox().await.unwrap();
        if !t.calls_to(tg::SEND_MESSAGE).await.is_empty() {
            break;
        }
    }
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent[0].contains("providers.local"), "{sent:?}");
    assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
}

#[test]
fn audio_filenames_carry_a_format_servers_understand() {
    assert_eq!(
        audio_filename("voice/file_12.oga", None, Some("audio/ogg")),
        "audio.ogg"
    );
    assert_eq!(
        audio_filename("music/file_3", Some("note.m4a"), None),
        "audio.m4a"
    );
    assert_eq!(
        audio_filename("music/file_4", None, Some("audio/mpeg")),
        "audio.mp3"
    );
    assert_eq!(audio_filename("x", None, None), "audio.ogg");
}

/// Issue #41 : « réponds-moi en vocal » appelle `send_voice` une fois ; le vocal part en
/// OGG/Opus, avec sa durée, dans le même fil.
#[tokio::test]
async fn a_spoken_answer_arrives_as_a_voice_note() {
    if penelope_platform::audio::ffmpeg().is_none() {
        eprintln!("ffmpeg absent : envoi vocal non testé");
        return;
    }
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    p.reply(r#"{"complexity":"low"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "v1".into(),
            name: "send_voice".into(),
            arguments: json!({"text": "## Veille\n\n**Mistral** sort Voxtral, 8 % plus rapide. 🎙️ Bonne journée !"}),
        }],
    ));
    p.reply("C'est parti en vocal.");
    g.process_update(&updates::text_message(
        160,
        OWNER,
        OWNER,
        "Réponds-moi en vocal : la veille du jour",
    ))
    .await
    .unwrap();
    drain(&g).await;

    let spoken = p.spoken.lock().unwrap().clone();
    assert_eq!(spoken.len(), 1, "une synthèse");
    assert_eq!(
        spoken[0],
        (
            "Veille. Mistral sort Voxtral, 8 pour cent plus rapide. Bonne journée !".to_string(),
            "fr_female".to_string()
        )
    );
    let voices = t.calls_to(tg::SEND_VOICE).await;
    assert_eq!(voices.len(), 1, "{voices:?}");
    let v = &voices[0];
    assert_eq!(v["chat_id"], OWNER.to_string());
    assert!(v["duration"].as_str().unwrap().parse::<u32>().unwrap() >= 1);
    assert!(
        v["reply_parameters"]
            .as_str()
            .unwrap()
            .contains("message_id")
    );
    let name = v["voice"].as_str().unwrap();
    assert!(name.ends_with(".ogg"), "{name}");
    let file = g
        .daemon
        .services
        .platform
        .dirs
        .data()
        .join("media/voice")
        .join(name);
    let bytes = std::fs::read(&file).unwrap();
    assert!(
        bytes.starts_with(b"OggS") && bytes.len() > 100,
        "OGG/Opus non vide"
    );
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|m| m == "C'est parti en vocal."),
        "{sent:?}"
    );
}

/// Issue #41 : synthèse impossible, la réponse part en texte avec la raison.
#[tokio::test]
async fn a_failed_synthesis_falls_back_to_text() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();
    p.set_speech_error(Some("serveur mlx-audio arrêté"));
    p.reply(r#"{"complexity":"low"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "v1".into(),
            name: "send_voice".into(),
            arguments: json!({"text": "Trois tickets à traiter ce matin."}),
        }],
    ));
    p.reply("Je te l'ai écrit.");
    g.process_update(&updates::text_message(
        170,
        OWNER,
        OWNER,
        "lis-moi mes tickets",
    ))
    .await
    .unwrap();
    drain(&g).await;
    assert!(t.calls_to(tg::SEND_VOICE).await.is_empty());
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|m| m.contains("Trois tickets à traiter ce matin.")
                && m.contains("vocal indisponible : serveur mlx-audio arrêté")),
        "{sent:?}"
    );
}
