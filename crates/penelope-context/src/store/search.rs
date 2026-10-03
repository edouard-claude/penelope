use super::*;
use crate::derive::tool_node;
use crate::journal::ConvEvent;
use penelope_store::rusqlite::Connection;

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

/// Ce que l'index plein texte garde d'un appel d'outil, en caractères : son nom et ses
/// arguments textuels aplatis. Une commande `shell_exec` et ses options tiennent
/// largement ; au-delà, l'appel est coupé et `history_expand` le rend en entier (#300).
pub(crate) const TOOL_CALL_INDEX_CHARS: usize = 2_000;

/// Arguments qu'un appel n'apporte pas à l'index : le corps d'un fichier écrit est
/// volumineux et vit dans le fichier (le chemin qui l'a produit reste indexé) ; la requête
/// d'une recherche dans l'historique se retrouverait elle-même à chaque recherche.
const UNINDEXED_ARGUMENTS: &[(&str, &str)] = &[
    ("fs_write", "content"),
    ("history_grep", "query"),
    ("history_expand_query", "question"),
];

/// Le texte qu'un message apporte au plein texte : le sien, puis chaque appel d'outil sur
/// sa ligne (`outil clé: valeur …`), secrets masqués par le rédacteur et coupé à
/// [`TOOL_CALL_INDEX_CHARS`]. Jusqu'à #300, un message assistant qui n'était qu'un appel
/// d'outil entrait dans l'index avec une chaîne vide : la commande qu'il portait était
/// introuvable une fois la conversation résumée.
pub(crate) fn searchable_text(m: &ChatMessage) -> String {
    let mut out = m.text();
    for call in &m.tool_calls {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&tool_call_index_line(call));
    }
    out
}

/// Une ligne d'index pour un appel : son nom, puis ses arguments aplatis.
fn tool_call_index_line(call: &ToolCall) -> String {
    let mut parts = vec![call.name.clone()];
    flatten_arguments(&call.name, "", &call.arguments, &mut parts);
    let joined = penelope_observe::redact::redact(&parts.join(" "));
    match joined.char_indices().nth(TOOL_CALL_INDEX_CHARS) {
        Some((i, _)) => format!("{}…", &joined[..i]),
        None => joined,
    }
}

/// Les feuilles d'un objet d'arguments, `chemin.de.clé: valeur`, sans celles que
/// [`UNINDEXED_ARGUMENTS`] écarte pour cet outil.
fn flatten_arguments(tool: &str, key: &str, v: &Value, out: &mut Vec<String>) {
    let labelled = |value: String| {
        if key.is_empty() {
            value
        } else {
            format!("{key}: {value}")
        }
    };
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                let path = if key.is_empty() {
                    k.clone()
                } else {
                    format!("{key}.{k}")
                };
                if UNINDEXED_ARGUMENTS.contains(&(tool, path.as_str())) {
                    continue;
                }
                flatten_arguments(tool, &path, v, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                flatten_arguments(tool, key, item, out);
            }
        }
        Value::String(s) => {
            let s = s.trim();
            if !s.is_empty() {
                out.push(labelled(s.to_string()));
            }
        }
        Value::Number(n) => out.push(labelled(n.to_string())),
        Value::Bool(b) => out.push(labelled(b.to_string())),
        Value::Null => {}
    }
}

/// Le texte que le plein texte indexe pour une ligne : [`searchable_text`] du message, sauf
/// pour un corps externalisé (niveau 1), dont l'index garde le texte d'origine, celui de
/// l'événement d'ajout (`externalise` ne touche pas `messages_fts`). Le projecteur, la
/// double écriture et `rebuild_fts` passent tous ici : un même message donne la même
/// entrée, quel que soit le chemin qui l'écrit.
pub(crate) fn searchable(
    c: &Connection,
    m: &ChatMessage,
    artifact_id: Option<&str>,
    event_id: Option<i64>,
) -> penelope_store::Result<String> {
    let original = match (artifact_id, event_id) {
        (Some(_), Some(id)) => c
            .query_row(
                "SELECT kind, payload FROM events WHERE id = ?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .and_then(|(kind, payload)| {
                let value: Value = serde_json::from_str(&payload).ok()?;
                match ConvEvent::decode(&kind, &value).ok()?? {
                    ConvEvent::ToolResult(p) => Some(tool_node(p).message.text()),
                    _ => None,
                }
            }),
        _ => None,
    };
    Ok(original.unwrap_or_else(|| searchable_text(m)))
}

/// Une ligne de `messages` relue pour refaire son entrée plein texte.
struct Stored {
    id: i64,
    sid: String,
    role: String,
    content: String,
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    artifact_id: Option<String>,
    event_id: Option<i64>,
}

impl HistoryStore {
    /// Reconstruit l'index plein texte des messages depuis l'historique canonique, lignes
    /// scellées comprises, avec la même entrée que le projecteur ([`searchable`]) : c'est
    /// ce qui met les appels d'outils des lignes d'avant #300 dans l'index.
    pub async fn rebuild_fts(&self) -> penelope_store::Result<usize> {
        self.store
            .write(|tx| {
                tx.execute("DELETE FROM messages_fts", [])?;
                let mut st = tx.prepare(
                    "SELECT id, session_id, role, content, tool_call_id, tool_name, artifact_id,
                            event_id
                     FROM messages",
                )?;
                let rows: Vec<Stored> = st
                    .query_map([], |r| {
                        Ok(Stored {
                            id: r.get(0)?,
                            sid: r.get(1)?,
                            role: r.get(2)?,
                            content: r.get(3)?,
                            tool_call_id: r.get(4)?,
                            tool_name: r.get(5)?,
                            artifact_id: r.get(6)?,
                            event_id: r.get(7)?,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                drop(st);
                let mut n = 0;
                for row in rows {
                    let message = deserialise_content(
                        Role::parse(&row.role).unwrap_or(Role::User),
                        &row.content,
                        row.tool_call_id,
                        row.tool_name,
                    );
                    let text = searchable(tx, &message, row.artifact_id.as_deref(), row.event_id)?;
                    tx.execute(
                        "INSERT INTO messages_fts(content, session_id, msg_id) VALUES(?1,?2,?3)",
                        params![text, row.sid, row.id],
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
                             AND m.sealed != 2
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
