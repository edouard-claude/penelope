//! #467 : qui a le droit de finir un tour sans message.

use super::*;

/// La session du tir d'une planification à cible `prompt`, livrable donné ou non.
async fn fired(
    d: &Harness,
    clock: &TestClock,
    livrable: Option<&str>,
) -> penelope_kernel::session::Session {
    let s = &d.services;
    let mut target = json!({"type": "prompt", "prompt": "Regarde, et ne dis rien sans nouveauté"});
    if let Some(l) = livrable {
        target["livrable"] = json!(l);
    }
    let sched = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            target,
            json!({}),
        )
        .await
        .unwrap();
    clock.advance_ms(3_600_500);
    tick(d, &d.scheduler()).await.unwrap();
    let turn = s.turns.claim("t").await.unwrap().expect("tour");
    s.turns.complete(&turn).await.unwrap();
    s.schedules.set_state(&sched.id, "paused").await.unwrap();
    s.sessions.get(&turn.session_id).await.unwrap().unwrap()
}

/// Une exécution planifiée peut se taire, sauf si sa planification promet un message.
#[tokio::test]
async fn a_scheduled_run_may_end_silent_unless_it_owes_a_message() {
    let (d, clock, _rec) = harness().await;
    let s = &d.services;
    for (livrable, expected) in [
        (None, true),
        (Some("fichier:veille/etat.json"), true),
        (Some("run"), true),
        (Some("message"), false),
        (Some(" message "), false),
    ] {
        let session = fired(&d, &clock, livrable).await;
        assert_eq!(session.kind, SessionKind::Scheduled);
        assert_eq!(
            may_end_silent(s, &session).await,
            expected,
            "livrable {livrable:?}"
        );
    }
}

/// Dans une conversation, quelqu'un attend : la réponse vide y reste un raté, relancé.
#[tokio::test]
async fn a_conversation_never_ends_silent() {
    let (d, _clock, _rec) = harness().await;
    let s = &d.services;
    let session = s.sessions.create(SessionKind::Chat, None).await.unwrap();
    assert!(!may_end_silent(s, &session).await);
}
