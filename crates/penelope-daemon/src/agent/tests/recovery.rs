//! T20 (épopée #208) : reprise après crash. Un tour ouvert au moment de l'arrêt est
//! fermé `interrupted` au démarrage ; le tour rejoué ouvre sa propre borne, tentative
//! suivante, et retrouve l'appel resté sans résultat.

use super::*;
use crate::bus::Origin;
use crate::runtime::Daemon;
use penelope_context::journal::{Provenance, TurnIdentity, UserSource, started_payload};

#[tokio::test]
async fn a_turn_open_at_the_crash_is_closed_as_interrupted_then_replayed() {
    let (_dir, s, p) = setup().await;
    let d = Arc::new(Daemon::from_services(s.clone()));
    d.set_provider_override(p.clone());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    d.enqueue_message(&sid, "quelle heure est-il ?", &Origin::Cli, None)
        .await
        .unwrap();

    // Le tour a ouvert sa borne, écrit le message et la réponse qui appelle un outil ;
    // le processus meurt avant le résultat.
    let t = s.turns.claim("runner-0").await.unwrap().unwrap();
    let id = TurnIdentity {
        turn_id: t.id.to_string(),
        kind: "message".into(),
        attempt: t.attempts,
    };
    s.events
        .append(
            EventDraft::new(
                "turn.started",
                started_payload(None, Some(t.id.as_str()), Some(&id)),
            )
            .session(&sid),
        )
        .await
        .unwrap();
    let history = &s.context.history;
    let prov = Provenance::queued(UserSource::Owner, t.id.as_str(), &t.enqueued_at);
    history
        .append_queued(
            &sid,
            &ChatMessage::user("quelle heure est-il ?"),
            5,
            0,
            &prov,
        )
        .await
        .unwrap();
    s.kv_set(&format!("turn.recorded.{}", t.id), "1")
        .await
        .unwrap();
    let asks = ChatMessage::assistant("").with_tool_calls(vec![call("c1", "time_now", json!({}))]);
    let prov = Provenance {
        turn: Some(t.id.to_string()),
        step: 1,
        ..Default::default()
    };
    history
        .append_as(&sid, &asks, 5, 0, false, None, &prov)
        .await
        .unwrap();

    // Redémarrage, deux fois : une seule fermeture.
    d.recover().await.unwrap();
    d.recover().await.unwrap();
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let finished: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "turn.finished")
        .collect();
    assert_eq!(finished.len(), 1, "{events:?}");
    assert_eq!(
        finished[0].payload,
        json!({"reason": "interrupted", "turn_id": t.id.to_string(), "kind": "message",
               "attempt": t.attempts, "origin_turn": t.id.to_string()})
    );

    // Le tour rejoué : tentative suivante, l'appel en attente est résolu avant le modèle.
    p.reply("Il est quatre heures.");
    let again = s.turns.claim("runner-0").await.unwrap().unwrap();
    assert_eq!(again.id, t.id);
    assert_eq!(again.attempts, t.attempts + 1);
    let out = d.run_turn(&again).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let started: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "turn.started")
        .map(|e| e.payload["attempt"].clone())
        .collect();
    assert_eq!(started, vec![json!(t.attempts), json!(t.attempts + 1)]);
    let result = events
        .iter()
        .find(|e| e.kind == "conv.tool_result")
        .expect("l'appel sans résultat est résolu au rejeu");
    assert_eq!(result.payload["call_id"], "c1");
    let users = events.iter().filter(|e| e.kind == "conv.user").count();
    assert_eq!(users, 1, "le message n'est pas réécrit");
    let last = events.iter().rfind(|e| e.kind == "turn.finished").unwrap();
    assert_eq!(last.payload["reason"], "answered");
    assert_eq!(last.payload["attempt"], json!(t.attempts + 1));
}
