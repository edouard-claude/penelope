//! Fin sans message d'un tour que personne n'attend (#467).

use super::*;

fn work() -> Scripted {
    Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path": "etat.json"}))],
    )
}

async fn empty_events(s: &AgentServices, sid: &str) -> Vec<Value> {
    s.events
        .session_events(sid, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "turn.empty_answer")
        .map(|e| e.payload)
        .collect()
}

/// Le cas relevé en production : un prompt planifié dit « n'envoie rien s'il n'y a rien
/// de neuf », le modèle regarde, puis rend une réponse vide. C'est la fin du tour : pas de
/// second appel, pas de « réponds maintenant, en texte », rien à livrer.
#[tokio::test]
async fn a_turn_nobody_waits_for_ends_without_a_message_after_its_work() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(work());
    p.reply("");
    let conv =
        MemoryConversation::new("Tu es Pénélope.", "Regarde ; n'envoie rien sans nouveauté.");
    let e = exec(false);
    let out = AgentLoop::new(s.clone(), p.clone())
        .may_end_silent(true)
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    match out {
        TurnOutcome::Answered {
            text, iterations, ..
        } => {
            assert_eq!(text, "");
            assert_eq!(iterations, 2);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(p.call_count(), 2, "aucune relance");
    assert!(p.requests().iter().all(|r| {
        !r.messages
            .last()
            .unwrap()
            .text()
            .contains("Relance automatique")
    }));
    // Le travail reste dans le transcript, la réponse vide n'y entre pas.
    assert_eq!(conv.messages().len(), 3, "{:?}", conv.messages());
    let events = empty_events(&s, &sid).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["silent"], true);
    assert_eq!(events[0]["retried"], false);
}

/// Une conversation attend toujours sa réponse : sans le drapeau, la même suite est
/// relancée comme avant.
#[tokio::test]
async fn a_conversation_still_gets_its_retry() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(work());
    p.reply("");
    p.reply("Rien de neuf.");
    let conv = MemoryConversation::new("Tu es Pénélope.", "regarde");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Answered { ref text, .. } if text == "Rien de neuf."),
        "{out:?}"
    );
    assert_eq!(p.call_count(), 3);
    let events = empty_events(&s, &sid).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["silent"], false);
}

/// Se taire sans avoir rien regardé n'est pas une décision, c'est un raté : au premier
/// appel, la réponse vide est relancée même quand personne n'attend le tour.
#[tokio::test]
async fn an_empty_first_answer_is_a_miss_even_when_nobody_waits() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.reply("");
    p.reply("Voici le point du matin.");
    let conv = MemoryConversation::new("Tu es Pénélope.", "Fais le point du matin.");
    let out = AgentLoop::new(s.clone(), p.clone())
        .may_end_silent(true)
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Answered { ref text, .. } if text.starts_with("Voici")),
        "{out:?}"
    );
    assert!(
        p.requests()[1]
            .messages
            .last()
            .unwrap()
            .text()
            .contains("Relance automatique")
    );
}

/// Une sortie mangée par le raisonnement n'est pas un silence voulu : l'erreur qui le dit
/// reste, travail fait ou non.
#[tokio::test]
async fn an_answer_cut_by_its_ceiling_is_not_a_silence() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(work());
    p.push(Scripted::ReasonedOnly {
        completion: 4096,
        reasoning: 4096,
    });
    let conv = MemoryConversation::new("Tu es Pénélope.", "regarde");
    let out = AgentLoop::new(s.clone(), p.clone())
        .may_end_silent(true)
        .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Failed { .. }), "{out:?}");
    assert_eq!(empty_events(&s, &sid).await[0]["silent"], false);
}
