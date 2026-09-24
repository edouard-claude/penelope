use super::*;
use crate::replay::fixture::{rewound, rich, sealed, world};

fn whats(report: &VerifyReport) -> Vec<(&str, &str)> {
    report
        .divergences
        .iter()
        .map(|d| (d.session.as_str(), d.what.as_str()))
        .collect()
}

#[tokio::test]
async fn every_write_path_verifies_clean() {
    let w = world().await;
    rich(&w).await;
    sealed(&w).await;
    rewound(&w).await;
    let report = w.history().verify(None, None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);
    assert_eq!(report.archives, vec!["s2".to_string()]);
    assert!(
        w.int("SELECT COUNT(*) FROM lcm_nodes WHERE session_id='s1'")
            .await
            >= 2
    );
    let s1 = w.history().verify_session("s1").await.unwrap();
    assert_eq!(s1.kind, "journal");
    let s3 = w.history().verify_session("s3").await.unwrap();
    assert_eq!((s3.kind.as_str(), s3.nodes), ("sealed", 10));
}

/// Critère de T12 : une altération à la main est une divergence qui nomme la session et
/// le nœud.
#[tokio::test]
async fn a_hand_edited_row_names_its_session_and_its_node() {
    let w = world().await;
    rich(&w).await;
    w.sql(
        "UPDATE messages SET content = '{\"blocks\":[{\"type\":\"text\",\"text\":\"falsifié\"}],\"tool_calls\":[]}'
         WHERE session_id = 's1' AND seq = 3",
    )
    .await;
    let report = w.history().verify(None, None).await.unwrap();
    assert_eq!(whats(&report), vec![("s1", "content")]);
    let d = &report.divergences[0];
    assert_eq!(d.seq, Some(3));
    assert_eq!(d.node, Some(w.history().address("s1", 3).await.unwrap()));
    assert!(d.node > d.seq, "l'adresse du nœud, pas le numéro de ligne");
}

/// Critère de T12 : le préfixe scellé modifié change l'empreinte.
#[tokio::test]
async fn an_edited_sealed_prefix_breaks_its_digest() {
    let w = world().await;
    sealed(&w).await;
    assert!(w.history().verify(None, None).await.unwrap().ok);
    w.sql("UPDATE message_context SET context = '<autre/>' WHERE session_id = 's3' AND seq = 1")
        .await;
    let report = w.history().verify(None, None).await.unwrap();
    assert_eq!(whats(&report), vec![("s3", "digest")]);
}

#[tokio::test]
async fn missing_extra_and_unjournaled_rows_are_each_named() {
    let w = world().await;
    rich(&w).await;
    w.sql(
        "DELETE FROM messages WHERE session_id = 's1' AND seq = 2;
         INSERT INTO messages(session_id, seq, role, content, ts)
           VALUES('s1', 9999, 'user', '{\"blocks\":[]}', 't');
         UPDATE messages SET compacted = 1 - compacted WHERE session_id = 's1' AND seq = 120;",
    )
    .await;
    let report = w.history().verify(Some("s1"), None).await.unwrap();
    let mut got: Vec<&str> = report.divergences.iter().map(|d| d.what.as_str()).collect();
    got.sort();
    assert_eq!(
        got,
        vec!["compacted", "missing_row", "unjournaled_row"],
        "{:#?}",
        report.divergences
    );
}

#[tokio::test]
async fn contexts_and_summaries_are_compared() {
    let w = world().await;
    rich(&w).await;
    w.sql(
        "UPDATE message_context SET context = 'x' WHERE session_id = 's1' AND seq = 1;
         UPDATE lcm_nodes SET summary = 'autre' WHERE session_id = 's1' AND superseded_by IS NULL;",
    )
    .await;
    let report = w.history().verify(None, None).await.unwrap();
    assert_eq!(
        whats(&report),
        vec![("s1", "context"), ("s1", "summary")],
        "{:#?}",
        report.divergences
    );
}

/// L'archive d'un retour arrière n'a pas de journal : ses lignes sont comparées à ce que
/// la coupe a retiré de sa mère.
#[tokio::test]
async fn a_rewind_archive_is_checked_against_its_mother() {
    let w = world().await;
    rich(&w).await;
    rewound(&w).await;
    w.sql("DELETE FROM messages WHERE session_id = 's2' AND role = 'assistant'")
        .await;
    let report = w.history().verify(None, None).await.unwrap();
    assert_eq!(whats(&report), vec![("s2", "missing_row")]);
}

/// Un journal qui ne se plie pas est une divergence, pas une panne.
#[tokio::test]
async fn an_incoherent_journal_is_a_divergence() {
    let w = world().await;
    rich(&w).await;
    w.log
        .append(
            penelope_kernel::event::EventDraft::new(
                crate::journal::KIND_REWIND,
                json!({"v": 1, "surface": {"op": "cut", "after": 99_999}}),
            )
            .session("s1"),
        )
        .await
        .unwrap();
    let report = w.history().verify(None, None).await.unwrap();
    assert_eq!(whats(&report), vec![("s1", "journal")]);
}
