use super::*;
use crate::journal::{Provenance, message_event};
use crate::replay::fixture::{World, caches, rewound, rich, sealed, world};
use penelope_llm::types::ChatMessage;

async fn clean(w: &World) {
    let report = w.history().verify(None, None).await.unwrap();
    assert!(report.ok, "{:#?}", report.divergences);
}

/// Critère de T13 : toutes les lignes non scellées effacées, `reindex` les redonne à
/// l'identique et `verify` est à zéro.
#[tokio::test]
async fn reindex_after_erasing_unsealed_rows_gives_them_back() {
    let w = world().await;
    rich(&w).await;
    sealed(&w).await;
    rewound(&w).await;
    clean(&w).await;
    let before = caches(&w).await;
    w.sql(
        "DELETE FROM messages_fts WHERE msg_id IN (SELECT id FROM messages WHERE sealed = 0);
         DELETE FROM messages WHERE sealed = 0;
         DELETE FROM message_context WHERE session_id != 's3' OR seq > 6;
         DELETE FROM lcm_nodes;",
    )
    .await;
    assert!(!w.history().verify(None, None).await.unwrap().ok);

    let report = w.history().reindex(None).await.unwrap();
    assert!(report.ok, "{:?}", report.refused);
    assert_eq!(report.sessions.len(), 3);
    clean(&w).await;
    assert_eq!(caches(&w).await, before);

    let again = w.history().reindex(None).await.unwrap();
    assert_eq!(again, report, "idempotent");
    assert_eq!(caches(&w).await, before);
}

/// Une session que le journal ne sait pas refaire est laissée intacte et nommée.
#[tokio::test]
async fn reindex_refuses_rows_it_cannot_rebuild() {
    let w = world().await;
    rich(&w).await;
    w.sql(
        "INSERT INTO messages(session_id, seq, role, content, ts)
         VALUES('s1', 9999, 'user', '{\"blocks\":[]}', 't')",
    )
    .await;
    let before = caches(&w).await;
    let report = w.history().reindex(Some("s1")).await.unwrap();
    assert!(!report.ok);
    assert_eq!(report.refused[0].0, "s1");
    assert_eq!(caches(&w).await, before, "rien n'est effacé");
}

/// Critère de T13 : une écriture `after` qui échoue laisse l'événement sans sa ligne ;
/// le prochain accès (`catch_up`) la rattrape.
#[tokio::test]
async fn a_failed_second_transaction_is_caught_up_at_next_access() {
    let w = world().await;
    rich(&w).await;
    assert_eq!(w.history().catch_up("s1").await.unwrap(), CatchUp::Applied(w.int("SELECT COUNT(*) FROM events WHERE session_id = 's1' AND (kind LIKE 'conv.%' OR kind LIKE 'turn.%')").await as usize));
    let prov = Provenance::queued(
        crate::journal::UserSource::Owner,
        "q9",
        "2026-01-01T00:00:09Z",
    );
    let event = message_event(&ChatMessage::user("perdu"), 3, 0, false, &prov).unwrap();
    let failed = w
        .history()
        .journaled("s1", event, |_, _| -> penelope_store::Result<()> {
            Err(penelope_store::StoreError::other("coupure simulée"))
        })
        .await;
    assert!(failed.is_err());
    let report = w.history().verify(None, None).await.unwrap();
    assert_eq!(report.divergences.len(), 1);
    assert_eq!(report.divergences[0].what, "missing_row");

    assert_eq!(
        w.history().catch_up("s1").await.unwrap(),
        CatchUp::Applied(1)
    );
    clean(&w).await;
    assert_eq!(
        w.int("SELECT COUNT(*) FROM messages WHERE source_turn_id = 'q9'")
            .await,
        1,
        "la clé d'idempotence de la file suit la ligne rattrapée"
    );
    assert_eq!(w.history().catch_up("s1").await.unwrap(), CatchUp::Current);
}

/// Un nœud de résumé, un corps externalisé, une coupe, un contexte dont la seconde
/// transaction manque : un rattrapage depuis le début les refait, sans rien réécrire de
/// ce qui est déjà là.
#[tokio::test]
async fn catching_up_from_the_start_restores_what_is_missing_and_nothing_else() {
    let w = world().await;
    rich(&w).await;
    rewound(&w).await;
    let before = caches(&w).await;
    assert!(matches!(
        w.history().catch_up("s1").await.unwrap(),
        CatchUp::Applied(_)
    ));
    assert_eq!(caches(&w).await, before, "une base à jour ne bouge pas");

    // Les secondes transactions de s1 perdues, et le filigrane avec.
    w.sql(
        "DELETE FROM lcm_nodes WHERE session_id = 's1';
         UPDATE messages SET compacted = 0 WHERE session_id = 's1';
         UPDATE messages SET artifact_id = NULL, tokens_est = 800 WHERE session_id = 's1' AND role = 'tool';
         DELETE FROM message_context WHERE session_id = 's1' AND seq = 5;
         DELETE FROM projections_session;",
    )
    .await;
    assert!(!w.history().verify(None, None).await.unwrap().ok);
    w.history().catch_up("s1").await.unwrap();
    clean(&w).await;
}

/// Critère de T13 : une nouvelle `FOLD_VERSION` refond la session à son prochain
/// rattrapage ; à version égale, rien n'est refondu.
#[tokio::test]
async fn a_new_fold_version_rebuilds_the_session() {
    let w = world().await;
    rich(&w).await;
    w.history().catch_up("s1").await.unwrap();
    w.sql(
        "UPDATE messages SET content = '{\"blocks\":[{\"type\":\"text\",\"text\":\"abîmé\"}],\"tool_calls\":[]}'
         WHERE session_id = 's1' AND seq = 1",
    )
    .await;
    assert_eq!(w.history().catch_up("s1").await.unwrap(), CatchUp::Current);
    assert!(!w.history().verify(None, None).await.unwrap().ok);

    w.sql(
        "UPDATE projections_session SET state = json_set(state, '$.fold_version', 0)
         WHERE session_id = 's1'",
    )
    .await;
    assert_eq!(w.history().catch_up("s1").await.unwrap(), CatchUp::Rebuilt);
    clean(&w).await;
    assert_eq!(
        w.int("SELECT json_extract(state, '$.fold_version') FROM projections_session WHERE session_id = 's1'")
            .await,
        FOLD_VERSION as i64
    );
}

/// Une erreur de rattrapage n'écrit rien à moitié : le filigrane porte `dirty`, que
/// `doctor` relit.
#[tokio::test]
async fn a_failed_catch_up_marks_the_watermark_dirty() {
    let w = world().await;
    rich(&w).await;
    w.log
        .append(
            penelope_kernel::event::EventDraft::new(
                crate::journal::KIND_USER,
                serde_json::json!({"v": 99, "surface": {"op": "append"}}),
            )
            .session("s1"),
        )
        .await
        .unwrap();
    assert!(w.history().catch_up("s1").await.is_err());
    let dirty = w.history().dirty_projections().await.unwrap();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].0, "s1");
    assert!(dirty[0].1.contains("v99"), "{}", dirty[0].1);
}
