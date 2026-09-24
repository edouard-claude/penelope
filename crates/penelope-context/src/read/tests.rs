use super::*;
use crate::replay::fixture::{World, compact, exchanges, rewound, rich, sealed, tool_round, world};

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

/// Temps d'une lecture depuis le journal (projection et queue) et ce qu'elle a plié.
async fn timed(w: &World, sid: &str) -> (std::time::Duration, usize, Vec<Entry>) {
    let e = &w.engine;
    let start = std::time::Instant::now();
    let projected = e
        .projected_entries(sid, HistorySource::Journal)
        .await
        .unwrap();
    let folded = w.history().reads.lock().unwrap().folded;
    e.tail(sid, 40, HistorySource::Journal).await.unwrap();
    (start.elapsed(), folded, projected)
}

/// La même lecture, repliée depuis le début.
async fn fresh(w: &World, sid: &str) -> (Vec<Entry>, Vec<Entry>) {
    w.history().reads.lock().unwrap().clear();
    let e = &w.engine;
    let projected = e.projected_entries(sid, HistorySource::Journal).await;
    let tail = e.tail(sid, 40, HistorySource::Journal).await;
    (projected.unwrap(), tail.unwrap())
}

/// Coût de lecture (T14, note « Coût ») : une session de 2 000 messages et une fille de
/// fork ; la requête n après un échange de plus ne plie que ce qui est nouveau, et lit ce
/// qu'un pliage complet aurait lu.
#[tokio::test]
async fn a_read_after_a_new_exchange_folds_only_what_is_new() {
    let w = world().await;
    exchanges(&w, "s1", 0..1_000).await;
    let h = w.history();
    h.journal_fork("s3", "s1").await.unwrap();
    h.copy_messages("s1", "s3", 0, None).await.unwrap();
    exchanges(&w, "s3", 0..2).await;
    for sid in ["s1", "s3"] {
        h.reads.lock().unwrap().clear();
        let (cold, all, _) = timed(&w, sid).await;
        let (warm, none, _) = timed(&w, sid).await;
        exchanges(&w, sid, 2_000..2_001).await;
        let (next, new, projected) = timed(&w, sid).await;
        eprintln!(
            "{sid} : première lecture {cold:?} ({all} événements pliés), \
             sans rien de neuf {warm:?} ({none}), après un échange {next:?} ({new})"
        );
        assert!(projected.len() > 2_000, "{sid}");
        assert!(all > 0, "{sid}");
        assert_eq!(
            (none, new),
            (0, 4),
            "{sid} : turn.started, user, context, assistant"
        );
        let tail = w
            .engine
            .tail(sid, 40, HistorySource::Journal)
            .await
            .unwrap();
        let (again, again_tail) = fresh(&w, sid).await;
        assert_eq!(text(&projected), text(&again), "{sid}");
        assert_eq!(text(&tail), text(&again_tail), "{sid}");
    }
}

/// Lire entre chaque écriture (compaction, prolongation, niveau 1, retour arrière) : la
/// surface reprise est celle d'un pliage complet, et celle des tables.
#[tokio::test]
async fn a_resumed_read_matches_a_full_fold_at_every_step() {
    let w = world().await;
    let check = |label: &'static str| {
        let w = &w;
        async move {
            let e = &w.engine;
            let journal = e
                .projected_entries("s1", HistorySource::Journal)
                .await
                .unwrap();
            let tail = e.tail("s1", 40, HistorySource::Journal).await.unwrap();
            if label != "purge" {
                // Les tables d'une session purgée sont effacées par le daemon, pas ici.
                let tables = e
                    .projected_entries("s1", HistorySource::Tables)
                    .await
                    .unwrap();
                assert_eq!(text(&tables), text(&journal), "{label}");
            }
            let (again, again_tail) = fresh(w, "s1").await;
            assert_eq!(text(&journal), text(&again), "{label}");
            assert_eq!(text(&tail), text(&again_tail), "{label}");
        }
    };
    exchanges(&w, "s1", 0..20).await;
    check("échanges").await;
    tool_round(&w, "s1").await;
    check("niveau 1").await;
    exchanges(&w, "s1", 20..30).await;
    compact(&w, "s1").await;
    check("résumé").await;
    exchanges(&w, "s1", 30..60).await;
    check("après le résumé").await;
    compact(&w, "s1").await;
    check("prolongation").await;
    exchanges(&w, "s1", 60..62).await;
    rewound(&w).await;
    check("retour arrière").await;
    w.log.purge_session("s1", "test").await.unwrap();
    check("purge").await;
    let e = &w.engine;
    let purged = e
        .projected_entries("s1", HistorySource::Journal)
        .await
        .unwrap();
    assert!(purged.is_empty(), "une purge jette la surface pliée");
}
