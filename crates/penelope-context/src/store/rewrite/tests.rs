use super::*;
use crate::replay::fixture::{World, caches, exchanges, sealed_with_summary, world};
use crate::transcript::Entry;

async fn clean(w: &World) {
    let report = w.history().verify(None, None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);
}

async fn session(w: &World, id: &'static str) {
    w.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at) VALUES(?1, 'chat', 't', 't')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Les entrées projetées, identifiant du résumé masqué (tiré au hasard en V0).
async fn request(w: &World, sid: &str) -> String {
    let id: String = w
        .store
        .read(|c| Ok(c.query_row("SELECT id FROM lcm_nodes", [], |r| r.get(0))?))
        .await
        .unwrap();
    let entries: Vec<Entry> = w.engine.projected_entries(sid).await.unwrap();
    serde_json::to_string(&entries).unwrap().replace(&id, "N")
}

/// La lecture pliée depuis le journal, sans repli sur les caches, et les caches
/// eux-mêmes : les mêmes octets.
async fn read_folds(w: &World, sid: &str) -> Vec<Entry> {
    let read = w
        .history()
        .read_journal(sid)
        .await
        .unwrap()
        .expect("le journal se plie")
        .expect("le journal redonne la session");
    let tables = w.engine.projected_from_tables(sid).await.unwrap();
    assert_eq!(
        serde_json::to_string(&read.projected).unwrap(),
        serde_json::to_string(&tables).unwrap(),
        "{sid}"
    );
    read.projected
}

/// La même session en V0, retour arrière fait avant le scellement : trois échanges
/// au lieu de quatre, même résumé.
async fn rewound_in_v0() -> World {
    let w = world().await;
    let bare = w.history();
    for i in 0..3 {
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
    w.engine
        .lcm
        .insert_leaf("s3", 1, 4, "résumé ancien", &[], 680, 25)
        .await
        .unwrap();
    bare.mark_compacted("s3", 1, 4).await.unwrap();
    w.history().seal_legacy().await.unwrap();
    w
}

/// Un `/rewind` qui coupe dans le préfixe scellé d'une session d'avant le journal : les
/// lignes coupées restent en base, masquées (`sealed = 2`), parce que le `conv.import`
/// les compte et que son empreinte les couvre. La mère se replie, sa requête est celle
/// d'un retour arrière V0 équivalent, `verify` est à zéro, avant et après un message
/// nouveau, et la refonte redonne les mêmes caches (défaut relevé par `i-retrait.md`).
#[tokio::test]
async fn a_rewind_inside_the_sealed_prefix_keeps_the_prefix_and_folds() {
    let w = world().await;
    sealed_with_summary(&w).await;
    session(&w, "s4").await;
    // Le dernier échange scellé (7, 8) part dans l'archive `s4`.
    let done = w
        .history()
        .rewind_from("s3", 7, 1, Some("s4"))
        .await
        .unwrap();
    assert_eq!(
        done,
        Rewound {
            removed: 2,
            archived: 2
        }
    );
    assert_eq!(
        w.int("SELECT COUNT(*) FROM messages WHERE session_id = 's3' AND sealed = 2")
            .await,
        2,
        "les lignes scellées coupées restent, masquées"
    );
    let found: Vec<i64> = w
        .history()
        .grep("vieux", Some("s3"), 20)
        .await
        .unwrap()
        .iter()
        .map(|h| h.seq)
        .collect();
    assert!(
        !found.contains(&8) && found.contains(&6),
        "la recherche ne trouve plus la ligne masquée : {found:?}"
    );

    let projected = read_folds(&w, "s3").await;
    assert_eq!(projected.len(), 3, "le résumé, puis 5 et 6");
    assert_eq!(
        request(&w, "s3").await,
        request(&rewound_in_v0().await, "s3").await
    );
    clean(&w).await;
    assert!(w.history().seal_legacy().await.unwrap().sealed.is_empty());
    let loaded: Vec<i64> = w
        .history()
        .load("s3", 0)
        .await
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(loaded, vec![1, 2, 3, 4, 5, 6]);

    // Un message nouveau : numéroté après les lignes masquées, comme la V0 l'aurait fait
    // de lignes gardées.
    exchanges(&w, "s3", 0..1).await;
    let projected = read_folds(&w, "s3").await;
    let seqs: Vec<i64> = projected.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![0, 5, 6, 9, 10]);
    clean(&w).await;

    let before = caches(&w).await;
    for session in [Some("s3"), None] {
        let report = w.history().reindex(session).await.unwrap();
        assert!(report.ok, "{:?}", report.refused);
        assert_eq!(caches(&w).await, before);
    }
    clean(&w).await;

    // Un fork de la mère n'hérite pas des lignes masquées (la copie du résumé y porte un
    // identifiant neuf, comme en V0 : sa lecture ne se compare pas octet pour octet).
    session(&w, "s5").await;
    assert_eq!(w.history().fork("s5", "s3").await.unwrap(), 8);
    let s5 = w
        .history()
        .read_journal("s5")
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(s5.entries.len(), 8);
    clean(&w).await;
}

/// La seconde transaction du retour arrière perdue : le rattrapage masque les lignes
/// scellées coupées au lieu de les effacer.
#[tokio::test]
async fn a_lost_sealed_cut_is_caught_up_as_masked_rows() {
    let w = world().await;
    sealed_with_summary(&w).await;
    session(&w, "s4").await;
    w.history()
        .rewind_from("s3", 5, 2, Some("s4"))
        .await
        .unwrap();
    clean(&w).await;
    let before = caches(&w).await;
    w.sql(
        "UPDATE messages SET sealed = 1 WHERE session_id = 's3' AND sealed = 2;
         DELETE FROM projections_session;",
    )
    .await;
    assert!(!w.history().verify(Some("s3"), None).await.unwrap().ok);
    w.history().catch_up("s3").await.unwrap();
    clean(&w).await;
    assert_eq!(caches(&w).await, before);
}
