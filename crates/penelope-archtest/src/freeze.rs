//! Les règles de gel de la dette (`design/v1/gel-et-outillage.md` §3 et
//! `design/v1/decoupage-daemon.md` §7), confrontées à `budget.toml` :
//!
//! - R1, R2, R3 : plafond par fichier, plafond des tests, liste de référence qui ne peut
//!   que décroître (#209) ;
//! - R4, R5, R6 : plafond de crate, liste blanche des modules du daemon, couplage au type
//!   `Daemon` (#210) ;
//! - R7 : allows de `clippy::too_many_lines` comptés (#211) ;
//! - R8 : les critères d'acceptation ne disparaissent pas (#213) ;
//! - R8 du découpage : le cœur ne nomme pas le canal (#214).
//!
//! Chaque règle est une fonction pure sur une `Snapshot` et un `Budget`, pour que le test
//! du détecteur puisse lui donner des fichiers fictifs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use crate::Violation;
use crate::budget::{Budget, Measures};
use crate::snapshot::{Snapshot, SourceFile, code_lines, is_test_path};

/// Classe d'un fichier Rust pour les plafonds (R1, R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Source,
    Test,
}

/// Un fichier est un fichier de tests s'il en a le chemin (`is_test_path` : répertoire
/// `tests/`, nom `tests.rs`, `*_tests.rs` ou `testing.rs`) ou s'il est listé dans
/// `[files].test_modules`.
pub fn file_kind(rel: &str, budget: &Budget) -> FileKind {
    if is_test_path(rel) || budget.test_modules.contains(rel) {
        FileKind::Test
    } else {
        FileKind::Source
    }
}

fn ceiling_for(kind: FileKind, budget: &Budget) -> usize {
    match kind {
        FileKind::Source => budget.ceiling,
        FileKind::Test => budget.test_ceiling,
    }
}

/// `13716` → `13 716`, comme dans les messages des issues.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn violation(f: &SourceFile, line: usize, rule: &'static str, text: String) -> Violation {
    Violation {
        crate_name: f.crate_name.clone(),
        file: PathBuf::from(&f.rel),
        line,
        rule,
        text,
    }
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("motif valide"))
}

// ---------------------------------------------------------------------------------------
// R1, R2, R3 : taille des fichiers et liste de référence
// ---------------------------------------------------------------------------------------

/// Plafond par fichier (R1, R2) et liste de référence (R3, trois échecs distincts : nouveau
/// dépassement, hausse d'un fichier listé, entrée périmée).
pub fn size_violations(snap: &Snapshot, budget: &Budget) -> Vec<Violation> {
    let mut out = Vec::new();
    for f in &snap.files {
        let n = f.line_count();
        let kind = file_kind(&f.rel, budget);
        let cap = ceiling_for(kind, budget);
        match budget.oversized.get(&f.rel) {
            None if n > cap => {
                let text = match kind {
                    FileKind::Source => format!(
                        "{} lignes, plafond {} (hors liste de référence) : découper, ou \
                         déplacer les tests dans {}/tests.rs (tests.rs, tests/, *_tests.rs \
                         et testing.rs sont des fichiers de tests, plafond {})",
                        thousands(n),
                        thousands(cap),
                        f.name().trim_end_matches(".rs"),
                        thousands(budget.test_ceiling)
                    ),
                    FileKind::Test => format!(
                        "{} lignes, plafond {} : scinder le fichier de tests",
                        thousands(n),
                        thousands(cap)
                    ),
                };
                let rule = match kind {
                    FileKind::Source => "plafond de taille",
                    FileKind::Test => "plafond des tests",
                };
                out.push(violation(f, 1, rule, text));
            }
            None => {}
            Some(_) if n <= cap => out.push(violation(
                f,
                1,
                "entrée périmée",
                format!(
                    "{} lignes, plafond {} : retirer l'entrée de budget.toml \
                     [files.oversized], ou UPDATE_BUDGET=1 cargo test -p penelope-archtest",
                    thousands(n),
                    thousands(cap)
                ),
            )),
            Some(&allowed) if n > allowed => out.push(violation(
                f,
                1,
                "liste de référence",
                format!(
                    "{} lignes, la liste de référence lui en accorde {} : ce fichier ne \
                     peut que décroître (budget.toml [files.oversized])",
                    thousands(n),
                    thousands(allowed)
                ),
            )),
            Some(_) => {}
        }
    }
    for (rel, allowed) in &budget.oversized {
        if snap.find(rel).is_none() {
            out.push(Violation {
                crate_name: crate_of(rel),
                file: PathBuf::from(rel),
                line: 1,
                rule: "entrée périmée",
                text: format!(
                    "fichier disparu (la liste lui accordait {}) : retirer l'entrée de \
                     budget.toml [files.oversized], ou UPDATE_BUDGET=1 cargo test -p \
                     penelope-archtest",
                    thousands(*allowed)
                ),
            });
        }
    }
    out
}

fn crate_of(rel: &str) -> String {
    rel.split('/').nth(1).unwrap_or("?").to_string()
}

// ---------------------------------------------------------------------------------------
// R4 : plafond par crate
// ---------------------------------------------------------------------------------------

/// Lignes de `src/` par crate.
pub fn crate_lines(snap: &Snapshot) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for f in snap.files.iter().filter(|f| f.in_src()) {
        *out.entry(f.crate_name.clone()).or_default() += f.line_count();
    }
    out
}

/// R4 : chaque crate de `[crates]` tient sous son plafond.
pub fn crate_size_violations(snap: &Snapshot, budget: &Budget) -> Vec<String> {
    let lines = crate_lines(snap);
    let mut out = Vec::new();
    for (name, cap) in &budget.crates {
        let n = lines.get(name).copied().unwrap_or(0);
        if n > *cap {
            out.push(format!(
                "{name}/src : {} lignes, plafond {} (budget.toml [crates]) : la 0.17 ne \
                 grossit plus, la fonctionnalité va dans la branche v1",
                thousands(n),
                thousands(*cap)
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// R5 : liste blanche des modules du daemon
// ---------------------------------------------------------------------------------------

/// Déclarations `mod x;` (fichier séparé) dans les lignes de code d'un fichier :
/// `(ligne, nom)`. `mod tests;` n'est pas retourné : c'est la soupape de R1.
pub fn declared_modules(raw: &str) -> Vec<(usize, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = regex(
        &RE,
        r"^\s*(?:pub(?:\([a-z:]+\))?\s+)?mod\s+([a-z_0-9]+)\s*;",
    );
    code_lines(raw)
        .into_iter()
        .filter_map(|(n, line)| re.captures(line).map(|c| (n, c[1].to_string())))
        .filter(|(_, name)| name != "tests")
        .collect()
}

fn daemon_code_files<'a>(snap: &'a Snapshot, budget: &Budget) -> Vec<&'a SourceFile> {
    snap.files
        .iter()
        .filter(|f| f.daemon_rel().is_some() && file_kind(&f.rel, budget) == FileKind::Source)
        .collect()
}

/// R5 ne regarde que les modules de premier niveau, déclarés dans `lib.rs` : découper un
/// fichier existant en sous-modules est le but de la V1, pas une fonctionnalité nouvelle.
fn daemon_root_files<'a>(snap: &'a Snapshot, budget: &Budget) -> Vec<&'a SourceFile> {
    daemon_code_files(snap, budget)
        .into_iter()
        .filter(|f| f.daemon_rel() == Some("lib.rs"))
        .collect()
}

/// R5 : tout module de premier niveau du daemon est dans `[daemon].modules`.
pub fn daemon_module_violations(snap: &Snapshot, budget: &Budget) -> Vec<Violation> {
    let mut out = Vec::new();
    for f in daemon_root_files(snap, budget) {
        for (line, name) in declared_modules(&f.raw) {
            if !budget.daemon_modules.contains(&name) {
                out.push(violation(
                    f,
                    line,
                    "liste blanche des modules",
                    format!(
                        "module « {name} » absent de la liste blanche (budget.toml \
                         [daemon].modules) : un nouveau module du daemon se fait dans v1, \
                         pas dans la 0.17"
                    ),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// R6 : couplage au type Daemon
// ---------------------------------------------------------------------------------------

/// Occurrences du mot `Daemon` dans les lignes de code (frontière de mot : `DaemonHandle`
/// et `DaemonTokens` ne comptent pas) : `(ligne, occurrence)`.
pub fn daemon_mentions(raw: &str) -> Vec<usize> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = regex(&RE, r"\bDaemon\b");
    code_lines(raw)
        .into_iter()
        .flat_map(|(n, line)| re.find_iter(line).map(move |_| n))
        .collect()
}

/// Lignes de code ouvrant un bloc `impl Daemon` ou `impl … for Daemon`.
pub fn impl_daemon_lines(raw: &str) -> Vec<usize> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = regex(
        &RE,
        r"^impl(?:<[^>]*>)?\s+Daemon\b|^impl\b.*\bfor\s+Daemon\b",
    );
    code_lines(raw)
        .into_iter()
        .filter(|(_, line)| re.is_match(line))
        .map(|(n, _)| n)
        .collect()
}

/// R6 : le couplage au `Daemon` ne croît pas, et `impl Daemon` reste dans ses quatre
/// fichiers.
pub fn daemon_coupling_violations(snap: &Snapshot, budget: &Budget) -> Vec<Violation> {
    let mut out = Vec::new();
    for f in daemon_code_files(snap, budget) {
        let rel = f.daemon_rel().unwrap_or(&f.rel);
        let mentions = daemon_mentions(&f.raw);
        let allowed = budget.daemon_users.get(rel).copied().unwrap_or(0);
        if mentions.len() > allowed {
            out.push(violation(
                f,
                mentions[allowed],
                "couplage au Daemon",
                format!(
                    "{rel} nomme le type Daemon {} fois hors tests, budget {allowed} \
                     (budget.toml [daemon.daemon_users]) : prendre &Services ou un trait, \
                     pas le daemon entier",
                    mentions.len()
                ),
            ));
        }
        if !budget.impl_daemon.contains(rel) {
            for line in impl_daemon_lines(&f.raw) {
                out.push(violation(
                    f,
                    line,
                    "impl Daemon",
                    format!(
                        "bloc « impl Daemon » dans {rel}, hors de la liste (budget.toml \
                         [daemon].impl_daemon = {}) : les méthodes du daemon vivent dans \
                         ces fichiers-là",
                        budget
                            .impl_daemon
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// R7 : allows de clippy::too_many_lines
// ---------------------------------------------------------------------------------------

/// Les `#[allow(clippy::too_many_lines)]` (et `expect`, et la forme `#![…]`) des sources,
/// commentaires exclus : `(fichier, ligne)`.
pub fn too_many_lines_allows(snap: &Snapshot) -> Vec<(String, usize)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = regex(&RE, r"#!?\[(?:allow|expect)\([^\]]*clippy::too_many_lines");
    let mut out = Vec::new();
    for f in &snap.files {
        for (n, line) in f.raw.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if re.is_match(line) {
                out.push((f.rel.clone(), n + 1));
            }
        }
    }
    out
}

/// R7 : le nombre d'allows ne dépasse pas `[lints].allow_too_many_lines`.
pub fn too_many_lines_allow_violations(snap: &Snapshot, budget: &Budget) -> Vec<String> {
    let allows = too_many_lines_allows(snap);
    if allows.len() <= budget.allow_too_many_lines {
        return Vec::new();
    }
    // Le nom du lint est injecté pour que cette ligne ne compte pas elle-même.
    let mut out = vec![format!(
        "{} #[allow(clippy::{lint})] dans les sources, budget {} (budget.toml [lints]) : \
         découper la fonction",
        allows.len(),
        budget.allow_too_many_lines,
        lint = "too_many_lines"
    )];
    out.extend(allows.iter().map(|(rel, line)| format!("  {rel}:{line}")));
    out
}

// ---------------------------------------------------------------------------------------
// R8 : les critères d'acceptation ne disparaissent pas
// ---------------------------------------------------------------------------------------

/// Noms de fonctions `ca_<section>_<n>_<nom>` d'un fichier (les vingt lignes de
/// `penelope_evals::ca_matrix::scan_source`, recopiées pour ne pas dépendre d'`evals`).
pub fn ca_names(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let Some(rest) = line.split("fn ").nth(1) else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let Some(tail) = name.strip_prefix("ca_") else {
            continue;
        };
        let mut parts = tail.splitn(3, '_');
        let (Some(section), Some(number), Some(_)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if section.parse::<u32>().is_ok() && number.parse::<u32>().is_ok() {
            out.push(name);
        }
    }
    out
}

/// Tous les critères d'acceptation présents dans le workspace.
pub fn present_ca_names(snap: &Snapshot) -> BTreeSet<String> {
    snap.files.iter().flat_map(|f| ca_names(&f.raw)).collect()
}

/// R8 : chaque nom de `[ca].required` existe encore comme fonction.
pub fn acceptance_test_violations(snap: &Snapshot, budget: &Budget) -> Vec<String> {
    let present = present_ca_names(snap);
    budget
        .ca_required
        .iter()
        .filter(|name| !present.contains(*name))
        .map(|name| {
            format!(
                "critère d'acceptation disparu : {name} (budget.toml [ca].required le cite, \
                 aucune fonction ne le porte) : le renommer est interdit, le déplacer est \
                 permis"
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// R8 du découpage : frontière canal / cœur
// ---------------------------------------------------------------------------------------

/// Crates qui ne doivent pas nommer un canal. Les crates de la V1 qui n'existent pas
/// encore y figurent déjà : elles seront protégées dès leur création.
pub const CHANNEL_AGNOSTIC_CRATES: &[&str] = &[
    "penelope-kernel",
    "penelope-store",
    "penelope-observe",
    "penelope-platform",
    "penelope-tools",
    "penelope-hitl",
    "penelope-llm",
    "penelope-context",
    "penelope-memory",
    "penelope-mcp",
    "penelope-skills",
    "penelope-workflow",
    "penelope-daemon",
    "penelope-app",
    "penelope-mcp-host",
    "penelope-agent",
    "penelope-executor",
    "penelope-vault",
    "penelope-dream",
];

/// Crates qui ont le droit de nommer un canal : la passerelle, ce qui est au-dessus
/// d'elle (la CLI et les évaluations), et ce crate-ci, qui porte la table des motifs.
pub const CHANNEL_CRATES: &[&str] = &[
    "penelope-telegram",
    "penelope-gateway-telegram",
    "penelope-cli",
    "penelope-evals",
    "penelope-archtest",
];

/// Motifs de canal : `telegram` (toute casse : import `penelope_telegram::`, chemin
/// `crate::telegram::`, configuration `cfg.telegram.`, comparaison `Origin::Telegram`,
/// texte utilisateur), identifiants `tg_…`, `chat_id`, `topic_id`, `callback_data`,
/// `find_by_topic`.
pub const CHANNEL_PATTERN: &str =
    r"(?i:telegram)|\btg_[a-z0-9_]*|\bchat_id\b|\btopic_id\b|\bcallback_data\b|\bfind_by_topic\b";

/// Mentions du canal dans les lignes de code : `(ligne, texte apparié)`.
pub fn channel_mentions(raw: &str) -> Vec<(usize, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = regex(&RE, CHANNEL_PATTERN);
    code_lines(raw)
        .into_iter()
        .flat_map(|(n, line)| re.find_iter(line).map(move |m| (n, m.as_str().to_string())))
        .collect()
}

fn channel_files<'a>(snap: &'a Snapshot, budget: &Budget) -> Vec<&'a SourceFile> {
    snap.files
        .iter()
        .filter(|f| {
            CHANNEL_AGNOSTIC_CRATES.contains(&f.crate_name.as_str())
                && file_kind(&f.rel, budget) == FileKind::Source
        })
        .collect()
}

/// R8 du découpage : les surfaces agnostiques ne nomment le canal que dans la mesure
/// de `[channel.allowed]`.
pub fn channel_violations(snap: &Snapshot, budget: &Budget) -> Vec<Violation> {
    let mut out = Vec::new();
    for f in channel_files(snap, budget) {
        let mentions = channel_mentions(&f.raw);
        let allowed = budget.channel_allowed.get(&f.rel).copied().unwrap_or(0);
        if mentions.len() > allowed {
            let (line, text) = &mentions[allowed];
            out.push(violation(
                f,
                *line,
                "frontière canal/cœur",
                format!(
                    "{} nomme le canal {} fois hors tests (« {text} » ligne {line}), budget \
                     {allowed} (budget.toml [channel.allowed]) : le cœur ne connaît un canal \
                     que par ChannelDelivery, Messenger ou OwnerChannel",
                    f.name(),
                    mentions.len()
                ),
            ));
        }
    }
    out
}

/// Chaque crate du workspace est déclaré d'un côté ou de l'autre de la frontière.
pub fn undeclared_channel_crates(snap: &Snapshot) -> Vec<String> {
    let names: BTreeSet<&str> = snap.files.iter().map(|f| f.crate_name.as_str()).collect();
    names
        .into_iter()
        .filter(|n| !CHANNEL_AGNOSTIC_CRATES.contains(n) && !CHANNEL_CRATES.contains(n))
        .map(|n| {
            format!(
                "`{n}` n'est ni dans CHANNEL_AGNOSTIC_CRATES ni dans CHANNEL_CRATES : dire \
                 de quel côté de la frontière canal/cœur il vit"
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Mesure de l'état courant, pour UPDATE_BUDGET=1
// ---------------------------------------------------------------------------------------

/// Relève l'état courant du workspace, ce que `budget::tighten` confronte au fichier.
pub fn measure(snap: &Snapshot, budget: &Budget) -> Measures {
    let mut m = Measures::default();
    for f in &snap.files {
        let n = f.line_count();
        if n > ceiling_for(file_kind(&f.rel, budget), budget) {
            m.oversized.insert(f.rel.clone(), n);
        }
    }
    for f in daemon_root_files(snap, budget) {
        m.daemon_modules
            .extend(declared_modules(&f.raw).into_iter().map(|(_, name)| name));
    }
    for f in daemon_code_files(snap, budget) {
        let rel = f.daemon_rel().unwrap_or(&f.rel).to_string();
        if !impl_daemon_lines(&f.raw).is_empty() {
            m.impl_daemon.insert(rel.clone());
        }
        let mentions = daemon_mentions(&f.raw).len();
        if mentions > 0 {
            m.daemon_users.insert(rel, mentions);
        }
    }
    m.allow_too_many_lines = too_many_lines_allows(snap).len();
    m.ca_present = present_ca_names(snap);
    for f in channel_files(snap, budget) {
        let mentions = channel_mentions(&f.raw).len();
        if mentions > 0 {
            m.channel.insert(f.rel.clone(), mentions);
        }
    }
    m
}

#[cfg(test)]
mod tests;
