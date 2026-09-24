//! Tentatives hors surface (issue #206, épopée #208, T9) : ce qu'un appel a coûté sans
//! répondre est dans le journal, jamais dans l'historique ni dans la requête suivante.

use super::*;

async fn attempts_of(s: &Services, sid: &str) -> Vec<penelope_kernel::event::Event> {
    s.events
        .session_events(sid, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "conv.attempt")
        .collect()
}

/// Trois échecs d'avant flux, dont un repli : trois tentatives, une seule ligne d'usage.
#[tokio::test(start_paused = true)]
async fn failures_before_the_stream_leave_one_attempt_each_and_one_usage_line() {
    let (_d, s, p) = setup().await;
    for _ in 0..3 {
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    }
    p.reply("réponse du repli");
    let sid = session(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = vec!["mock/repli".into()];
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let attempts = attempts_of(&s, &sid).await;
    let seen: Vec<(String, String)> = attempts
        .iter()
        .map(|e| {
            (
                e.payload["cause"].as_str().unwrap().to_string(),
                e.payload["model"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            ("before_stream".into(), "mock/model".into()),
            ("fallback".into(), "mock/model".into()),
            ("before_stream".into(), "mock/repli".into()),
        ]
    );
    assert!(
        attempts
            .iter()
            .all(|e| e.payload["error"].as_str().unwrap().contains("503"))
    );
    let (calls, _) = s.budget.turn_totals("t_test").await.unwrap();
    assert_eq!(calls, 1, "seule la réponse est comptée");
}

/// Une boucle de replis ne remplit pas la base : dix tentatives gardées par tour.
#[tokio::test(start_paused = true)]
async fn a_turn_keeps_at_most_ten_attempts() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let mut sp = spec(&sid);
    sp.fallback_models = (0..8).map(|i| format!("mock/repli{i}")).collect();
    for _ in 0..16 {
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
    }
    p.reply("enfin");
    let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&sp, &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(p.call_count(), 17);
    assert_eq!(
        attempts_of(&s, &sid).await.len(),
        MAX_ATTEMPTS_PER_TURN as usize
    );
}
