use super::*;

impl ToolRegistry {
    /// `tool_search` avec le vecteur de la requête : les outils proches par le sens
    /// complètent ceux trouvés par les mots (issue #11).
    pub async fn search_hybrid(
        &self,
        query: &str,
        vector: Option<&[f32]>,
        server: Option<&str>,
        limit: usize,
    ) -> penelope_store::Result<Vec<SearchHit>> {
        let mut hits = self.search(query, server, limit).await?;
        let Some(qv) = vector.map(|v| v.to_vec()) else {
            return Ok(hits);
        };
        let srv = server.map(String::from);
        let known: Vec<String> = hits.iter().map(|h| h.tool.qualified.clone()).collect();
        let near: Vec<SearchHit> = self
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT t.qualified, t.server, t.name, t.title, t.description, t.input_schema,
                            t.output_schema, t.annotations, t.risk, t.schema_bytes, v.embedding
                     FROM mcp_tools_vec v JOIN mcp_tools t ON t.qualified = v.qualified
                     WHERE (?1 IS NULL OR t.server = ?1)",
                )?;
                let rows = st.query_map(params![srv], |r| {
                    Ok((row_to_tool(r)?, r.get::<_, Vec<u8>>(10)?))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    let (tool, blob) = r?;
                    let sim = penelope_store::cosine_similarity(
                        &qv,
                        &penelope_store::decode_embedding(&blob),
                    );
                    if sim >= VECTOR_MIN_SIMILARITY && !known.contains(&tool.qualified) {
                        out.push(SearchHit { tool, score: sim });
                    }
                }
                out.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                Ok(out)
            })
            .await?;
        // Les correspondances par mots gardent la tête ; le sens a au moins la moitié des
        // places restantes.
        let limit = limit.max(1);
        let keep = limit - near.len().min(limit / 2);
        hits.truncate(keep);
        let room = limit - hits.len();
        hits.extend(near.into_iter().take(room));
        Ok(hits)
    }

    /// `tool_search` : recherche hybride (FTS + similarité lexicale de repli).
    pub async fn search(
        &self,
        query: &str,
        server: Option<&str>,
        limit: usize,
    ) -> penelope_store::Result<Vec<SearchHit>> {
        let q = crate::registry::fts_query(query);
        let srv = server.map(String::from);
        let needle = query.to_lowercase();
        let lim = limit.max(1) as i64;

        self.store
            .read(move |c| {
                let mut hits: Vec<SearchHit> = Vec::new();
                if !q.is_empty() {
                    let mut st = c.prepare(
                        "SELECT t.qualified, t.server, t.name, t.title, t.description,
                                t.input_schema, t.output_schema, t.annotations, t.risk,
                                t.schema_bytes, rank
                         FROM mcp_tools_fts f
                         JOIN mcp_tools t ON t.qualified = f.qualified
                         WHERE mcp_tools_fts MATCH ?1 AND (?2 IS NULL OR t.server = ?2)
                         ORDER BY rank LIMIT ?3",
                    )?;
                    let rows = st.query_map(params![q, srv, lim], |r| {
                        let rank: f64 = r.get(10)?;
                        Ok((row_to_tool(r)?, (-rank) as f32))
                    })?;
                    for row in rows {
                        let (tool, score) = row?;
                        hits.push(SearchHit { tool, score });
                    }
                }

                // Repli lexical : garantit un résultat même si FTS ne matche pas
                // (requête d'un seul caractère, terme partiel).
                if hits.len() < limit {
                    let mut st = c.prepare(
                        "SELECT qualified, server, name, title, description, input_schema,
                                output_schema, annotations, risk, schema_bytes
                         FROM mcp_tools WHERE (?1 IS NULL OR server = ?1) LIMIT 2000",
                    )?;
                    let rows = st.query_map(params![srv], row_to_tool)?;
                    for r in rows {
                        let t = r?;
                        if hits.iter().any(|h| h.tool.qualified == t.qualified) {
                            continue;
                        }
                        let score = lexical_score(&needle, &t);
                        if score > 0.0 {
                            hits.push(SearchHit { tool: t, score });
                        }
                    }
                }

                hits.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.tool.qualified.cmp(&b.tool.qualified))
                });
                hits.truncate(limit);
                Ok(hits)
            })
            .await
    }
}

fn lexical_score(needle: &str, t: &RegisteredTool) -> f32 {
    if needle.is_empty() {
        return 0.0;
    }
    let mut score = 0.0f32;
    for term in needle.split_whitespace() {
        if t.name.to_lowercase().contains(term) {
            score += 3.0;
        }
        if t.qualified.to_lowercase().contains(term) {
            score += 1.5;
        }
        if t.description.to_lowercase().contains(term) {
            score += 1.0;
        }
        if t.server.to_lowercase().contains(term) {
            score += 0.5;
        }
    }
    score
}

/// Requête FTS5 assainie.
pub fn fts_query(q: &str) -> String {
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
        .filter(|w| !matches!(*w, "AND" | "OR" | "NOT" | "NEAR"))
        .filter(|w| w.chars().count() > 1)
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}
