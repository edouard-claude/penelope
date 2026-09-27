//! Banc d'essai de la mémoire (issue #37) : des conversations anonymisées, les souvenirs
//! attendus, et des questions dont la réponse n'est que dans la mémoire.
//!
//! ```text
//!  jeu (bench/memoire/*.json)
//!    ├─ vault initial ──────────► vault réel, indexé
//!    ├─ échanges ───────────────► candidats (relecture simulée, secrets rangés)
//!    ├─ réponse du modèle ──────► simulée (CI) ou vrai modèle (suite réseau)
//!    └─ attendu, questions ─────► mesures : précision, rappel, journal, faux souvenirs,
//!                                  périmés, doublons, fuites de secrets, réponses
//! ```
//!
//! En CI, le modèle de consolidation est simulé de façon déterministe : le banc vérifie
//! que le chemin complet (tri, mise à jour, journal, secrets, recherche) garde ce qu'il
//! doit garder. Avec le vrai modèle, il mesure la qualité du tri lui-même.

use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_memory::grid::{JOURNAL_SECTION, normalized};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

/// Les jeux livrés, dans l'ordre.
pub const FIXTURES: &[(&str, &str)] = &[
    (
        "01-projet-atlas",
        include_str!("../bench/memoire/01-projet-atlas.json"),
    ),
    (
        "02-style-de-reponse",
        include_str!("../bench/memoire/02-style-de-reponse.json"),
    ),
    (
        "03-migration-infra",
        include_str!("../bench/memoire/03-migration-infra.json"),
    ),
    (
        "04-deja-connu",
        include_str!("../bench/memoire/04-deja-connu.json"),
    ),
    (
        "05-acces-et-secrets",
        include_str!("../bench/memoire/05-acces-et-secrets.json"),
    ),
];

/// Date des jeux : les expirations qu'ils demandent s'y rapportent.
pub const BENCH_START_MS: i64 = 1_789_516_800_000;

#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub id: String,
    pub titre: String,
    #[serde(default)]
    pub vault: BTreeMap<String, String>,
    pub echanges: Vec<Exchange>,
    pub simule: Value,
    pub attendu: Expected,
    #[serde(default)]
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Exchange {
    pub session: String,
    #[serde(default)]
    pub proprietaire: String,
    pub candidats: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Expected {
    #[serde(default)]
    pub garde: Vec<String>,
    #[serde(default)]
    pub journal: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    /// uid d'entrées qui ne doivent plus être actives.
    #[serde(default)]
    pub perime: Vec<String>,
    /// Textes remplacés qui ne doivent plus figurer en mémoire.
    #[serde(default)]
    pub perime_textes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Question {
    pub question: String,
    pub reponse: String,
}

pub fn fixtures() -> Vec<Fixture> {
    FIXTURES
        .iter()
        .map(|(name, raw)| {
            serde_json::from_str(raw).unwrap_or_else(|e| panic!("jeu {name} illisible : {e}"))
        })
        .collect()
}

/// Mesures d'un jeu (ou de tous, additionnées).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Score {
    pub written: usize,
    pub written_expected: usize,
    pub kept_expected: usize,
    pub kept_found: usize,
    pub journal_expected: usize,
    pub journal_found: usize,
    pub false_memories: usize,
    pub stale: usize,
    pub duplicates: usize,
    pub secret_leaks: usize,
    pub secrets_missing: usize,
    pub questions: usize,
    pub answered: usize,
    /// Détail lisible des écarts.
    pub notes: Vec<String>,
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        1.0
    } else {
        num as f64 / den as f64
    }
}

impl Score {
    pub fn precision(&self) -> f64 {
        ratio(self.written_expected, self.written)
    }
    pub fn recall(&self) -> f64 {
        ratio(self.kept_found, self.kept_expected)
    }
    pub fn journal(&self) -> f64 {
        ratio(self.journal_found, self.journal_expected)
    }
    pub fn accuracy(&self) -> f64 {
        ratio(self.answered, self.questions)
    }

    pub fn add(&mut self, o: &Score) {
        self.written += o.written;
        self.written_expected += o.written_expected;
        self.kept_expected += o.kept_expected;
        self.kept_found += o.kept_found;
        self.journal_expected += o.journal_expected;
        self.journal_found += o.journal_found;
        self.false_memories += o.false_memories;
        self.stale += o.stale;
        self.duplicates += o.duplicates;
        self.secret_leaks += o.secret_leaks;
        self.secrets_missing += o.secrets_missing;
        self.questions += o.questions;
        self.answered += o.answered;
    }
}

/// Seuils de non-régression.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub precision: f64,
    pub recall: f64,
    pub journal: f64,
    pub accuracy: f64,
    pub false_memories: usize,
    pub stale: usize,
    pub duplicates: usize,
}

/// Modèle simulé : le chemin doit être exact.
pub const SIMULATED: Thresholds = Thresholds {
    precision: 1.0,
    recall: 1.0,
    journal: 1.0,
    accuracy: 1.0,
    false_memories: 0,
    stale: 0,
    duplicates: 0,
};

/// Vrai modèle : la qualité du tri, avec une marge.
pub const LIVE: Thresholds = Thresholds {
    precision: 0.8,
    recall: 0.8,
    journal: 0.5,
    accuracy: 0.7,
    false_memories: 1,
    stale: 1,
    duplicates: 1,
};

impl Thresholds {
    /// Écarts aux seuils ; les fuites de secrets ne sont jamais tolérées.
    pub fn failures(&self, s: &Score) -> Vec<String> {
        let mut out = Vec::new();
        let mut min = |name: &str, got: f64, want: f64| {
            if got + 1e-9 < want {
                out.push(format!("{name} {got:.2} < {want:.2}"));
            }
        };
        min("précision", s.precision(), self.precision);
        min("rappel", s.recall(), self.recall);
        min("journal", s.journal(), self.journal);
        min("réponses", s.accuracy(), self.accuracy);
        for (name, got, max) in [
            ("faux souvenirs", s.false_memories, self.false_memories),
            ("souvenirs périmés", s.stale, self.stale),
            ("doublons", s.duplicates, self.duplicates),
            ("fuites de secrets", s.secret_leaks, 0),
            ("secrets non rangés", s.secrets_missing, 0),
        ] {
            if got > max {
                out.push(format!("{name} {got} > {max}"));
            }
        }
        out
    }
}

/// Prépare le vault et enregistre les candidats des échanges.
pub async fn seed(d: &Arc<Daemon>, f: &Fixture) {
    let s = &d.services;
    let vault = penelope_conversation::vault_dir(s);
    std::fs::create_dir_all(&vault).expect("vault");
    for (rel, content) in &f.vault {
        let path = vault.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("répertoire du vault");
        }
        std::fs::write(&path, content).expect("fichier du vault");
    }
    penelope_vault::vault_ops::reindex(s, &vault)
        .await
        .expect("indexation");
    for (i, e) in f.echanges.iter().enumerate() {
        let raw = json!({"candidats": e.candidats}).to_string();
        penelope_vault::review::record_candidates(
            s,
            &raw,
            &e.session,
            &format!("bench:{}:{i}", f.id),
            false,
            10,
        )
        .await
        .expect("candidats");
    }
}

/// Réponse du modèle simulé : les candidats désignés par un extrait de leur texte
/// deviennent des numéros, `{{candidat}}` reprend le texte soumis (secrets compris, en
/// référence).
pub async fn simulated_reply(s: &Services, f: &Fixture) -> String {
    let order = penelope_dream::dream::submission_order(s)
        .await
        .expect("ordre des candidats");
    let resolve = |needle: &str| {
        order
            .iter()
            .position(|t| t.contains(needle))
            .map(|i| (i + 1, order[i].clone()))
    };
    let mut tri = Vec::new();
    for v in f.simule["tri"].as_array().cloned().unwrap_or_default() {
        if let Some((n, _)) = v["candidat"].as_str().and_then(resolve) {
            let mut v = v.clone();
            v["candidat"] = json!(n);
            tri.push(v);
        }
    }
    let mut operations = Vec::new();
    for op in f.simule["operations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if let Some((n, text)) = op["candidat"].as_str().and_then(resolve) {
            let mut op = op.clone();
            op["candidat"] = json!(n);
            if op["text"] == "{{candidat}}" {
                op["text"] = json!(text);
            }
            operations.push(op);
        }
    }
    json!({"tri": tri, "operations": operations}).to_string()
}

/// Entrée de mémoire lue dans le vault.
struct Line {
    file: String,
    section: String,
    text: String,
    uid: String,
    expire: Option<String>,
}

fn memory_lines(vault: &Path) -> Vec<Line> {
    let mut out = Vec::new();
    for file in ["profil.md", "memoire.md", "projets.md", "notes.md"] {
        let Ok(raw) = std::fs::read_to_string(vault.join(file)) else {
            continue;
        };
        let (entries, _) = penelope_memory::vault::parse_entries(&raw);
        for e in entries {
            out.push(Line {
                file: file.to_string(),
                section: e.section.clone(),
                text: e.text.clone(),
                uid: e.uid.clone(),
                expire: e.annotations.expire.clone(),
            });
        }
    }
    out
}

fn all_markdown(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            all_markdown(&p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

fn has(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Mesure un jeu après la passe : `before` donne les entrées du vault initial.
pub async fn score(d: &Arc<Daemon>, f: &Fixture, before: &BTreeMap<String, String>) -> Score {
    let s = &d.services;
    let vault = penelope_conversation::vault_dir(s);
    let lines = memory_lines(&vault);
    let (journal, durable): (Vec<&Line>, Vec<&Line>) = lines
        .iter()
        .partition(|l| l.file == "projets.md" && l.section == JOURNAL_SECTION);
    let mut sc = Score::default();

    // Écrit cette nuit : entrée nouvelle ou texte changé.
    for l in &durable {
        if before.get(&l.uid) == Some(&l.text) {
            continue;
        }
        sc.written += 1;
        if f.attendu.garde.iter().any(|g| has(&l.text, g)) {
            sc.written_expected += 1;
        } else {
            sc.notes.push(format!(
                "écrit sans être attendu : « {} » ({})",
                l.text, l.file
            ));
        }
    }
    for g in &f.attendu.garde {
        sc.kept_expected += 1;
        if durable.iter().any(|l| has(&l.text, g)) {
            sc.kept_found += 1;
        } else {
            sc.notes
                .push(format!("attendu en mémoire, absent : « {g} »"));
        }
    }
    for j in &f.attendu.journal {
        sc.journal_expected += 1;
        if journal
            .iter()
            .any(|l| has(&l.text, j) && l.expire.is_some())
        {
            sc.journal_found += 1;
        } else {
            sc.notes
                .push(format!("attendu au journal, absent : « {j} »"));
        }
    }
    for i in &f.attendu.ignore {
        if lines.iter().any(|l| has(&l.text, i)) {
            sc.false_memories += 1;
            sc.notes.push(format!("gardé à tort : « {i} »"));
        }
    }
    for uid in &f.attendu.perime {
        if lines.iter().any(|l| &l.uid == uid) {
            sc.stale += 1;
            sc.notes
                .push(format!("entrée périmée encore active : {uid}"));
        }
    }
    for t in &f.attendu.perime_textes {
        if durable.iter().any(|l| has(&l.text, t)) {
            sc.stale += 1;
            sc.notes
                .push(format!("texte périmé encore en mémoire : « {t} »"));
        }
    }
    let mut seen = BTreeSet::new();
    for l in &lines {
        if !seen.insert(normalized(&l.text)) {
            sc.duplicates += 1;
            sc.notes.push(format!("doublon : « {} »", l.text));
        }
    }

    // Secrets : aucune valeur dans le vault, chacune dans le magasin.
    let mut files = Vec::new();
    all_markdown(&vault, &mut files);
    let stored: Vec<String> = s
        .platform
        .secrets
        .list()
        .unwrap_or_default()
        .iter()
        .filter_map(|n| s.platform.secrets.get(n).ok().flatten())
        .collect();
    for secret in &f.attendu.secrets {
        for file in &files {
            if std::fs::read_to_string(file).is_ok_and(|raw| raw.contains(secret.as_str())) {
                sc.secret_leaks += 1;
                sc.notes
                    .push(format!("secret en clair dans {}", file.display()));
            }
        }
        if !stored.iter().any(|v| v == secret) {
            sc.secrets_missing += 1;
            sc.notes.push("secret absent du magasin".to_string());
        }
    }

    // Questions : la réponse doit figurer dans les cinq premiers résultats.
    let filter = penelope_memory::SearchFilter {
        limit: 5,
        ..Default::default()
    };
    for q in &f.questions {
        sc.questions += 1;
        let hits = s
            .memory
            .search(&q.question, None, &filter, &[])
            .await
            .unwrap_or_default();
        if hits.iter().any(|h| has(&h.entry.text, &q.reponse)) {
            sc.answered += 1;
        } else {
            sc.notes.push(format!(
                "question sans réponse : « {} » (attendu « {} »)",
                q.question, q.reponse
            ));
        }
    }
    sc
}

/// Entrées du vault initial d'un jeu : uid et texte.
pub fn initial_entries(f: &Fixture) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for raw in f.vault.values() {
        let (entries, _) = penelope_memory::vault::parse_entries(raw);
        for e in entries {
            out.insert(e.uid, e.text);
        }
    }
    out
}

/// Rapport Markdown du banc.
pub fn render(mode: &str, results: &[(Fixture, Score)], thresholds: &Thresholds) -> String {
    let mut total = Score::default();
    for (_, s) in results {
        total.add(s);
    }
    let pct = |x: f64| format!("{:.0} %", x * 100.0);
    let mut out = format!(
        "# Banc d'essai de la mémoire\n\nModèle de consolidation : {mode}. {} jeu(x).\n\n\
         | Jeu | Précision | Rappel | Journal | Faux souvenirs | Périmés | Doublons | Fuites | Réponses |\n\
         |---|---|---|---|---|---|---|---|---|\n",
        results.len()
    );
    let row = |name: &str, s: &Score| {
        format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {}/{} |\n",
            pct(s.precision()),
            pct(s.recall()),
            pct(s.journal()),
            s.false_memories,
            s.stale,
            s.duplicates,
            s.secret_leaks + s.secrets_missing,
            s.answered,
            s.questions
        )
    };
    for (f, s) in results {
        out.push_str(&row(&f.id, s));
    }
    out.push_str(&row("**Total**", &total));
    let failures = thresholds.failures(&total);
    out.push_str(&format!(
        "\nSeuils : précision {}, rappel {}, journal {}, réponses {}, faux souvenirs ≤ {}, \
         périmés ≤ {}, doublons ≤ {}, aucune fuite.\n\n**{}**\n",
        pct(thresholds.precision),
        pct(thresholds.recall),
        pct(thresholds.journal),
        pct(thresholds.accuracy),
        thresholds.false_memories,
        thresholds.stale,
        thresholds.duplicates,
        if failures.is_empty() {
            "Seuils tenus.".to_string()
        } else {
            format!("Seuils manqués : {}", failures.join(" ; "))
        }
    ));
    for (f, s) in results {
        if !s.notes.is_empty() {
            out.push_str(&format!("\n## {} : {}\n\n", f.id, f.titre));
            for n in &s.notes {
                out.push_str(&format!("- {n}\n"));
            }
        }
    }
    out
}

/// Écrit le rapport là où `PENELOPE_BENCH_REPORT` le demande.
pub fn write_report(report: &str) {
    if let Ok(path) = std::env::var("PENELOPE_BENCH_REPORT")
        && !path.trim().is_empty()
    {
        std::fs::write(&path, report).expect("rapport du banc");
    }
}
