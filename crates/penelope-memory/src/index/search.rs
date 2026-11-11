use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

type Candidates = BTreeMap<String, (IndexedEntry, Option<usize>, Option<usize>, f64)>;
type SqlFilter = (String, Vec<penelope_store::rusqlite::types::Value>);

/// Ce qu'une recherche laisse voir pendant qu'elle tourne (#450). Le rappel qui dépasse
/// son budget y prend la voie lexicale déjà classée au lieu de partir sans rien, dit à la
/// recherche de s'arrêter (abandonnée, elle décodait des vecteurs pour personne en tenant
/// un lecteur) et y lit où le temps est parti.
#[derive(Debug, Default)]
pub struct Progress {
    abandoned: AtomicBool,
    lexical: std::sync::Mutex<Option<Vec<Scored>>>,
    /// Millisecondes avant de tenir un lecteur, puis jusqu'à la fin de la voie lexicale ;
    /// zéro tant que l'étape n'est pas atteinte.
    reader_ms: AtomicU64,
    lexical_ms: AtomicU64,
    /// Couture de test : la voie que la recherche n'achève pas avant d'être abandonnée.
    #[cfg(test)]
    pub(crate) stall: Option<Stall>,
}

/// Une recherche retenue devant une voie, comme par un index trop gros ou un lecteur
/// qui se fait attendre.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stall {
    Lexical,
    Vectors,
}

impl Progress {
    /// Le rappel n'attend plus : la recherche s'arrête au prochain contrôle.
    pub fn abandon(&self) {
        self.abandoned.store(true, Ordering::Relaxed);
    }

    fn abandoned(&self) -> bool {
        self.abandoned.load(Ordering::Relaxed)
    }

    /// La voie lexicale classée, si la recherche est allée jusque-là avec une voie
    /// vectorielle encore à faire.
    pub fn lexical(&self) -> Option<Vec<Scored>> {
        self.lexical.lock().ok().and_then(|mut l| l.take())
    }

    /// Où la recherche en était : l'étape, et les millisecondes passées à attendre un
    /// lecteur puis à finir la voie lexicale.
    pub fn stage(&self) -> (&'static str, u64, u64) {
        let (reader, lexical) = (
            self.reader_ms.load(Ordering::Relaxed),
            self.lexical_ms.load(Ordering::Relaxed),
        );
        let stage = match (reader, lexical) {
            (0, _) => "attente d'un lecteur",
            (_, 0) => "voie lexicale",
            _ => "voie vectorielle",
        };
        (stage, reader.saturating_sub(1), lexical.saturating_sub(1))
    }

    fn reached(&self, step: &AtomicU64, asked: Instant) {
        step.store(asked.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
    }

    /// Attend l'abandon devant la voie `lane` si le test l'a demandé.
    #[cfg(test)]
    fn stall_at(&self, lane: Stall) {
        let started = Instant::now();
        while self.stall == Some(lane) && !self.abandoned() && started.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

/// Ce qu'il faut pour classer des candidats, hors de la connexion.
struct Ranking {
    filter: SearchFilter,
    terms: Vec<String>,
    projects: Vec<String>,
    params: ScoreParams,
    now_ms: i64,
    hidden: std::collections::HashSet<String>,
}

impl MemoryIndex {
    /// Recherche hybride : FTS puis vecteurs, fusionnés par RRF, puis pondérés.
    pub async fn search(
        &self,
        query: &str,
        query_vector: Option<Vec<f32>>,
        filter: &SearchFilter,
        active_projects: &[String],
    ) -> penelope_store::Result<Vec<Scored>> {
        self.search_watched(query, query_vector, filter, active_projects, None)
            .await
    }

    /// Comme [`Self::search`], sous le regard de l'appelant : `progress` reçoit la voie
    /// lexicale dès qu'elle est classée et porte l'ordre d'abandon (#450).
    pub async fn search_watched(
        &self,
        query: &str,
        query_vector: Option<Vec<f32>>,
        filter: &SearchFilter,
        active_projects: &[String],
        progress: Option<Arc<Progress>>,
    ) -> penelope_store::Result<Vec<Scored>> {
        let asked = Instant::now();
        let fts = crate::index::fts_query(query);
        let decoded = self.vectors_decoded.clone();
        let ranking = Ranking {
            filter: filter.clone(),
            terms: crate::index::query_terms(query),
            projects: active_projects.to_vec(),
            params: self.score_params(),
            now_ms: self.clock.now_ms(),
            // Rappel automatique : pas d'entrée expirée (issues #25 et #37).
            hidden: if filter.automatic {
                self.hidden_uids().await?
            } else {
                Default::default()
            },
        };

        self.store
            .read(move |c| {
                let watch = progress.as_deref();
                if let Some(p) = watch {
                    p.reached(&p.reader_ms, asked);
                    #[cfg(test)]
                    p.stall_at(Stall::Lexical);
                    // Le budget est parti à attendre un lecteur : on le rend aussitôt.
                    if p.abandoned() {
                        return Ok(Vec::new());
                    }
                }
                // Filtrer d'abord (décision 0002) : la coupe aux 200 premiers porte sur des
                // candidats admissibles (issue #87).
                let sql = ranking.filter.sql();
                let mut candidates = lexical(c, &fts, &sql)?;
                if let (Some(p), Some(_)) = (watch, &query_vector) {
                    let ranked = ranking.rank(c, candidates.clone())?;
                    if let Ok(mut slot) = p.lexical.lock() {
                        *slot = Some(ranked);
                    }
                    p.reached(&p.lexical_ms, asked);
                }
                if let Some(qv) = &query_vector
                    && !vectors(c, qv, &sql, &mut candidates, watch, &decoded)?
                {
                    // Abandonnée : plus personne n'attend le classement.
                    return Ok(Vec::new());
                }
                ranking.rank(c, candidates)
            })
            .await
    }
}

/// 1. FTS : les 200 premiers par rang.
fn lexical(
    c: &penelope_store::rusqlite::Connection,
    fts: &str,
    (clause, filter_params): &SqlFilter,
) -> penelope_store::Result<Candidates> {
    let mut candidates = Candidates::new();
    if fts.is_empty() {
        return Ok(candidates);
    }
    let mut st = c.prepare(&format!(
        "{SELECT_PREFIXED} FROM mem_fts f JOIN mem_entries e ON e.uid = f.uid
         WHERE mem_fts MATCH ? AND {clause}
         ORDER BY rank LIMIT 200"
    ))?;
    let mut params = vec![penelope_store::rusqlite::types::Value::Text(
        fts.to_string(),
    )];
    params.extend(filter_params.iter().cloned());
    let rows = st.query_map(
        penelope_store::rusqlite::params_from_iter(params),
        row_to_entry,
    )?;
    for (i, r) in rows.enumerate() {
        let e = r?;
        candidates.insert(e.uid.clone(), (e, Some(i), None, 0.0));
    }
    Ok(candidates)
}

/// 2. Vecteurs, recherche exhaustive (§6.11).
///
/// Rend `false` si l'appelant a abandonné en route : le parcours s'arrête et rend son
/// lecteur.
fn vectors(
    c: &penelope_store::rusqlite::Connection,
    qv: &[f32],
    (clause, filter_params): &SqlFilter,
    candidates: &mut Candidates,
    watch: Option<&Progress>,
    decoded: &AtomicU64,
) -> penelope_store::Result<bool> {
    #[cfg(test)]
    if let Some(p) = watch {
        p.stall_at(Stall::Vectors);
    }
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
    for (i, r) in rows.enumerate() {
        if i % 64 == 0 && watch.is_some_and(Progress::abandoned) {
            return Ok(false);
        }
        let (uid, blob) = r?;
        decoded.fetch_add(1, Ordering::Relaxed);
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
    Ok(true)
}

impl Ranking {
    /// 3. Filtrage et pondération.
    fn rank(
        &self,
        c: &penelope_store::rusqlite::Connection,
        candidates: Candidates,
    ) -> penelope_store::Result<Vec<Scored>> {
        let mut out: Vec<Scored> = Vec::new();
        for (_, (entry, fr, vr, sim)) in candidates {
            if !self.filter.accepts(&entry) || self.hidden.contains(&entry.uid) {
                continue;
            }
            // Le rang ordonne ; la pertinence, elle, est absolue : part des termes
            // retrouvés ou similarité vectorielle calibrée (#411).
            let base = rrf(fr, vr, self.params.rrf_k);
            let relevance = crate::index::coverage(&self.terms, &entry.text).max(if vr.is_some() {
                crate::index::vector_relevance(sim)
            } else {
                0.0
            });
            let age = age_days(&entry.maj, self.now_ms);
            let usage = usage_factor(&signals_row(c, &entry.uid)?);
            let score = base
                * crate::index::provenance_factor(&entry.file)
                * decay(age, self.params.half_life_days, entry.pinned)
                * importance_factor(entry.importance)
                * project_factor(entry.projet.as_deref(), &self.projects)
                * confidence_factor(entry.confiance)
                * usage;
            out.push(Scored {
                entry,
                score,
                relevance,
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
        out.truncate(self.filter.limit.max(1));
        Ok(out)
    }
}

impl SearchFilter {
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

#[cfg(test)]
impl MemoryIndex {
    /// Tours comptés sans mémoire, tous jours confondus : `(tous, sans interlocuteur)`.
    pub(crate) async fn recall_misses(&self) -> (u64, u64) {
        self.store
            .read(|c| {
                let sum = |like: &str| {
                    c.query_row(
                        "SELECT COALESCE(SUM(CAST(v AS INTEGER)), 0) FROM kv WHERE k LIKE ?1",
                        [like],
                        |r| r.get::<_, i64>(0).map(|n| n as u64),
                    )
                };
                let unattended = sum("memory.recall.misses.unattended.%")?;
                Ok((sum("memory.recall.misses.2%")?, unattended))
            })
            .await
            .unwrap()
    }
}
