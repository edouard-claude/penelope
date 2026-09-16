//! Formats de fichiers du vault (§6.4) : annotations, règles défaisables, prédicats
//! `quand`.
//!
//! Le vault est la **source de vérité**. Tout y est du Markdown lisible et éditable en
//! SSH ; l'index SQLite en est dérivé et reconstructible.

use penelope_kernel::frontmatter::{self, FmValue};
use penelope_kernel::ids::Ulid;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

// ------------------------------------------------------------------ prédicats

/// Prédicat de contexte : conjonction de `clé=valeur`, valeurs multiples séparées par `|`.
///
/// Clés reconnues (§6.4) : `projet`, `client`, `depot`, `langage`, `tache`, `canal`,
/// `criticite`, `codeur`, `outil`, `serveur_mcp`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct When {
    pub clauses: BTreeMap<String, Vec<String>>,
}

pub const WHEN_KEYS: &[&str] = &[
    "projet",
    "client",
    "depot",
    "langage",
    "tache",
    "canal",
    "criticite",
    "codeur",
    "outil",
    "serveur_mcp",
];

pub const TASK_VALUES: &[&str] = &[
    "code",
    "revue",
    "deploiement",
    "redaction",
    "support",
    "analyse",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhenMatch {
    /// Tous les prédicats sont satisfaits par le contexte.
    Satisfied,
    /// Un prédicat est contredit par le contexte.
    Contradicted,
    /// Une clé est absente du contexte : **non satisfait**, mais peut être listé
    /// « À vérifier » (§6.7).
    Unknown(Vec<String>),
}

impl When {
    /// Analyse `tache=code; criticite=haute; codeur=agent`.
    pub fn parse(raw: &str) -> Result<When, String> {
        let mut clauses = BTreeMap::new();
        for part in raw.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let Some((k, v)) = part.split_once('=') else {
                return Err(format!("prédicat sans `=` : `{part}`"));
            };
            let key = k.trim().to_lowercase();
            if !WHEN_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "clé `{key}` inconnue (attendu : {})",
                    WHEN_KEYS.join(", ")
                ));
            }
            let values: Vec<String> = v
                .split('|')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if values.is_empty() {
                return Err(format!("valeur vide pour `{key}`"));
            }
            clauses.insert(key, values);
        }
        if clauses.is_empty() {
            return Err("prédicat vide".into());
        }
        Ok(When { clauses })
    }

    pub fn render(&self) -> String {
        self.clauses
            .iter()
            .map(|(k, v)| format!("{k}={}", v.join("|")))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Évalue contre le contexte courant.
    pub fn evaluate(&self, ctx: &BTreeMap<String, String>) -> WhenMatch {
        let mut unknown = Vec::new();
        for (k, allowed) in &self.clauses {
            match ctx.get(k) {
                None => unknown.push(k.clone()),
                Some(actual) => {
                    let a = actual.to_lowercase();
                    if !allowed.iter().any(|v| v == &a) {
                        return WhenMatch::Contradicted;
                    }
                }
            }
        }
        if unknown.is_empty() {
            WhenMatch::Satisfied
        } else {
            WhenMatch::Unknown(unknown)
        }
    }

    /// Deux signatures sont compatibles si leur intersection est non vide sur chaque clé
    /// commune (§6.8, promotion d'un écart en exception).
    pub fn compatible_with(&self, other: &When) -> bool {
        for (k, a) in &self.clauses {
            if let Some(b) = other.clauses.get(k) {
                if !a.iter().any(|x| b.contains(x)) {
                    return false;
                }
            }
        }
        true
    }

    /// Intersection de deux signatures compatibles.
    pub fn intersect(&self, other: &When) -> When {
        let mut clauses = self.clauses.clone();
        for (k, b) in &other.clauses {
            match clauses.get_mut(k) {
                Some(a) => {
                    let inter: Vec<String> = a.iter().filter(|x| b.contains(x)).cloned().collect();
                    if !inter.is_empty() {
                        *a = inter;
                    }
                }
                None => {
                    clauses.insert(k.clone(), b.clone());
                }
            }
        }
        When { clauses }
    }
}

// ------------------------------------------------------------------ annotations

/// Métadonnées d'une entrée, portées par des commentaires HTML de fin de ligne (§6.4).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Annotations {
    pub uid: Option<String>,
    pub importance: Option<u8>,
    pub declencheurs: Vec<String>,
    pub projet: Option<String>,
    pub depuis: Option<String>,
    pub source: Option<String>,
    pub quand: Option<When>,
    pub confiance: Option<f64>,
    pub preuves: Vec<String>,
    pub occurrences: Option<u32>,
    pub revue: Option<String>,
}

fn comment_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"<!--\s*([a-zA-Zé]+)\s*:\s*(.*?)\s*-->").expect("regex valide"))
}

impl Annotations {
    /// Extrait les annotations d'une ligne. Une ligne sans commentaire donne des valeurs
    /// neutres : le parseur tolère l'absence (§6.4).
    pub fn parse(line: &str) -> Annotations {
        let mut a = Annotations::default();
        for c in comment_re().captures_iter(line) {
            let key = c[1].to_lowercase();
            let val = c[2].trim().to_string();
            match key.as_str() {
                "uid" => a.uid = Some(val),
                "importance" => a.importance = val.parse().ok(),
                "declencheurs" => {
                    a.declencheurs = val
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                "projet" => a.projet = Some(val),
                "depuis" => a.depuis = Some(val),
                "source" => a.source = Some(val),
                "quand" => a.quand = When::parse(&val).ok(),
                "confiance" => a.confiance = val.parse().ok(),
                "preuves" => {
                    a.preuves = val
                        .split(',')
                        .map(|s| s.trim().trim_matches(|c| c == '[' || c == ']').to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                "occurrences" => a.occurrences = val.parse().ok(),
                "revue" => a.revue = Some(val),
                _ => {}
            }
        }
        a
    }

    /// Rend les annotations en commentaires, dans un ordre stable.
    pub fn render(&self) -> String {
        let mut parts = Vec::new();
        if let Some(u) = &self.uid {
            parts.push(format!("<!-- uid: {u} -->"));
        }
        if let Some(i) = self.importance {
            parts.push(format!("<!-- importance: {i} -->"));
        }
        if !self.declencheurs.is_empty() {
            parts.push(format!(
                "<!-- declencheurs: {} -->",
                self.declencheurs.join(", ")
            ));
        }
        if let Some(p) = &self.projet {
            parts.push(format!("<!-- projet: {p} -->"));
        }
        if let Some(q) = &self.quand {
            parts.push(format!("<!-- quand: {} -->", q.render()));
        }
        if let Some(c) = self.confiance {
            parts.push(format!("<!-- confiance: {c} -->"));
        }
        if !self.preuves.is_empty() {
            parts.push(format!("<!-- preuves: [{}] -->", self.preuves.join(", ")));
        }
        if let Some(o) = self.occurrences {
            parts.push(format!("<!-- occurrences: {o} -->"));
        }
        if let Some(d) = &self.depuis {
            parts.push(format!("<!-- depuis: {d} -->"));
        }
        if let Some(s) = &self.source {
            parts.push(format!("<!-- source: {s} -->"));
        }
        if let Some(r) = &self.revue {
            parts.push(format!("<!-- revue: {r} -->"));
        }
        parts.join(" ")
    }
}

/// Retire les commentaires d'annotation pour obtenir le texte seul.
pub fn strip_annotations(line: &str) -> String {
    comment_re().replace_all(line, "").trim().to_string()
}

/// Une entrée de liste dans un fichier du vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultEntry {
    pub uid: String,
    pub text: String,
    pub annotations: Annotations,
    /// Section du fichier (`Défaut`, `Exceptions`, `Écarts observés`, `Faits`…).
    pub section: String,
    /// Numéro de ligne dans le fichier, pour les messages d'erreur.
    pub line: usize,
}

/// Analyse un fichier du vault en entrées de liste.
///
/// Les uid manquants sont **ajoutés automatiquement** (§6.4) : la fonction renvoie aussi
/// le fichier réécrit si des uid ont été générés.
pub fn parse_entries(raw: &str) -> (Vec<VaultEntry>, Option<String>) {
    let fm = frontmatter::parse(raw).unwrap_or_default();
    let offset = fm.header_lines;
    let mut entries = Vec::new();
    let mut section = String::new();
    let mut changed = false;
    let mut out_lines: Vec<String> = raw
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();

    for (i, line) in out_lines.clone().iter().enumerate() {
        if i < offset {
            continue;
        }
        let trimmed = line.trim();
        if let Some(h) = trimmed.strip_prefix("## ") {
            section = h.trim().to_string();
            continue;
        }
        if trimmed.starts_with("# ") {
            continue;
        }
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        let mut ann = Annotations::parse(item);
        let text = strip_annotations(item);
        if text.is_empty() {
            continue;
        }
        let uid = match &ann.uid {
            Some(u) => u.clone(),
            None => {
                let u = Ulid::new().to_string();
                ann.uid = Some(u.clone());
                // Réécriture de la ligne avec l'uid ajouté.
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                let bullet = if trimmed.starts_with("* ") {
                    "* "
                } else {
                    "- "
                };
                out_lines[i] = format!("{indent}{bullet}{} <!-- uid: {u} -->", item.trim());
                changed = true;
                u
            }
        };
        entries.push(VaultEntry {
            uid,
            text,
            annotations: ann,
            section: section.clone(),
            line: i + 1,
        });
    }

    (entries, changed.then(|| out_lines.join("\n")))
}

// ------------------------------------------------------------------ pratiques

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PracticeStatus {
    Active,
    Contestee,
    Retiree,
}

impl PracticeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PracticeStatus::Active => "active",
            PracticeStatus::Contestee => "contestee",
            PracticeStatus::Retiree => "retiree",
        }
    }
    pub fn parse(s: &str) -> PracticeStatus {
        match s {
            "contestee" | "contestée" => PracticeStatus::Contestee,
            "retiree" | "retirée" => PracticeStatus::Retiree,
            _ => PracticeStatus::Active,
        }
    }
}

/// Règle défaisable (§6.4) : un défaut, des exceptions contextuelles, des écarts observés.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Practice {
    pub id: String,
    pub scope: String,
    pub confiance: f64,
    pub preuves: u32,
    pub statut: PracticeStatus,
    pub maj: String,
    pub declencheurs: Vec<String>,
    pub title: String,
    pub default_entry: Option<VaultEntry>,
    pub exceptions: Vec<VaultEntry>,
    pub ecarts: Vec<VaultEntry>,
}

/// Résultat du rappel d'une pratique dans un contexte donné (§6.7 point 3).
#[derive(Debug, Clone, PartialEq)]
pub struct PracticeRecall {
    pub id: String,
    pub confiance: f64,
    pub default_text: String,
    /// Exceptions dont le `quand` est **satisfait**.
    pub applicable: Vec<(String, When, f64)>,
    /// Exceptions dont une clé est inconnue mais dont la similarité est forte.
    pub to_verify: Vec<String>,
}

impl PracticeRecall {
    /// Rendu exact du PRD §6.7.
    pub fn render(&self) -> String {
        let mut s = format!(
            "[pratique: {} · confiance {:.1}]\nDéfaut : {}",
            self.id, self.confiance, self.default_text
        );
        for (text, when, conf) in &self.applicable {
            s.push_str(&format!(
                "\nS'applique ici : {text} ({}) · confiance {conf:.1}",
                when.render().replace("; ", ", ")
            ));
        }
        if !self.to_verify.is_empty() {
            s.push_str(&format!("\nÀ vérifier : {}", self.to_verify.join(" ; ")));
        }
        s
    }
}

impl Practice {
    pub fn parse(raw: &str, fallback_id: &str) -> Result<Practice, String> {
        let fm = frontmatter::parse(raw).map_err(|e| e.to_string())?;
        if fm.str("type") != Some("pratique") {
            return Err("`type: pratique` attendu dans le frontmatter".into());
        }
        let (entries, _) = parse_entries(raw);

        let title = fm
            .body
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or(fallback_id)
            .trim()
            .to_string();

        let by_section = |name: &str| -> Vec<VaultEntry> {
            entries
                .iter()
                .filter(|e| section_matches(&e.section, name))
                .cloned()
                .collect()
        };

        let defaults = by_section("Défaut");
        let exceptions = by_section("Exceptions");
        let ecarts = by_section("Écarts observés");

        Ok(Practice {
            id: {
                let id = fm.string("id");
                if id.is_empty() {
                    fallback_id.to_string()
                } else {
                    id
                }
            },
            scope: {
                let s = fm.string("scope");
                if s.is_empty() { "global".into() } else { s }
            },
            confiance: fm.f64("confiance").unwrap_or(0.5).clamp(0.0, 1.0),
            preuves: fm.f64("preuves").unwrap_or(0.0) as u32,
            statut: PracticeStatus::parse(&fm.string("statut")),
            maj: fm.string("maj"),
            declencheurs: fm.list("declencheurs"),
            title,
            default_entry: defaults.into_iter().next(),
            exceptions,
            ecarts,
        })
    }

    /// Entrées invalides : une exception ou un écart **sans `quand`** est signalé et
    /// ignoré au rappel (§6.4).
    pub fn invalid_entries(&self) -> Vec<(&VaultEntry, &'static str)> {
        let mut out = Vec::new();
        for e in self.exceptions.iter().chain(self.ecarts.iter()) {
            if e.annotations.quand.is_none() {
                out.push((e, "exception ou écart sans prédicat `quand`"));
            }
        }
        out
    }

    /// Rappel contextuel : défaut **et uniquement** les exceptions satisfaites.
    ///
    /// Les écarts ne sont **jamais** injectés automatiquement : ce n'est pas encore du
    /// savoir (§6.7 point 4).
    pub fn recall(&self, ctx: &BTreeMap<String, String>, verify_threshold: f64) -> PracticeRecall {
        let mut applicable = Vec::new();
        let mut to_verify = Vec::new();

        for e in &self.exceptions {
            let Some(when) = &e.annotations.quand else {
                continue; // invalide : ignoré
            };
            match when.evaluate(ctx) {
                WhenMatch::Satisfied => applicable.push((
                    e.text.clone(),
                    when.clone(),
                    e.annotations.confiance.unwrap_or(self.confiance),
                )),
                WhenMatch::Unknown(missing) => {
                    // Prédicat inconnu ⇒ pas satisfait, mais listé si la similarité est
                    // forte : on approxime la similarité par la proportion de clés connues.
                    let known = when.clauses.len().saturating_sub(missing.len()) as f64;
                    let ratio = if when.clauses.is_empty() {
                        0.0
                    } else {
                        known / when.clauses.len() as f64
                    };
                    if ratio >= verify_threshold {
                        to_verify.push(format!("{} ({})", e.text, when.render()));
                    }
                }
                WhenMatch::Contradicted => {}
            }
        }

        PracticeRecall {
            id: self.id.clone(),
            confiance: self.confiance,
            default_text: self
                .default_entry
                .as_ref()
                .map(|e| e.text.clone())
                .unwrap_or_else(|| self.title.clone()),
            applicable,
            to_verify,
        }
    }

    /// Confiance déterministe (§6.8) : `(succès + 1) / (succès + contradictions + 2)`.
    pub fn confidence(successes: u32, contradictions: u32) -> f64 {
        (successes as f64 + 1.0) / (successes as f64 + contradictions as f64 + 2.0)
    }

    /// Statut dérivé de la confiance (§6.8).
    pub fn derived_status(
        confiance: f64,
        observations: u32,
        threshold: f64,
        min_obs: u32,
    ) -> PracticeStatus {
        if confiance < threshold && observations >= min_obs {
            PracticeStatus::Contestee
        } else {
            PracticeStatus::Active
        }
    }

    pub fn render(&self) -> String {
        let mut fields: BTreeMap<String, FmValue> = BTreeMap::new();
        fields.insert("type".into(), FmValue::Str("pratique".into()));
        fields.insert("id".into(), FmValue::Str(self.id.clone()));
        fields.insert("scope".into(), FmValue::Str(self.scope.clone()));
        fields.insert("confiance".into(), FmValue::Num(self.confiance));
        fields.insert("preuves".into(), FmValue::Num(self.preuves as f64));
        fields.insert("statut".into(), FmValue::Str(self.statut.as_str().into()));
        fields.insert("maj".into(), FmValue::Str(self.maj.clone()));
        fields.insert(
            "declencheurs".into(),
            FmValue::List(self.declencheurs.clone()),
        );

        let mut body = format!("# {}\n\n## Défaut\n", self.title);
        if let Some(d) = &self.default_entry {
            body.push_str(&format!("{} {}\n", d.text, d.annotations.render()));
        }
        body.push_str("\n## Exceptions\n");
        for e in &self.exceptions {
            body.push_str(&format!("- {} {}\n", e.text, e.annotations.render()));
        }
        body.push_str("\n## Écarts observés\n");
        for e in &self.ecarts {
            body.push_str(&format!("- {} {}\n", e.text, e.annotations.render()));
        }
        frontmatter::render(&fields, &body)
    }
}

fn section_matches(actual: &str, expected: &str) -> bool {
    let norm = |s: &str| {
        s.to_lowercase()
            .replace(['é', 'è', 'ê'], "e")
            .replace('à', "a")
            .replace('ç', "c")
    };
    norm(actual) == norm(expected)
}

/// Niveaux de mémoire (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Instruction,
    Profil,
    Coeur,
    Projet,
    Cure,
    Episodic,
    Revue,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Instruction => "instruction",
            Level::Profil => "profil",
            Level::Coeur => "coeur",
            Level::Projet => "projet",
            Level::Cure => "cure",
            Level::Episodic => "episodic",
            Level::Revue => "revue",
        }
    }
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s {
            "instruction" => Level::Instruction,
            "profil" => Level::Profil,
            "coeur" => Level::Coeur,
            "projet" => Level::Projet,
            "cure" => Level::Cure,
            "episodic" => Level::Episodic,
            "revue" => Level::Revue,
            _ => return None,
        })
    }
    /// Injecté automatiquement dans le prompt ?
    pub fn auto_injected(&self) -> bool {
        matches!(
            self,
            Level::Instruction | Level::Profil | Level::Coeur | Level::Projet
        )
    }
    /// Déduit le niveau d'un chemin de fichier relatif au vault.
    pub fn from_path(rel: &str) -> Level {
        let p = rel.replace('\\', "/");
        if p.starts_with("journal/") {
            Level::Episodic
        } else if p == "profil.md" {
            Level::Profil
        } else if p == "memoire.md" {
            Level::Coeur
        } else if p == "AGENTS.md" || p == "SOUL.md" {
            Level::Instruction
        } else if p == "DREAMS.md" || p.starts_with(".dreams/") {
            Level::Revue
        } else {
            Level::Cure
        }
    }
}

/// Préfixes autorisés d'une directive de profil (§6.4).
pub const DIRECTIVE_PREFIXES: &[&str] = &["Toujours", "Jamais", "Préférer", "Éviter"];

pub fn is_directive(text: &str) -> bool {
    DIRECTIVE_PREFIXES
        .iter()
        .any(|p| text.trim_start().starts_with(p))
}

/// Liens `[[slug]]` d'un texte.
pub fn links(text: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\[\[([^\]]+)\]\]").expect("regex valide"));
    re.captures_iter(text)
        .map(|c| c[1].trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn when_parsing_and_rendering() {
        let w = When::parse("tache=code; criticite=haute; codeur=agent").unwrap();
        assert_eq!(w.clauses.len(), 3);
        assert_eq!(w.clauses["tache"], vec!["code"]);
        assert_eq!(w.render(), "codeur=agent; criticite=haute; tache=code");
        let multi = When::parse("tache=code|revue").unwrap();
        assert_eq!(multi.clauses["tache"], vec!["code", "revue"]);
    }

    #[test]
    fn when_rejects_unknown_keys_and_empty() {
        assert!(When::parse("inconnue=x").is_err());
        assert!(When::parse("tache").is_err());
        assert!(When::parse("").is_err());
        assert!(When::parse("tache=").is_err());
    }

    #[test]
    fn when_evaluation() {
        let w = When::parse("tache=code; criticite=haute").unwrap();
        assert_eq!(
            w.evaluate(&ctx(&[("tache", "code"), ("criticite", "haute")])),
            WhenMatch::Satisfied
        );
        assert_eq!(
            w.evaluate(&ctx(&[("tache", "redaction"), ("criticite", "haute")])),
            WhenMatch::Contradicted
        );
        assert_eq!(
            w.evaluate(&ctx(&[("tache", "code")])),
            WhenMatch::Unknown(vec!["criticite".into()])
        );
    }

    #[test]
    fn when_compatibility_and_intersection() {
        let a = When::parse("tache=code|revue; client=x").unwrap();
        let b = When::parse("tache=code; criticite=haute").unwrap();
        assert!(a.compatible_with(&b));
        let i = a.intersect(&b);
        assert_eq!(i.clauses["tache"], vec!["code"]);
        assert!(i.clauses.contains_key("criticite"));

        let c = When::parse("tache=deploiement").unwrap();
        assert!(!a.compatible_with(&c));
    }

    #[test]
    fn annotations_roundtrip() {
        let line = "Toujours répondre en français. <!-- uid: 01J8A --> <!-- importance: 9 --> \
                    <!-- depuis: 2026-01-10 --> <!-- declencheurs: a, b -->";
        let a = Annotations::parse(line);
        assert_eq!(a.uid.as_deref(), Some("01J8A"));
        assert_eq!(a.importance, Some(9));
        assert_eq!(a.declencheurs, vec!["a", "b"]);
        assert_eq!(a.depuis.as_deref(), Some("2026-01-10"));
        assert_eq!(strip_annotations(line), "Toujours répondre en français.");

        let rendered = a.render();
        let back = Annotations::parse(&rendered);
        assert_eq!(back.uid, a.uid);
        assert_eq!(back.importance, a.importance);
        assert_eq!(back.declencheurs, a.declencheurs);
    }

    #[test]
    fn missing_annotations_are_neutral() {
        let a = Annotations::parse("une ligne toute simple");
        assert!(a.uid.is_none());
        assert!(a.importance.is_none());
        assert!(a.declencheurs.is_empty());
    }

    #[test]
    fn uids_are_added_automatically() {
        let raw = "---\ntype: profil\n---\n# Profil\n\n\
                   - Toujours répondre en français. <!-- uid: 01J8A -->\n\
                   - Préférer Go pour le backend.\n";
        let (entries, rewritten) = parse_entries(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].uid, "01J8A");
        assert_eq!(entries[1].uid.len(), 26, "un ULID est généré");
        let rewritten = rewritten.expect("le fichier doit être réécrit");
        assert!(rewritten.contains(&format!("<!-- uid: {} -->", entries[1].uid)));
        // Une seconde passe ne change plus rien.
        let (again, changed) = parse_entries(&rewritten);
        assert!(changed.is_none());
        assert_eq!(again[1].uid, entries[1].uid);
    }

    #[test]
    fn sections_are_tracked() {
        let raw = "# T\n\n## Défaut\n- le défaut\n\n## Exceptions\n- une exception\n";
        let (entries, _) = parse_entries(raw);
        assert_eq!(entries[0].section, "Défaut");
        assert_eq!(entries[1].section, "Exceptions");
    }

    const PRACTICE: &str = "---\n\
        type: pratique\n\
        id: langage-backend\n\
        scope: global\n\
        confiance: 0.8\n\
        preuves: 9\n\
        statut: active\n\
        maj: 2026-09-17\n\
        declencheurs: [langage, stack, nouveau projet, backend]\n\
        ---\n\
        # Langage backend\n\n\
        ## Défaut\n\
        - Go (stdlib, architecture hexagonale, binaire unique). <!-- uid: 01J9A -->\n\n\
        ## Exceptions\n\
        - **Rust** si l'agent code seul sur un projet critique. <!-- uid: 01J9B --> \
          <!-- quand: tache=code; criticite=haute; codeur=agent --> <!-- confiance: 0.9 -->\n\n\
        ## Écarts observés\n\
        - 2026-09-12 · [[client-x]] : langage imposé par l'existant. <!-- uid: 01J9C --> \
          <!-- quand: client=client-x --> <!-- occurrences: 1 -->\n";

    #[test]
    fn practice_parses_all_sections() {
        let p = Practice::parse(PRACTICE, "langage-backend").unwrap();
        assert_eq!(p.id, "langage-backend");
        assert_eq!(p.confiance, 0.8);
        assert_eq!(p.preuves, 9);
        assert_eq!(p.statut, PracticeStatus::Active);
        assert!(p.default_entry.is_some());
        assert_eq!(p.exceptions.len(), 1);
        assert_eq!(p.ecarts.len(), 1);
        assert!(p.invalid_entries().is_empty());
    }

    /// CA 6 (règle défaisable) : un tour de code critique mené par l'agent injecte le
    /// défaut **et** l'exception ; un tour de rédaction n'injecte que le défaut.
    #[test]
    fn ca_6_1_defeasible_rule_recall() {
        let p = Practice::parse(PRACTICE, "langage-backend").unwrap();

        let code = p.recall(
            &ctx(&[
                ("tache", "code"),
                ("criticite", "haute"),
                ("codeur", "agent"),
            ]),
            0.8,
        );
        assert_eq!(code.applicable.len(), 1, "l'exception doit s'appliquer");
        let rendu = code.render();
        assert!(rendu.contains("[pratique: langage-backend · confiance 0.8]"));
        assert!(rendu.contains("Défaut : Go"));
        assert!(rendu.contains("S'applique ici : **Rust**"));
        assert!(rendu.contains("confiance 0.9"));

        let redaction = p.recall(
            &ctx(&[
                ("tache", "redaction"),
                ("criticite", "haute"),
                ("codeur", "agent"),
            ]),
            0.8,
        );
        assert!(
            redaction.applicable.is_empty(),
            "un tour de rédaction ne déclenche pas l'exception"
        );
        assert!(redaction.render().contains("Défaut : Go"));
        assert!(!redaction.render().contains("S'applique ici"));
    }

    #[test]
    fn unknown_predicate_is_not_satisfied_but_may_be_listed() {
        let p = Practice::parse(PRACTICE, "x").unwrap();
        // `codeur` absent du contexte : 2 clés sur 3 connues = 0,67 < 0,8 ⇒ non listé.
        let r = p.recall(&ctx(&[("tache", "code"), ("criticite", "haute")]), 0.8);
        assert!(r.applicable.is_empty());
        assert!(r.to_verify.is_empty());
        // Avec un seuil plus bas, l'exception est signalée « À vérifier ».
        let r = p.recall(&ctx(&[("tache", "code"), ("criticite", "haute")]), 0.6);
        assert_eq!(r.to_verify.len(), 1);
        assert!(r.render().contains("À vérifier"));
    }

    #[test]
    fn deviations_are_never_injected() {
        let p = Practice::parse(PRACTICE, "x").unwrap();
        let r = p.recall(&ctx(&[("client", "client-x")]), 0.0);
        assert!(
            !r.render().contains("langage imposé"),
            "un écart n'est jamais injecté automatiquement"
        );
    }

    #[test]
    fn exception_without_when_is_reported_and_ignored() {
        let raw = PRACTICE.replace(
            "<!-- quand: tache=code; criticite=haute; codeur=agent -->",
            "",
        );
        let p = Practice::parse(&raw, "x").unwrap();
        assert_eq!(p.invalid_entries().len(), 1);
        let r = p.recall(&ctx(&[("tache", "code")]), 0.0);
        assert!(r.applicable.is_empty());
    }

    #[test]
    fn practice_render_roundtrip() {
        let p = Practice::parse(PRACTICE, "x").unwrap();
        let rendered = p.render();
        let back = Practice::parse(&rendered, "x").unwrap();
        assert_eq!(back.id, p.id);
        assert_eq!(back.confiance, p.confiance);
        assert_eq!(back.exceptions.len(), 1);
        assert_eq!(
            back.exceptions[0].annotations.quand,
            p.exceptions[0].annotations.quand
        );
    }

    #[test]
    fn confidence_formula() {
        assert!((Practice::confidence(0, 0) - 0.5).abs() < 1e-9);
        assert!((Practice::confidence(9, 0) - 10.0 / 11.0).abs() < 1e-9);
        assert!((Practice::confidence(1, 4) - 2.0 / 7.0).abs() < 1e-9);
        assert_eq!(
            Practice::derived_status(Practice::confidence(1, 4), 5, 0.5, 4),
            PracticeStatus::Contestee
        );
        assert_eq!(
            Practice::derived_status(0.3, 2, 0.5, 4),
            PracticeStatus::Active,
            "moins de 4 observations : pas encore contestée"
        );
    }

    #[test]
    fn levels_from_paths() {
        assert_eq!(Level::from_path("profil.md"), Level::Profil);
        assert_eq!(Level::from_path("memoire.md"), Level::Coeur);
        assert_eq!(Level::from_path("journal/2026-09-16.md"), Level::Episodic);
        assert_eq!(Level::from_path("pratiques/x.md"), Level::Cure);
        assert_eq!(Level::from_path("AGENTS.md"), Level::Instruction);
        assert_eq!(Level::from_path("DREAMS.md"), Level::Revue);
        assert!(Level::Profil.auto_injected());
        assert!(!Level::Episodic.auto_injected());
        assert!(!Level::Cure.auto_injected());
    }

    #[test]
    fn directives_have_the_required_prefixes() {
        assert!(is_directive("Toujours répondre en français."));
        assert!(is_directive("Éviter les micro-services."));
        assert!(!is_directive("On fait comme ça d'habitude."));
    }

    #[test]
    fn wiki_links_are_extracted() {
        assert_eq!(
            links("voir [[client-x]] et [[projet-a]]"),
            vec!["client-x", "projet-a"]
        );
        assert!(links("aucun lien").is_empty());
    }
}
