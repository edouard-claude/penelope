//! Persistance de l'historique canonique et des artefacts (§5.5, §18).
//!
//! **Store immuable** : chaque message, appel et résultat d'outil est persisté verbatim
//! et indexé en FTS5. Les payloads volumineux partent en artefact et ne sont jamais
//! chargés directement dans le contexte.

use crate::transcript::Entry;
use penelope_kernel::clock::SharedClock;
use penelope_kernel::ids::ArtifactId;
use penelope_llm::types::{ChatMessage, Content, Role, ToolCall};
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct HistoryStore {
    store: Store,
    clock: SharedClock,
}

/// Résultat d'une recherche FTS sur l'historique (`history_grep`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrepHit {
    pub session_id: String,
    pub seq: i64,
    pub role: String,
    pub excerpt: String,
    /// `raw` pour un message brut, `summary` pour un nœud LCM.
    pub source: String,
    pub node_id: Option<String>,
}

/// Artefact externalisé (§5.5 « payloads volumineux »).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub kind: String,
    pub media_type: Option<String>,
    pub filename: Option<String>,
    pub bytes: u64,
    pub head: String,
    pub tail: String,
    pub summary: String,
    pub sha256: String,
}

impl HistoryStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        HistoryStore { store, clock }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Ajoute un message à l'historique canonique et à l'index FTS.
    pub async fn append(
        &self,
        session_id: &str,
        message: &ChatMessage,
        tokens: u64,
        episode: i64,
        eager: bool,
        artifact_id: Option<String>,
    ) -> penelope_store::Result<i64> {
        let sid = session_id.to_string();
        let content = serialise_content(message)?;
        let searchable = message.text();
        let ts = self.clock.now_rfc3339();
        let role = message.role.as_str().to_string();
        let tool_call_id = message.tool_call_id.clone();
        let tool_name = message.name.clone();

        self.store
            .write(move |tx| {
                let seq: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
                        [&sid],
                        |r| r.get(0),
                    )
                    .unwrap_or(1);
                tx.execute(
                    "INSERT INTO messages(session_id, seq, role, content, tool_call_id, tool_name,
                        tokens_est, ts, episode, eager, artifact_id)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        sid,
                        seq,
                        role,
                        content,
                        tool_call_id,
                        tool_name,
                        tokens as i64,
                        ts,
                        episode,
                        eager as i64,
                        artifact_id
                    ],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO messages_fts(content, session_id, msg_id) VALUES(?1,?2,?3)",
                    params![searchable, sid, id],
                )?;
                Ok(seq)
            })
            .await
    }

    /// Charge l'historique canonique d'une session.
    pub async fn load(
        &self,
        session_id: &str,
        from_seq: i64,
    ) -> penelope_store::Result<Vec<Entry>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT seq, role, content, tool_call_id, tool_name, tokens_est, episode,
                            eager, artifact_id, compacted
                     FROM messages WHERE session_id = ?1 AND seq >= ?2 ORDER BY seq",
                )?;
                let rows = st.query_map(params![sid, from_seq], row_to_entry)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
    }

    /// Marque une plage de séquences comme couverte par un nœud de résumé.
    pub async fn mark_compacted(
        &self,
        session_id: &str,
        from_seq: i64,
        to_seq: i64,
    ) -> penelope_store::Result<usize> {
        let sid = session_id.to_string();
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE messages SET compacted = 1
                     WHERE session_id = ?1 AND seq >= ?2 AND seq <= ?3",
                    params![sid, from_seq, to_seq],
                )?)
            })
            .await
    }

    /// Remplace le corps d'un message par sa version externalisée (niveau 1, canonique).
    pub async fn externalise(
        &self,
        session_id: &str,
        seq: i64,
        new_body: &str,
        artifact_id: &str,
        new_tokens: u64,
    ) -> penelope_store::Result<()> {
        let (sid, body, art) = (
            session_id.to_string(),
            new_body.to_string(),
            artifact_id.to_string(),
        );
        self.store
            .write(move |tx| {
                let content: String = tx.query_row(
                    "SELECT content FROM messages WHERE session_id=?1 AND seq=?2",
                    params![sid, seq],
                    |r| r.get(0),
                )?;
                let mut v: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| json!({"blocks": []}));
                v["blocks"] = json!([{"type":"text","text": body}]);
                tx.execute(
                    "UPDATE messages SET content=?3, artifact_id=?4, tokens_est=?5
                     WHERE session_id=?1 AND seq=?2",
                    params![sid, seq, v.to_string(), art, new_tokens as i64],
                )?;
                Ok(())
            })
            .await
    }

    /// Recherche plein texte sur les messages bruts et sur les résumés (`history_grep`).
    pub async fn grep(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: i64,
    ) -> penelope_store::Result<Vec<GrepHit>> {
        let q = sanitise_fts(query);
        let sid = session_id.map(String::from);
        self.store
            .read(move |c| {
                let mut out = Vec::new();
                if q.is_empty() {
                    return Ok(out);
                }
                // 1. Messages bruts.
                let sql =
                    "SELECT m.session_id, m.seq, m.role, snippet(messages_fts, 0, '', '', '…', 12)
                           FROM messages_fts f
                           JOIN messages m ON m.id = f.msg_id
                           WHERE messages_fts MATCH ?1
                             AND (?2 IS NULL OR m.session_id = ?2)
                           ORDER BY rank LIMIT ?3";
                let mut st = c.prepare(sql)?;
                let rows = st.query_map(params![q, sid, limit], |r| {
                    Ok(GrepHit {
                        session_id: r.get(0)?,
                        seq: r.get(1)?,
                        role: r.get(2)?,
                        excerpt: r.get(3)?,
                        source: "raw".into(),
                        node_id: None,
                    })
                })?;
                for r in rows {
                    out.push(r?);
                }

                // 2. Résumés LCM (recherche simple : ils sont peu nombreux).
                let mut st = c.prepare(
                    "SELECT id, session_id, from_seq, summary FROM lcm_nodes
                     WHERE (?2 IS NULL OR session_id = ?2) AND superseded_by IS NULL
                     ORDER BY level DESC, created_at DESC LIMIT 200",
                )?;
                let needle = q.to_lowercase();
                let rows = st.query_map(params![q, sid], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?;
                for r in rows {
                    let (id, session, from_seq, summary) = r?;
                    if summary.to_lowercase().contains(&needle) {
                        out.push(GrepHit {
                            session_id: session,
                            seq: from_seq.unwrap_or(0),
                            role: "summary".into(),
                            excerpt: excerpt_around(&summary, &needle, 160),
                            source: "summary".into(),
                            node_id: Some(id),
                        });
                    }
                }
                Ok(out)
            })
            .await
    }

    // ------------------------------------------------------------- artefacts

    /// Externalise un payload volumineux.
    pub async fn put_artifact(
        &self,
        session_id: Option<&str>,
        run_id: Option<&str>,
        kind: &str,
        filename: Option<&str>,
        body: &str,
    ) -> penelope_store::Result<Artifact> {
        let id = ArtifactId::new().0;
        let sha = penelope_kernel::canonical::sha256_hex(body.as_bytes());
        let (head, tail) = crate::transcript::head_tail(body, 800, 400);
        let summary = describe_payload(kind, body);
        let a = Artifact {
            id: id.clone(),
            kind: kind.to_string(),
            media_type: None,
            filename: filename.map(String::from),
            bytes: body.len() as u64,
            head,
            tail,
            summary,
            sha256: sha,
        };
        let row = a.clone();
        let (sid, rid, ts, content) = (
            session_id.map(String::from),
            run_id.map(String::from),
            self.clock.now_rfc3339(),
            body.to_string(),
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO artifacts(id, session_id, run_id, kind, filename, bytes,
                        inline, head, tail, summary, sha256, created_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        row.id,
                        sid,
                        rid,
                        row.kind,
                        row.filename,
                        row.bytes as i64,
                        content.as_bytes(),
                        row.head,
                        row.tail,
                        row.summary,
                        row.sha256,
                        ts
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(a)
    }

    /// Lecture paginée d'un artefact (`artifact_read`).
    ///
    /// **Le curseur ne franchit que les octets effectivement renvoyés** : un appel qui
    /// renvoie 4 000 octets avance le curseur de 4 000, jamais plus.
    pub async fn read_artifact(
        &self,
        id: &str,
        cursor: u64,
        max_bytes: u64,
    ) -> penelope_store::Result<Option<(String, u64, bool)>> {
        let id = id.to_string();
        self.store
            .read(move |c| {
                let raw: Option<Vec<u8>> = c
                    .query_row("SELECT inline FROM artifacts WHERE id = ?1", [&id], |r| {
                        r.get(0)
                    })
                    .ok();
                let Some(raw) = raw else { return Ok(None) };
                let start = (cursor as usize).min(raw.len());
                let mut end = (start + max_bytes as usize).min(raw.len());
                // Ne pas couper au milieu d'un caractère UTF-8.
                while end > start && end < raw.len() && (raw[end] & 0xC0) == 0x80 {
                    end -= 1;
                }
                let slice = String::from_utf8_lossy(&raw[start..end]).to_string();
                let advanced = (end - start) as u64;
                Ok(Some((slice, cursor + advanced, end >= raw.len())))
            })
            .await
    }

    pub async fn get_artifact(&self, id: &str) -> penelope_store::Result<Option<Artifact>> {
        let id = id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, kind, media_type, filename, bytes, head, tail, summary, sha256
                     FROM artifacts WHERE id = ?1",
                )?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(Artifact {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        media_type: r.get(2)?,
                        filename: r.get(3)?,
                        bytes: r.get::<_, i64>(4)? as u64,
                        head: r.get(5)?,
                        tail: r.get(6)?,
                        summary: r.get(7)?,
                        sha256: r.get(8)?,
                    })),
                    None => Ok(None),
                }
            })
            .await
    }
}

/// Résumé d'exploration typé d'un payload (§5.5).
pub fn describe_payload(kind: &str, body: &str) -> String {
    let lines = body.lines().count();
    let bytes = body.len();
    match kind {
        "json" => match serde_json::from_str::<Value>(body) {
            Ok(Value::Array(a)) => format!(
                "JSON : tableau de {} éléments, {bytes} octets. Clés du premier élément : {}",
                a.len(),
                a.first()
                    .and_then(|x| x.as_object())
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
                    .unwrap_or_else(|| "—".into())
            ),
            Ok(Value::Object(o)) => format!(
                "JSON : objet de {} clés ({}), {bytes} octets.",
                o.len(),
                o.keys().take(12).cloned().collect::<Vec<_>>().join(", ")
            ),
            _ => format!("JSON : {bytes} octets."),
        },
        "csv" => {
            let header = body.lines().next().unwrap_or("");
            format!(
                "CSV : {lines} lignes, colonnes : {}",
                header.split(',').take(20).collect::<Vec<_>>().join(", ")
            )
        }
        "log" => {
            let errors = body
                .lines()
                .filter(|l| {
                    let l = l.to_lowercase();
                    l.contains("error") || l.contains("erreur") || l.contains("fatal")
                })
                .count();
            format!("Log : {lines} lignes, {errors} lignes d'erreur, {bytes} octets.")
        }
        "code" => format!("Code : {lines} lignes, {bytes} octets."),
        "html" => format!("HTML : {bytes} octets, {lines} lignes."),
        _ => format!("Texte : {lines} lignes, {bytes} octets."),
    }
}

/// Devine le type d'un payload à partir de son contenu.
pub fn guess_kind(body: &str) -> &'static str {
    let t = body.trim_start();
    if (t.starts_with('{') || t.starts_with('[')) && serde_json::from_str::<Value>(t).is_ok() {
        return "json";
    }
    if t.starts_with("<!DOCTYPE html") || t.starts_with("<html") {
        return "html";
    }
    let first = t.lines().next().unwrap_or("");
    if first.matches(',').count() >= 2 && t.lines().take(5).all(|l| l.contains(',')) {
        return "csv";
    }
    let lower = t.to_lowercase();
    if lower.contains("error") || lower.contains("warn") || lower.contains("[info]") {
        return "log";
    }
    if t.contains("fn ") || t.contains("function ") || t.contains("class ") || t.contains("def ") {
        return "code";
    }
    "text"
}

fn serialise_content(m: &ChatMessage) -> penelope_store::Result<String> {
    let blocks: Vec<Value> = m
        .content
        .iter()
        .map(|c| match c {
            Content::Text { text } => json!({"type":"text","text":text}),
            Content::ImageUrl { url, detail } => {
                json!({"type":"image_url","url":url,"detail":detail})
            }
            Content::InputAudio { data, format } => {
                json!({"type":"input_audio","data":data,"format":format})
            }
        })
        .collect();
    let calls: Vec<Value> = m
        .tool_calls
        .iter()
        .map(|t| json!({"id":t.id,"name":t.name,"arguments":t.arguments}))
        .collect();
    Ok(json!({"blocks": blocks, "tool_calls": calls}).to_string())
}

fn deserialise_content(
    role: Role,
    raw: &str,
    tool_call_id: Option<String>,
    name: Option<String>,
) -> ChatMessage {
    let v: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({"blocks":[]}));
    let content = v
        .get("blocks")
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => Some(Content::text(
                        b.get("text").and_then(|t| t.as_str()).unwrap_or_default(),
                    )),
                    Some("image_url") => Some(Content::ImageUrl {
                        url: b
                            .get("url")
                            .and_then(|t| t.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        detail: b.get("detail").and_then(|t| t.as_str()).map(String::from),
                    }),
                    Some("input_audio") => Some(Content::InputAudio {
                        data: b
                            .get("data")
                            .and_then(|t| t.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        format: b
                            .get("format")
                            .and_then(|t| t.as_str())
                            .unwrap_or("wav")
                            .to_string(),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let tool_calls = v
        .get("tool_calls")
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    Some(ToolCall {
                        id: t.get("id")?.as_str()?.to_string(),
                        name: t.get("name")?.as_str()?.to_string(),
                        arguments: t.get("arguments").cloned().unwrap_or(json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    ChatMessage {
        role,
        content,
        tool_calls,
        tool_call_id,
        name,
        cache_marker: false,
    }
}

fn row_to_entry(r: &penelope_store::rusqlite::Row<'_>) -> penelope_store::rusqlite::Result<Entry> {
    let role: String = r.get(1)?;
    let content: String = r.get(2)?;
    let tool_call_id: Option<String> = r.get(3)?;
    let tool_name: Option<String> = r.get(4)?;
    Ok(Entry {
        seq: r.get(0)?,
        message: deserialise_content(
            Role::parse(&role).unwrap_or(Role::User),
            &content,
            tool_call_id,
            tool_name,
        ),
        tokens: r.get::<_, i64>(5)? as u64,
        episode: r.get(6)?,
        eager: r.get::<_, i64>(7)? != 0,
        artifact_id: r.get(8)?,
        compacted: r.get::<_, i64>(9)? != 0,
    })
}

/// Neutralise la syntaxe FTS5 d'une requête utilisateur : une recherche ne doit jamais
/// échouer parce que le texte contient un guillemet ou un opérateur.
pub fn sanitise_fts(q: &str) -> String {
    let cleaned: String = q
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        // Les opérateurs FTS5 écrits en clair sont retirés plutôt que cités : sinon une
        // requête « E0308 AND a.rs » exigerait le mot « AND » dans le document.
        .filter(|w| !matches!(*w, "AND" | "OR" | "NOT" | "NEAR"))
        .filter(|w| w.chars().count() > 1)
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

fn excerpt_around(text: &str, needle: &str, width: usize) -> String {
    let lower = text.to_lowercase();
    let pos = lower.find(needle).unwrap_or(0);
    let start = text[..pos]
        .char_indices()
        .rev()
        .nth(width / 2)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = text[pos..]
        .char_indices()
        .nth(width)
        .map(|(i, _)| pos + i)
        .unwrap_or(text.len());
    text[start..end].replace('\n', " ")
}

#[cfg(test)]
mod tests {
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
        HistoryStore::new(store, Arc::new(TestClock::default()))
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
        h.externalise(
            "s1",
            1,
            "[résultat externalisé — artefact art_1]",
            "art_1",
            30,
        )
        .await
        .unwrap();
        let e = h.load("s1", 0).await.unwrap();
        assert!(e[0].message.text().contains("externalisé"));
        assert_eq!(e[0].artifact_id.as_deref(), Some("art_1"));
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
}
