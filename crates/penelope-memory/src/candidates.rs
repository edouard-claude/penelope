//! Candidats d'apprentissage (§6.6) : ce que la revue de fond produit après chaque tour.
//!
//! Un candidat n'est **jamais** écrit dans le niveau curé : il est noté dans le journal et
//! dans `mem_candidates`, puis la consolidation nocturne décide.

use crate::provenance::Origin;
use crate::vault::When;
use penelope_kernel::clock::SharedClock;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateType {
    Fait,
    Preference,
    Correction,
    Ecart,
    Decision,
    ProcedureCandidate,
}

impl CandidateType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CandidateType::Fait => "fait",
            CandidateType::Preference => "preference",
            CandidateType::Correction => "correction",
            CandidateType::Ecart => "ecart",
            CandidateType::Decision => "decision",
            CandidateType::ProcedureCandidate => "procedure_candidate",
        }
    }
    pub fn parse(s: &str) -> Option<CandidateType> {
        Some(match s {
            "fait" => CandidateType::Fait,
            "preference" => CandidateType::Preference,
            "correction" => CandidateType::Correction,
            "ecart" => CandidateType::Ecart,
            "decision" => CandidateType::Decision,
            "procedure_candidate" => CandidateType::ProcedureCandidate,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub ctype: CandidateType,
    pub text: String,
    pub quand: Option<When>,
    pub importance: u8,
    pub origin: Origin,
    pub session_id: Option<String>,
    pub session_kind: String,
    pub observed_at: String,
    pub day: String,
    /// Clé de sujet : pratique, entité ou préférence visée.
    pub subject_key: Option<String>,
    pub target_slug: Option<String>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub source_ref: Option<String>,
    /// Anti-boucle : le texte vient d'un rappel mémoire, pas d'une observation.
    pub from_memory: bool,
}

impl Candidate {
    pub fn new(
        ctype: CandidateType,
        text: &str,
        origin: Origin,
        session_kind: &str,
        at: &str,
    ) -> Candidate {
        Candidate {
            id: format!("c_{}", penelope_kernel::ids::Ulid::new()),
            ctype,
            text: text.to_string(),
            quand: None,
            importance: 5,
            origin,
            session_id: None,
            session_kind: session_kind.to_string(),
            observed_at: at.to_string(),
            day: at.chars().take(10).collect(),
            subject_key: None,
            target_slug: None,
            state: "new".into(),
            reject_reason: None,
            source_ref: None,
            from_memory: false,
        }
    }

    pub fn with_when(mut self, w: When) -> Self {
        self.quand = Some(w);
        self
    }
    pub fn with_importance(mut self, i: u8) -> Self {
        self.importance = i;
        self
    }
    pub fn with_subject(mut self, s: &str) -> Self {
        self.subject_key = Some(s.to_string());
        self
    }
    pub fn in_session(mut self, id: &str) -> Self {
        self.session_id = Some(id.to_string());
        self
    }

    /// Signature de contexte : la clé de regroupement de la phase Light (§6.8).
    pub fn context_signature(&self) -> String {
        self.quand
            .as_ref()
            .map(|w| w.render())
            .unwrap_or_else(|| "*".into())
    }

    pub fn group_key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.ctype.as_str(),
            self.subject_key
                .clone()
                .unwrap_or_else(|| subject_from_text(&self.text)),
            self.context_signature()
        )
    }
}

/// Clé de sujet dérivée du texte, quand elle n'est pas fournie : les mots significatifs.
pub fn subject_from_text(text: &str) -> String {
    let stop: BTreeSet<&str> = [
        "le", "la", "les", "un", "une", "des", "de", "du", "et", "ou", "on", "il", "elle", "que",
        "qui", "pour", "par", "en", "dans", "sur", "avec", "sans", "est", "sont", "toujours",
        "jamais", "faut", "doit",
    ]
    .into_iter()
    .collect();
    let mut words: Vec<String> = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| w.chars().count() > 3 && !stop.contains(w))
        .map(String::from)
        .collect();
    words.sort();
    words.dedup();
    words.truncate(5);
    words.join("-")
}

/// Similarité de Jaccard sur les mots (§6.8, dédoublonnage ≥ 0,9).
pub fn jaccard(a: &str, b: &str) -> f64 {
    let wa: BTreeSet<String> = words(a);
    let wb: BTreeSet<String> = words(b);
    if wa.is_empty() && wb.is_empty() {
        return 1.0;
    }
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

fn words(s: &str) -> BTreeSet<String> {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// Groupe de candidats agrégés par sujet et signature de contexte.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateGroup {
    pub key: String,
    pub ctype: CandidateType,
    pub representative: Candidate,
    pub members: Vec<Candidate>,
    pub occurrences: u32,
    pub distinct_sessions: u32,
    pub distinct_days: u32,
    pub max_importance: u8,
    pub origins: BTreeSet<Origin>,
}

impl CandidateGroup {
    pub fn has_owner_origin(&self) -> bool {
        self.origins.contains(&Origin::Owner)
    }
    /// Signature `quand` commune, si les membres sont compatibles.
    pub fn common_when(&self) -> Option<When> {
        let mut iter = self.members.iter().filter_map(|m| m.quand.clone());
        let first = iter.next()?;
        let mut acc = first;
        for w in iter {
            if !acc.compatible_with(&w) {
                return None;
            }
            acc = acc.intersect(&w);
        }
        Some(acc)
    }
}

/// Regroupe et dédoublonne (phase Light, §6.8).
pub fn group(candidates: Vec<Candidate>, jaccard_threshold: f64) -> Vec<CandidateGroup> {
    let mut by_key: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for c in candidates {
        by_key.entry(c.group_key()).or_default().push(c);
    }

    let mut out = Vec::new();
    for (key, members) in by_key {
        // Dédoublonnage interne : deux formulations quasi identiques comptent pour une.
        let mut unique: Vec<Candidate> = Vec::new();
        for m in members {
            if unique
                .iter()
                .any(|u| jaccard(&u.text, &m.text) >= jaccard_threshold)
            {
                // Doublon : on garde la formulation, mais l'occurrence compte quand même
                // si elle vient d'une autre session ou d'un autre jour.
                if unique
                    .iter()
                    .any(|u| u.session_id == m.session_id && u.day == m.day)
                {
                    continue;
                }
            }
            unique.push(m);
        }

        let sessions: BTreeSet<String> =
            unique.iter().filter_map(|c| c.session_id.clone()).collect();
        let days: BTreeSet<String> = unique.iter().map(|c| c.day.clone()).collect();
        let origins: BTreeSet<Origin> = unique.iter().map(|c| c.origin).collect();
        let max_importance = unique.iter().map(|c| c.importance).max().unwrap_or(5);
        let representative = unique
            .iter()
            .max_by_key(|c| c.importance)
            .cloned()
            .expect("groupe non vide");
        let ctype = representative.ctype;

        out.push(CandidateGroup {
            key,
            ctype,
            representative,
            occurrences: unique.len() as u32,
            distinct_sessions: sessions.len() as u32,
            distinct_days: days.len() as u32,
            max_importance,
            origins,
            members: unique,
        });
    }
    out
}

#[derive(Clone)]
pub struct CandidateStore {
    store: Store,
    clock: SharedClock,
}

impl CandidateStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        CandidateStore { store, clock }
    }

    /// Enregistre les candidats d'un tour, sous plafond (§6.6 : au plus 5 par tour).
    pub async fn record(
        &self,
        mut candidates: Vec<Candidate>,
        max_per_turn: usize,
    ) -> penelope_store::Result<usize> {
        candidates.sort_by_key(|c| std::cmp::Reverse(c.importance));
        candidates.truncate(max_per_turn);
        // L'anti-boucle et la provenance filtrent **avant** l'écriture.
        let accepted: Vec<Candidate> = candidates
            .into_iter()
            .filter(|c| !c.from_memory)
            .filter(|c| {
                crate::provenance::session_allows_candidate(&c.session_kind, c.ctype.as_str(), true)
            })
            .collect();
        let n = accepted.len();

        self.store
            .write(move |tx| {
                for c in &accepted {
                    tx.execute(
                        "INSERT OR REPLACE INTO mem_candidates(id, ctype, text, quand, importance,
                            origin, session_id, session_kind, observed_at, day, subject_key,
                            target_slug, state, reject_reason, source_ref, from_memory)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                        params![
                            c.id,
                            c.ctype.as_str(),
                            c.text,
                            c.quand.as_ref().map(|w| w.render()),
                            c.importance as i64,
                            c.origin.as_str(),
                            c.session_id,
                            c.session_kind,
                            c.observed_at,
                            c.day,
                            c.subject_key,
                            c.target_slug,
                            c.state,
                            c.reject_reason,
                            c.source_ref,
                            c.from_memory as i64
                        ],
                    )?;
                }
                Ok(())
            })
            .await?;
        Ok(n)
    }

    /// Candidats à traiter par la consolidation.
    pub async fn pending(&self, since: Option<&str>) -> penelope_store::Result<Vec<Candidate>> {
        let since = since.map(String::from);
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, ctype, text, quand, importance, origin, session_id, session_kind,
                            observed_at, day, subject_key, target_slug, state, reject_reason,
                            source_ref, from_memory
                     FROM mem_candidates
                     WHERE state IN ('new','grouped','deferred')
                       AND (?1 IS NULL OR observed_at >= ?1)
                     ORDER BY observed_at",
                )?;
                let rows = st.query_map([since], row_to_candidate)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Candidats confirmés par le propriétaire : origine `owner`, de nouveau à consolider.
    pub async fn confirm_by_owner(&self, ids: &[String]) -> penelope_store::Result<usize> {
        let ids = ids.to_vec();
        self.store
            .write(move |tx| {
                let mut n = 0;
                for id in &ids {
                    n += tx.execute(
                        "UPDATE mem_candidates SET origin = 'owner', state = 'new', reject_reason = NULL
                         WHERE id = ?1",
                        [id],
                    )?;
                }
                Ok(n)
            })
            .await
    }

    /// Remet à consolider les règles rejetées pour leur seule origine (issue #24) : elles
    /// seront demandées au propriétaire.
    pub async fn retry_origin_rejections(&self) -> penelope_store::Result<usize> {
        self.store
            .write(|tx| {
                let mut n = 0;
                for reason in crate::consolidation::LEGACY_ORIGIN_REJECTIONS {
                    n += tx.execute(
                        "UPDATE mem_candidates SET state = 'new', reject_reason = NULL
                         WHERE state = 'rejected' AND reject_reason = ?1",
                        [reason],
                    )?;
                }
                Ok(n)
            })
            .await
    }

    pub async fn set_state(
        &self,
        ids: &[String],
        state: &str,
        reason: Option<&str>,
    ) -> penelope_store::Result<usize> {
        let ids = ids.to_vec();
        let (state, reason) = (state.to_string(), reason.map(String::from));
        self.store
            .write(move |tx| {
                let mut n = 0;
                for id in &ids {
                    n += tx.execute(
                        "UPDATE mem_candidates SET state=?2, reject_reason=?3 WHERE id=?1",
                        params![id, state, reason],
                    )?;
                }
                Ok(n)
            })
            .await
    }

    pub async fn count_by_state(&self, state: &str) -> penelope_store::Result<i64> {
        let s = state.to_string();
        self.store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM mem_candidates WHERE state = ?1",
                    [s],
                    |r| r.get(0),
                )?)
            })
            .await
    }

    /// Marque les écarts non promus depuis N jours comme expirés (§6.8, élagage).
    pub async fn expire_stale(&self, days: i64) -> penelope_store::Result<usize> {
        let cutoff = (self.clock.now_utc() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE mem_candidates SET state='expired'
                     WHERE ctype='ecart' AND state IN ('new','grouped','deferred')
                       AND observed_at < ?1",
                    [cutoff],
                )?)
            })
            .await
    }
}

fn row_to_candidate(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<Candidate> {
    let ctype: String = r.get(1)?;
    let quand: Option<String> = r.get(3)?;
    let origin: String = r.get(5)?;
    Ok(Candidate {
        id: r.get(0)?,
        ctype: CandidateType::parse(&ctype).unwrap_or(CandidateType::Fait),
        text: r.get(2)?,
        quand: quand.and_then(|q| When::parse(&q).ok()),
        importance: r.get::<_, i64>(4)? as u8,
        origin: Origin::parse(&origin).unwrap_or(Origin::System),
        session_id: r.get(6)?,
        session_kind: r.get(7)?,
        observed_at: r.get(8)?,
        day: r.get(9)?,
        subject_key: r.get(10)?,
        target_slug: r.get(11)?,
        state: r.get(12)?,
        reject_reason: r.get(13)?,
        source_ref: r.get(14)?,
        from_memory: r.get::<_, i64>(15)? != 0,
    })
}

/// Détecte une **correction** : l'utilisateur reprend l'agent (§6.6).
pub fn looks_like_correction(user_message: &str) -> bool {
    let m = user_message.to_lowercase();
    const NEEDLES: &[&str] = &[
        "non,",
        "non ",
        "pas comme ça",
        "ici on fait",
        "chez nous on",
        "c'est faux",
        "ce n'est pas",
        "plutôt",
        "en fait on",
        "attention, on",
        "ne fais pas",
        "j'ai dit",
        "je t'ai dit",
    ];
    NEEDLES.iter().any(|n| m.starts_with(n) || m.contains(n))
}

/// Détecte une **préférence formulée comme une règle** (§6.8 : 1 occurrence suffit).
pub fn stated_as_a_rule(text: &str) -> bool {
    let t = text.to_lowercase();
    const NEEDLES: &[&str] = &[
        "toujours",
        "jamais",
        "désormais",
        "à partir de maintenant",
        "systématiquement",
        "par défaut",
        "en règle générale",
    ];
    NEEDLES.iter().any(|n| t.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    fn cs() -> CandidateStore {
        CandidateStore::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        )
    }

    fn cand(text: &str, day: &str, session: &str) -> Candidate {
        let mut c = Candidate::new(
            CandidateType::Ecart,
            text,
            Origin::Owner,
            "interactive",
            &format!("{day}T10:00:00Z"),
        )
        .in_session(session)
        .with_subject("langage-backend");
        c.quand = When::parse("client=client-x").ok();
        c
    }

    #[test]
    fn jaccard_similarity() {
        assert!(
            jaccard(
                "le déploiement passe par caprover",
                "le déploiement passe par caprover"
            ) > 0.99
        );
        assert!(
            jaccard(
                "le déploiement passe par CapRover",
                "Le déploiement passe par caprover !"
            ) > 0.99
        );
        assert!(jaccard("le déploiement", "la revue de code") < 0.3);
    }

    #[test]
    fn subject_keys_ignore_stop_words() {
        let a = subject_from_text("Toujours utiliser Go pour le backend");
        let b = subject_from_text("utiliser Go pour le backend, toujours");
        assert_eq!(a, b, "l'ordre et les mots vides ne comptent pas");
        assert!(a.contains("backend"));
    }

    #[test]
    fn grouping_counts_sessions_and_days() {
        let candidates = vec![
            cand("langage imposé par l'existant", "2026-09-10", "s1"),
            cand("langage imposé par l'existant", "2026-09-11", "s2"),
            cand("langage imposé par l'existant", "2026-09-12", "s3"),
        ];
        let groups = group(candidates, 0.9);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.occurrences, 3);
        assert_eq!(g.distinct_sessions, 3);
        assert_eq!(g.distinct_days, 3);
        assert!(g.has_owner_origin());
        assert_eq!(g.common_when().unwrap().render(), "client=client-x");
    }

    #[test]
    fn duplicates_in_the_same_session_and_day_collapse() {
        let candidates = vec![
            cand("langage imposé par l'existant", "2026-09-10", "s1"),
            cand("langage imposé par l'existant !", "2026-09-10", "s1"),
        ];
        let groups = group(candidates, 0.9);
        assert_eq!(
            groups[0].occurrences, 1,
            "même session, même jour : une occurrence"
        );
    }

    #[test]
    fn different_context_signatures_are_different_groups() {
        let mut a = cand("langage imposé", "2026-09-10", "s1");
        a.quand = When::parse("client=client-x").ok();
        let mut b = cand("langage imposé", "2026-09-10", "s2");
        b.quand = When::parse("client=client-y").ok();
        assert_eq!(group(vec![a, b], 0.9).len(), 2);
    }

    #[tokio::test]
    async fn at_most_five_candidates_per_turn() {
        let s = cs();
        let candidates: Vec<Candidate> = (0..12)
            .map(|i| {
                Candidate::new(
                    CandidateType::Fait,
                    &format!("fait numéro {i}"),
                    Origin::Owner,
                    "interactive",
                    "2026-09-16T10:00:00Z",
                )
                .with_importance(i as u8)
            })
            .collect();
        let n = s.record(candidates, 5).await.unwrap();
        assert_eq!(n, 5);
        assert_eq!(s.count_by_state("new").await.unwrap(), 5);
        // Les plus importants sont gardés.
        let pending = s.pending(None).await.unwrap();
        assert!(pending.iter().all(|c| c.importance >= 7));
    }

    #[tokio::test]
    async fn memory_echoes_are_never_recorded() {
        let s = cs();
        let mut c = Candidate::new(
            CandidateType::Fait,
            "un fait rappelé depuis la mémoire",
            Origin::Owner,
            "interactive",
            "2026-09-16T10:00:00Z",
        );
        c.from_memory = true;
        assert_eq!(s.record(vec![c], 5).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn background_sessions_record_nothing() {
        let s = cs();
        let c = Candidate::new(
            CandidateType::Fait,
            "un fait vu par un cron",
            Origin::Agent,
            "scheduled",
            "2026-09-16T10:00:00Z",
        );
        assert_eq!(s.record(vec![c], 5).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn state_transitions_and_expiry() {
        let clock = TestClock::default();
        let s = CandidateStore::new(Store::open_memory().unwrap(), Arc::new(clock.clone()));
        let c = cand("un écart", "2026-01-01", "s1");
        let id = c.id.clone();
        s.record(vec![c], 5).await.unwrap();

        s.set_state(std::slice::from_ref(&id), "grouped", None)
            .await
            .unwrap();
        assert_eq!(s.count_by_state("grouped").await.unwrap(), 1);

        // 100 jours plus tard, un écart non promu expire.
        clock.advance_days(100);
        assert_eq!(s.expire_stale(90).await.unwrap(), 1);
        assert_eq!(s.count_by_state("expired").await.unwrap(), 1);
    }

    #[test]
    fn correction_detection() {
        assert!(looks_like_correction("non, ici on fait autrement"));
        assert!(looks_like_correction("Plutôt en Rust pour ce projet"));
        assert!(!looks_like_correction("ajoute un test d'intégration"));
    }

    #[test]
    fn rule_phrasing_detection() {
        assert!(stated_as_a_rule("Toujours répondre en français"));
        assert!(stated_as_a_rule("désormais on passe par CapRover"));
        assert!(!stated_as_a_rule("cette fois-ci on fait autrement"));
    }
}
