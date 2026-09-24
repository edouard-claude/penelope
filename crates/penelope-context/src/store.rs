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
    clock: SharedClock,
    /// Journal de la double écriture (T5) ; absent, les lignes s'écrivent seules.
    events: Option<penelope_kernel::event::EventLog>,
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

/// Mots vides retirés d'une question avant la recherche : sans eux, une phrase
/// entière ne correspond à rien, puisque le plein texte exige chaque mot.
const STOPWORDS: &[&str] = &[
    "au",
    "aux",
    "avec",
    "avait",
    "avais",
    "avions",
    "avoir",
    "ai",
    "as",
    "avons",
    "avez",
    "ont",
    "ce",
    "ces",
    "cet",
    "cette",
    "ceci",
    "cela",
    "ça",
    "comme",
    "comment",
    "dans",
    "de",
    "des",
    "du",
    "déjà",
    "donc",
    "dont",
    "elle",
    "elles",
    "en",
    "est",
    "et",
    "été",
    "être",
    "était",
    "étaient",
    "il",
    "ils",
    "je",
    "la",
    "le",
    "les",
    "leur",
    "leurs",
    "lui",
    "ma",
    "mais",
    "me",
    "mes",
    "moi",
    "mon",
    "ne",
    "nos",
    "notre",
    "nous",
    "on",
    "ou",
    "où",
    "par",
    "pas",
    "plus",
    "pour",
    "pourquoi",
    "quand",
    "que",
    "quel",
    "quelle",
    "quelles",
    "quels",
    "qui",
    "quoi",
    "sa",
    "sans",
    "se",
    "ses",
    "si",
    "son",
    "sont",
    "sur",
    "ta",
    "te",
    "tes",
    "toi",
    "ton",
    "tu",
    "un",
    "une",
    "vos",
    "votre",
    "vous",
    "parlé",
    "parler",
    "parlions",
    "discuté",
    "dit",
    "session",
    "sessions",
    "précédente",
    "précédent",
    "dernière",
    "dernier",
    "fois",
    "chose",
    "truc",
    "retrouve",
    "retrouver",
    "souviens",
    "rappelle",
    "vers",
    "pendant",
    "faut",
    "fait",
    "faire",
    "doit",
    "doivent",
    "peut",
    "peux",
    "bien",
    "aussi",
    "alors",
    "encore",
    "très",
    "tout",
    "tous",
    "toute",
    "toutes",
    "rien",
    "oui",
    "non",
    "merci",
    "the",
    "a",
    "an",
    "and",
    "or",
    "of",
    "to",
    "in",
    "on",
    "for",
    "with",
    "about",
    "was",
    "were",
    "is",
    "are",
    "we",
    "you",
    "it",
    "that",
    "this",
    "what",
    "which",
    "who",
    "how",
    "when",
    "why",
    "did",
    "do",
    "does",
    "had",
    "have",
    "has",
    "be",
    "been",
];

/// Mots significatifs d'une question en langage naturel, dans l'ordre, sans doublon.
pub fn significant_terms(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Les apostrophes coupent aussi : « d'un projet » donne « d », « un », « projet ».
    for raw in question.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')) {
        let w = raw.trim_matches(['-', '_']).to_lowercase();
        if w.chars().count() < 2 || STOPWORDS.contains(&w.as_str()) || out.contains(&w) {
            continue;
        }
        out.push(w);
    }
    out
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

    /// Reconstruit l'index plein texte des messages depuis l'historique canonique.
    pub async fn rebuild_fts(&self) -> penelope_store::Result<usize> {
        self.store
            .write(|tx| {
                tx.execute("DELETE FROM messages_fts", [])?;
                let mut st = tx.prepare("SELECT id, session_id, role, content FROM messages")?;
                let rows: Vec<(i64, String, String, String)> = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<Result<_, _>>()?;
                drop(st);
                let mut n = 0;
                for (id, sid, role, content) in rows {
                    let message = deserialise_content(
                        Role::parse(&role).unwrap_or(Role::User),
                        &content,
                        None,
                        None,
                    );
                    tx.execute(
                        "INSERT INTO messages_fts(content, session_id, msg_id) VALUES(?1,?2,?3)",
                        params![message.text(), sid, id],
                    )?;
                    n += 1;
                }
                Ok(n)
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

                // 2. Résumés LCM (recherche simple : ils sont peu nombreux). Comme en plein
                // texte, tous les mots doivent y être.
                let mut st = c.prepare(
                    "SELECT id, session_id, from_seq, summary FROM lcm_nodes
                     WHERE (?2 IS NULL OR session_id = ?2) AND superseded_by IS NULL
                     ORDER BY level DESC, created_at DESC LIMIT 200",
                )?;
                let words: Vec<String> = q
                    .split_whitespace()
                    .map(|w| w.trim_matches('"').to_lowercase())
                    .filter(|w| !w.is_empty())
                    .collect();
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
                    let lower = summary.to_lowercase();
                    if words.iter().all(|w| lower.contains(w.as_str())) {
                        out.push(GrepHit {
                            session_id: session,
                            seq: from_seq.unwrap_or(0),
                            role: "summary".into(),
                            excerpt: excerpt_around(&summary, &words[0], 160),
                            source: "summary".into(),
                            node_id: Some(id),
                        });
                    }
                }
                Ok(out)
            })
            .await
    }

    /// Recherche mot par mot (`history_expand_query`) : un passage par message, classé
    /// par nombre de mots trouvés, puis du plus récent au plus ancien.
    pub async fn grep_terms(
        &self,
        terms: &[String],
        session_id: Option<&str>,
        per_term: i64,
        limit: usize,
    ) -> penelope_store::Result<Vec<RankedHit>> {
        let mut found: BTreeMap<(String, i64, String), RankedHit> = BTreeMap::new();
        for term in terms {
            for hit in self.grep(term, session_id, per_term).await? {
                let key = (hit.session_id.clone(), hit.seq, hit.source.clone());
                let entry = found.entry(key).or_insert_with(|| RankedHit {
                    hit,
                    matched: Vec::new(),
                });
                if !entry.matched.contains(term) {
                    entry.matched.push(term.clone());
                }
            }
        }
        let mut ranked: Vec<RankedHit> = found.into_values().collect();
        // Les identifiants de session sont des ULID : l'ordre lexical suit le temps.
        ranked.sort_by(|a, b| {
            b.matched
                .len()
                .cmp(&a.matched.len())
                .then_with(|| b.hit.session_id.cmp(&a.hit.session_id))
                .then_with(|| b.hit.seq.cmp(&a.hit.seq))
        });
        ranked.truncate(limit);
        Ok(ranked)
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
    let mut v = json!({"blocks": blocks, "tool_calls": calls});
    if let Some(r) = &m.reasoning {
        v["reasoning"] = json!(r);
    }
    if let Some(d) = &m.reasoning_details {
        v["reasoning_details"] = d.clone();
    }
    Ok(v.to_string())
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

mod dual;
mod rewrite;
pub mod seal;
#[cfg(test)]
mod tests;
