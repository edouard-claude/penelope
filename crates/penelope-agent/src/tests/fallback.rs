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
    for _ in 0..5 {
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
            assert!(error.contains("5 tentatives"), "{error}");
            assert!(error.contains("15 s"), "{error}");
        }
        other => panic!("le tour devait échouer : {other:?}"),
    }
    assert_eq!(p.call_count(), 5, "quatre nouvelles tentatives, pas plus");
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

/// Issue #5 : une erreur arrivée pendant le flux, avant tout texte, est rejouée jusqu'au
/// budget (#313) puis passe au modèle de repli ; après du texte, elle est dite telle quelle.
#[tokio::test(start_paused = true)]
async fn a_stream_cut_before_any_text_is_retried_then_falls_back() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = vec!["mock/repli".into()];

    let budget = default_budget(&s).await;
    for _ in 0..=budget {
        p.push(Scripted::MidStreamError(
            String::new(),
            "Too many requests".into(),
        ));
    }
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
    let mut expected = vec!["mock/model".to_string(); budget + 1];
    expected.push("mock/repli".into());
    assert_eq!(models, expected);

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

/// Codex rend « servers are currently overloaded » dans le flux, sans code connu (#311).
fn overloaded() -> Scripted {
    Scripted::CodexEvents(vec![
        json!({"type": "response.created", "response": {"id": "r1"}}).to_string(),
        json!({"type": "response.failed", "response": {"error": {
            "message": "Our servers are currently overloaded. Please try again later."}}})
        .to_string(),
    ])
}

/// Budget de relances par défaut : au moins quatre, comme demandé au #313.
async fn default_budget(s: &AgentServices) -> usize {
    let n = s.config.config().providers.openrouter.request_retries as usize;
    assert!(n >= 4, "budget par défaut : {n}");
    n
}

/// #313 : avant tout texte, une surcharge relance le **même** modèle, avec une attente
/// croissante ; trois coupures puis une réponse : pas de repli.
#[tokio::test(start_paused = true)]
async fn a_codex_overload_retries_the_same_model() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = vec!["mock/repli".into()];
    for _ in 0..3 {
        p.push(overloaded());
    }
    p.reply("réponse du modèle principal");
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Answered { ref text, .. } if text == "réponse du modèle principal"),
        "{out:?}"
    );
    let models: Vec<String> = p.requests().iter().map(|r| r.model.clone()).collect();
    assert_eq!(models, vec!["mock/model"; 4]);
}

/// #313 : au-delà du budget et sans repli, le tour échoue en disant combien de fois il
/// a essayé et combien il a attendu.
#[tokio::test(start_paused = true)]
async fn a_codex_overload_beyond_the_budget_gives_up_clearly() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let budget = default_budget(&s).await;
    for _ in 0..=budget {
        p.push(overloaded());
    }
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    match out {
        TurnOutcome::Failed { error } => {
            assert!(
                error.contains(&format!("{} tentatives", budget + 1)),
                "{error}"
            );
            assert!(error.contains("s d'attente"), "{error}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(p.requests().len(), budget + 1);
}

/// #311, #313 : avec un repli, la bascule n'a lieu qu'une fois le budget épuisé.
#[tokio::test(start_paused = true)]
async fn a_codex_overload_falls_back_after_the_budget() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let budget = default_budget(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = vec!["mock/repli".into()];
    for _ in 0..=budget {
        p.push(overloaded());
    }
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
    let mut expected = vec!["mock/model".to_string(); budget + 1];
    expected.push("mock/repli".into());
    assert_eq!(models, expected);
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
