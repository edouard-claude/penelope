//! Index SQLite dérivé du vault (§6.2, §6.11, §6.12).
//!
//! Le vault est la vérité ; l'index est **reconstructible** (`penelope mem reindex`).
//! Seuls la provenance et les signaux ne sont pas reconstructibles : ils sont conservés
//! par `entry_uid`, qui est stable.

use crate::provenance::{Origin, Provenance};
use crate::vault::{Annotations, Level, VaultEntry, When};
use penelope_kernel::clock::SharedClock;
use penelope_store::{
    Store, cosine_similarity, decode_embedding, encode_embedding, rusqlite::params,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Entrée indexée.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexedEntry {
    pub uid: String,
    pub file: String,
    pub anchor: Option<String>,
    pub level: Level,
    pub etype: String,
    pub slug: Option<String>,
    pub text: String,
    pub quand: Option<When>,
    pub importance: Option<u8>,
    pub projet: Option<String>,
    pub confiance: Option<f64>,
    pub statut: String,
    pub depuis: Option<String>,
    pub maj: String,
    pub pinned: bool,
    pub declencheurs: Vec<String>,
    pub content_hash: String,
    pub retired_at: Option<String>,
}

impl IndexedEntry {
    pub fn from_vault(
        e: &VaultEntry,
        file: &str,
        level: Level,
        etype: &str,
        slug: Option<&str>,
        maj: &str,
    ) -> IndexedEntry {
        IndexedEntry {
            uid: e.uid.clone(),
            file: file.to_string(),
            anchor: (!e.section.is_empty()).then(|| e.section.clone()),
            level,
            etype: etype.to_string(),
            slug: slug.map(String::from),
            text: e.text.clone(),
            quand: e.annotations.quand.clone(),
            importance: e.annotations.importance,
            projet: e.annotations.projet.clone(),
            confiance: e.annotations.confiance,
            statut: "active".into(),
            depuis: e.annotations.depuis.clone(),
            maj: maj.to_string(),
            pinned: matches!(level, Level::Profil | Level::Coeur),
            declencheurs: e.annotations.declencheurs.clone(),
            content_hash: penelope_kernel::canonical::sha256_hex(e.text.as_bytes()),
            retired_at: None,
        }
    }
}

/// Signaux d'une entrée (§6.8, non reconstructibles).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    pub occurrences: u32,
    pub sessions: u32,
    pub days: u32,
    pub recalls: u32,
    pub useful_recalls: u32,
    pub successes: u32,
    pub contradictions: u32,
    pub last_recall: Option<String>,
    pub distinct_queries: Vec<String>,
    /// Apparitions dans les résultats du rappel automatique sans être retenue (#86).
    pub seen: u32,
}

/// Entrée avec son score de classement.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub entry: IndexedEntry,
    /// Priorité : pertinence pondérée par la récence, l'importance, le projet et la
    /// confiance. Ordonne les résultats et départage sous budget.
    pub score: f64,
    /// Pertinence seule (rang RRF normalisé : 1 pour un premier rang dans une liste, 2
    /// dans les deux). C'est elle que compare le seuil du rappel automatique : un
    /// souvenir ancien reste rappelable, il passe seulement après un récent (#86).
    pub relevance: f64,
    /// Facteur d'usage appliqué au score ([`usage_factor`], issue #105).
    pub usage: f64,
    pub fts_rank: Option<usize>,
    pub vec_rank: Option<usize>,
    pub similarity: f64,
}

/// Filtres de recherche (`mem_search`).
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub level: Option<Level>,
    pub etype: Option<String>,
    pub projet: Option<String>,
    pub slug: Option<String>,
    /// Inclure le niveau épisodique et le contenu non fiable (recherche explicite).
    pub include_episodic: bool,
    pub include_untrusted: bool,
    pub limit: usize,
    /// Injection d'office (rappel automatique) : sans entrées expirées.
    pub automatic: bool,
}

impl SearchFilter {
    pub fn explicit() -> Self {
        SearchFilter {
            include_episodic: true,
            include_untrusted: true,
            limit: 20,
            ..Default::default()
        }
    }
}

/// Facteurs de score (§6.7).
#[derive(Debug, Clone, Copy)]
pub struct ScoreParams {
    pub half_life_days: f64,
    pub rrf_k: f64,
}

impl Default for ScoreParams {
    fn default() -> Self {
        ScoreParams {
            half_life_days: 180.0,
            rrf_k: 60.0,
        }
    }
}

/// `decay = exp(−âge/demi-vie × ln 2)` ; 1 pour profil, cœur et épinglés.
pub fn decay(age_days: f64, half_life_days: f64, pinned: bool) -> f64 {
    if pinned {
        return 1.0;
    }
    if half_life_days <= 0.0 {
        return 1.0;
    }
    (-(age_days / half_life_days) * std::f64::consts::LN_2).exp()
}

/// `imp = 0,5 + importance/20` (neutre = 1 si absente).
pub fn importance_factor(importance: Option<u8>) -> f64 {
    match importance {
        Some(i) => 0.5 + (i as f64) / 20.0,
        None => 1.0,
    }
}

/// `proj` : 1,3 si projet actif, 0,85 si autre projet, 1 si non annoté.
pub fn project_factor(entry_project: Option<&str>, active: &[String]) -> f64 {
    match entry_project {
        None => 1.0,
        Some(p) => {
            if active.iter().any(|a| a == p) {
                1.3
            } else {
                0.85
            }
        }
    }
}

/// `conf = 0,5 + confiance/2` pour pratiques et exceptions, 1 sinon.
pub fn confidence_factor(confiance: Option<f64>) -> f64 {
    match confiance {
        Some(c) => 0.5 + c / 2.0,
        None => 1.0,
    }
}

/// Gain maximal du facteur d'usage (issue #105) : ×1,2 au plus, sous le facteur projet.
pub const USAGE_MAX_GAIN: f64 = 0.2;
/// Perte maximale : ×0,85 pour une entrée souvent servie sans servir, ou contredite.
pub const USAGE_MAX_LOSS: f64 = 0.15;
/// Rappels sous lesquels la part d'inutiles ne dit encore rien.
pub const USAGE_MIN_RECALLS: u32 = 5;

/// `usage` (issue #105) : la preuve d'usage d'une entrée, bornée à [0,85 ; 1,2].
///
/// ```text
///  gain  = 0,2 × (1 − e^(−(rappels utiles + succès) / 5))      dix rappels utiles : ×1,17
///  perte = 0,15 × max(part des rappels inutiles × min(1, rappels / 20),
///                     part des contradictions)             vingt rappels inutiles : ×0,85
/// ```
///
/// Le gain ne compte que ce qui a servi, pas ce qui a été servi : une entrée rappelée
/// souvent et jamais utile descend. La borne tient une entrée populaire derrière une
/// correspondance nettement meilleure (deux listes contre une, rang RRF) : l'usage
/// départage, il ne remplace pas la pertinence. Le seuil du rappel automatique compare
/// la pertinence seule, l'usage ne fait entrer aucun souvenir.
pub fn usage_factor(s: &Signals) -> f64 {
    let good = (s.useful_recalls + s.successes) as f64;
    let gain = USAGE_MAX_GAIN * (1.0 - (-good / 5.0).exp());
    let useless = if s.recalls >= USAGE_MIN_RECALLS {
        let wasted = s.recalls.saturating_sub(s.useful_recalls) as f64 / s.recalls as f64;
        wasted * (s.recalls as f64 / 20.0).min(1.0)
    } else {
        0.0
    };
    let judged = s.successes + s.contradictions;
    let contradicted = if judged > 0 {
        s.contradictions as f64 / judged as f64
    } else {
        0.0
    };
    let loss = USAGE_MAX_LOSS * useless.max(contradicted);
    (1.0 + gain - loss).clamp(1.0 - USAGE_MAX_LOSS, 1.0 + USAGE_MAX_GAIN)
}

/// Fusion de rangs réciproques (RRF).
pub fn rrf(fts_rank: Option<usize>, vec_rank: Option<usize>, k: f64) -> f64 {
    let f = fts_rank.map(|r| 1.0 / (k + r as f64 + 1.0)).unwrap_or(0.0);
    let v = vec_rank.map(|r| 1.0 / (k + r as f64 + 1.0)).unwrap_or(0.0);
    // Normalisé pour que deux rangs 0 donnent ~1.
    (f + v) * (k + 1.0)
}

/// Demi-vie lue à chaque recherche : `memory.half_life_days` s'applique à chaud (#86).
type HalfLife = std::sync::Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Clone)]
pub struct MemoryIndex {
    store: Store,
    clock: SharedClock,
    params: ScoreParams,
    half_life: Option<HalfLife>,
    /// Vecteurs décodés par les recherches : un filtre appliqué trop tard se voit ici.
    vectors_decoded: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl MemoryIndex {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        MemoryIndex {
            store,
            clock,
            params: ScoreParams::default(),
            half_life: None,
            vectors_decoded: Default::default(),
        }
    }

    pub fn with_params(mut self, p: ScoreParams) -> Self {
        self.params = p;
        self
    }

    /// Demi-vie de la récence relue à chaque recherche (configuration à chaud, #86).
    pub fn with_half_life(mut self, f: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        self.half_life = Some(std::sync::Arc::new(f));
        self
    }

    /// Vecteurs décodés depuis la création de l'index (diagnostic et tests, #87).
    pub fn vectors_decoded(&self) -> u64 {
        self.vectors_decoded
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn score_params(&self) -> ScoreParams {
        let mut p = self.params;
        if let Some(f) = &self.half_life {
            p.half_life_days = f();
        }
        p
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Insère ou met à jour une entrée, avec sa provenance.
    pub async fn upsert(&self, e: &IndexedEntry, prov: &Provenance) -> penelope_store::Result<()> {
        let row = e.clone();
        let p = prov.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mem_entries(uid, file, anchor, level, etype, slug, text, quand,
                        importance, projet, confiance, statut, depuis, maj, pinned, declencheurs,
                        content_hash, retired_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
                     ON CONFLICT(uid) DO UPDATE SET
                        file=excluded.file, anchor=excluded.anchor, level=excluded.level,
                        etype=excluded.etype, slug=excluded.slug, text=excluded.text,
                        quand=excluded.quand, importance=excluded.importance,
                        projet=excluded.projet, confiance=excluded.confiance,
                        statut=excluded.statut, depuis=excluded.depuis, maj=excluded.maj,
                        pinned=excluded.pinned, declencheurs=excluded.declencheurs,
                        content_hash=excluded.content_hash, retired_at=excluded.retired_at",
                    params![
                        row.uid,
                        row.file,
                        row.anchor,
                        row.level.as_str(),
                        row.etype,
                        row.slug,
                        row.text,
                        row.quand.as_ref().map(|q| q.render()),
                        row.importance.map(|i| i as i64),
                        row.projet,
                        row.confiance,
                        row.statut,
                        row.depuis,
                        row.maj,
                        row.pinned as i64,
                        serde_json::to_string(&row.declencheurs).unwrap_or_default(),
                        row.content_hash,
                        row.retired_at
                    ],
                )?;

                tx.execute("DELETE FROM mem_fts WHERE uid = ?1", [&row.uid])?;
                tx.execute(
                    "INSERT INTO mem_fts(text, declencheurs, uid) VALUES(?1,?2,?3)",
                    params![row.text, row.declencheurs.join(" "), row.uid],
                )?;

                tx.execute("DELETE FROM mem_links WHERE from_uid = ?1", [&row.uid])?;
                for target in crate::vault::links(&row.text) {
                    tx.execute(
                        "INSERT OR IGNORE INTO mem_links(from_uid, to_slug) VALUES(?1,?2)",
                        params![row.uid, target],
                    )?;
                }

                // La provenance n'est écrite qu'une fois : elle ne peut pas être
                // « améliorée » après coup par l'agent.
                tx.execute(
                    "INSERT OR IGNORE INTO mem_provenance(uid, origin, session_kind, observed_at,
                        supersedes_uid, source_ref, session_id)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        row.uid,
                        p.origin.as_str(),
                        p.session_kind,
                        p.observed_at,
                        p.supersedes_uid,
                        p.source_ref,
                        p.session_id
                    ],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO mem_signals(uid) VALUES(?1)",
                    [&row.uid],
                )?;
                Ok(())
            })
            .await
    }

    /// Retire une entrée (suppression manuelle dans le fichier). Le signal est conservé
    /// 30 jours pour permettre l'annulation (§6.12).
    pub async fn retire(&self, uid: &str) -> penelope_store::Result<()> {
        let (uid, now) = (uid.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE mem_entries SET statut='retiree', retired_at=?2 WHERE uid=?1",
                    params![uid, now],
                )?;
                tx.execute("DELETE FROM mem_fts WHERE uid = ?1", [&uid])?;
                Ok(())
            })
            .await
    }

    pub async fn get(&self, uid: &str) -> penelope_store::Result<Option<IndexedEntry>> {
        let uid = uid.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!("{SELECT} WHERE uid = ?1"))?;
                let mut rows = st.query([&uid])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_entry(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    pub async fn by_slug(&self, slug: &str) -> penelope_store::Result<Vec<IndexedEntry>> {
        let slug = slug.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE slug = ?1 AND statut != 'retiree' ORDER BY uid"
                ))?;
                let rows = st.query_map([slug], row_to_entry)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    pub async fn by_level(&self, level: Level) -> penelope_store::Result<Vec<IndexedEntry>> {
        let l = level.as_str();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE level = ?1 AND statut != 'retiree' ORDER BY uid"
                ))?;
                let rows = st.query_map([l], row_to_entry)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Entrées durables jamais rappelées depuis `cutoff` (`AAAA-MM-JJ`), ni récentes, ni
    /// datées d'une expiration, et vues au moins `min_seen` fois dans les résultats sans
    /// être retenues : candidates au retrait (retour d'usage, issues #37 et #86). Une
    /// entrée qu'aucune question n'a jamais approchée n'a pas eu sa chance.
    pub async fn unrecalled_since(
        &self,
        levels: &[Level],
        cutoff: &str,
        min_seen: u32,
    ) -> penelope_store::Result<Vec<IndexedEntry>> {
        let levels: Vec<String> = levels.iter().map(|l| l.as_str().to_string()).collect();
        let cutoff = cutoff.to_string();
        let min_seen = min_seen as i64;
        self.store
            .read(move |c| {
                let marks = vec!["?"; levels.len()].join(",");
                let mut st = c.prepare(&format!(
                    "{SELECT_PREFIXED} FROM mem_entries e
                     LEFT JOIN mem_signals g ON g.uid = e.uid
                     LEFT JOIN mem_flags f ON f.uid = e.uid
                     WHERE e.statut != 'retiree' AND e.retired_at IS NULL
                       AND e.level IN ({marks})
                       AND COALESCE(e.depuis, e.maj) < ?
                       AND (g.last_recall IS NULL OR substr(g.last_recall, 1, 10) < ?)
                       AND COALESCE(g.seen, 0) >= ?
                       AND f.expire IS NULL
                     ORDER BY COALESCE(e.depuis, e.maj), e.uid"
                ))?;
                let mut params: Vec<&dyn penelope_store::rusqlite::ToSql> = Vec::new();
                for l in &levels {
                    params.push(l);
                }
                params.push(&cutoff);
                params.push(&cutoff);
                params.push(&min_seen);
                let rows = st.query_map(params.as_slice(), row_to_entry)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    pub async fn origin_of(&self, uid: &str) -> penelope_store::Result<Option<Origin>> {
        let uid = uid.to_string();
        self.store
            .read(move |c| {
                let s: Option<String> = c
                    .query_row(
                        "SELECT origin FROM mem_provenance WHERE uid = ?1",
                        [&uid],
                        |r| r.get(0),
                    )
                    .ok();
                Ok(s.and_then(|s| Origin::parse(&s)))
            })
            .await
    }

    pub async fn signals_of(&self, uid: &str) -> penelope_store::Result<Signals> {
        let uid = uid.to_string();
        self.store.read(move |c| signals_row(c, &uid)).await
    }

    /// Enregistre un rappel, avec la requête qui l'a déclenché (diversité des requêtes).
    pub async fn record_recall(
        &self,
        uid: &str,
        query: &str,
        useful: bool,
    ) -> penelope_store::Result<()> {
        self.bump_recall(uid, query, 1, useful as i64).await
    }

    /// Un souvenir servi au modèle (issue #105) : sa date de rappel et la requête sont
    /// notées ; `counted`, il compte parmi les rappels et son utilité sera jugée sur la
    /// réponse ([`MemoryIndex::mark_useful`]). Servi hors conversation, où rien ne juge,
    /// il ne pèse pas sur la part des rappels utiles.
    pub async fn record_served(
        &self,
        uid: &str,
        query: &str,
        counted: bool,
    ) -> penelope_store::Result<()> {
        self.bump_recall(uid, query, counted as i64, 0).await
    }

    /// Le souvenir servi a servi : la réponse s'en est servie (issue #105). Jamais plus de
    /// rappels utiles que de rappels.
    pub async fn mark_useful(&self, uid: &str) -> penelope_store::Result<()> {
        let uid = uid.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE mem_signals SET useful_recalls = MIN(recalls, useful_recalls + 1)
                     WHERE uid = ?1",
                    [&uid],
                )?;
                Ok(())
            })
            .await
    }

    async fn bump_recall(
        &self,
        uid: &str,
        query: &str,
        recalls: i64,
        useful: i64,
    ) -> penelope_store::Result<()> {
        let (uid, q, now) = (uid.to_string(), query.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                let existing: String = tx
                    .query_row(
                        "SELECT distinct_queries FROM mem_signals WHERE uid = ?1",
                        [&uid],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| "[]".into());
                let mut queries: Vec<String> = serde_json::from_str(&existing).unwrap_or_default();
                let norm = q.to_lowercase();
                if !queries.iter().any(|x| x == &norm) {
                    queries.push(norm);
                    if queries.len() > 50 {
                        queries.remove(0);
                    }
                }
                tx.execute(
                    "INSERT INTO mem_signals(uid, recalls, useful_recalls, last_recall,
                        distinct_queries)
                     VALUES(?1, ?5, ?2, ?3, ?4)
                     ON CONFLICT(uid) DO UPDATE SET
                        recalls = recalls + ?5,
                        useful_recalls = useful_recalls + ?2,
                        last_recall = ?3,
                        distinct_queries = ?4",
                    params![
                        uid,
                        useful,
                        now,
                        serde_json::to_string(&queries).unwrap_or_default(),
                        recalls
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Compte les entrées apparues dans les résultats du rappel automatique sans être
    /// retenues (#86) : le retour d'usage ne propose au retrait que ce qui a eu sa chance.
    pub async fn record_seen(&self, uids: &[String]) -> penelope_store::Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let uids = uids.to_vec();
        self.store
            .write(move |tx| {
                for uid in &uids {
                    tx.execute(
                        "INSERT INTO mem_signals(uid, seen) VALUES(?1, 1)
                         ON CONFLICT(uid) DO UPDATE SET seen = seen + 1",
                        [uid],
                    )?;
                }
                Ok(())
            })
            .await
    }

    /// Enregistre un succès ou une contradiction (calcul de confiance, §6.8).
    pub async fn record_outcome(&self, uid: &str, success: bool) -> penelope_store::Result<()> {
        let uid = uid.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mem_signals(uid, successes, contradictions)
                     VALUES(?1, ?2, ?3)
                     ON CONFLICT(uid) DO UPDATE SET
                        successes = successes + ?2, contradictions = contradictions + ?3",
                    params![uid, success as i64, (!success) as i64],
                )?;
                Ok(())
            })
            .await
    }

    /// Marqueurs d'une entrée (issue #25).
    pub async fn set_flags(
        &self,
        uid: &str,
        sensible: bool,
        expire: Option<&str>,
    ) -> penelope_store::Result<()> {
        let (uid, expire) = (uid.to_string(), expire.map(String::from));
        self.store
            .write(move |tx| {
                if !sensible && expire.is_none() {
                    tx.execute("DELETE FROM mem_flags WHERE uid = ?1", [&uid])?;
                } else {
                    tx.execute(
                        "INSERT INTO mem_flags(uid, sensible, expire) VALUES(?1, ?2, ?3)
                         ON CONFLICT(uid) DO UPDATE SET sensible = excluded.sensible,
                            expire = excluded.expire",
                        params![uid, sensible as i64, expire],
                    )?;
                }
                Ok(())
            })
            .await
    }

    /// Entrées à ne pas injecter d'office : expirées à ce jour. `sensible` n'est plus qu'un
    /// marqueur : le vault est privé, une information client ou d'infrastructure utile se
    /// garde et se sert (issue #37).
    pub async fn hidden_uids(&self) -> penelope_store::Result<std::collections::HashSet<String>> {
        let today: String = self.clock.now_rfc3339().chars().take(10).collect();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT uid FROM mem_flags
                     WHERE expire IS NOT NULL AND expire < ?1",
                )?;
                let rows = st.query_map([today], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }

    /// Stocke un embedding.
    pub async fn put_embedding(
        &self,
        uid: &str,
        model: &str,
        vector: &[f32],
    ) -> penelope_store::Result<()> {
        let (uid, model, blob, dim, now) = (
            uid.to_string(),
            model.to_string(),
            encode_embedding(vector),
            vector.len() as i64,
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mem_vec(uid, dim, model, embedding, updated_at)
                     VALUES(?1,?2,?3,?4,?5)
                     ON CONFLICT(uid) DO UPDATE SET dim=excluded.dim, model=excluded.model,
                        embedding=excluded.embedding, updated_at=excluded.updated_at",
                    params![uid, dim, model, blob, now],
                )?;
                Ok(())
            })
            .await
    }

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

    pub async fn count(&self) -> penelope_store::Result<i64> {
        self.store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM mem_entries WHERE statut != 'retiree'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
    }

    /// Efface l'index dérivé, **sans** toucher à la provenance ni aux signaux :
    /// c'est ce qui permet à `penelope mem reindex` de tout reconstruire du vault seul.
    pub async fn clear_derived(&self) -> penelope_store::Result<()> {
        self.store
            .write(|tx| {
                tx.execute("DELETE FROM mem_entries", [])?;
                tx.execute("DELETE FROM mem_fts", [])?;
                tx.execute("DELETE FROM mem_links", [])?;
                Ok(())
            })
            .await
    }

    /// Retire toutes les entrées dérivées d'une session (`/forget`, §6.5).
    pub async fn forget_session(&self, session_id: &str) -> penelope_store::Result<Vec<String>> {
        let sid = session_id.to_string();
        let now = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                let uids: Vec<String> = {
                    let mut st =
                        tx.prepare("SELECT uid FROM mem_provenance WHERE session_id = ?1")?;
                    let rows = st.query_map([&sid], |r| r.get::<_, String>(0))?;
                    let mut v = Vec::new();
                    for r in rows {
                        v.push(r?);
                    }
                    v
                };
                for uid in &uids {
                    tx.execute(
                        "UPDATE mem_entries SET statut='retiree', retired_at=?2 WHERE uid=?1",
                        params![uid, now],
                    )?;
                    tx.execute("DELETE FROM mem_fts WHERE uid = ?1", [uid])?;
                }
                tx.execute(
                    "UPDATE mem_candidates SET state='rejected',
                     reject_reason='session oubliée' WHERE session_id = ?1",
                    [&sid],
                )?;
                Ok(uids)
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

fn age_days(maj: &str, now_ms: i64) -> f64 {
    let parsed = chrono::NaiveDate::parse_from_str(maj, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp_millis())
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(maj)
                .ok()
                .map(|d| d.timestamp_millis())
        });
    match parsed {
        Some(ms) => ((now_ms - ms).max(0) as f64) / 86_400_000.0,
        None => 0.0,
    }
}

/// Requête FTS assainie.
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
        .filter(|w| w.chars().count() > 2)
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Signaux d'une entrée, neutres si elle n'en a pas.
fn signals_row(
    c: &penelope_store::rusqlite::Connection,
    uid: &str,
) -> penelope_store::Result<Signals> {
    let mut st = c.prepare_cached(
        "SELECT occurrences, sessions, days, recalls, useful_recalls, successes,
                contradictions, last_recall, distinct_queries, seen
         FROM mem_signals WHERE uid = ?1",
    )?;
    let mut rows = st.query([uid])?;
    match rows.next()? {
        Some(r) => {
            let dq: String = r.get(8)?;
            Ok(Signals {
                occurrences: r.get::<_, i64>(0)? as u32,
                sessions: r.get::<_, i64>(1)? as u32,
                days: r.get::<_, i64>(2)? as u32,
                recalls: r.get::<_, i64>(3)? as u32,
                useful_recalls: r.get::<_, i64>(4)? as u32,
                successes: r.get::<_, i64>(5)? as u32,
                contradictions: r.get::<_, i64>(6)? as u32,
                last_recall: r.get(7)?,
                distinct_queries: serde_json::from_str(&dq).unwrap_or_default(),
                seen: r.get::<_, i64>(9)? as u32,
            })
        }
        None => Ok(Signals::default()),
    }
}

const COLUMNS: &str = "uid, file, anchor, level, etype, slug, text, quand, importance, projet,
     confiance, statut, depuis, maj, pinned, declencheurs, content_hash, retired_at";

const SELECT: &str = "SELECT uid, file, anchor, level, etype, slug, text, quand, importance,
     projet, confiance, statut, depuis, maj, pinned, declencheurs, content_hash, retired_at
     FROM mem_entries";

const SELECT_PREFIXED: &str = "SELECT e.uid, e.file, e.anchor, e.level, e.etype, e.slug, e.text,
     e.quand, e.importance, e.projet, e.confiance, e.statut, e.depuis, e.maj, e.pinned,
     e.declencheurs, e.content_hash, e.retired_at";

fn row_to_entry(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<IndexedEntry> {
    let level: String = r.get(3)?;
    let quand: Option<String> = r.get(7)?;
    let declencheurs: String = r.get(15)?;
    Ok(IndexedEntry {
        uid: r.get(0)?,
        file: r.get(1)?,
        anchor: r.get(2)?,
        level: Level::parse(&level).unwrap_or(Level::Cure),
        etype: r.get(4)?,
        slug: r.get(5)?,
        text: r.get(6)?,
        quand: quand.and_then(|q| When::parse(&q).ok()),
        importance: r.get::<_, Option<i64>>(8)?.map(|i| i as u8),
        projet: r.get(9)?,
        confiance: r.get(10)?,
        statut: r.get(11)?,
        depuis: r.get(12)?,
        maj: r.get(13)?,
        pinned: r.get::<_, i64>(14)? != 0,
        declencheurs: serde_json::from_str(&declencheurs).unwrap_or_default(),
        content_hash: r.get(16)?,
        retired_at: r.get(17)?,
    })
}

/// Utilisé par les tests d'intégration pour vérifier la liste de colonnes.
pub fn columns() -> &'static str {
    COLUMNS
}

/// Construit une entrée d'annotation minimale (utilitaire de test et d'import).
pub fn simple_entry(uid: &str, text: &str, level: Level, maj: &str) -> IndexedEntry {
    IndexedEntry {
        uid: uid.to_string(),
        file: "profil.md".into(),
        anchor: None,
        level,
        etype: "fait".into(),
        slug: None,
        text: text.to_string(),
        quand: None,
        importance: None,
        projet: None,
        confiance: None,
        statut: "active".into(),
        depuis: None,
        maj: maj.to_string(),
        pinned: matches!(level, Level::Profil | Level::Coeur),
        declencheurs: Vec::new(),
        content_hash: penelope_kernel::canonical::sha256_hex(text.as_bytes()),
        retired_at: None,
    }
}

/// Annotations d'une entrée indexée, pour réécriture du fichier.
pub fn annotations_of(e: &IndexedEntry) -> Annotations {
    Annotations {
        uid: Some(e.uid.clone()),
        importance: e.importance,
        declencheurs: e.declencheurs.clone(),
        projet: e.projet.clone(),
        depuis: e.depuis.clone(),
        source: None,
        quand: e.quand.clone(),
        confiance: e.confiance,
        preuves: Vec::new(),
        occurrences: None,
        revue: None,
        expire: None,
        sensible: false,
        remplace: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    fn index(clock: TestClock) -> MemoryIndex {
        MemoryIndex::new(Store::open_memory().unwrap(), Arc::new(clock))
    }

    fn prov() -> Provenance {
        Provenance::owner("s1", "interactive", "2026-09-16T10:00:00Z")
    }

    #[test]
    fn scoring_factors_follow_the_prd_table() {
        assert!((decay(0.0, 30.0, false) - 1.0).abs() < 1e-9);
        assert!((decay(30.0, 30.0, false) - 0.5).abs() < 1e-9);
        assert!((decay(60.0, 30.0, false) - 0.25).abs() < 1e-9);
        assert_eq!(decay(365.0, 30.0, true), 1.0, "un épinglé ne décroît pas");

        assert!((importance_factor(Some(10)) - 1.0).abs() < 1e-9);
        assert!((importance_factor(Some(0)) - 0.5).abs() < 1e-9);
        assert_eq!(importance_factor(None), 1.0);

        let active = vec!["penelope".to_string()];
        assert!((project_factor(Some("penelope"), &active) - 1.3).abs() < 1e-9);
        assert!((project_factor(Some("autre"), &active) - 0.85).abs() < 1e-9);
        assert_eq!(project_factor(None, &active), 1.0);

        assert!((confidence_factor(Some(0.8)) - 0.9).abs() < 1e-9);
        assert_eq!(confidence_factor(None), 1.0);
    }

    #[test]
    fn rrf_rewards_being_in_both_lists() {
        let both = rrf(Some(0), Some(0), 60.0);
        let one = rrf(Some(0), None, 60.0);
        assert!(both > one);
        assert!(rrf(Some(0), None, 60.0) > rrf(Some(10), None, 60.0));
        assert_eq!(rrf(None, None, 60.0), 0.0);
    }

    #[tokio::test]
    async fn upsert_and_get() {
        let i = index(TestClock::default());
        let e = simple_entry(
            "u1",
            "Le déploiement passe par CapRover.",
            Level::Coeur,
            "2026-09-16",
        );
        i.upsert(&e, &prov()).await.unwrap();
        let back = i.get("u1").await.unwrap().unwrap();
        assert_eq!(back.text, e.text);
        assert!(back.pinned);
        assert_eq!(i.origin_of("u1").await.unwrap(), Some(Origin::Owner));
    }

    #[tokio::test]
    async fn provenance_is_written_once_and_never_upgraded() {
        let i = index(TestClock::default());
        let e = simple_entry("u1", "texte", Level::Cure, "2026-09-16");
        let untrusted = Provenance {
            origin: Origin::Untrusted,
            session_kind: "interactive".into(),
            observed_at: "t".into(),
            supersedes_uid: None,
            source_ref: None,
            session_id: Some("s1".into()),
        };
        i.upsert(&e, &untrusted).await.unwrap();
        // Une seconde écriture prétendant `owner` ne doit pas écraser la provenance.
        i.upsert(&e, &prov()).await.unwrap();
        assert_eq!(
            i.origin_of("u1").await.unwrap(),
            Some(Origin::Untrusted),
            "la provenance est fixée à la première écriture"
        );
    }

    #[tokio::test]
    async fn fts_search_finds_entries() {
        let i = index(TestClock::default());
        for (uid, text) in [
            ("u1", "Le déploiement se fait par CapRover en production."),
            ("u2", "Les revues de code passent par une pull request."),
        ] {
            i.upsert(&simple_entry(uid, text, Level::Cure, "2026-09-16"), &prov())
                .await
                .unwrap();
        }
        let hits = i
            .search("deploiement", None, &SearchFilter::explicit(), &[])
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.uid, "u1");
        assert!(hits[0].fts_rank.is_some());
    }

    #[tokio::test]
    async fn vector_search_complements_fts() {
        let i = index(TestClock::default());
        i.upsert(
            &simple_entry("u1", "mise en ligne du service", Level::Cure, "2026-09-16"),
            &prov(),
        )
        .await
        .unwrap();
        i.put_embedding("u1", "m", &[1.0, 0.0, 0.0]).await.unwrap();

        // Le mot « déploiement » n'apparaît pas : seul le vecteur peut trouver.
        let hits = i
            .search(
                "déploiement",
                Some(vec![0.95, 0.1, 0.0]),
                &SearchFilter::explicit(),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].vec_rank.is_some());
        assert!(hits[0].similarity > 0.9);
    }

    #[tokio::test]
    async fn active_project_entries_rank_higher() {
        let clock = TestClock::default();
        let i = index(clock);
        for (uid, projet) in [("u1", "penelope"), ("u2", "autre")] {
            let mut e = simple_entry(
                uid,
                "la convention de nommage des branches",
                Level::Cure,
                "2026-09-16",
            );
            e.projet = Some(projet.to_string());
            i.upsert(&e, &prov()).await.unwrap();
        }
        let hits = i
            .search(
                "convention nommage",
                None,
                &SearchFilter::explicit(),
                &["penelope".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(hits[0].entry.uid, "u1", "le projet actif remonte");
    }

    #[tokio::test]
    async fn episodic_is_excluded_unless_explicitly_requested() {
        let i = index(TestClock::default());
        i.upsert(
            &{
                let mut e = simple_entry(
                    "u1",
                    "note du journal sur le déploiement",
                    Level::Episodic,
                    "2026-09-16",
                );
                e.file = "journal/2026-09-16.md".into();
                e
            },
            &prov(),
        )
        .await
        .unwrap();

        let implicit = SearchFilter {
            limit: 10,
            ..Default::default()
        };
        assert!(
            i.search("deploiement", None, &implicit, &[])
                .await
                .unwrap()
                .is_empty(),
            "l'épisodique n'est jamais injecté automatiquement"
        );
        assert_eq!(
            i.search("deploiement", None, &SearchFilter::explicit(), &[])
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn retire_removes_from_search_but_keeps_the_row() {
        let i = index(TestClock::default());
        i.upsert(
            &simple_entry(
                "u1",
                "une entrée sur le déploiement",
                Level::Cure,
                "2026-09-16",
            ),
            &prov(),
        )
        .await
        .unwrap();
        i.retire("u1").await.unwrap();
        assert!(
            i.search("deploiement", None, &SearchFilter::explicit(), &[])
                .await
                .unwrap()
                .is_empty()
        );
        let row = i.get("u1").await.unwrap().unwrap();
        assert_eq!(row.statut, "retiree");
        assert!(
            row.retired_at.is_some(),
            "le signal reste annulable 30 jours"
        );
    }

    /// #105 : le facteur d'usage monte avec les rappels utiles, descend quand les rappels
    /// ne servent pas, et reste borné.
    #[test]
    fn usage_factor_is_bounded_and_rewards_useful_recalls_only() {
        let sig = |recalls, useful_recalls, successes, contradictions| Signals {
            recalls,
            useful_recalls,
            successes,
            contradictions,
            ..Default::default()
        };
        assert_eq!(usage_factor(&Signals::default()), 1.0);
        let ten_useful = usage_factor(&sig(10, 10, 0, 0));
        assert!(
            ten_useful > 1.15 && ten_useful <= 1.0 + USAGE_MAX_GAIN,
            "{ten_useful}"
        );
        assert!(
            usage_factor(&sig(20, 0, 0, 0)) < 1.0,
            "vingt rappels inutiles"
        );
        assert!((usage_factor(&sig(20, 0, 0, 0)) - (1.0 - USAGE_MAX_LOSS)).abs() < 1e-9);
        assert_eq!(usage_factor(&sig(3, 0, 0, 0)), 1.0, "trop peu pour juger");
        assert!(usage_factor(&sig(0, 0, 0, 4)) < 1.0, "contredite");
        let huge = usage_factor(&sig(10_000, 10_000, 10_000, 0));
        assert!(huge <= 1.0 + USAGE_MAX_GAIN + 1e-12, "{huge}");
    }

    /// #105 : à pertinence égale, dix rappels utiles font passer devant ; vingt rappels
    /// inutiles ne font pas monter ; une correspondance exacte jamais rappelée reste
    /// devant une entrée populaire qui ne correspond qu'à moitié.
    #[tokio::test]
    async fn usage_orders_equal_matches_without_beating_a_better_one() {
        let i = index(TestClock::default());
        for uid in ["neuf", "servi", "ignore"] {
            i.upsert(
                &simple_entry(
                    uid,
                    "la sauvegarde nocturne du serveur",
                    Level::Cure,
                    "2026-09-16",
                ),
                &prov(),
            )
            .await
            .unwrap();
        }
        for _ in 0..10 {
            i.record_recall("servi", "sauvegarde", true).await.unwrap();
        }
        for _ in 0..20 {
            i.record_served("ignore", "sauvegarde", true).await.unwrap();
        }
        let hits = i
            .search("sauvegarde nocturne", None, &SearchFilter::explicit(), &[])
            .await
            .unwrap();
        let order: Vec<&str> = hits.iter().map(|h| h.entry.uid.as_str()).collect();
        // Textes identiques : leurs rangs FTS ne diffèrent que par l'ordre d'insertion
        // (moins de 4 % de pertinence), l'usage décide.
        assert_eq!(order, ["servi", "neuf", "ignore"], "{hits:?}");

        // Correspondance exacte (mots et sens) jamais rappelée, contre une entrée
        // populaire trouvée par un seul des deux chemins.
        let i = index(TestClock::default());
        i.upsert(
            &simple_entry(
                "exacte",
                "le code du portail est 4521",
                Level::Cure,
                "2026-09-16",
            ),
            &prov(),
        )
        .await
        .unwrap();
        i.upsert(
            &simple_entry(
                "populaire",
                "la boîte aux lettres du voisin",
                Level::Cure,
                "2026-09-16",
            ),
            &prov(),
        )
        .await
        .unwrap();
        i.put_embedding("exacte", "m", &[1.0, 0.0, 0.0])
            .await
            .unwrap();
        i.put_embedding("populaire", "m", &[0.6, 0.8, 0.0])
            .await
            .unwrap();
        for _ in 0..50 {
            i.record_recall("populaire", "portail", true).await.unwrap();
        }
        let hits = i
            .search(
                "code du portail",
                Some(vec![1.0, 0.0, 0.0]),
                &SearchFilter::explicit(),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(hits[0].entry.uid, "exacte", "{hits:?}");
        assert!(hits[1].usage > 1.15);
    }

    #[tokio::test]
    async fn signals_accumulate() {
        let i = index(TestClock::default());
        i.upsert(
            &simple_entry("u1", "texte", Level::Cure, "2026-09-16"),
            &prov(),
        )
        .await
        .unwrap();
        i.record_recall("u1", "Comment on déploie ?", true)
            .await
            .unwrap();
        i.record_recall("u1", "comment on déploie ?", true)
            .await
            .unwrap();
        i.record_recall("u1", "quelle est la procédure ?", false)
            .await
            .unwrap();
        let s = i.signals_of("u1").await.unwrap();
        assert_eq!(s.recalls, 3);
        assert_eq!(s.useful_recalls, 2);
        assert_eq!(
            s.distinct_queries.len(),
            2,
            "deux requêtes distinctes seulement (casse normalisée)"
        );

        i.record_outcome("u1", true).await.unwrap();
        i.record_outcome("u1", false).await.unwrap();
        let s = i.signals_of("u1").await.unwrap();
        assert_eq!(s.successes, 1);
        assert_eq!(s.contradictions, 1);

        // #105 : servi hors conversation, rien ne compte ; servi en conversation, le
        // rappel compte et son utilité se juge après coup, jamais au-delà des rappels.
        i.record_served("u1", "hors conversation", false)
            .await
            .unwrap();
        let s = i.signals_of("u1").await.unwrap();
        assert_eq!((s.recalls, s.useful_recalls), (3, 2));
        assert!(
            s.distinct_queries
                .contains(&"hors conversation".to_string())
        );
        i.record_served("u1", "où ?", true).await.unwrap();
        i.mark_useful("u1").await.unwrap();
        i.mark_useful("u1").await.unwrap();
        i.mark_useful("u1").await.unwrap();
        let s = i.signals_of("u1").await.unwrap();
        assert_eq!((s.recalls, s.useful_recalls), (4, 4));
    }

    /// CA 6 (reconstruction) : l'index dérivé peut être effacé et reconstruit ; la
    /// provenance et les signaux survivent.
    #[tokio::test]
    async fn ca_6_9_reindex_keeps_provenance_and_signals() {
        let i = index(TestClock::default());
        let e = simple_entry(
            "u1",
            "Le déploiement passe par CapRover.",
            Level::Cure,
            "2026-09-16",
        );
        i.upsert(&e, &prov()).await.unwrap();
        i.record_recall("u1", "deploiement", true).await.unwrap();

        i.clear_derived().await.unwrap();
        assert_eq!(i.count().await.unwrap(), 0);
        assert_eq!(
            i.origin_of("u1").await.unwrap(),
            Some(Origin::Owner),
            "la provenance n'est pas effacée"
        );
        assert_eq!(i.signals_of("u1").await.unwrap().recalls, 1);

        // Reconstruction depuis le vault : mêmes uid, mêmes résultats.
        i.upsert(&e, &prov()).await.unwrap();
        let hits = i
            .search("deploiement", None, &SearchFilter::explicit(), &[])
            .await
            .unwrap();
        assert_eq!(hits[0].entry.uid, "u1");
        assert_eq!(i.signals_of("u1").await.unwrap().recalls, 1);
    }

    #[tokio::test]
    async fn forget_session_retires_its_entries() {
        let i = index(TestClock::default());
        i.upsert(
            &simple_entry(
                "u1",
                "entrée de la session s1 sur le déploiement",
                Level::Cure,
                "2026-09-16",
            ),
            &Provenance::owner("s1", "interactive", "t"),
        )
        .await
        .unwrap();
        i.upsert(
            &simple_entry(
                "u2",
                "entrée de la session s2 sur le déploiement",
                Level::Cure,
                "2026-09-16",
            ),
            &Provenance::owner("s2", "interactive", "t"),
        )
        .await
        .unwrap();

        let forgotten = i.forget_session("s1").await.unwrap();
        assert_eq!(forgotten, vec!["u1"]);
        let hits = i
            .search("deploiement", None, &SearchFilter::explicit(), &[])
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.uid, "u2");
    }

    #[tokio::test]
    async fn links_are_indexed() {
        let i = index(TestClock::default());
        i.upsert(
            &simple_entry(
                "u1",
                "voir [[client-x]] et [[projet-a]]",
                Level::Cure,
                "2026-09-16",
            ),
            &prov(),
        )
        .await
        .unwrap();
        let n: i64 = i
            .store()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM mem_links WHERE from_uid='u1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn age_is_computed_from_dates_and_timestamps() {
        // 2026-09-16T00:00:00Z
        let now = 1_789_516_800_000i64;
        assert!((age_days("2026-09-16", now) - 0.0).abs() < 0.01);
        assert!((age_days("2026-09-06", now) - 10.0).abs() < 0.01);
        assert_eq!(age_days("pas une date", now), 0.0);
    }

    #[test]
    fn fts_query_drops_short_words() {
        assert_eq!(fts_query("le déploiement"), "\"déploiement\"*");
        assert_eq!(fts_query("a b"), "");
    }
}
