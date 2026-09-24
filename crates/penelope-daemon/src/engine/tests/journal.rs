//! Épopée #208, T5 : un tour réel écrit chaque message en double, table et journal.

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
