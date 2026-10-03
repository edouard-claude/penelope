use super::search::TOOL_CALL_INDEX_CHARS;
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

fn shell(id: &str, command: &str) -> ChatMessage {
    ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
        id: id.into(),
        name: "shell_exec".into(),
        arguments: json!({"command": command, "cwd": "/w"}),
    }])
}

/// Les entrées plein texte d'une session, par numéro de message.
async fn fts_rows(h: &HistoryStore, sid: &str) -> Vec<(i64, String)> {
    let sid = sid.to_string();
    h.store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT m.seq, f.content FROM messages_fts f
                 JOIN messages m ON m.id = f.msg_id WHERE m.session_id = ?1 ORDER BY m.seq",
            )?;
            let rows = st.query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
}

/// #300 : un message assistant qui n'est qu'un appel d'outil s'indexait vide ; la commande
/// se retrouve maintenant par un mot de ses arguments, avec le nom de l'outil dans
/// l'extrait, et le résultat de l'outil reste indexé à part.
#[tokio::test]
async fn fts_finds_a_tool_call_by_a_word_of_its_arguments() {
    let h = hs().await;
    h.append(
        "s1",
        &shell(
            "c1",
            "ffmpeg -i in.wav -af acompressor=threshold=-18dB,loudnorm=I=-16 out.wav",
        ),
        20,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    h.append(
        "s1",
        &ChatMessage::tool_result("c1", "shell_exec", "rendu écrit"),
        5,
        0,
        true,
        None,
    )
    .await
    .unwrap();
    let hits = h.grep("acompressor", Some("s1"), 10).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!((hits[0].seq, hits[0].role.as_str()), (1, "assistant"));
    assert!(
        hits[0].excerpt.contains("acompressor"),
        "{}",
        hits[0].excerpt
    );
    let rows = fts_rows(&h, "s1").await;
    assert!(
        rows[0]
            .1
            .starts_with("shell_exec command: ffmpeg -i in.wav"),
        "{}",
        rows[0].1
    );
    assert!(rows[0].1.ends_with("out.wav cwd: /w"), "{}", rows[0].1);
    let loud = h.grep("loudnorm=I", Some("s1"), 10).await.unwrap();
    assert_eq!(loud.len(), 1, "{loud:?}");
    assert_eq!(h.grep("rendu", Some("s1"), 10).await.unwrap()[0].seq, 2);
}

/// Le corps d'un `fs_write` n'entre pas dans l'index (son chemin si), un jeton dans une
/// commande est masqué, et un appel démesuré est coupé à son plafond.
#[tokio::test]
async fn tool_call_index_skips_file_bodies_masks_secrets_and_caps() {
    let h = hs().await;
    let write = ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
        id: "c1".into(),
        name: "fs_write".into(),
        arguments: json!({"path": "notes/plan.md", "content": "framboise ".repeat(50)}),
    }]);
    h.append("s1", &write, 20, 0, false, None).await.unwrap();
    h.append(
        "s1",
        &shell(
            "c2",
            "curl -H 'Authorization: Bearer ghp_0123456789abcdefghijABCDEFGHIJ012345' https://x.example",
        ),
        20,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    let huge = ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
        id: "c3".into(),
        name: "fs_search".into(),
        arguments: json!({"pattern": "x".repeat(5_000)}),
    }]);
    h.append("s1", &huge, 20, 0, false, None).await.unwrap();

    assert!(
        h.grep("framboise", Some("s1"), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.grep("plan.md", Some("s1"), 10).await.unwrap().len(), 1);
    assert!(
        h.grep("ghp_0123456789abcdefghijABCDEFGHIJ012345", Some("s1"), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.grep("curl", Some("s1"), 10).await.unwrap().len(), 1);
    let rows = fts_rows(&h, "s1").await;
    assert!(!rows[1].1.contains("ghp_0123"), "{}", rows[1].1);
    assert_eq!(
        rows[2].1.chars().count(),
        TOOL_CALL_INDEX_CHARS + 1,
        "coupé, puis « … »"
    );
}

/// `rebuild_fts` redonne les mêmes entrées que la double écriture, appels compris et texte
/// d'origine d'un corps externalisé compris : c'est lui que la marque de la migration 0026
/// fait tourner sur les lignes d'avant #300.
#[tokio::test]
async fn rebuild_fts_gives_the_same_entries_as_the_direct_write() {
    let h = hs().await;
    h.append("s1", &ChatMessage::user("fais le rendu"), 5, 0, false, None)
        .await
        .unwrap();
    h.append(
        "s1",
        &shell(
            "c1",
            "ffmpeg -i in.wav -af acompressor=threshold=-18dB out.wav",
        ),
        20,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    let body = "ligne du rendu\n".repeat(200);
    let seq = h
        .append(
            "s1",
            &ChatMessage::tool_result("c1", "shell_exec", &body),
            800,
            0,
            true,
            None,
        )
        .await
        .unwrap();
    let artifact = h
        .put_artifact(Some("s1"), None, "text", None, &body)
        .await
        .unwrap();
    h.externalise_as("s1", seq, "[externalisé]", &artifact, 5, 800)
        .await
        .unwrap();
    let before = fts_rows(&h, "s1").await;
    assert!(before[1].1.contains("acompressor"), "{}", before[1].1);
    assert!(before[2].1.contains("ligne du rendu"), "{}", before[2].1);

    h.store
        .write(|tx| {
            tx.execute("UPDATE messages_fts SET content = 'périmé'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(h.rebuild_fts().await.unwrap(), 3);
    assert_eq!(fts_rows(&h, "s1").await, before);
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

/// Images et audio survivent à l'écriture et à la relecture d'un message ; un bloc
/// inconnu ou un contenu illisible ne font pas échouer la lecture.
#[test]
fn image_and_audio_blocks_round_trip() {
    let mut m = ChatMessage::user("regarde");
    m.content.push(Content::ImageUrl {
        url: "data:image/png;base64,AAA".into(),
        detail: Some("low".into()),
    });
    m.content.push(Content::InputAudio {
        data: "T2dn".into(),
        format: "ogg".into(),
    });
    let raw = serialise_content(&m).unwrap();
    let back = deserialise_content(Role::User, &raw, None, None);
    assert_eq!(back.content, m.content);

    let odd = r#"{"blocks":[{"type":"hologramme"},{"type":"input_audio","data":"x"}]}"#;
    let back = deserialise_content(Role::User, odd, None, None);
    assert_eq!(
        back.content,
        vec![Content::InputAudio {
            data: "x".into(),
            format: "wav".into()
        }]
    );
    assert!(
        deserialise_content(Role::User, "pas du json", None, None)
            .content
            .is_empty()
    );
}

/// Un message système n'entre pas dans l'historique : le préfixe a son propre
/// événement (`conv.system`).
#[tokio::test]
async fn a_system_message_is_refused_by_the_history() {
    let h = hs().await;
    let e = h
        .append("s1", &ChatMessage::system("règles"), 3, 0, false, None)
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("n'entre pas dans l'historique"),
        "{e}"
    );
}

/// La requête d'une recherche dans l'historique n'entre pas dans l'index : sinon chaque
/// `history_grep` se retrouvait lui-même, en tête des résultats.
#[tokio::test]
async fn a_history_search_does_not_index_its_own_query() {
    let h = hs().await;
    let search = ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
        id: "c1".into(),
        name: "history_grep".into(),
        arguments: json!({"query": "acompressor", "scope": "all"}),
    }]);
    h.append("s1", &search, 10, 0, false, None).await.unwrap();
    assert!(
        h.grep("acompressor", Some("s1"), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fts_rows(&h, "s1").await[0].1, "history_grep scope: all");
}
