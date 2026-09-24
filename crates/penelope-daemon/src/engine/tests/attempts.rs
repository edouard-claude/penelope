//! Tentatives hors surface (issue #206, épopée #208, T9), vues d'un tour de la file :
//! sorties de `agent/tests/attempts.rs` (T09), elles ont besoin du daemon.

use super::recovery::setup;
use super::*;
use crate::agent::EMPTY_RETRY_PROMPT;
use crate::runtime::Services;
use penelope_context::derive::{Sealed, derive, derive_until};

/// Un daemon de test, sa session de chat et son fournisseur simulé.
async fn chat() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>, String) {
    let (dir, s, p) = setup().await;
    let d = Arc::new(Daemon::from_services(s));
    d.set_provider_override(p.clone());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    (dir, d, p, sid)
}

/// Un tour de la file, de bout en bout.
async fn turn(d: &Arc<Daemon>, sid: &str, text: &str) -> TurnOutcome {
    d.enqueue_message(sid, text, &Origin::Cli, None)
        .await
        .unwrap();
    let t = d.services.turns.claim("test").await.unwrap().unwrap();
    let out = d.run_turn(&t).await;
    d.services.turns.complete(&t).await.unwrap();
    out
}

async fn attempts_of(s: &Services, sid: &str) -> Vec<penelope_kernel::event::Event> {
    s.events
        .session_events(sid, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "conv.attempt")
        .collect()
}

/// Flux coupé après 200 caractères : le tour échoue en citant le début, le partiel est
/// une tentative, et la requête du tour suivant est la même avec ou sans elle.
#[tokio::test]
async fn a_stream_cut_after_text_keeps_the_partial_out_of_the_history() {
    let (_dir, d, p, sid) = chat().await;
    let partial = "Le plan de migration tient en trois étapes. ".repeat(6);
    assert!(partial.chars().count() > 200);
    p.push(Scripted::MidStreamError(
        partial.clone(),
        "connexion perdue".into(),
    ));
    match turn(&d, &sid, "explique-moi le plan de migration").await {
        TurnOutcome::Failed { error } => {
            assert!(error.contains("coupée en cours d'écriture"), "{error}");
            assert!(
                error.contains("« Le plan de migration tient en trois étapes."),
                "le message cite le début gardé : {error}"
            );
        }
        other => panic!("{other:?}"),
    }
    let s = &d.services;
    let attempts = attempts_of(s, &sid).await;
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    let a = &attempts[0].payload;
    assert_eq!(a["cause"], "stream_cut");
    assert_eq!(a["partial_text"], partial.as_str());
    assert!(a["model"].is_string());
    assert!(a["llm_request_id"].is_string());
    assert!(a["turn"].is_string());
    assert!(
        a.get("surface").is_none(),
        "une tentative n'a pas de surface"
    );

    p.reply("Voici la suite.");
    assert!(matches!(
        turn(&d, &sid, "et donc ?").await,
        TurnOutcome::Answered { .. }
    ));
    let next = p.requests().pop().unwrap();
    assert!(
        next.messages
            .iter()
            .all(|m| !m.text().contains("trois étapes")),
        "le partiel ne repart pas au modèle"
    );
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let without: Vec<_> = events
        .iter()
        .filter(|e| e.kind != "conv.attempt")
        .cloned()
        .collect();
    assert_eq!(
        derive(&Sealed::none(), &events)
            .unwrap()
            .request_messages("système"),
        derive(&Sealed::none(), &without)
            .unwrap()
            .request_messages("système"),
    );

    // La purge de la session emporte le partiel (#46, #78).
    s.events.purge_session(&sid, "test").await.unwrap();
    let purged = attempts_of(s, &sid).await;
    assert!(
        purged
            .iter()
            .all(|e| e.payload.get("partial_text").is_none())
    );
}

/// La consigne de relance n'est pas dans l'historique : elle est dans la tentative, et
/// la requête envoyée est celle que le journal plié redonne.
#[tokio::test]
async fn the_retry_prompt_sent_is_the_one_the_journal_derives() {
    let (_dir, d, p, sid) = chat().await;
    p.reply("");
    p.reply("Bonjour !");
    assert!(matches!(
        turn(&d, &sid, "salut").await,
        TurnOutcome::Answered { .. }
    ));
    let s = &d.services;
    let attempts = attempts_of(s, &sid).await;
    assert_eq!(attempts.len(), 1);
    let a = &attempts[0];
    assert_eq!(a.payload["cause"], "empty_answer");
    assert_eq!(a.payload["retry_prompt"], EMPTY_RETRY_PROMPT);
    assert!(a.payload["usage"].is_object());
    let history = s.context.history.load(&sid, 0).await.unwrap();
    assert!(
        history
            .iter()
            .all(|e| !e.message.text().contains("Relance automatique")),
        "la consigne n'est pas dans l'historique"
    );

    // Phase 1 : la boucle ajoute la consigne à la main ; le pliage l'ajoute de lui-même.
    let requests = p.requests();
    let sent = &requests[requests.len() - 1].messages;
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let derived = derive_until(&Sealed::none(), &events, a.seq)
        .unwrap()
        .request_messages("");
    assert_eq!(sent.last(), derived.last());
    assert_eq!(sent.last().unwrap().text(), EMPTY_RETRY_PROMPT);
    assert_eq!(sent, &derived, "requête envoyée = journal plié");
}
