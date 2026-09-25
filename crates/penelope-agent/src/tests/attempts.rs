//! Tentatives hors surface (issue #206, épopée #208, T9, T15) : ce qu'un appel a coûté
//! sans répondre passe par le port `AttemptSink`, jamais par l'historique ni la requête
//! suivante. Ici, l'implémentation en mémoire.

use super::*;
use penelope_app::attempts::{Attempt, AttemptCause, MemoryAttempts};

/// Des services de test dont les tentatives restent lisibles.
async fn setup_attempts() -> (
    tempfile::TempDir,
    Arc<AgentServices>,
    Arc<MockProvider>,
    Arc<MemoryAttempts>,
) {
    let (dir, s, p) = setup().await;
    let mem = Arc::new(MemoryAttempts::default());
    let s = Arc::new(AgentServices {
        attempts: mem.clone(),
        ..(*s).clone()
    });
    (dir, s, p, mem)
}

async fn run(s: &Arc<AgentServices>, p: &Arc<MockProvider>, sp: &TurnSpec) -> TurnOutcome {
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    run_on(s, p, sp, &conv).await
}

async fn run_on(
    s: &Arc<AgentServices>,
    p: &Arc<MockProvider>,
    sp: &TurnSpec,
    conv: &MemoryConversation,
) -> TurnOutcome {
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(sp, conv, &exec(false), &NullSink)
        .await
        .unwrap()
}

/// Trois échecs d'avant flux, dont un repli : trois tentatives, une seule ligne d'usage.
#[tokio::test(start_paused = true)]
async fn failures_before_the_stream_leave_one_attempt_each_and_one_usage_line() {
    let (_d, s, p, mem) = setup_attempts().await;
    for _ in 0..3 {
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    }
    p.reply("réponse du repli");
    let sid = session(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = vec!["mock/repli".into()];
    let out = run(&s, &p, &sp).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let attempts = mem.of_session(&sid);
    let seen: Vec<(AttemptCause, String)> = attempts
        .iter()
        .map(|a| (a.cause, a.model.clone().unwrap()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (AttemptCause::BeforeStream, "mock/model".into()),
            (AttemptCause::Fallback, "mock/model".into()),
            (AttemptCause::BeforeStream, "mock/repli".into()),
        ]
    );
    assert!(
        attempts
            .iter()
            .all(|a| a.error.as_deref().unwrap().contains("503"))
    );
    // Chaque tentative est épinglée à sa propre requête.
    let ids: BTreeSet<_> = attempts.iter().map(|a| a.llm_request_id.clone()).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.iter().all(Option::is_some));
    let (calls, _) = s.budget.turn_totals("t_test").await.unwrap();
    assert_eq!(calls, 1, "seule la réponse est comptée");
    assert!(
        attempts_in_journal(&s, &sid).await == 0,
        "la boucle n'écrit que par le port"
    );
}

/// Une boucle de replis ne remplit pas la base : dix tentatives gardées par tour.
#[tokio::test(start_paused = true)]
async fn a_turn_keeps_at_most_ten_attempts() {
    let (_d, s, p, mem) = setup_attempts().await;
    let sid = session(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = (0..8).map(|i| format!("mock/repli{i}")).collect();
    for _ in 0..16 {
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    }
    p.reply("enfin");
    let out = run(&s, &p, &sp).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(p.call_count(), 17);
    assert_eq!(mem.of_session(&sid).len(), MAX_ATTEMPTS_PER_TURN as usize);
}

/// Flux coupé après 200 caractères : une tentative, qui porte le texte reçu.
#[tokio::test(start_paused = true)]
async fn a_stream_cut_after_text_leaves_one_attempt_with_the_text() {
    let (_d, s, p, mem) = setup_attempts().await;
    let partial = "Le plan de migration tient en trois étapes. ".repeat(6);
    assert!(partial.chars().count() > 200);
    p.push(Scripted::MidStreamError(
        partial.clone(),
        "connexion perdue".into(),
    ));
    let sid = session(&s).await;
    let out = run(&s, &p, &spec(&sid)).await;
    assert!(matches!(out, TurnOutcome::Failed { .. }), "{out:?}");
    let attempts = mem.of_session(&sid);
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    let a = &attempts[0];
    assert_eq!(a.cause, AttemptCause::StreamCut);
    assert_eq!(a.partial_text.as_deref(), Some(partial.as_str()));
    assert_eq!(a.turn.as_deref(), Some("t_test"));
    assert_eq!(a.step, 1);
    assert!(a.llm_request_id.is_some());
}

/// Réponse vide relancée : une tentative qui porte la consigne de la requête suivante
/// (l'injection « request only ») et la requête de l'appel vide.
#[tokio::test]
async fn an_empty_answer_leaves_an_attempt_with_the_retry_prompt_and_its_request() {
    let (_d, s, p, mem) = setup_attempts().await;
    p.reply("");
    p.reply("Bonjour !");
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = run_on(&s, &p, &spec(&sid), &conv).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let attempts = mem.of_session(&sid);
    assert_eq!(attempts.len(), 1);
    let a: &Attempt = &attempts[0];
    assert_eq!(a.cause, AttemptCause::EmptyAnswer);
    assert_eq!(a.retry_prompt.as_deref(), Some(EMPTY_RETRY_PROMPT));
    assert!(a.usage.is_some() && a.cost_usd.is_some());
    assert!(
        a.llm_request_id.is_some(),
        "une réponse vide est épinglée à sa requête (#206, T15)"
    );
    // La consigne part avec la relance, et seulement là.
    let requests = p.requests();
    assert_eq!(
        requests[1].messages.last().unwrap().text(),
        EMPTY_RETRY_PROMPT
    );
    assert!(
        conv.request_messages()
            .await
            .unwrap()
            .iter()
            .all(|m| m.text() != EMPTY_RETRY_PROMPT),
        "la consigne n'entre pas dans le transcript"
    );
}

/// Ce que la conversation redonne au modèle ne dépend pas des tentatives.
#[tokio::test(start_paused = true)]
async fn the_request_messages_are_the_same_with_or_without_attempts() {
    let (_d, s, p, mem) = setup_attempts().await;
    for _ in 0..3 {
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    }
    p.reply("réponse");
    p.reply("réponse");
    let (with, without) = (session(&s).await, session(&s).await);
    let mut sp = spec(&with);
    sp.fallback_models = vec!["mock/repli".into()];
    let conv_with = MemoryConversation::new("Tu es Pénélope.", "salut");
    run_on(&s, &p, &sp, &conv_with).await;
    let conv_without = MemoryConversation::new("Tu es Pénélope.", "salut");
    run_on(&s, &p, &spec(&without), &conv_without).await;
    assert_eq!(mem.of_session(&with).len(), 3);
    assert!(mem.of_session(&without).is_empty());
    assert_eq!(
        conv_with.request_messages().await.unwrap(),
        conv_without.request_messages().await.unwrap()
    );
}

async fn attempts_in_journal(s: &AgentServices, sid: &str) -> usize {
    s.events
        .session_events(sid, 0)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.kind == "conv.attempt")
        .count()
}
