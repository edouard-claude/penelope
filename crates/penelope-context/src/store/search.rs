use super::*;

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

impl HistoryStore {
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
