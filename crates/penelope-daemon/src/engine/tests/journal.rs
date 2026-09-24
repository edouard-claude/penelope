//! Épopée #208, T5 et T6 : un tour réel écrit chaque message en double, table et
//! journal, et le préfixe une fois par changement.

use super::*;
use penelope_context::derive::{Sealed, derive};

/// Après un tour avec un appel d'outil, chaque ligne de `messages` cite un événement
/// dont le payload redonne le même message, et le journal plié redonne l'historique.
#[tokio::test]
async fn every_row_of_a_turn_has_an_event_that_gives_it_back() {
    let (_dir, d, out, sid) = tool_turn(vec![("fs_read", json!({"path": "absent.rs"}))]).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let s = &d.services;
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let loaded = s.context.history.load(&sid, 0).await.unwrap();
    let ids: Vec<Option<i64>> = s
        .store
        .read({
            let sid = sid.clone();
            move |c| {
                let mut st =
                    c.prepare("SELECT event_id FROM messages WHERE session_id=?1 ORDER BY seq")?;
                let rows = st.query_map([sid], |r| r.get(0))?;
                Ok(rows.collect::<penelope_store::rusqlite::Result<Vec<_>>>()?)
            }
        })
        .await
        .unwrap();
    assert_eq!(ids.len(), loaded.len());
    assert!(loaded.len() >= 4, "user, appel, résultat, réponse");
    for (id, entry) in ids.iter().zip(&loaded) {
        let ev = events
            .iter()
            .find(|e| Some(e.id) == *id)
            .unwrap_or_else(|| panic!("ligne {} sans événement", entry.seq));
        let back = derive(&Sealed::none(), std::slice::from_ref(ev))
            .unwrap()
            .entries();
        assert_eq!(back[0].message, entry.message, "{}", ev.kind);
    }
    let surface = derive(&Sealed::none(), &events).unwrap();
    let derived: Vec<_> = surface.entries().into_iter().map(|e| e.message).collect();
    let tables: Vec<_> = loaded.into_iter().map(|e| e.message).collect();
    assert_eq!(derived, tables);

    // L'appel au modèle est nommé dans la réponse qui en sort.
    let answer = events.iter().rfind(|e| e.kind == "conv.assistant").unwrap();
    assert_eq!(answer.payload["step"], 2);
    assert!(answer.payload["request_hash"].is_string());
    assert!(answer.payload["turn"].is_string());
    let user = events.iter().find(|e| e.kind == "conv.user").unwrap();
    assert_eq!(user.payload["source"], "owner");
    assert!(user.payload["turn_message_id"].is_string());
}

/// T6 : deux tours sans changement de préfixe n'écrivent qu'un `conv.system`, qui porte
/// le texte entier ; le contexte figé de chaque message l'est aussi.
#[tokio::test]
async fn two_turns_journal_one_prefix_and_their_frozen_contexts() {
    let (_dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    for text in ["bonjour", "et ensuite ?"] {
        p.reply("Réponse.");
        d.enqueue_message(&sid, text, &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        assert!(matches!(
            d.run_turn(&turn).await,
            TurnOutcome::Answered { .. }
        ));
        d.services.turns.complete(&turn).await.unwrap();
    }
    let s = &d.services;
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let systems: Vec<_> = events.iter().filter(|e| e.kind == "conv.system").collect();
    assert_eq!(systems.len(), 1, "{:?}", systems);
    assert_eq!(systems[0].payload["reason"], "first");
    let request = p.requests().pop().unwrap();
    assert_eq!(
        systems[0].payload["rendered"].as_str().unwrap(),
        request.messages[0].text(),
        "le journal porte le préfixe envoyé"
    );
    let contexts = s.context.history.contexts(&sid).await.unwrap();
    let journaled = events.iter().filter(|e| e.kind == "conv.context").count();
    assert_eq!(journaled, contexts.len());
    let surface = derive(&Sealed::none(), &events).unwrap();
    assert_eq!(surface.contexts.len(), contexts.len());
}
