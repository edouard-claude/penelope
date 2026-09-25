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

mod search;
mod signals;

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
mod tests;
