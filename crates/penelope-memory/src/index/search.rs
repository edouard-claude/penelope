use super::*;

impl MemoryIndex {
    /// Recherche hybride : FTS puis vecteurs, fusionnés par RRF, puis pondérés.
    pub async fn search(
        &self,
        query: &str,
        query_vector: Option<Vec<f32>>,
        filter: &SearchFilter,
        active_projects: &[String],
    ) -> penelope_store::Result<Vec<Scored>> {
        let fts = crate::index::fts_query(query);
        let f = filter.clone_for_move();
        let projects = active_projects.to_vec();
        let params_ = self.score_params();
        let now_ms = self.clock.now_ms();
        let decoded = self.vectors_decoded.clone();
        // Rappel automatique : pas d'entrée expirée (issues #25 et #37).
        let hidden = if filter.automatic {
            self.hidden_uids().await?
        } else {
            Default::default()
        };

        self.store
            .read(move |c| {
                let mut candidates: BTreeMap<
                    String,
                    (IndexedEntry, Option<usize>, Option<usize>, f64),
                > = BTreeMap::new();

                // Filtrer d'abord (décision 0002) : la coupe aux 200 premiers porte sur des
                // candidats admissibles (issue #87).
                let (clause, filter_params) = f.sql();

                // 1. FTS.
                if !fts.is_empty() {
                    let mut st = c.prepare(&format!(
                        "{SELECT_PREFIXED} FROM mem_fts f JOIN mem_entries e ON e.uid = f.uid
                         WHERE mem_fts MATCH ? AND {clause}
                         ORDER BY rank LIMIT 200"
                    ))?;
                    let mut params =
                        vec![penelope_store::rusqlite::types::Value::Text(fts.clone())];
                    params.extend(filter_params.iter().cloned());
                    let rows = st.query_map(
                        penelope_store::rusqlite::params_from_iter(params),
                        row_to_entry,
                    )?;
                    for (i, r) in rows.enumerate() {
                        let e = r?;
                        candidates.insert(e.uid.clone(), (e, Some(i), None, 0.0));
                    }
                }

                // 2. Vecteurs, recherche exhaustive (§6.11).
                if let Some(qv) = &query_vector {
                    // Les vecteurs écartés par le filtre ne sont même pas décodés.
                    let mut st = c.prepare(&format!(
                        "SELECT v.uid, v.embedding FROM mem_vec v
                         JOIN mem_entries e ON e.uid = v.uid
                         WHERE {clause}"
                    ))?;
                    let rows = st.query_map(
                        penelope_store::rusqlite::params_from_iter(filter_params.iter()),
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
                    )?;
                    let mut sims: Vec<(String, f64)> = Vec::new();
                    for r in rows {
                        let (uid, blob) = r?;
                        decoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let v = decode_embedding(&blob);
                        let s = cosine_similarity(qv, &v) as f64;
                        if s > 0.0 {
                            sims.push((uid, s));
                        }
                    }
                    sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    for (rank, (uid, sim)) in sims.into_iter().take(200).enumerate() {
                        match candidates.get_mut(&uid) {
                            Some(entry) => {
                                entry.2 = Some(rank);
                                entry.3 = sim;
                            }
                            None => {
                                let mut st = c.prepare(&format!("{SELECT} WHERE uid = ?1"))?;
                                let mut rows = st.query([&uid])?;
                                if let Some(r) = rows.next()? {
                                    let e = row_to_entry(r)?;
                                    candidates.insert(uid, (e, None, Some(rank), sim));
                                }
                            }
                        }
                    }
                }

                // 3. Filtrage et pondération.
                let mut out: Vec<Scored> = Vec::new();
                for (_, (entry, fr, vr, sim)) in candidates {
                    if !f.accepts(&entry) || hidden.contains(&entry.uid) {
                        continue;
                    }
                    let base = rrf(fr, vr, params_.rrf_k);
                    let age = age_days(&entry.maj, now_ms);
                    let usage = usage_factor(&signals_row(c, &entry.uid)?);
                    let score = base
                        * decay(age, params_.half_life_days, entry.pinned)
                        * importance_factor(entry.importance)
                        * project_factor(entry.projet.as_deref(), &projects)
                        * confidence_factor(entry.confiance)
                        * usage;
                    out.push(Scored {
                        entry,
                        score,
                        relevance: base,
                        usage,
                        fts_rank: fr,
                        vec_rank: vr,
                        similarity: sim,
                    });
                }
                out.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.entry.uid.cmp(&b.entry.uid))
                });
                out.truncate(f.limit.max(1));
                Ok(out)
            })
            .await
    }
}

impl SearchFilter {
    fn clone_for_move(&self) -> SearchFilter {
        self.clone()
    }

    /// Conditions SQL équivalentes à [`Self::accepts`] (alias `e`, paramètres anonymes
    /// dans l'ordre) : le filtre s'applique **avant** la coupe aux 200 premiers, sinon les
    /// passages de documents ingérés évinçaient les souvenirs du rappel (issue #87).
    fn sql(&self) -> (String, Vec<penelope_store::rusqlite::types::Value>) {
        use penelope_store::rusqlite::types::Value;
        let mut w: Vec<&str> = vec!["e.statut != 'retiree'"];
        let mut p: Vec<Value> = Vec::new();
        if let Some(l) = self.level {
            w.push("e.level = ?");
            p.push(Value::Text(l.as_str().to_string()));
        }
        if let Some(t) = &self.etype {
            w.push("e.etype = ?");
            p.push(Value::Text(t.clone()));
        }
        if let Some(pr) = &self.projet {
            w.push("e.projet = ?");
            p.push(Value::Text(pr.clone()));
        }
        if let Some(sl) = &self.slug {
            w.push("e.slug = ?");
            p.push(Value::Text(sl.clone()));
        }
        if !self.include_episodic {
            w.push("e.level != ?");
            p.push(Value::Text(Level::Episodic.as_str().to_string()));
        }
        if !self.include_untrusted {
            w.push("e.etype != ?");
            p.push(Value::Text(crate::ingest::SOURCE_ETYPE.to_string()));
        }
        if self.automatic {
            w.push(
                "e.etype NOT IN ('exception', 'ecart')
                 AND COALESCE(instr(e.anchor, 'Exceptions'), 0) != 1
                 AND COALESCE(instr(e.anchor, 'Écarts'), 0) != 1",
            );
        }
        (w.join(" AND "), p)
    }

    fn accepts(&self, e: &IndexedEntry) -> bool {
        /// Entrée d'une section « Exceptions » ou « Écarts observés » d'une pratique.
        fn is_practice_part(e: &IndexedEntry) -> bool {
            matches!(e.etype.as_str(), "exception" | "ecart")
                || e.anchor
                    .as_deref()
                    .is_some_and(|a| a.starts_with("Exceptions") || a.starts_with("Écarts"))
        }

        if let Some(l) = self.level
            && e.level != l
        {
            return false;
        }
        if let Some(t) = &self.etype
            && &e.etype != t
        {
            return false;
        }
        if let Some(p) = &self.projet
            && e.projet.as_deref() != Some(p.as_str())
        {
            return false;
        }
        if let Some(s) = &self.slug
            && e.slug.as_deref() != Some(s.as_str())
        {
            return false;
        }
        if !self.include_episodic && e.level == Level::Episodic {
            return false;
        }
        // Passages de documents ingérés : non fiables, jamais rappelés sans demande.
        if !self.include_untrusted && e.etype == crate::ingest::SOURCE_ETYPE {
            return false;
        }
        // Exceptions et écarts d'une pratique : ils ne valent que sous leur `quand`, et
        // c'est le rappel de la pratique qui l'évalue. Jamais injectés d'office par la
        // recherche, mais toujours trouvables par `mem_search` (§6.7, issue #58).
        if self.automatic && is_practice_part(e) {
            return false;
        }
        true
    }
}
