//! Crédits épuisés (#339) : jamais relancés en boucle, rangés pour la session, dits comme
//! une pause avec le retour prévu.

use super::*;

/// Quota Codex épuisé en plein tour de conversation : un seul appel, pas de relance ;
/// l'échec commence par le préfixe de pause et l'arrêt est rangé avec l'heure de retour.
#[tokio::test(start_paused = true)]
async fn a_spent_quota_stops_the_turn_once_and_says_when() {
    let (_d, s, p) = setup().await;
    p.push(Scripted::UsageLimit(Some(5_400)));
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    let TurnOutcome::Failed { error } = out else {
        panic!("le tour devait s'arrêter : {out:?}");
    };
    assert!(error.starts_with(CREDITS_EXHAUSTED), "{error}");
    assert!(error.contains("crédits Codex épuisés"), "{error}");
    assert!(error.contains("Retour prévu à"), "{error}");
    assert_eq!(p.call_count(), 1, "une limite d'usage n'est pas relancée");
    let stop = penelope_app::credits::take(&s.store, &sid).await.unwrap();
    assert_eq!(stop.provider, "codex");
    assert_eq!(stop.until_ms, Some(s.clock.now_ms() + 5_400_000));
}

/// Crédit OpenRouter épuisé (402) : arrêt immédiat, sans relance, reprise à la recharge.
#[tokio::test(start_paused = true)]
async fn an_openrouter_402_stops_the_turn() {
    let (_d, s, p) = setup().await;
    p.named("openrouter");
    p.push(Scripted::Error(
        LlmErrorKind::PaymentRequired,
        "Insufficient credits".into(),
    ));
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    let TurnOutcome::Failed { error } = out else {
        panic!("le tour devait s'arrêter : {out:?}");
    };
    assert!(error.starts_with(CREDITS_EXHAUSTED), "{error}");
    assert!(error.contains("à la recharge des crédits"), "{error}");
    assert_eq!(p.call_count(), 1);
    let stop = penelope_app::credits::take(&s.store, &sid).await.unwrap();
    assert_eq!(
        (stop.provider.as_str(), stop.until_ms),
        ("openrouter", None)
    );
}

/// Un repli configuré reste prioritaire : le quota épuisé passe au repli, sans arrêt.
#[tokio::test(start_paused = true)]
async fn a_configured_fallback_takes_over_a_spent_quota() {
    let (_d, s, p) = setup().await;
    p.push(Scripted::UsageLimit(None));
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
    assert_eq!(
        models,
        vec!["mock/model", "mock/repli"],
        "pas d'attente sur le quota"
    );
    assert_eq!(penelope_app::credits::take(&s.store, &sid).await, None);
}
