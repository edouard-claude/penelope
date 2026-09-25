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
use penelope_store::rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct HistoryStore {
    store: Store,
    pub(crate) clock: SharedClock,
    /// Journal de la double écriture (T5) ; absent, les lignes s'écrivent seules.
    pub(crate) events: Option<penelope_kernel::event::EventLog>,
    /// Surfaces déjà pliées, reprises sur les seuls événements nouveaux (T14).
    pub(crate) reads: crate::read::SharedReadCache,
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

/// Passage retrouvé par [`HistoryStore::grep_terms`], avec les mots qui l'ont trouvé.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedHit {
    #[serde(flatten)]
    pub hit: GrepHit,
    pub matched: Vec<String>,
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
        HistoryStore {
            store,
            clock,
            events: None,
            reads: Default::default(),
        }
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
        let prov = crate::journal::Provenance::default();
        self.append_as(
            session_id,
            message,
            tokens,
            episode,
            eager,
            artifact_id,
            &prov,
        )
        .await
    }

    /// Ajoute une seule fois un message reçu par la file, avec son horodatage
    /// original. L'ID du tour est la clé d'idempotence après crash (#161).
    pub async fn append_user_turn_at(
        &self,
        session_id: &str,
        turn_id: &str,
        text: &str,
        arrived_at: &str,
        tokens: u64,
        episode: i64,
    ) -> penelope_store::Result<i64> {
        let prov = crate::journal::Provenance::queued(
            crate::journal::UserSource::Owner,
            turn_id,
            arrived_at,
        );
        self.append_queued(session_id, &ChatMessage::user(text), tokens, episode, &prov)
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

    /// Les `limit` dernières entrées d'une session, dans l'ordre : la queue du transcript
    /// sans relire tout l'historique (issue #55).
    pub async fn tail(&self, session_id: &str, limit: usize) -> penelope_store::Result<Vec<Entry>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT seq, role, content, tool_call_id, tool_name, tokens_est, episode,
                            eager, artifact_id, compacted
                     FROM messages WHERE session_id = ?1 ORDER BY seq DESC LIMIT ?2",
                )?;
                let rows = st.query_map(params![sid, limit as i64], row_to_entry)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                out.reverse();
                Ok(out)
            })
            .await
    }

    /// Derniers résultats d'outils encore entiers d'une session, du plus ancien au plus
    /// récent : ce qu'un groupe d'appels parallèles vient d'écrire (issue #52). Un
    /// résultat déjà externalisé (`artifact_id`) est laissé de côté : l'admission reste
    /// idempotente.
    pub async fn recent_tool_results(
        &self,
        session_id: &str,
        limit: usize,
    ) -> penelope_store::Result<Vec<(i64, String)>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT seq, role, content, tool_call_id, tool_name
                     FROM messages
                     WHERE session_id = ?1 AND role = 'tool' AND artifact_id IS NULL
                     ORDER BY seq DESC LIMIT ?2",
                )?;
                let rows = st.query_map(params![sid, limit as i64], |r| {
                    let role: String = r.get(1)?;
                    let content: String = r.get(2)?;
                    let message = deserialise_content(
                        Role::parse(&role).unwrap_or(Role::Tool),
                        &content,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    );
                    Ok((r.get::<_, i64>(0)?, message.text()))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                out.reverse();
                Ok(out)
            })
            .await
    }

    /// Contextes figés d'une session, par numéro de message.
    pub async fn contexts(
        &self,
        session_id: &str,
    ) -> penelope_store::Result<std::collections::HashMap<i64, String>> {
        self.contexts_from(session_id, 0).await
    }

    /// Contextes figés des messages à partir de `from_seq` : ce que la projection
    /// réutilise vraiment (issue #55).
    pub async fn contexts_from(
        &self,
        session_id: &str,
        from_seq: i64,
    ) -> penelope_store::Result<std::collections::HashMap<i64, String>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT seq, context FROM message_context
                     WHERE session_id = ?1 AND seq >= ?2",
                )?;
                let rows = st.query_map(params![sid, from_seq], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut out = std::collections::HashMap::new();
                for r in rows {
                    let (seq, ctx): (i64, String) = r?;
                    out.insert(seq, ctx);
                }
                Ok(out)
            })
            .await
    }

    /// Messages d'un épisode, dans l'ordre.
    pub async fn load_episode(
        &self,
        session_id: &str,
        episode: i64,
    ) -> penelope_store::Result<Vec<Entry>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT seq, role, content, tool_call_id, tool_name, tokens_est, episode,
                            eager, artifact_id, compacted
                     FROM messages WHERE session_id = ?1 AND episode = ?2 ORDER BY seq",
                )?;
                let rows = st.query_map(params![sid, episode], row_to_entry)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
    }

    /// Dernier message d'une session.
    pub async fn last_entry(&self, session_id: &str) -> penelope_store::Result<Option<Entry>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT seq, role, content, tool_call_id, tool_name, tokens_est, episode,
                            eager, artifact_id, compacted
                     FROM messages WHERE session_id = ?1 ORDER BY seq DESC LIMIT 1",
                )?;
                let mut rows = st.query_map(params![sid], row_to_entry)?;
                Ok(rows.next().transpose()?)
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

pub(crate) fn serialise_content(m: &ChatMessage) -> penelope_store::Result<String> {
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
    let mut v = json!({"blocks": blocks, "tool_calls": calls});
    if let Some(r) = &m.reasoning {
        v["reasoning"] = json!(r);
    }
    if let Some(d) = &m.reasoning_details {
        v["reasoning_details"] = d.clone();
    }
    Ok(v.to_string())
}

pub(crate) fn deserialise_content(
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
        reasoning: v
            .get("reasoning")
            .and_then(|r| r.as_str())
            .map(String::from),
        reasoning_details: v.get("reasoning_details").filter(|d| !d.is_null()).cloned(),
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

mod dual;
mod rewrite;
pub mod seal;
mod search;
pub(crate) use dual::origin_in;
pub(crate) use rewrite::mark_compacted_in;
pub use search::{sanitise_fts, significant_terms};

#[cfg(test)]
mod tests;
