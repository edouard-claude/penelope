use super::*;

#[tokio::test(start_paused = true)]
async fn a_transient_failure_falls_back_to_the_next_model() {
    let (_d, s, p) = setup().await;
    p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    p.reply("réponse du repli");
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let mut sp = spec(&sid);
    sp.fallback_models = vec!["mock/repli".into()];
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let models: Vec<String> = p.requests().iter().map(|r| r.model.clone()).collect();
    // Une attente, puis le repli : tant qu'un autre modèle reste, on n'insiste pas
    // sur celui qui vient d'échouer (issue #50).
    assert_eq!(models, vec!["mock/model", "mock/model", "mock/repli"]);
}

/// #50 : avec OpenRouter, une erreur transitoire d'avant flux est réessayée au lieu
/// de faire échouer le tour, et le nouvel essai est tracé.
#[tokio::test(start_paused = true)]
async fn a_transient_error_before_the_stream_is_retried_with_openrouter() {
    let (_d, s, p) = setup().await;
    p.named("openrouter");
    p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    p.reply("réponse après nouvel essai");
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let models: Vec<String> = p.requests().iter().map(|r| r.model.clone()).collect();
    assert_eq!(models, vec!["mock/model", "mock/model"], "même modèle");
    let events = s.events.range(0, 200).await.unwrap();
    assert!(
        events.iter().any(|e| e.kind == "llm.retried"),
        "le nouvel essai doit être tracé"
    );
}

/// #50 : après les tentatives, l'échec dit combien de fois on a essayé.
#[tokio::test(start_paused = true)]
async fn repeated_connection_timeouts_say_how_many_attempts_were_made() {
    let (_d, s, p) = setup().await;
    p.named("openrouter");
    for _ in 0..4 {
        p.push(Scripted::Error(
            LlmErrorKind::Transient,
            "délai de connexion".into(),
        ));
    }
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    match out {
        TurnOutcome::Failed { error } => {
            assert!(error.contains("4 tentatives"), "{error}");
            assert!(error.contains("7 s"), "{error}");
        }
        other => panic!("le tour devait échouer : {other:?}"),
    }
    assert_eq!(p.call_count(), 4, "trois nouvelles tentatives, pas plus");
}

/// #50 : `/stop` pendant l'attente arrête le tour sans rappeler le modèle.
#[tokio::test(start_paused = true)]
async fn a_stop_during_the_retry_wait_ends_the_turn() {
    let (_d, s, p) = setup().await;
    p.named("openrouter");
    p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    p.reply("ne devrait jamais partir");
    let sid = session(&s).await;
    let sp = spec(&sid);
    // L'arrêt arrive pendant l'attente, juste après le premier appel refusé.
    let cancel = sp.cancel.clone();
    let watcher = p.clone();
    tokio::spawn(async move {
        loop {
            if watcher.call_count() >= 1 {
                cancel.cancel();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Cancelled | TurnOutcome::Failed { .. }),
        "{out:?}"
    );
    assert_eq!(p.call_count(), 1, "aucun nouvel appel après l'arrêt");
}

/// Issue #5 : une erreur arrivée pendant le flux, avant tout texte, est rejouée puis
/// passe au modèle de repli ; après du texte, elle est dite telle quelle.
#[tokio::test]
async fn a_stream_cut_before_any_text_is_retried_then_falls_back() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = vec!["mock/repli".into()];

    p.push(Scripted::MidStreamError(
        String::new(),
        "Too many requests".into(),
    ));
    p.push(Scripted::MidStreamError(
        String::new(),
        "Too many requests".into(),
    ));
    p.reply("réponse du repli");
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Answered { ref text, .. } if text == "réponse du repli"),
        "{out:?}"
    );
    let models: Vec<String> = p.requests().iter().map(|r| r.model.clone()).collect();
    assert_eq!(models, vec!["mock/model", "mock/model", "mock/repli"]);

    // Du texte est déjà parti : pas de relance silencieuse, l'échec le dit.
    p.push(Scripted::MidStreamError(
        "Voici le début".into(),
        "Too many requests".into(),
    ));
    let conv = MemoryConversation::new("Tu es Pénélope.", "encore");
    let before = p.requests().len();
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    match out {
        TurnOutcome::Failed { error } => {
            assert!(error.contains("coupée en cours d'écriture"), "{error}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(p.requests().len(), before + 1, "un seul appel");
}

#[tokio::test]
async fn an_empty_answer_is_retried_once_then_reported() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;

    // Vide puis correcte : la relance suffit, rien de vide dans le transcript.
    p.reply("");
    p.reply("Bonjour !");
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Answered { ref text, .. } if text == "Bonjour !"),
        "{out:?}"
    );
    assert_eq!(
        conv.messages().len(),
        2,
        "la réponse vide n'est pas enregistrée"
    );
    let last_request = p.requests().last().unwrap().clone();
    assert!(
        last_request
            .messages
            .last()
            .unwrap()
            .text()
            .contains("Relance automatique")
    );

    // Vide deux fois : erreur explicite, qui nomme le modèle.
    p.reply("");
    p.reply("   ");
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    match out {
        TurnOutcome::Failed { error } => {
            assert!(error.contains("aucun texte"), "{error}");
            assert!(error.contains("mock/model"), "{error}");
        }
        other => panic!("{other:?}"),
    }
}
