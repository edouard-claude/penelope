//! Rappel sur le chemin de réponse (§6.7).
//!
//! **La mémoire ne bloque jamais une réponse** : la voie 1 est déterministe, bornée à
//! 150 ms, et dégrade en FTS seul si le backend d'embeddings est indisponible.

use crate::index::{MemoryIndex, Scored, SearchFilter};
use crate::vault::{Level, Practice, PracticeRecall};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Contexte courant, calculé de façon **déterministe** à chaque tour (§6.7).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CurrentContext {
    pub projet: Option<String>,
    pub depot: Option<String>,
    pub client: Option<String>,
    pub tache: Option<String>,
    pub langage: Option<String>,
    pub canal: Option<String>,
    pub criticite: Option<String>,
    pub codeur: Option<String>,
    pub outil: Option<String>,
    pub serveur_mcp: Option<String>,
    /// Jusqu'à 4 projets actifs par session, éviction LRU.
    pub active_projects: Vec<String>,
    /// uid déjà servis d'office dans l'instantané (T2) : la voie 1 ne les répète pas
    /// dans T4 (issue #62).
    pub injected_uids: Vec<String>,
}

impl CurrentContext {
    /// Vue « prédicats » consommée par `When::evaluate`.
    pub fn predicates(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        let mut put = |k: &str, v: &Option<String>| {
            if let Some(v) = v
                && !v.is_empty()
            {
                m.insert(k.to_string(), v.to_lowercase());
            }
        };
        put("projet", &self.projet);
        put("depot", &self.depot);
        put("client", &self.client);
        put("tache", &self.tache);
        put("langage", &self.langage);
        put("canal", &self.canal);
        put("criticite", &self.criticite);
        put("codeur", &self.codeur);
        put("outil", &self.outil);
        put("serveur_mcp", &self.serveur_mcp);
        m
    }

    /// Termes utilisés pour la recherche de déclencheurs.
    pub fn terms(&self) -> Vec<String> {
        self.predicates().into_values().collect()
    }

    /// Ajoute un projet actif, avec éviction LRU bornée à 4 (§6.7).
    pub fn touch_project(&mut self, key: &str) {
        self.active_projects.retain(|p| p != key);
        self.active_projects.insert(0, key.to_string());
        self.active_projects.truncate(4);
        self.projet = Some(key.to_string());
    }
}

/// Clé de projet : remote `origin` normalisé, sinon chemin racine (§6.7).
pub fn project_key(remote: Option<&str>, root_path: &str) -> String {
    match remote.filter(|r| !r.is_empty()) {
        Some(r) => normalise_remote(r),
        None => root_path.trim_end_matches('/').to_lowercase(),
    }
}

/// `git@github.com:org/repo.git` et `https://github.com/org/repo` donnent la même clé.
pub fn normalise_remote(remote: &str) -> String {
    let r = remote.trim().trim_end_matches(".git");
    let r = r
        .strip_prefix("git@")
        .map(|s| s.replacen(':', "/", 1))
        .unwrap_or_else(|| {
            r.strip_prefix("https://")
                .or_else(|| r.strip_prefix("http://"))
                .or_else(|| r.strip_prefix("ssh://git@"))
                .unwrap_or(r)
                .to_string()
        });
    r.trim_start_matches("www.").to_lowercase()
}

/// Classifieur déterministe du type de tâche, sans appel au modèle.
pub fn classify_task(message: &str) -> Option<String> {
    let m = message.to_lowercase();
    const RULES: &[(&str, &[&str])] = &[
        (
            "deploiement",
            &["déploie", "deploie", "mise en prod", "rollback", "release"],
        ),
        (
            "revue",
            &["relis", "revue", "review", "pull request", "merge request"],
        ),
        (
            "code",
            &[
                "corrige",
                "implémente",
                "implemente",
                "bug",
                "refactor",
                "écris le code",
                "ajoute une fonction",
            ],
        ),
        (
            "support",
            &["ne marche pas", "erreur", "panne", "incident", "dépanne"],
        ),
        (
            "redaction",
            &[
                "rédige",
                "redige",
                "écris un texte",
                "note de",
                "documentation",
            ],
        ),
        (
            "analyse",
            &["analyse", "compare", "évalue", "chiffre", "estime"],
        ),
    ];
    for (task, needles) in RULES {
        if needles.iter().any(|n| m.contains(n)) {
            return Some(task.to_string());
        }
    }
    None
}

/// Intention de rappel détectée dans un message (§6.7 voie 2).
pub fn shows_recall_intent(message: &str) -> bool {
    let m = message.to_lowercase();
    const NEEDLES: &[&str] = &[
        "on avait dit",
        "tu te souviens",
        "rappelle-moi",
        "la dernière fois",
        "l'autre jour",
        "comme d'habitude",
        "qu'est-ce qu'on avait",
        "on avait décidé",
        "j'avais dit",
        "précédemment",
        "la fois d'avant",
        "hier",
        "la semaine dernière",
        "le mois dernier",
        "pourquoi on",
        "qui avait",
    ];
    NEEDLES.iter().any(|n| m.contains(n))
}

/// Ce que la voie 1 injecte en T4.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecallResult {
    /// Entrées curées déclenchées (au plus `max_injected`).
    pub triggered: Vec<Scored>,
    /// Pratiques applicables, rendues au format du §6.7.
    pub practices: Vec<PracticeRecall>,
    /// Vrai si la recherche vectorielle a été sautée (backend indisponible ou budget).
    pub degraded: bool,
    /// Pertinence maximale obtenue : sert à décider l'escalade en voie 2.
    pub max_score: f64,
    pub tokens_used: u64,
    /// Entrées apparues dans les résultats sans être retenues (retour d'usage, #86).
    pub seen: Vec<String>,
}

impl RecallResult {
    /// Bloc T4 prêt à injecter.
    pub fn render(&self) -> String {
        let mut parts = Vec::new();
        if !self.triggered.is_empty() {
            let mut s = String::from("Rappel mémoire :");
            for t in &self.triggered {
                s.push_str(&format!("\n- {}", t.entry.text));
            }
            parts.push(s);
        }
        for p in &self.practices {
            parts.push(p.render());
        }
        parts.join("\n\n")
    }

    pub fn is_empty(&self) -> bool {
        self.triggered.is_empty() && self.practices.is_empty()
    }

    /// L'escalade en voie 2 n'a lieu que si le message montre une intention de rappel
    /// **et** que la voie 1 n'a rien donné de fort (§6.7).
    pub fn should_escalate(&self, message: &str, threshold: f64) -> bool {
        shows_recall_intent(message) && self.max_score < threshold
    }
}

/// Paramètres de rappel, issus de la configuration.
#[derive(Debug, Clone, Copy)]
pub struct RecallParams {
    pub trigger_threshold: f64,
    pub max_injected: usize,
    pub budget_tokens: u64,
    pub timeout_ms: u64,
    pub verify_threshold: f64,
}

impl Default for RecallParams {
    fn default() -> Self {
        RecallParams {
            trigger_threshold: 0.72,
            max_injected: 3,
            budget_tokens: 1000,
            timeout_ms: 150,
            verify_threshold: 0.8,
        }
    }
}

impl RecallParams {
    pub fn from_config(m: &penelope_kernel::config::Memory) -> Self {
        RecallParams {
            trigger_threshold: m.trigger_threshold,
            max_injected: m.max_injected_per_turn,
            budget_tokens: m.recall_budget_tokens as u64,
            timeout_ms: m.recall_timeout_ms,
            verify_threshold: 0.8,
        }
    }
}

/// Voie 1 : déterministe, sans appel au modèle (§6.7).
pub struct Recall<'a> {
    pub index: &'a MemoryIndex,
    pub params: RecallParams,
}

impl<'a> Recall<'a> {
    pub fn new(index: &'a MemoryIndex, params: RecallParams) -> Self {
        Recall { index, params }
    }

    /// Exécute la voie 1 sous contrainte de temps.
    ///
    /// Le dépassement du budget n'est **jamais** une erreur : on renvoie ce qu'on a.
    pub async fn path1(
        &self,
        message: &str,
        ctx: &CurrentContext,
        query_vector: Option<Vec<f32>>,
        practices: &[Practice],
    ) -> RecallResult {
        let degraded = query_vector.is_none();
        let filter = SearchFilter {
            limit: 20,
            automatic: true,
            ..Default::default()
        };

        let search = self
            .index
            .search(message, query_vector, &filter, &ctx.active_projects);
        let timeout = std::time::Duration::from_millis(self.params.timeout_ms.max(1));

        let hits: Vec<Scored> = match tokio::time::timeout(timeout, search).await {
            Ok(Ok(h)) => h,
            // Ni une erreur d'index ni un dépassement de délai ne retardent la réponse.
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "rappel : index indisponible, tour sans mémoire");
                Vec::new()
            }
            Err(_) => {
                tracing::warn!("rappel : budget de 150 ms dépassé, tour sans mémoire");
                Vec::new()
            }
        };

        let max_score = hits.iter().map(|h| h.relevance).fold(0.0, f64::max);

        // 2. Déclencheurs : seuil de **pertinence**, puis plafond, puis budget de tokens.
        //    Récence, importance, projet et confiance ordonnent les résultats (`score`),
        //    ils ne décident pas seuls qu'un souvenir ne sera plus jamais servi (#86).
        let mut triggered = Vec::new();
        let mut tokens_used = 0u64;
        for h in hits.iter() {
            if h.relevance < self.params.trigger_threshold {
                continue;
            }
            // 5. Le contenu non fiable et l'épisodique ne sont jamais injectés.
            if h.entry.level == Level::Episodic {
                continue;
            }
            // Déjà dans l'instantané : le répéter coûterait deux fois (issue #62).
            if ctx.injected_uids.contains(&h.entry.uid) {
                continue;
            }
            let cost = (h.entry.text.chars().count() as u64 / 4).max(1);
            if tokens_used + cost > self.params.budget_tokens {
                break;
            }
            tokens_used += cost;
            triggered.push(h.clone());
            if triggered.len() >= self.params.max_injected {
                break;
            }
        }

        // 3. Pratiques : défaut + exceptions satisfaites seulement.
        let predicates = ctx.predicates();
        let terms = ctx.terms();
        let msg_lower = message.to_lowercase();
        let mut recalls = Vec::new();
        for p in practices {
            if p.statut == crate::vault::PracticeStatus::Retiree {
                continue;
            }
            let triggered_by_message = p
                .declencheurs
                .iter()
                .any(|d| msg_lower.contains(&d.to_lowercase()));
            let triggered_by_context = p.declencheurs.iter().any(|d| {
                terms
                    .iter()
                    .any(|t| t.contains(&d.to_lowercase()) || d.to_lowercase().contains(t))
            });
            if !triggered_by_message && !triggered_by_context {
                continue;
            }
            let r = p.recall(&predicates, self.params.verify_threshold);
            tokens_used += (r.render().chars().count() as u64 / 4).max(1);
            recalls.push(r);
        }

        // Vues sans être retenues : ni servies, ni déjà dans l'instantané.
        let seen = hits
            .iter()
            .filter(|h| !ctx.injected_uids.contains(&h.entry.uid))
            .filter(|h| {
                !triggered
                    .iter()
                    .any(|t: &Scored| t.entry.uid == h.entry.uid)
            })
            .map(|h| h.entry.uid.clone())
            .collect();

        RecallResult {
            triggered,
            practices: recalls,
            degraded,
            max_score,
            tokens_used,
            seen,
        }
    }
}

/// Bloc d'instantanés T2 (profil, cœur, projet), figé par épisode (§6.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshots {
    pub profile: String,
    pub core: String,
    pub project: String,
    /// Empreinte : deux instantanés identiques donnent le même hash, donc le même cache.
    pub hash: String,
}

impl Snapshots {
    pub fn compute(profile: &str, core: &str, project: &str) -> Snapshots {
        let hash = penelope_kernel::canonical::sha256_hex(
            format!("{profile}\u{0}{core}\u{0}{project}").as_bytes(),
        );
        Snapshots {
            profile: profile.to_string(),
            core: core.to_string(),
            project: project.to_string(),
            hash,
        }
    }

    /// Construit un bloc sous budget de tokens, entrées les plus importantes d'abord.
    /// Coût estimé d'un niveau et nombre d'entrées laissées hors du bloc injecté.
    pub fn budget_use(entries: &[crate::index::IndexedEntry], budget_tokens: u64) -> (u64, usize) {
        let block = Self::build_block(entries, budget_tokens);
        let total: u64 = entries
            .iter()
            .map(|e| (e.text.chars().count() as u64 / 4).max(1))
            .sum();
        (total, entries.len() - block.lines().count())
    }

    pub fn build_block(entries: &[crate::index::IndexedEntry], budget_tokens: u64) -> String {
        Self::build_block_with_uids(entries, budget_tokens).0
    }

    /// Bloc injecté **et** les uid qu'il contient : ce qui est servi d'office n'a pas
    /// d'usage mesurable par entrée, et n'a pas à être rappelé une seconde fois (#62).
    pub fn build_block_with_uids(
        entries: &[crate::index::IndexedEntry],
        budget_tokens: u64,
    ) -> (String, Vec<String>) {
        let mut sorted: Vec<&crate::index::IndexedEntry> = entries.iter().collect();
        sorted.sort_by(|a, b| {
            b.importance
                .unwrap_or(5)
                .cmp(&a.importance.unwrap_or(5))
                .then_with(|| a.uid.cmp(&b.uid))
        });
        let mut out = String::new();
        let mut uids = Vec::new();
        let mut used = 0u64;
        for e in sorted {
            let cost = (e.text.chars().count() as u64 / 4).max(1);
            if used + cost > budget_tokens {
                continue;
            }
            used += cost;
            out.push_str(&format!("- {}\n", e.text));
            uids.push(e.uid.clone());
        }
        (out, uids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{IndexedEntry, simple_entry};
    use crate::provenance::Provenance;
    use penelope_kernel::clock::TestClock;
    use penelope_store::Store;
    use std::sync::Arc;

    fn index() -> MemoryIndex {
        MemoryIndex::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        )
    }

    fn prov() -> Provenance {
        Provenance::owner("s1", "interactive", "2026-09-16T10:00:00Z")
    }

    const PRACTICE: &str = "---\n\
        type: pratique\n\
        id: langage-backend\n\
        confiance: 0.8\n\
        preuves: 9\n\
        statut: active\n\
        maj: 2026-09-17\n\
        declencheurs: [langage, stack, backend]\n\
        ---\n\
        # Langage backend\n\n\
        ## Défaut\n- Go (stdlib, hexagonal). <!-- uid: 01J9A -->\n\n\
        ## Exceptions\n\
        - Rust si projet critique. <!-- uid: 01J9B --> \
          <!-- quand: tache=code; criticite=haute; codeur=agent --> <!-- confiance: 0.9 -->\n";

    #[test]
    fn project_keys_are_normalised() {
        assert_eq!(
            project_key(Some("git@github.com:org/repo.git"), "/x"),
            "github.com/org/repo"
        );
        assert_eq!(
            project_key(Some("https://github.com/org/repo"), "/x"),
            "github.com/org/repo"
        );
        assert_eq!(
            project_key(None, "/Users/moi/Code/projet/"),
            "/users/moi/code/projet"
        );
    }

    #[test]
    fn active_projects_are_lru_bounded_to_four() {
        let mut c = CurrentContext::default();
        for i in 0..6 {
            c.touch_project(&format!("p{i}"));
        }
        assert_eq!(c.active_projects.len(), 4);
        assert_eq!(c.active_projects[0], "p5");
        assert!(!c.active_projects.contains(&"p0".to_string()));

        c.touch_project("p2");
        assert_eq!(c.active_projects[0], "p2", "un projet réutilisé remonte");
        assert_eq!(c.active_projects.len(), 4);
    }

    #[test]
    fn task_classification() {
        assert_eq!(
            classify_task("déploie en prod stp").as_deref(),
            Some("deploiement")
        );
        assert_eq!(
            classify_task("relis ma pull request").as_deref(),
            Some("revue")
        );
        assert_eq!(
            classify_task("corrige le bug de TVA").as_deref(),
            Some("code")
        );
        assert_eq!(classify_task("bonjour").as_deref(), None);
    }

    #[test]
    fn recall_intent_detection() {
        assert!(shows_recall_intent("on avait dit quoi pour les branches ?"));
        assert!(shows_recall_intent("rappelle-moi la procédure"));
        assert!(shows_recall_intent("pourquoi on a choisi Go ?"));
        assert!(!shows_recall_intent("corrige ce bug"));
    }

    #[tokio::test]
    async fn path1_injects_triggered_entries_under_budget() {
        let i = index();
        for n in 0..10 {
            let mut e = simple_entry(
                &format!("u{n}"),
                &format!("Le déploiement du service {n} se fait par CapRover."),
                Level::Cure,
                "2026-09-16",
            );
            e.importance = Some(9);
            i.upsert(&e, &prov()).await.unwrap();
        }
        let r = Recall::new(&i, RecallParams::default())
            .path1(
                "comment se fait le déploiement ?",
                &CurrentContext::default(),
                None,
                &[],
            )
            .await;
        assert!(!r.triggered.is_empty());
        assert!(
            r.triggered.len() <= 3,
            "au plus 3 entrées injectées : {}",
            r.triggered.len()
        );
        assert!(r.tokens_used <= 1000);
        assert!(r.degraded, "sans vecteur, la voie 1 est en mode FTS seul");
    }

    /// Entrée de niveau Cure, importance 5 (défaut d'un candidat), datée de `maj`.
    fn aged(uid: &str, text: &str, maj: &str) -> IndexedEntry {
        let mut e = simple_entry(uid, text, Level::Cure, maj);
        e.file = "notes.md".into();
        e.importance = Some(5);
        e
    }

    /// #86 : l'horloge de test est au 1er janvier 2026. Seule réponse lexicale à la
    /// question, une entrée de 30, 90 ou 180 jours est toujours injectée : la récence
    /// ordonne, elle ne décide pas seule.
    #[tokio::test]
    async fn an_old_curated_entry_is_still_recalled() {
        for (maj, age) in [("2025-12-02", 30), ("2025-10-03", 90), ("2025-07-05", 180)] {
            let i = index();
            i.upsert(
                &aged("vieux", "Le code du portail de la résidence est 4812.", maj),
                &prov(),
            )
            .await
            .unwrap();
            let r = Recall::new(&i, RecallParams::default())
                .path1(
                    "quel est le code du portail ?",
                    &CurrentContext::default(),
                    None,
                    &[],
                )
                .await;
            assert_eq!(r.triggered.len(), 1, "âgée de {age} jours");
            assert!(r.seen.is_empty());
        }
    }

    /// #86 : à pertinence égale, la récente passe avant l'ancienne.
    #[tokio::test]
    async fn a_recent_entry_ranks_before_an_old_equivalent() {
        let i = index();
        i.upsert(
            &aged(
                "ancien",
                "Le portail de la résidence a pour code 4812.",
                "2025-07-05",
            ),
            &prov(),
        )
        .await
        .unwrap();
        i.upsert(
            &aged(
                "recent",
                "Le portail de la résidence a pour code 7731.",
                "2025-12-30",
            ),
            &prov(),
        )
        .await
        .unwrap();
        let r = Recall::new(&i, RecallParams::default())
            .path1(
                "le code du portail de la résidence",
                &CurrentContext::default(),
                None,
                &[],
            )
            .await;
        let uids: Vec<&str> = r.triggered.iter().map(|t| t.entry.uid.as_str()).collect();
        assert_eq!(uids, vec!["recent", "ancien"]);
    }

    /// #86 : `memory.half_life_days` change l'ordre entre une entrée ancienne importante
    /// et une récente ordinaire.
    #[tokio::test]
    async fn the_half_life_setting_changes_the_order() {
        let half = Arc::new(std::sync::Mutex::new(10.0));
        let i = {
            let half = half.clone();
            index().with_half_life(move || *half.lock().unwrap())
        };
        let mut ancien = aged(
            "ancien",
            "Le portail de la résidence a pour code 4812.",
            "2025-11-02",
        );
        ancien.importance = Some(10);
        let mut recent = aged(
            "recent",
            "Le portail de la résidence a pour code 7731.",
            "2025-12-31",
        );
        recent.importance = Some(1);
        i.upsert(&ancien, &prov()).await.unwrap();
        i.upsert(&recent, &prov()).await.unwrap();
        let first = |i: MemoryIndex| async move {
            i.search(
                "portail résidence code",
                None,
                &SearchFilter::explicit(),
                &[],
            )
            .await
            .unwrap()[0]
                .entry
                .uid
                .clone()
        };
        assert_eq!(
            first(i.clone()).await,
            "recent",
            "demi-vie courte : la récente"
        );
        *half.lock().unwrap() = 365.0;
        assert_eq!(
            first(i.clone()).await,
            "ancien",
            "demi-vie longue : l'importante"
        );
    }

    /// #86 : ce qui n'est jamais apparu dans un résultat n'est pas proposé au retrait ;
    /// ce qui y est apparu dix fois sans être retenu l'est.
    #[tokio::test]
    async fn only_entries_that_had_their_chance_are_proposed_for_retirement() {
        let i = index();
        i.upsert(
            &aged(
                "jamais_vu",
                "Le wifi du garage est Garage-5G.",
                "2025-06-01",
            ),
            &prov(),
        )
        .await
        .unwrap();
        i.upsert(
            &aged("vu", "La haie se taille en mars.", "2025-06-01"),
            &prov(),
        )
        .await
        .unwrap();
        for _ in 0..crate::grid::SEEN_BEFORE_RETIRE {
            i.record_seen(&["vu".to_string()]).await.unwrap();
        }
        let proposed: Vec<String> = i
            .unrecalled_since(
                &[Level::Cure],
                "2025-11-01",
                crate::grid::SEEN_BEFORE_RETIRE,
            )
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.uid)
            .collect();
        assert_eq!(proposed, vec!["vu"]);
        assert_eq!(i.signals_of("vu").await.unwrap().seen, 10);
    }

    /// #87 : mille passages de documents ingérés, plus proches de la question que le
    /// souvenir, ne le chassent plus du rappel automatique ; leurs vecteurs ne sont même
    /// pas décodés. Le même souvenir en épisodique n'est pas injecté, `mem_search`
    /// explicite le trouve.
    #[tokio::test]
    async fn ingested_passages_do_not_evict_memories_from_recall() {
        let i = index();
        for n in 0..1000 {
            let uid = format!("p{n:04}");
            let mut e = simple_entry(
                &uid,
                &format!("Le code du portail de la résidence, le portail, son code : section {n}."),
                Level::Cure,
                "2026-01-01",
            );
            e.etype = crate::ingest::SOURCE_ETYPE.into();
            e.file = format!("sources/reglement-{}.md", n / 100);
            i.upsert(&e, &prov()).await.unwrap();
            i.put_embedding(&uid, "m", &[1.0, 0.01 * (n % 10) as f32, 0.0])
                .await
                .unwrap();
        }
        let mut souvenir = simple_entry(
            "souvenir",
            "Pour entrer chez Paul, taper le code du portail : 4812.",
            Level::Cure,
            "2025-12-20",
        );
        souvenir.file = "notes.md".into();
        i.upsert(&souvenir, &prov()).await.unwrap();
        i.put_embedding("souvenir", "m", &[0.6, 0.8, 0.0])
            .await
            .unwrap();
        let mut journal = simple_entry(
            "journal",
            "Paul a redonné le code du portail de sa résidence.",
            Level::Episodic,
            "2025-12-21",
        );
        journal.file = "journal/2025-12-21.md".into();
        i.upsert(&journal, &prov()).await.unwrap();
        i.put_embedding("journal", "m", &[0.6, 0.8, 0.0])
            .await
            .unwrap();

        let before = i.vectors_decoded();
        let params = RecallParams {
            timeout_ms: 5_000,
            ..RecallParams::default()
        };
        let r = Recall::new(&i, params)
            .path1(
                "quel est le code du portail de la résidence ?",
                &CurrentContext::default(),
                Some(vec![1.0, 0.0, 0.0]),
                &[],
            )
            .await;
        let injected: Vec<&str> = r.triggered.iter().map(|t| t.entry.uid.as_str()).collect();
        assert_eq!(injected, vec!["souvenir"]);
        assert_eq!(
            i.vectors_decoded() - before,
            1,
            "ni passage ni épisodique décodé pour le rappel automatique"
        );

        let explicit = i
            .search("code portail Paul", None, &SearchFilter::explicit(), &[])
            .await
            .unwrap();
        assert!(explicit.iter().any(|h| h.entry.uid == "journal"));
    }

    #[tokio::test]
    async fn path1_never_injects_episodic_or_low_score() {
        let i = index();
        let mut e = simple_entry(
            "u1",
            "note du journal sur le déploiement",
            Level::Episodic,
            "2026-09-16",
        );
        e.file = "journal/2026-09-16.md".into();
        i.upsert(&e, &prov()).await.unwrap();

        let r = Recall::new(&i, RecallParams::default())
            .path1("déploiement", &CurrentContext::default(), None, &[])
            .await;
        assert!(r.triggered.is_empty());
    }

    /// CA 6 (rappel sans blocage) : backend d'embeddings indisponible ⇒ la voie 1
    /// fonctionne en FTS seul et ne retarde pas la réponse.
    #[tokio::test]
    async fn ca_6_12_recall_never_blocks() {
        let i = index();
        i.upsert(
            &simple_entry(
                "u1",
                "Le déploiement passe par CapRover.",
                Level::Cure,
                "2026-09-16",
            ),
            &prov(),
        )
        .await
        .unwrap();

        let started = std::time::Instant::now();
        let r = Recall::new(
            &i,
            RecallParams {
                timeout_ms: 150,
                ..Default::default()
            },
        )
        .path1("déploiement", &CurrentContext::default(), None, &[])
        .await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(600),
            "le rappel a pris {elapsed:?}"
        );
        assert!(r.degraded);
    }

    #[tokio::test]
    async fn practice_recall_is_context_dependent() {
        let i = index();
        let p = Practice::parse(PRACTICE, "langage-backend").unwrap();
        let recall = Recall::new(&i, RecallParams::default());

        let mut ctx = CurrentContext {
            tache: Some("code".into()),
            criticite: Some("haute".into()),
            codeur: Some("agent".into()),
            ..Default::default()
        };
        ctx.touch_project("penelope");

        let r = recall
            .path1(
                "quel langage pour le backend ?",
                &ctx,
                None,
                std::slice::from_ref(&p),
            )
            .await;
        assert_eq!(r.practices.len(), 1);
        let rendered = r.render();
        assert!(rendered.contains("Défaut : Go"));
        assert!(rendered.contains("S'applique ici : Rust"));

        // Tour de rédaction : le défaut seul.
        let ctx2 = CurrentContext {
            tache: Some("redaction".into()),
            criticite: Some("haute".into()),
            codeur: Some("agent".into()),
            ..Default::default()
        };
        let r2 = recall
            .path1(
                "quel langage pour le backend ?",
                &ctx2,
                None,
                std::slice::from_ref(&p),
            )
            .await;
        assert!(r2.render().contains("Défaut : Go"));
        assert!(!r2.render().contains("S'applique ici"));
    }

    #[tokio::test]
    async fn practice_not_triggered_is_not_injected() {
        let i = index();
        let p = Practice::parse(PRACTICE, "langage-backend").unwrap();
        let r = Recall::new(&i, RecallParams::default())
            .path1(
                "comment va la météo ?",
                &CurrentContext::default(),
                None,
                &[p],
            )
            .await;
        assert!(r.practices.is_empty());
    }

    #[tokio::test]
    async fn escalation_requires_intent_and_weak_path1() {
        let i = index();
        let weak = Recall::new(&i, RecallParams::default())
            .path1("on avait dit quoi ?", &CurrentContext::default(), None, &[])
            .await;
        assert!(weak.should_escalate("on avait dit quoi ?", 0.72));
        assert!(
            !weak.should_escalate("corrige ce bug", 0.72),
            "sans intention de rappel, pas d'escalade"
        );

        let strong = RecallResult {
            max_score: 0.95,
            ..Default::default()
        };
        assert!(
            !strong.should_escalate("on avait dit quoi ?", 0.72),
            "la voie 1 a répondu : pas d'escalade"
        );
    }

    #[test]
    fn snapshots_hash_is_stable() {
        let a = Snapshots::compute("p", "c", "j");
        let b = Snapshots::compute("p", "c", "j");
        assert_eq!(a.hash, b.hash);
        assert_ne!(a.hash, Snapshots::compute("p2", "c", "j").hash);
    }

    #[test]
    fn snapshot_block_respects_budget_and_importance() {
        let entries: Vec<IndexedEntry> = (0..20)
            .map(|i| {
                let mut e = simple_entry(
                    &format!("u{i:02}"),
                    &format!("entrée numéro {i:02} avec un texte de longueur raisonnable"),
                    Level::Profil,
                    "2026-09-16",
                );
                // Importance croissante : la dernière est la plus importante.
                e.importance = Some(i as u8);
                e
            })
            .collect();
        let block = Snapshots::build_block(&entries, 30);
        let tokens: u64 = block
            .lines()
            .map(|l| (l.chars().count() as u64 / 4).max(1))
            .sum();
        assert!(tokens <= 35, "budget dépassé : {tokens}");
        assert!(
            block.lines().next().unwrap().contains("numéro 19"),
            "la plus importante d'abord : {block}"
        );
        assert!(
            !block.contains("numéro 00"),
            "la moins importante est écartée par le budget"
        );
    }

    #[test]
    fn predicates_are_lowercased_and_sparse() {
        let c = CurrentContext {
            tache: Some("Code".into()),
            criticite: Some("Haute".into()),
            ..Default::default()
        };
        let p = c.predicates();
        assert_eq!(p["tache"], "code");
        assert_eq!(p["criticite"], "haute");
        assert!(
            !p.contains_key("client"),
            "les clés absentes ne sont pas posées"
        );
        let w = crate::vault::When::parse("tache=code").unwrap();
        assert_eq!(w.evaluate(&p), crate::vault::WhenMatch::Satisfied);
    }
}
