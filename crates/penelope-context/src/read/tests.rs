use super::*;
use crate::replay::fixture::{World, compact, exchanges, rewound, rich, sealed, tool_round, world};

/// La projection lue dans les caches, puis celle que la lecture rend (le journal).
async fn both(w: &World, sid: &str) -> (Vec<Entry>, Vec<Entry>) {
    let e = &w.engine;
    let caches = e.projected_from_tables(sid).await;
    let journal = e.projected_entries(sid).await;
    (caches.unwrap(), journal.unwrap())
}

fn text(entries: &[Entry]) -> String {
    serde_json::to_string(entries).unwrap()
}

/// Sans daemon : compaction, niveau 1, arguments d'appel dans l'ordre du fournisseur,
/// scellement, retour arrière ; la lecture depuis le journal et les caches que le
/// projecteur a écrits donnent les mêmes octets, numéros de ligne compris (T14, T16).
#[tokio::test]
async fn the_journal_and_its_caches_read_the_same_bytes() {
    let w = world().await;
    rich(&w).await;
    sealed(&w).await;
    rewound(&w).await;
    for sid in ["s1", "s2", "s3"] {
        let (caches, journal) = both(&w, sid).await;
        assert!(!caches.is_empty(), "{sid}");
        assert_eq!(text(&caches), text(&journal), "{sid}");
        let e = &w.engine;
        let tail = w.history().tail(sid, 7).await.unwrap();
        let from_journal = e.tail(sid, 7).await.unwrap();
        assert_eq!(tail.len(), if sid == "s2" { 2 } else { 7 }, "{sid}");
        assert_eq!(text(&tail), text(&from_journal), "{sid}");
    }
    let h = w.history();
    assert!(h.read_journal("s1").await.unwrap().unwrap().is_some());
    assert!(h.read_journal("s3").await.unwrap().unwrap().is_some());
    assert!(
        h.read_journal("s2").await.unwrap().unwrap().is_none(),
        "l'archive d'un retour arrière se lit dans ses caches, projetés du journal de sa mère"
    );
}

/// Une ligne de cache modifiée à la main n'est pas lue : la conversation vient du
/// journal (`history verify` nomme la ligne).
#[tokio::test]
async fn a_hand_edited_cache_row_is_not_read() {
    let w = world().await;
    rich(&w).await;
    let before = both(&w, "s1").await.1;
    w.sql(
        "UPDATE messages SET content = '{\"blocks\":[{\"type\":\"text\",\"text\":\"falsifié\"}],\"tool_calls\":[]}'
         WHERE session_id = 's1' AND seq = (SELECT MAX(seq) FROM messages WHERE session_id = 's1')",
    )
    .await;
    let (caches, journal) = both(&w, "s1").await;
    assert!(text(&caches).contains("falsifié"));
    assert_eq!(text(&journal), text(&before));
    let tail = w.engine.tail("s1", 4).await.unwrap();
    assert!(!text(&tail).contains("falsifié"));
    let report = w.history().verify(Some("s1"), None).await.unwrap();
    assert!(!report.ok, "la vérification voit la ligne");
}

/// Des lignes V0 ni journalisées ni scellées (une base que le scellement n'a pas encore
/// vue) se lisent dans les caches.
#[tokio::test]
async fn rows_without_events_are_read_from_the_caches() {
    let w = world().await;
    w.history()
        .append_legacy("s2", &ChatMessage::user("sans journal"), 3, 0)
        .await
        .unwrap();
    assert!(
        w.history()
            .read_journal("s2")
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    let (caches, journal) = both(&w, "s2").await;
    assert_eq!(caches.len(), 1);
    assert_eq!(text(&caches), text(&journal));
}

/// Temps d'une lecture depuis le journal (projection et queue) et ce qu'elle a plié.
async fn timed(w: &World, sid: &str) -> (std::time::Duration, usize, Vec<Entry>) {
    let e = &w.engine;
    let start = std::time::Instant::now();
    let projected = e.projected_entries(sid).await.unwrap();
    let folded = w.history().reads.lock().unwrap().folded;
    e.tail(sid, 40).await.unwrap();
    (start.elapsed(), folded, projected)
}

/// La même lecture, repliée depuis le début.
async fn fresh(w: &World, sid: &str) -> (Vec<Entry>, Vec<Entry>) {
    w.history().reads.lock().unwrap().clear();
    let e = &w.engine;
    let projected = e.projected_entries(sid).await;
    let tail = e.tail(sid, 40).await;
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
    h.fork("s3", "s1").await.unwrap();
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
        let tail = w.engine.tail(sid, 40).await.unwrap();
        let (again, again_tail) = fresh(&w, sid).await;
        assert_eq!(text(&projected), text(&again), "{sid}");
        assert_eq!(text(&tail), text(&again_tail), "{sid}");
    }
}

/// Lire entre chaque écriture (compaction, prolongation, niveau 1, retour arrière) : la
/// surface reprise est celle d'un pliage complet, et celle des caches.
#[tokio::test]
async fn a_resumed_read_matches_a_full_fold_at_every_step() {
    let w = world().await;
    let check = |label: &'static str| {
        let w = &w;
        async move {
            let e = &w.engine;
            let journal = e.projected_entries("s1").await.unwrap();
            let tail = e.tail("s1", 40).await.unwrap();
            if label != "purge" {
                // Les caches d'une session purgée sont effacés par `penelope-ops`.
                let caches = e.projected_from_tables("s1").await.unwrap();
                assert_eq!(text(&caches), text(&journal), "{label}");
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
    let purged = e.projected_entries("s1").await.unwrap();
    assert!(purged.is_empty(), "une purge jette la surface pliée");
}
