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

/// Mots distinctifs d'un texte, pour juger l'usage d'un souvenir : minuscules, sans
/// accents, quatre lettres au moins, hors mots outils, ramenés à leurs cinq premières
/// lettres (« habite » et « habiter » se rejoignent).
fn distinctive(text: &str) -> std::collections::BTreeSet<String> {
    const TOOL_WORDS: &[&str] = &[
        "avec", "dans", "pour", "sans", "sous", "chez", "vers", "mais", "donc", "comme", "plus",
        "moins", "tres", "tout", "tous", "toute", "toutes", "cette", "ces", "leur", "leurs",
        "nous", "vous", "elle", "elles", "sont", "etre", "avoir", "fait", "faire", "peut", "doit",
        "quand", "alors", "aussi", "encore", "deja", "bien", "ceci", "cela", "celui", "celle",
        "entre", "depuis", "avant", "apres", "toujours", "jamais", "rien", "quelque", "chose",
        "notre", "votre", "mon", "ton", "son",
    ];
    let folded: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    folded
        .split_whitespace()
        .filter(|w| w.chars().count() >= 4 && !TOOL_WORDS.contains(w))
        .map(|w| w.chars().take(5).collect())
        .collect()
}

/// Un souvenir servi a-t-il servi (issue #105) ? Vrai quand la réponse reprend des mots
/// distinctifs du souvenir que la question ne contenait pas : un seul suffit pour un
/// souvenir court (quatre mots distinctifs au plus), deux au-delà. « Martin habite à
/// Lyon », servi pour « où habite Martin ? », a servi si la réponse dit « Lyon ».
///
/// Un indice, pas une preuve : il départage des souvenirs par leur usage, borné par
/// [`crate::index::usage_factor`], et ne décide jamais d'un retrait.
pub fn used_in_answer(entry: &str, asked: &str, answer: &str) -> bool {
    let asked = distinctive(asked);
    let own: Vec<String> = distinctive(entry)
        .into_iter()
        .filter(|w| !asked.contains(w))
        .collect();
    if own.is_empty() {
        return false;
    }
    let said = distinctive(answer);
    let hits = own.iter().filter(|w| said.contains(*w)).count();
    hits >= if own.len() <= 4 { 1 } else { 2 }
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
mod tests;
