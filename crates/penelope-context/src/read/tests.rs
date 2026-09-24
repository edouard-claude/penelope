use super::*;
use crate::replay::fixture::{World, rewound, rich, sealed, world};

async fn both(w: &World, sid: &str) -> (Vec<Entry>, Vec<Entry>) {
    let e = &w.engine;
    let tables = e.projected_entries(sid, HistorySource::Tables).await;
    let journal = e.projected_entries(sid, HistorySource::Journal).await;
    (tables.unwrap(), journal.unwrap())
}

fn text(entries: &[Entry]) -> String {
    serde_json::to_string(entries).unwrap()
}

/// Critère de T14, sans daemon : compaction, niveau 1, arguments d'appel dans l'ordre du
/// fournisseur, scellement, retour arrière ; les deux sources donnent les mêmes octets
/// (la comparaison du mode `tables` passe), numéros de ligne compris.
#[tokio::test]
async fn both_sources_read_the_same_bytes() {
    let w = world().await;
    rich(&w).await;
    sealed(&w).await;
    rewound(&w).await;
    for sid in ["s1", "s2", "s3"] {
        let (tables, journal) = both(&w, sid).await;
        assert!(!tables.is_empty(), "{sid}");
        assert_eq!(text(&tables), text(&journal), "{sid}");
        let e = &w.engine;
        let tail = e.tail(sid, 7, HistorySource::Tables).await.unwrap();
        let from_journal = e.tail(sid, 7, HistorySource::Journal).await.unwrap();
        assert_eq!(tail.len(), if sid == "s2" { 2 } else { 7 }, "{sid}");
        assert_eq!(text(&tail), text(&from_journal), "{sid}");
    }
    let h = w.history();
    assert!(h.read_journal("s1").await.unwrap().unwrap().is_some());
    assert!(h.read_journal("s3").await.unwrap().unwrap().is_some());
    assert!(
        h.read_journal("s2").await.unwrap().unwrap().is_none(),
        "l'archive d'un retour arrière se lit dans les tables"
    );
}

/// Une ligne modifiée à la main : le mode `tables` le dit (session et entrée nommées),
/// le mode `journal` ne la lit pas.
#[tokio::test]
async fn a_hand_edited_row_fails_the_comparison_and_is_not_read_from_the_journal() {
    let w = world().await;
    rich(&w).await;
    let before = both(&w, "s1").await.1;
    w.sql(
        "UPDATE messages SET content = '{\"blocks\":[{\"type\":\"text\",\"text\":\"falsifié\"}],\"tool_calls\":[]}'
         WHERE session_id = 's1' AND seq = (SELECT MAX(seq) FROM messages WHERE session_id = 's1')",
    )
    .await;
    let e = &w.engine;
    let err = e
        .projected_entries("s1", HistorySource::Tables)
        .await
        .unwrap_err();
    let said = err.to_string();
    assert!(
        matches!(err, ReadError::Diverged { .. })
            && said.contains("session s1")
            && said.contains("falsifié"),
        "{said}"
    );
    assert!(e.tail("s1", 4, HistorySource::Tables).await.is_err());
    let journal = e
        .projected_entries("s1", HistorySource::Journal)
        .await
        .unwrap();
    assert_eq!(text(&journal), text(&before));
}

/// Une session écrite sans journal (lignes sans événement) se lit dans les tables, quelle
/// que soit la source.
#[tokio::test]
async fn rows_without_events_are_read_from_the_tables() {
    let w = world().await;
    let bare = HistoryStore::new(w.store.clone(), w.clock.clone());
    bare.append("s9", &ChatMessage::user("sans journal"), 3, 0, false, None)
        .await
        .unwrap();
    assert!(
        w.history()
            .read_journal("s9")
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    let (tables, journal) = both(&w, "s9").await;
    assert_eq!(tables.len(), 1);
    assert_eq!(text(&tables), text(&journal));
}
