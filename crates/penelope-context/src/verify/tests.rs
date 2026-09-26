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

/// `s3` en V0 : huit messages, un contexte figé par demande, un résumé actif sur 1..4 qui
/// porte ses ancres et un `tokens_src` distinct de `tokens_self` (ce que la 0.17 écrit),
/// puis le scellement.
async fn sealed_with_summary(w: &crate::replay::fixture::World) {
    use crate::anchors::{Anchor, AnchorKind};
    let bare = w.history();
    for i in 0..4 {
        let seq = bare
            .append_legacy("s3", &ChatMessage::user(format!("ancien {i}")), 300, 0)
            .await
            .unwrap();
        bare.freeze_legacy("s3", seq, &format!("<ancien>{i}</ancien>"))
            .await
            .unwrap();
        bare.append_legacy("s3", &ChatMessage::assistant(format!("vieux {i}")), 40, 0)
            .await
            .unwrap();
    }
    let anchors = [
        Anchor {
            kind: AnchorKind::Path,
            value: "src/main.rs".into(),
        },
        Anchor {
            kind: AnchorKind::Ticket,
            value: "#147".into(),
        },
    ];
    w.engine
        .lcm
        .insert_leaf("s3", 1, 4, "résumé ancien", &anchors, 680, 25)
        .await
        .unwrap();
    bare.mark_compacted("s3", 1, 4).await.unwrap();
    let report = w.history().seal_legacy().await.unwrap();
    assert_eq!(report.sealed, vec![("s3".to_string(), 8)]);
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
/// La mère, elle, ne se replie plus : la coupe a retiré des lignes que son `conv.import`
/// compte (défaut antérieur à T16, relevé dans `design/v1/notes/i-retrait.md`).
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
}
