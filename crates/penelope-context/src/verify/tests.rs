use super::*;
use crate::replay::fixture::{rewound, rich, sealed, sealed_with_summary, world};

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

/// Constat de la 1.0.0-alpha.15 sur une vraie base : une session scellée qui porte un
/// résumé actif divergeait (« jetons, ancres ») parce que la relecture du préfixe scellé
/// perdait `tokens_src` et `anchors` ; un fork de cette session héritait de l'écart.
#[tokio::test]
async fn a_sealed_summary_and_its_fork_verify_clean() {
    let w = world().await;
    sealed_with_summary(&w).await;
    let report = w.history().verify(Some("s3"), None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);

    // Le fork tel que `penelope-ops` le fait : `conv.fork`, puis la fille projetée depuis
    // son héritage (lignes scellées recopiées avec leur drapeau, résumé actif recopié).
    w.sql("INSERT INTO sessions(id, kind, created_at, updated_at) VALUES('s4', 'chat', 't', 't')")
        .await;
    assert_eq!(w.history().fork("s4", "s3").await.unwrap(), 8);
    assert_eq!(
        w.int("SELECT COUNT(*) FROM messages WHERE session_id = 's4' AND sealed = 1")
            .await,
        8
    );
    crate::replay::fixture::exchanges(&w, "s4", 0..1).await;
    let report = w.history().verify(None, None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);
    let s4 = w.history().verify_session("s4").await.unwrap();
    assert_eq!(
        (s4.kind.as_str(), s4.nodes),
        ("fork", 10),
        "8 lignes héritées, 2 neuves"
    );

    // La refonte garde les lignes héritées du préfixe scellé et la provenance du résumé
    // recopié ; tout revérifie.
    w.history().reindex(None).await.unwrap();
    assert_eq!(
        w.int("SELECT COUNT(*) FROM messages WHERE session_id = 's4'")
            .await,
        10
    );
    assert_eq!(
        w.int("SELECT tokens_src FROM lcm_nodes WHERE session_id = 's4' AND anchors LIKE '%#147%'")
            .await,
        680
    );
    let report = w.history().verify(None, None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);
}

/// Une compaction journalisée qui prolonge le résumé scellé additionne son `tokens_src`
/// à celui du nœud scellé, comme `Lcm::replace` en base.
#[tokio::test]
async fn a_summary_extending_a_sealed_one_verifies_clean() {
    let w = world().await;
    sealed_with_summary(&w).await;
    crate::replay::fixture::exchanges(&w, "s3", 0..60).await;
    crate::replay::fixture::compact(&w, "s3").await;
    let src: i64 = w
        .int("SELECT tokens_src FROM lcm_nodes WHERE session_id = 's3' AND superseded_by IS NULL")
        .await;
    assert!(
        src > 680,
        "la prolongation compte la source scellée ({src})"
    );
    let report = w.history().verify(None, None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);
}

/// T16 : l'archive d'un retour arrière qui coupe dans le préfixe scellé est projetée
/// depuis le journal de sa mère, lignes scellées recopiées avec leur drapeau : le
/// scellement du démarrage suivant ne la prend pas pour une session 0.17.
///
/// La mère garde ses lignes coupées, masquées : son `conv.import` les compte toujours et
/// elle se replie (`i-rewind-scelle`, défaut relevé dans `design/v1/notes/i-retrait.md`).
#[tokio::test]
async fn an_archive_cut_inside_the_sealed_prefix_keeps_its_sealed_rows() {
    let w = world().await;
    sealed_with_summary(&w).await;
    w.sql("INSERT INTO sessions(id, kind, created_at, updated_at) VALUES('s4', 'chat', 't', 't')")
        .await;
    // Le dernier échange scellé (7, 8) part dans l'archive `s4`.
    let done = w
        .history()
        .rewind_from("s3", 7, 1, Some("s4"))
        .await
        .unwrap();
    assert_eq!(
        done,
        crate::store::Rewound {
            removed: 2,
            archived: 2
        }
    );
    assert_eq!(
        w.int("SELECT COUNT(*) FROM messages WHERE session_id = 's4' AND sealed = 1 AND seq >= 7")
            .await,
        2
    );
    let report = w.history().seal_legacy().await.unwrap();
    assert!(report.sealed.is_empty(), "{:?}", report.sealed);
    let report = w.history().verify(None, None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);
}

fn details<'a>(report: &'a VerifyReport, what: &str) -> Vec<&'a str> {
    report
        .divergences
        .iter()
        .filter(|d| d.what == what)
        .map(|d| d.detail.as_str())
        .collect()
}

/// Un contexte figé qu'aucun événement `conv.context` ne porte est un écart.
#[tokio::test]
async fn an_unjournaled_context_is_named() {
    let w = world().await;
    rich(&w).await;
    w.sql("INSERT INTO message_context(session_id, seq, context) VALUES('s1', 7777, '<x/>')")
        .await;
    let report = w.history().verify(Some("s1"), None).await.unwrap();
    assert_eq!(
        details(&report, "context"),
        ["contexte figé qu'aucun conv.context ne porte"],
        "{:#?}",
        report.divergences
    );
}

/// Deux lignes qui portent le même événement, et deux lignes échangées : chaque écart a
/// son nom.
#[tokio::test]
async fn duplicated_and_swapped_rows_are_named() {
    let w = world().await;
    rich(&w).await;
    w.sql(
        "INSERT INTO messages(session_id, seq, role, content, ts, event_id, sealed)
           SELECT session_id, 8888, role, content, ts, event_id, sealed
           FROM messages WHERE session_id = 's1' AND seq = 3",
    )
    .await;
    let report = w.history().verify(Some("s1"), None).await.unwrap();
    assert!(
        details(&report, "extra_row").contains(&"deux lignes pour le même nœud"),
        "{:#?}",
        report.divergences
    );

    let w = world().await;
    rich(&w).await;
    w.sql(
        "UPDATE messages SET seq = -1 WHERE session_id = 's1' AND seq = 3;
         UPDATE messages SET seq = 3 WHERE session_id = 's1' AND seq = 4;
         UPDATE messages SET seq = 4 WHERE session_id = 's1' AND seq = -1;",
    )
    .await;
    let report = w.history().verify(Some("s1"), None).await.unwrap();
    assert!(
        report.divergences.iter().any(|d| d.what == "order"),
        "{:#?}",
        report.divergences
    );
}

/// Un résumé dont l'événement, les jetons ou les ancres ne sont plus ceux du journal le
/// dit, champ par champ.
#[tokio::test]
async fn a_summary_with_altered_fields_names_each_of_them() {
    let w = world().await;
    rich(&w).await;
    w.sql(
        "UPDATE lcm_nodes SET event_id = 424242, tokens_self = tokens_self + 1, anchors = '[\"x\"]'
         WHERE session_id = 's1' AND superseded_by IS NULL",
    )
    .await;
    let report = w.history().verify(Some("s1"), None).await.unwrap();
    let d = details(&report, "summary").join(" | ");
    assert!(d.contains("event_id"), "{d}");
    assert!(d.contains("jetons"), "{d}");
    assert!(d.contains("ancres"), "{d}");
}
