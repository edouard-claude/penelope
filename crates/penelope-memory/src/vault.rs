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

mod practice;
pub use practice::{Practice, PracticeRecall, PracticeStatus};

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
            if let Some(b) = other.clauses.get(k)
                && !a.iter().any(|x| b.contains(x))
            {
                return false;
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
    /// Date (`AAAA-MM-JJ`) après laquelle l'entrée n'est plus injectée d'office.
    pub expire: Option<String>,
    /// Donnée client, financière ou de sécurité : un marqueur, pas un filtre (issue #37).
    pub sensible: bool,
    /// uid de l'entrée que celle-ci remplace (`supersede`, issue #37) ; la date est `depuis`.
    pub remplace: Option<String>,
}

fn comment_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"<!--\s*([a-zA-Zé]+)\s*:\s*(.*?)\s*-->").expect("regex valide"))
}

/// Identifiant de bloc en fin de ligne : ` ^id` (issue #29).
fn block_id_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?:^|\s)\^([A-Za-z0-9-]+)\s*$").expect("regex valide"))
}

/// Identifiant de bloc valide : lettres latines, chiffres et tirets.
pub fn is_valid_block_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Identifiant de bloc terminant la ligne, s'il y en a un.
pub fn block_id(line: &str) -> Option<String> {
    let without = comment_re().replace_all(line, "");
    block_id_re()
        .captures(without.trim_end())
        .map(|c| c[1].to_string())
}

/// Ligne réduite à un identifiant de bloc (`^id`), qui désigne le bloc précédent.
pub fn standalone_block_id(line: &str) -> Option<String> {
    line.trim()
        .strip_prefix('^')
        .filter(|id| is_valid_block_id(id))
        .map(str::to_string)
}

/// Ligne au format antérieur (`<!-- uid: X -->`) réécrite avec l'identifiant de bloc final,
/// annotations conservées ; `None` si rien à migrer. N'ajoute jamais d'uid.
pub fn migrate_legacy_line(line: &str) -> Option<String> {
    if !Annotations::has_legacy_uid(line) {
        return None;
    }
    let ann = Annotations::parse(line);
    if !ann.uid.as_deref().is_some_and(is_valid_block_id) {
        return None;
    }
    let text = strip_annotations(line);
    Some(format!("{text} {}", ann.render()))
}

/// uid d'une ligne : identifiant de bloc, ou ancien commentaire `<!-- uid: … -->`.
pub fn line_uid(line: &str) -> Option<String> {
    Annotations::parse(line).uid
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
                "expire" => a.expire = Some(val),
                "sensible" => a.sensible = matches!(val.as_str(), "oui" | "true" | "1"),
                "remplace" => a.remplace = Some(val),
                _ => {}
            }
        }
        if a.uid.is_none() {
            a.uid = block_id(line);
        }
        a
    }

    /// Vrai si l'uid est encore écrit en commentaire (format antérieur à 0.9.0).
    pub fn has_legacy_uid(line: &str) -> bool {
        comment_re()
            .captures_iter(line)
            .any(|c| c[1].eq_ignore_ascii_case("uid"))
    }

    /// Rend les annotations en commentaires, dans un ordre stable, et l'uid en identifiant
    /// de bloc final (`^uid`) : un wikilink peut alors viser l'entrée, `[[note#^uid]]`. Un uid
    /// que l'identifiant de bloc ne peut pas porter reste en commentaire.
    pub fn render(&self) -> String {
        let mut parts = Vec::new();
        if let Some(u) = self.uid.as_ref().filter(|u| !is_valid_block_id(u)) {
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
        if let Some(e) = &self.expire {
            parts.push(format!("<!-- expire: {e} -->"));
        }
        if self.sensible {
            parts.push("<!-- sensible: oui -->".to_string());
        }
        if let Some(d) = &self.depuis {
            parts.push(format!("<!-- depuis: {d} -->"));
        }
        if let Some(r) = &self.remplace {
            parts.push(format!("<!-- remplace: {r} -->"));
        }
        if let Some(s) = &self.source {
            parts.push(format!("<!-- source: {s} -->"));
        }
        if let Some(r) = &self.revue {
            parts.push(format!("<!-- revue: {r} -->"));
        }
        if let Some(u) = self.uid.as_ref().filter(|u| is_valid_block_id(u)) {
            parts.push(format!("^{u}"));
        }
        parts.join(" ")
    }
}

/// Retire les commentaires d'annotation et l'identifiant de bloc final pour obtenir le
/// texte seul.
pub fn strip_annotations(line: &str) -> String {
    let without = comment_re().replace_all(line, "");
    block_id_re()
        .replace(without.trim_end(), "")
        .trim()
        .to_string()
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
    // Encadré en cours : texte, puis texte brut pour ses annotations.
    let mut callout: Option<(String, String)> = None;
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
            callout = None;
            continue;
        }
        if trimmed.starts_with("# ") {
            callout = None;
            continue;
        }
        // Encadré (`> [!abstract] …`) suivi d'un identifiant de bloc seul sur sa
        // ligne : une entrée dont le texte est celui de l'encadré.
        if trimmed.starts_with('>') {
            let body = trimmed.trim_start_matches('>').trim();
            let body = match body.strip_prefix("[!") {
                Some(rest) => rest.split_once(']').map(|(_, t)| t).unwrap_or("").trim(),
                None => body,
            };
            let (text, ann) = callout.get_or_insert_with(|| (String::new(), String::new()));
            if !body.is_empty() {
                ann.push_str(body);
                ann.push(' ');
                let plain = strip_annotations(body);
                if !plain.is_empty() {
                    if !text.is_empty() {
                        text.push_str(if text.ends_with(['.', ':', '!', '?']) {
                            " "
                        } else {
                            " : "
                        });
                    }
                    text.push_str(&plain);
                }
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(id) = standalone_block_id(trimmed) {
            if let Some((text, raw_ann)) = callout.take()
                && !text.is_empty()
            {
                let mut annotations = Annotations::parse(&raw_ann);
                annotations.uid = Some(id.clone());
                entries.push(VaultEntry {
                    uid: id,
                    text,
                    annotations,
                    section: section.clone(),
                    line: i + 1,
                });
            }
            continue;
        }
        callout = None;
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
        let legacy =
            Annotations::has_legacy_uid(item) && ann.uid.as_deref().is_some_and(is_valid_block_id);
        if ann.uid.is_none() {
            ann.uid = Some(Ulid::new().to_string());
        }
        let uid = ann.uid.clone().unwrap_or_default();
        if legacy || block_id(item).is_none() && is_valid_block_id(&uid) {
            // Réécriture avec identifiant de bloc : uid ajouté, ou déplacé du commentaire vers
            // l'identifiant de bloc final, annotations conservées.
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            let bullet = if trimmed.starts_with("* ") {
                "* "
            } else {
                "- "
            };
            out_lines[i] = format!("{indent}{bullet}{} {}", text, ann.render());
            changed = true;
        }
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

/// Cibles des wikilinks d'un texte, réduites au nom : `[[dossier/slug#^bloc|texte]]` donne
/// `slug`.
pub fn links(text: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\[\[([^\]]+)\]\]").expect("regex valide"));
    re.captures_iter(text)
        .filter_map(|c| {
            let target = c[1].split(['|', '#']).next().unwrap_or_default().trim();
            let name = target.rsplit('/').next().unwrap_or(target);
            let name = name.strip_suffix(".md").unwrap_or(name);
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests;
