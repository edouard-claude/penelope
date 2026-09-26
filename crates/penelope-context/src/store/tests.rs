use super::*;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

async fn hs() -> HistoryStore {
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
    let events = penelope_kernel::event::EventLog::new(store.clone(), clock.clone());
    HistoryStore::new(store, clock, events)
}

#[tokio::test]
async fn reasoning_survives_a_reload() {
    let h = hs().await;
    let m = ChatMessage {
        reasoning: Some("je dois lire le fichier".into()),
        reasoning_details: Some(json!([{"type":"reasoning.text","text":"je dois","index":0}])),
        ..ChatMessage::assistant("")
    }
    .with_tool_calls(vec![ToolCall {
        id: "c1".into(),
        name: "fs_read".into(),
        arguments: json!({"path":"a.rs"}),
    }]);
    h.append("s1", &m, 10, 0, false, None).await.unwrap();
    let back = &h.load("s1", 0).await.unwrap()[0].message;
    assert_eq!(back.reasoning.as_deref(), Some("je dois lire le fichier"));
    assert_eq!(
        back.reasoning_details.as_ref().unwrap()[0]["text"],
        "je dois"
    );
    assert_eq!(back.tool_calls[0].id, "c1");
}

#[tokio::test]
async fn append_and_load_roundtrip() {
    let h = hs().await;
    h.append("s1", &ChatMessage::user("bonjour"), 10, 0, false, None)
        .await
        .unwrap();
    let call = ChatMessage::assistant("je lis").with_tool_calls(vec![ToolCall {
        id: "c1".into(),
        name: "fs_read".into(),
        arguments: json!({"path":"a.rs"}),
    }]);
    h.append("s1", &call, 20, 0, false, None).await.unwrap();
    h.append(
        "s1",
        &ChatMessage::tool_result("c1", "fs_read", "contenu"),
        30,
        0,
        true,
        None,
    )
    .await
    .unwrap();

    let entries = h.load("s1", 0).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].message.text(), "bonjour");
    assert_eq!(entries[1].message.tool_calls[0].name, "fs_read");
    assert_eq!(entries[2].message.tool_call_id.as_deref(), Some("c1"));
    assert!(entries[2].eager);
    assert!(crate::transcript::pairs_are_valid(
        &entries
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
    ));
}

/// #161 : le message venu de la file porte sa date de réception et n'est pas
/// dupliqué si le tour est rejoué après une écriture interrompue.
#[tokio::test]
async fn queued_user_message_keeps_arrival_time_and_is_idempotent() {
    let h = hs().await;
    let at = "2026-09-22T10:00:00.000Z";
    let prov = crate::journal::Provenance::queued(crate::journal::UserSource::Owner, "t1", at);
    let user = ChatMessage::user("bonjour");
    let first = h.append_queued("s1", &user, 10, 0, &prov).await.unwrap();
    assert!(h.recorded("s1", "t1").await.unwrap());
    assert!(!h.recorded("s1", "t2").await.unwrap());
    let again = h.append_queued("s1", &user, 10, 0, &prov).await.unwrap();
    assert_eq!(first, again);
    let entries = h.load("s1", 0).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message.text(), "bonjour");
    let timestamp: String = h
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT ts FROM messages WHERE source_turn_id='t1'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(timestamp, at);
}

#[tokio::test]
async fn fts_finds_messages_without_accents() {
    let h = hs().await;
    h.append(
        "s1",
        &ChatMessage::user("le déploiement a échoué en préproduction"),
        10,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    let hits = h.grep("deploiement", Some("s1"), 10).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].source, "raw");
}

#[tokio::test]
async fn fts_query_with_punctuation_does_not_fail() {
    let h = hs().await;
    h.append(
        "s1",
        &ChatMessage::user("erreur E0308 dans a.rs"),
        5,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    let hits = h
        .grep("\"E0308\" AND (a.rs)", Some("s1"), 10)
        .await
        .unwrap();
    assert!(
        !hits.is_empty(),
        "une requête bruitée doit quand même chercher"
    );
}

#[tokio::test]
async fn artifact_cursor_only_advances_over_returned_bytes() {
    let h = hs().await;
    let body = "0123456789".repeat(100); // 1000 octets
    let a = h
        .put_artifact(Some("s1"), None, "text", None, &body)
        .await
        .unwrap();
    let (chunk, cursor, done) = h.read_artifact(&a.id, 0, 400).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 400);
    assert_eq!(cursor, 400);
    assert!(!done);
    let (chunk2, cursor2, done2) = h.read_artifact(&a.id, cursor, 1000).await.unwrap().unwrap();
    assert_eq!(chunk2.len(), 600);
    assert_eq!(cursor2, 1000);
    assert!(done2);
}

#[tokio::test]
async fn artifact_read_never_splits_utf8() {
    let h = hs().await;
    let body = "éàü".repeat(100);
    let a = h
        .put_artifact(None, None, "text", None, &body)
        .await
        .unwrap();
    // 5 octets tombe au milieu d'un caractère multi-octets.
    let (chunk, cursor, _) = h.read_artifact(&a.id, 0, 5).await.unwrap().unwrap();
    assert!(
        !chunk.contains('\u{FFFD}'),
        "aucun caractère de remplacement"
    );
    assert!(cursor <= 5);
}

#[tokio::test]
async fn externalise_rewrites_the_canonical_body() {
    let h = hs().await;
    h.append(
        "s1",
        &ChatMessage::tool_result("c1", "t", "x".repeat(5000)),
        2000,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    let art = h
        .put_artifact(Some("s1"), None, "text", None, &"x".repeat(5000))
        .await
        .unwrap();
    h.externalise_as(
        "s1",
        1,
        &format!("[résultat externalisé — artefact {}]", art.id),
        &art,
        30,
        2000,
    )
    .await
    .unwrap();
    let e = h.load("s1", 0).await.unwrap();
    assert!(e[0].message.text().contains("externalisé"));
    assert_eq!(e[0].artifact_id.as_deref(), Some(art.id.as_str()));
    assert_eq!(e[0].tokens, 30);
}

#[test]
fn payload_description_is_typed() {
    assert!(describe_payload("json", r#"[{"a":1},{"a":2}]"#).contains("tableau de 2"));
    assert!(describe_payload("csv", "a,b,c\n1,2,3").contains("colonnes"));
    assert!(describe_payload("log", "info\nerror: x\nerror: y").contains("2 lignes d'erreur"));
}

#[test]
fn kind_guessing() {
    assert_eq!(guess_kind(r#"{"a":1}"#), "json");
    assert_eq!(guess_kind("a,b,c\n1,2,3\n4,5,6\n7,8,9\n1,1,1"), "csv");
    assert_eq!(guess_kind("<!DOCTYPE html><html>"), "html");
    assert_eq!(guess_kind("fn main() {}"), "code");
    assert_eq!(guess_kind("une phrase"), "text");
}

#[test]
fn fts_sanitiser_quotes_terms_and_drops_operators() {
    assert_eq!(sanitise_fts("a.rs OR erreur"), "\"rs\" \"erreur\"");
    assert_eq!(sanitise_fts("  "), "");
    assert_eq!(sanitise_fts("\"quoted\" AND (x)"), "\"quoted\"");
}
