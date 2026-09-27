//! Mémoire prospective : intentions événementielles (§6.9).
//!
//! Une intention **temporelle** est compilée en schedule. Une intention **événementielle**
//! attend qu'un sujet revienne : préfiltre déterministe (lexical + vecteur ≥ 0,72),
//! cooldown, budget de tirs, expiration.

use penelope_kernel::clock::SharedClock;
use penelope_store::{
    Store, cosine_similarity, decode_embedding, encode_embedding, rusqlite::params,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentState {
    Armee,
    Tiree,
    Terminee,
    Annulee,
    Expiree,
}

impl IntentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntentState::Armee => "armee",
            IntentState::Tiree => "tiree",
            IntentState::Terminee => "terminee",
            IntentState::Annulee => "annulee",
            IntentState::Expiree => "expiree",
        }
    }
    pub fn parse(s: &str) -> Option<IntentState> {
        Some(match s {
            "armee" => IntentState::Armee,
            "tiree" => IntentState::Tiree,
            "terminee" => IntentState::Terminee,
            "annulee" => IntentState::Annulee,
            "expiree" => IntentState::Expiree,
            _ => return None,
        })
    }
    pub fn can_fire(&self) -> bool {
        matches!(self, IntentState::Armee | IntentState::Tiree)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    pub id: String,
    pub texte: String,
    pub declencheurs: Vec<String>,
    pub portee: Option<String>,
    pub created_at: String,
    pub expire_at: Option<String>,
    pub budget_tirs: u32,
    pub tirs: u32,
    pub cooldown_ms: i64,
    pub last_fired: Option<String>,
    pub etat: IntentState,
}

impl Intent {
    /// Vrai si l'intention peut tirer maintenant : état, budget, cooldown, expiration.
    pub fn is_eligible(&self, now_ms: i64) -> bool {
        if !self.etat.can_fire() {
            return false;
        }
        if self.tirs >= self.budget_tirs {
            return false;
        }
        if let Some(exp) = &self.expire_at
            && rfc3339_ms(exp) <= now_ms
        {
            return false;
        }
        if let Some(last) = &self.last_fired
            && now_ms - rfc3339_ms(last) < self.cooldown_ms
        {
            return false;
        }
        true
    }

    /// Score de correspondance lexicale : proportion de déclencheurs présents.
    pub fn lexical_score(&self, message: &str) -> f64 {
        if self.declencheurs.is_empty() {
            return 0.0;
        }
        let m = message.to_lowercase();
        let hits = self
            .declencheurs
            .iter()
            .filter(|d| m.contains(&d.to_lowercase()))
            .count();
        hits as f64 / self.declencheurs.len() as f64
    }
}

/// Distingue une intention temporelle (compilée en schedule) d'une intention
/// événementielle (§6.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentKind {
    /// « rappelle-moi vendredi » : compilée immédiatement en schedule.
    Temporal(String),
    /// « quand on reparle de X… » : stockée et armée.
    Eventual,
}

/// Analyse déterministe de l'énoncé d'une intention.
pub fn classify_intent(text: &str) -> IntentKind {
    let t = text.to_lowercase();
    const TEMPORAL: &[&str] = &[
        "demain",
        "après-demain",
        "lundi",
        "mardi",
        "mercredi",
        "jeudi",
        "vendredi",
        "samedi",
        "dimanche",
        "dans une heure",
        "dans 1 heure",
        "ce soir",
        "la semaine prochaine",
        "le mois prochain",
        "à 9h",
        "à 14h",
        "chaque jour",
        "tous les jours",
        "chaque semaine",
    ];
    const EVENTUAL: &[&str] = &[
        "quand on",
        "dès qu'on",
        "la prochaine fois qu",
        "si on reparle",
    ];

    if EVENTUAL.iter().any(|n| t.contains(n)) {
        return IntentKind::Eventual;
    }
    for n in TEMPORAL {
        if t.contains(n) {
            return IntentKind::Temporal((*n).to_string());
        }
    }
    IntentKind::Eventual
}

/// Extrait des déclencheurs lexicaux d'un énoncé d'intention.
pub fn extract_triggers(text: &str) -> Vec<String> {
    let stop = [
        "quand",
        "on",
        "reparle",
        "de",
        "du",
        "des",
        "la",
        "le",
        "les",
        "rappelle",
        "moi",
        "rappelle-moi",
        "prochaine",
        "fois",
        "qu",
        "que",
        "si",
        "un",
        "une",
        "et",
        "à",
        "au",
        "pour",
        "dans",
        "sur",
        "avec",
    ];
    let mut v: Vec<String> = text
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| w.chars().count() > 3 && !stop.contains(w))
        .map(String::from)
        .collect();
    v.sort();
    v.dedup();
    v.truncate(8);
    v
}

#[derive(Clone)]
pub struct IntentStore {
    store: Store,
    clock: SharedClock,
}

impl IntentStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        IntentStore { store, clock }
    }

    pub async fn create(
        &self,
        texte: &str,
        declencheurs: Vec<String>,
        portee: Option<&str>,
        cooldown_ms: i64,
        budget_tirs: u32,
        expiry_ms: Option<i64>,
    ) -> penelope_store::Result<Intent> {
        let now_ms = self.clock.now_ms();
        let intent = Intent {
            id: format!("i_{}", penelope_kernel::ids::Ulid::new()),
            texte: texte.to_string(),
            declencheurs,
            portee: portee.map(String::from),
            created_at: self.clock.now_rfc3339(),
            expire_at: expiry_ms.map(|d| ms_to_rfc3339(now_ms + d)),
            budget_tirs,
            tirs: 0,
            cooldown_ms,
            last_fired: None,
            etat: IntentState::Armee,
        };
        let row = intent.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO intents(id, texte, declencheurs, portee, created_at, expire_at,
                        budget_tirs, tirs, cooldown_ms, last_fired, etat)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8,NULL,'armee')",
                    params![
                        row.id,
                        row.texte,
                        serde_json::to_string(&row.declencheurs).unwrap_or_default(),
                        row.portee,
                        row.created_at,
                        row.expire_at,
                        row.budget_tirs as i64,
                        row.cooldown_ms
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(intent)
    }

    pub async fn put_embedding(&self, id: &str, v: &[f32]) -> penelope_store::Result<()> {
        let (id, blob, dim) = (id.to_string(), encode_embedding(v), v.len() as i64);
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO intent_vec(id, dim, embedding) VALUES(?1,?2,?3)
                     ON CONFLICT(id) DO UPDATE SET dim=excluded.dim, embedding=excluded.embedding",
                    params![id, dim, blob],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn active(&self) -> penelope_store::Result<Vec<Intent>> {
        self.store
            .read(|c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE etat IN ('armee','tiree') ORDER BY created_at"
                ))?;
                let rows = st.query_map([], row_to_intent)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    pub async fn all(&self) -> penelope_store::Result<Vec<Intent>> {
        self.store
            .read(|c| {
                let mut st = c.prepare(&format!("{SELECT} ORDER BY created_at DESC"))?;
                let rows = st.query_map([], row_to_intent)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Préfiltre déterministe (§6.9) : lexical **ou** vecteur ≥ seuil, au plus N par tour.
    pub async fn matching(
        &self,
        message: &str,
        message_vector: Option<&[f32]>,
        threshold: f64,
        max_per_turn: usize,
    ) -> penelope_store::Result<Vec<Intent>> {
        let now_ms = self.clock.now_ms();
        let candidates = self.active().await?;
        let vectors = match message_vector {
            Some(_) => self.embeddings().await?,
            None => Default::default(),
        };

        let mut scored: Vec<(Intent, f64)> = Vec::new();
        for i in candidates {
            if !i.is_eligible(now_ms) {
                continue;
            }
            let lex = i.lexical_score(message);
            let vec_score = match (message_vector, vectors.get(&i.id)) {
                (Some(q), Some(v)) => cosine_similarity(q, v) as f64,
                _ => 0.0,
            };
            let score = lex.max(vec_score);
            if score >= threshold {
                scored.push((i, score));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored
            .into_iter()
            .take(max_per_turn)
            .map(|(i, _)| i)
            .collect())
    }

    async fn embeddings(
        &self,
    ) -> penelope_store::Result<std::collections::BTreeMap<String, Vec<f32>>> {
        self.store
            .read(|c| {
                let mut st = c.prepare("SELECT id, embedding FROM intent_vec")?;
                let rows = st.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })?;
                let mut m = std::collections::BTreeMap::new();
                for r in rows {
                    let (id, blob) = r?;
                    m.insert(id, decode_embedding(&blob));
                }
                Ok(m)
            })
            .await
    }

    /// Enregistre un tir : incrémente le compteur, pose le cooldown, termine si le budget
    /// est épuisé.
    pub async fn fire(&self, id: &str) -> penelope_store::Result<Intent> {
        let (id, now) = (id.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE intents SET tirs = tirs + 1, last_fired = ?2,
                     etat = CASE WHEN tirs + 1 >= budget_tirs THEN 'terminee' ELSE 'tiree' END
                     WHERE id = ?1",
                    params![id, now],
                )?;
                let mut st = tx.prepare(&format!("{SELECT} WHERE id = ?1"))?;
                let mut rows = st.query([&id])?;
                let r = rows
                    .next()?
                    .ok_or_else(|| penelope_store::StoreError::other("intention introuvable"))?;
                row_to_intent(r).map_err(penelope_store::StoreError::from)
            })
            .await
    }

    pub async fn cancel(&self, id: &str) -> penelope_store::Result<bool> {
        let id = id.to_string();
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE intents SET etat='annulee' WHERE id=?1 AND etat IN ('armee','tiree')",
                    [id],
                )? > 0)
            })
            .await
    }

    /// Passe les intentions échues en `expiree`.
    pub async fn expire_due(&self) -> penelope_store::Result<usize> {
        let now = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE intents SET etat='expiree'
                     WHERE etat IN ('armee','tiree') AND expire_at IS NOT NULL AND expire_at <= ?1",
                    [now],
                )?)
            })
            .await
    }
}

const SELECT: &str = "SELECT id, texte, declencheurs, portee, created_at, expire_at,
     budget_tirs, tirs, cooldown_ms, last_fired, etat FROM intents";

fn row_to_intent(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<Intent> {
    let declencheurs: String = r.get(2)?;
    let etat: String = r.get(10)?;
    Ok(Intent {
        id: r.get(0)?,
        texte: r.get(1)?,
        declencheurs: serde_json::from_str(&declencheurs).unwrap_or_default(),
        portee: r.get(3)?,
        created_at: r.get(4)?,
        expire_at: r.get(5)?,
        budget_tirs: r.get::<_, i64>(6)? as u32,
        tirs: r.get::<_, i64>(7)? as u32,
        cooldown_ms: r.get(8)?,
        last_fired: r.get(9)?,
        etat: IntentState::parse(&etat).unwrap_or(IntentState::Armee),
    })
}

fn rfc3339_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    fn intents(clock: TestClock) -> IntentStore {
        IntentStore::new(Store::open_memory().unwrap(), Arc::new(clock))
    }

    #[test]
    fn temporal_and_eventual_intents_are_distinguished() {
        assert_eq!(
            classify_intent("rappelle-moi vendredi de relancer le client"),
            IntentKind::Temporal("vendredi".into())
        );
        assert_eq!(
            classify_intent("quand on reparle du déploiement de X, rappelle-moi le changelog"),
            IntentKind::Eventual
        );
        assert_eq!(classify_intent("pense au changelog"), IntentKind::Eventual);
    }

    #[test]
    fn triggers_are_extracted_without_stop_words() {
        let t = extract_triggers(
            "quand on reparle du déploiement de penelope, rappelle-moi le changelog",
        );
        assert!(t.contains(&"déploiement".to_string()));
        assert!(t.contains(&"penelope".to_string()));
        assert!(t.contains(&"changelog".to_string()));
        assert!(!t.contains(&"quand".to_string()));
    }

    /// CA 6 : une intention événementielle tire une fois quand le sujet revient,
    /// respecte le cooldown, et expire.
    #[tokio::test]
    async fn ca_6_10_intent_fires_respects_cooldown_and_expires() {
        let clock = TestClock::default();
        let s = intents(clock.clone());
        let i = s
            .create(
                "quand on reparle du déploiement, rappelle le changelog",
                vec!["déploiement".into()],
                None,
                24 * 3_600_000,
                3,
                Some(90 * 86_400_000),
            )
            .await
            .unwrap();

        // Le sujet revient : l'intention correspond.
        let m = s
            .matching("on reprend le déploiement demain", None, 0.72, 3)
            .await
            .unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, i.id);

        let fired = s.fire(&i.id).await.unwrap();
        assert_eq!(fired.tirs, 1);
        assert_eq!(fired.etat, IntentState::Tiree);

        // Cooldown : rien ne tire dans les 24 h.
        clock.advance_hours(1);
        assert!(
            s.matching("le déploiement avance", None, 0.72, 3)
                .await
                .unwrap()
                .is_empty()
        );

        // Après le cooldown, elle peut retirer.
        clock.advance_hours(24);
        assert_eq!(
            s.matching("le déploiement avance", None, 0.72, 3)
                .await
                .unwrap()
                .len(),
            1
        );

        // Budget épuisé : terminée.
        s.fire(&i.id).await.unwrap();
        clock.advance_hours(25);
        let last = s.fire(&i.id).await.unwrap();
        assert_eq!(last.etat, IntentState::Terminee);
        assert!(
            s.matching("le déploiement avance", None, 0.72, 3)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn expiry_marks_intents_expired() {
        let clock = TestClock::default();
        let s = intents(clock.clone());
        s.create("x", vec!["sujet".into()], None, 0, 3, Some(86_400_000))
            .await
            .unwrap();
        assert_eq!(s.expire_due().await.unwrap(), 0);
        clock.advance_days(2);
        assert_eq!(s.expire_due().await.unwrap(), 1);
        assert!(s.active().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn at_most_three_intents_per_turn() {
        let s = intents(TestClock::default());
        for i in 0..6 {
            s.create(
                &format!("intention {i}"),
                vec!["déploiement".into()],
                None,
                0,
                3,
                None,
            )
            .await
            .unwrap();
        }
        let m = s.matching("le déploiement", None, 0.72, 3).await.unwrap();
        assert_eq!(m.len(), 3);
    }

    #[tokio::test]
    async fn vector_prefilter_catches_paraphrases() {
        let s = intents(TestClock::default());
        let i = s
            .create(
                "quand on reparle de la mise en production",
                vec!["mise-en-production".into()],
                None,
                0,
                3,
                None,
            )
            .await
            .unwrap();
        s.put_embedding(&i.id, &[1.0, 0.0, 0.0]).await.unwrap();

        // Aucun déclencheur lexical ne correspond, mais le vecteur oui.
        assert!(
            s.matching("on déploie quand ?", None, 0.72, 3)
                .await
                .unwrap()
                .is_empty()
        );
        let m = s
            .matching("on déploie quand ?", Some(&[0.98, 0.1, 0.0]), 0.72, 3)
            .await
            .unwrap();
        assert_eq!(m.len(), 1);
    }

    #[tokio::test]
    async fn cancel_disarms() {
        let s = intents(TestClock::default());
        let i = s
            .create("x", vec!["sujet".into()], None, 0, 3, None)
            .await
            .unwrap();
        assert!(s.cancel(&i.id).await.unwrap());
        assert!(
            !s.cancel(&i.id).await.unwrap(),
            "annuler deux fois ne fait rien"
        );
        assert!(s.active().await.unwrap().is_empty());
        assert_eq!(
            s.all().await.unwrap().len(),
            1,
            "l'historique reste visible"
        );
    }

    #[test]
    fn eligibility_rules() {
        let base = Intent {
            id: "i".into(),
            texte: "x".into(),
            declencheurs: vec!["a".into()],
            portee: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            expire_at: None,
            budget_tirs: 3,
            tirs: 0,
            cooldown_ms: 1000,
            last_fired: None,
            etat: IntentState::Armee,
        };
        assert!(base.is_eligible(0));

        let epuisee = Intent {
            tirs: 3,
            ..base.clone()
        };
        assert!(!epuisee.is_eligible(0));

        let annulee = Intent {
            etat: IntentState::Annulee,
            ..base.clone()
        };
        assert!(!annulee.is_eligible(0));

        let recente = Intent {
            last_fired: Some("2026-01-01T00:00:00Z".into()),
            ..base.clone()
        };
        assert!(!recente.is_eligible(rfc3339_ms("2026-01-01T00:00:00Z") + 500));
        assert!(recente.is_eligible(rfc3339_ms("2026-01-01T00:00:00Z") + 1500));
    }

    #[test]
    fn lexical_score_is_a_proportion() {
        let i = Intent {
            id: "i".into(),
            texte: "x".into(),
            declencheurs: vec!["déploiement".into(), "changelog".into()],
            portee: None,
            created_at: String::new(),
            expire_at: None,
            budget_tirs: 3,
            tirs: 0,
            cooldown_ms: 0,
            last_fired: None,
            etat: IntentState::Armee,
        };
        assert_eq!(i.lexical_score("on parle du déploiement"), 0.5);
        assert_eq!(i.lexical_score("déploiement et changelog"), 1.0);
        assert_eq!(i.lexical_score("rien à voir"), 0.0);
    }

    /// Chaque état d'intention se relit depuis son nom ; un nom inconnu n'en est pas un.
    #[test]
    fn intent_states_round_trip() {
        for st in [
            IntentState::Armee,
            IntentState::Tiree,
            IntentState::Terminee,
            IntentState::Annulee,
            IntentState::Expiree,
        ] {
            assert_eq!(IntentState::parse(st.as_str()), Some(st));
        }
        assert_eq!(IntentState::parse("armée"), None);
    }
}
