use super::*;
use crate::derive::{Sealed, derive};
use crate::journal::UserSource;
use penelope_kernel::clock::TestClock;

async fn journaled() -> (HistoryStore, EventLog) {
    let store = Store::open_memory().unwrap();
    store
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                 VALUES('s1','chat','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let clock: SharedClock = Arc::new(TestClock::default());
    let log = EventLog::new(store.clone(), clock.clone());
    (
        HistoryStore::new(store, clock).with_events(log.clone()),
        log,
    )
}

/// `(seq, event_id)` de chaque ligne de la session.
async fn rows(h: &HistoryStore) -> Vec<(i64, Option<i64>)> {
    h.store()
        .read(|c| {
            let mut st =
                c.prepare("SELECT seq, event_id FROM messages WHERE session_id='s1' ORDER BY seq")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .await
        .unwrap()
}

/// Chaque ligne cite son événement, et l'événement redonne le même message.
async fn assert_each_row_has_its_event(h: &HistoryStore, log: &EventLog) {
    let events = log.session_events("s1", 0).await.unwrap();
    let loaded = h.load("s1", 0).await.unwrap();
    let rows = rows(h).await;
    assert_eq!(rows.len(), loaded.len());
    for ((seq, event_id), entry) in rows.iter().zip(&loaded) {
        let ev = events
            .iter()
            .find(|e| Some(e.id) == *event_id)
            .unwrap_or_else(|| panic!("ligne {seq} sans événement : {event_id:?}"));
        let surface = derive(&Sealed::none(), std::slice::from_ref(ev)).unwrap();
        let derived = surface.entries();
        assert_eq!(derived.len(), 1, "{}", ev.kind);
        assert_eq!(derived[0].message, entry.message, "ligne {seq}");
        assert_eq!(derived[0].tokens, entry.tokens);
        assert_eq!(derived[0].episode, entry.episode);
        assert_eq!(derived[0].eager, entry.eager);
    }
}

#[tokio::test]
async fn every_message_is_written_with_its_event() {
    let (h, log) = journaled().await;
    h.append_user_turn_at("s1", "q1", "lis le fichier", "2026-09-24T08:00:00Z", 4, 1)
        .await
        .unwrap();
    let call = ToolCall {
        id: "c1".into(),
        name: "fs_read".into(),
        arguments: json!({"path": "a.rs"}),
    };
    let answer = ChatMessage {
        reasoning: Some("je lis".into()),
        ..ChatMessage::assistant("")
    }
    .with_tool_calls(vec![call]);
    let prov = Provenance {
        turn: Some("q1".into()),
        step: 1,
        ..Default::default()
    };
    h.append_as("s1", &answer, 7, 1, false, None, &prov)
        .await
        .unwrap();
    let result = ChatMessage::tool_result("c1", "fs_read", "fn main() {}");
    h.append("s1", &result, 5, 1, true, None).await.unwrap();
    h.append(
        "s1",
        &ChatMessage::assistant("c'est un main vide"),
        6,
        1,
        false,
        None,
    )
    .await
    .unwrap();

    assert_each_row_has_its_event(&h, &log).await;
    let events = log.session_events("s1", 0).await.unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "conv.user",
            "conv.assistant",
            "conv.tool_result",
            "conv.assistant"
        ]
    );
    assert_eq!(events[0].payload["arrived_at"], "2026-09-24T08:00:00Z");
    assert_eq!(events[0].payload["turn_message_id"], "q1");
    assert_eq!(events[1].payload["turn"], "q1");
    assert_eq!(events[1].payload["step"], 1);
    assert_eq!(events[2].payload["eager"], true);
    // Le journal plié redonne tout l'historique, dans l'ordre.
    let surface = derive(&Sealed::none(), &events).unwrap();
    let loaded = h.load("s1", 0).await.unwrap();
    let derived: Vec<_> = surface.entries().into_iter().map(|e| e.message).collect();
    let tables: Vec<_> = loaded.into_iter().map(|e| e.message).collect();
    assert_eq!(derived, tables);
}

/// #161, et §2.7 : un message de la file rejoué ne s'écrit ni ne se journalise deux
/// fois ; un crash entre l'événement et la ligne ne réécrit que la ligne.
#[tokio::test]
async fn a_queued_message_is_written_once_even_across_a_crash() {
    let (h, log) = journaled().await;
    let first = h
        .append_user_turn_at("s1", "q1", "bonjour", "2026-09-24T08:00:00Z", 2, 0)
        .await
        .unwrap();
    let again = h
        .append_user_turn_at("s1", "q1", "bonjour", "2026-09-24T08:00:00Z", 2, 0)
        .await
        .unwrap();
    assert_eq!(first, again);

    // Crash simulé : l'événement de q2 est commité, sa ligne jamais écrite.
    let prov = Provenance::queued(UserSource::Merged, "q2", "2026-09-24T08:01:00Z");
    let lost = message_event(&ChatMessage::user("encore"), 1, 0, false, &prov).unwrap();
    let ev = log
        .append(EventDraft::new(lost.kind(), lost.payload()).session("s1"))
        .await
        .unwrap();
    h.append_queued("s1", &ChatMessage::user("encore"), 1, 0, &prov)
        .await
        .unwrap();

    let events = log.session_events("s1", 0).await.unwrap();
    assert_eq!(events.len(), 2, "aucun événement en double");
    let rows = rows(&h).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[1].1,
        Some(ev.id),
        "la ligne réparée cite l'événement existant"
    );
    assert_each_row_has_its_event(&h, &log).await;
}

/// Sans journal attaché, rien ne change : la ligne s'écrit seule.
#[tokio::test]
async fn without_a_journal_the_row_is_written_alone() {
    let store = Store::open_memory().unwrap();
    let h = HistoryStore::new(store, Arc::new(TestClock::default()));
    h.store()
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                 VALUES('s1','chat','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    h.append("s1", &ChatMessage::user("seul"), 1, 0, false, None)
        .await
        .unwrap();
    assert_eq!(rows(&h).await, [(1, None)]);
}
