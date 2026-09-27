//! R10 : chaque surface visible est exercée par un scénario rejouable
//! (`design/v1/gel-et-outillage.md` §R10, épopée #208).
//!
//! Les surfaces sont lues dans le code, comme le reste d'archtest, par motifs :
//! - les commandes Telegram, entrées `c("nom", …)` de `commands::all`
//!   (`crates/penelope-telegram/src/commands.rs` et `commands/*.rs` s'il est découpé) ;
//! - les outils natifs, entrées `spec("nom", …)` du catalogue
//!   (`crates/penelope-tools/src/spec.rs` et ses sections `spec/*.rs`) ;
//!
//! chaque fois hors fichiers et modules de tests.
//! - les méthodes RPC, constantes `pub const X: &str = "nom";` de `api::method`
//!   (`crates/penelope-kernel/src/api.rs`).
//!
//! Ce qu'un scénario exerce est lu dans ses fichiers, pas déclaré à la main : une étape
//! `kind = "command"` de `scenario.toml` exerce sa commande, une étape `kind = "telegram"`
//! dont le `text` commence par `/` aussi, une étape `kind = "rpc"` sa méthode
//! (`method = "session.new"`) ; un appel d'outil de `model.jsonl` (champ `"name"`, y
//! compris le nom passé à `tool_call`) exerce l'outil. Une déclaration aurait pu mentir ;
//! ce qui est joué ne ment pas.
//!
//! Identifiants : `/compact`, `outil:fs_read`, `rpc:session.new`. Ce qui n'a pas de
//! scénario est inscrit dans `budget.toml` `[scenarios].missing`, liste qui ne fait que
//! rétrécir (`UPDATE_BUDGET=1`, cliquet de `ratchet.rs`).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::budget::{BUDGET_FILE, Budget};
use crate::snapshot::{Snapshot, SourceFile, test_module_start};

/// Les scénarios, relatifs à la racine du dépôt.
pub const SCENARIOS_DIR: &str = "crates/penelope-evals/scenarios";
/// Catalogue des commandes Telegram.
pub const COMMANDS_FILE: &str = "crates/penelope-telegram/src/commands.rs";
/// Catalogue des outils natifs ; ses sections sont sous `spec/`.
pub const TOOLS_FILE: &str = "crates/penelope-tools/src/spec.rs";
/// Catalogue des méthodes RPC.
pub const API_FILE: &str = "crates/penelope-kernel/src/api.rs";

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("motif valide"))
}

/// Le code d'un fichier, sans son module de tests inline.
fn code_part(raw: &str) -> String {
    match test_module_start(raw) {
        Some(i) => raw.lines().take(i).collect::<Vec<_>>().join("\n"),
        None => raw.to_string(),
    }
}

/// Les commandes du catalogue : `c("nom", …)`, où que `commands::all` les range (un
/// découpage de la fonction en sections ne doit pas les faire disparaître).
pub fn commands_in(raw: &str) -> BTreeSet<String> {
    static C: OnceLock<Regex> = OnceLock::new();
    regex(&C, r#"\bc\(\s*"([a-z0-9_]+)""#)
        .captures_iter(&code_part(raw))
        .map(|c| format!("/{}", &c[1]))
        .collect()
}

/// Les outils d'une section du catalogue : `spec("nom", …)`.
pub fn tools_in(raw: &str) -> BTreeSet<String> {
    static S: OnceLock<Regex> = OnceLock::new();
    regex(&S, r#"\bspec\(\s*"([a-z0-9_]+)""#)
        .captures_iter(&code_part(raw))
        .map(|c| format!("outil:{}", &c[1]))
        .collect()
}

/// Les méthodes de `pub mod method { … }`.
pub fn methods_in(raw: &str) -> BTreeSet<String> {
    static M: OnceLock<Regex> = OnceLock::new();
    let body = raw
        .split_once("pub mod method {")
        .map_or("", |(_, rest)| rest.split("\n}\n").next().unwrap_or(rest));
    regex(&M, r#"pub const [A-Z0-9_]+: &str = "([^"]+)";"#)
        .captures_iter(body)
        .map(|c| format!("rpc:{}", &c[1]))
        .collect()
}

/// Toutes les surfaces visibles du code.
pub fn catalog(snap: &Snapshot) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let under = |f: &SourceFile, file: &str| {
        f.rel == file
            || (f
                .rel
                .starts_with(&format!("{}/", file.trim_end_matches(".rs")))
                && f.name() != "tests.rs")
    };
    for f in &snap.files {
        if under(f, COMMANDS_FILE) {
            out.extend(commands_in(&f.raw));
        } else if under(f, TOOLS_FILE) {
            out.extend(tools_in(&f.raw));
        } else if f.rel == API_FILE {
            out.extend(methods_in(&f.raw));
        }
    }
    out
}

/// Les fichiers d'un scénario que R10 lit.
#[derive(Debug, Clone, Default)]
pub struct ScenarioFiles {
    pub name: String,
    /// `scenario.toml`.
    pub spec: String,
    /// `model.jsonl` (vide s'il n'y en a pas).
    pub model: String,
}

/// Les surfaces qu'un scénario joue réellement : ses commandes, ses méthodes RPC et les
/// outils que le modèle scripté appelle.
pub fn exercised(s: &ScenarioFiles) -> Result<BTreeSet<String>, String> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    let doc: toml::Table = s
        .spec
        .parse()
        .map_err(|e| format!("scénario {} : scenario.toml invalide : {e}", s.name))?;
    let mut out = BTreeSet::new();
    for step in doc
        .get("steps")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let cmd = match step.get("kind").and_then(|k| k.as_str()) {
            Some("command") => step.get("command"),
            Some("telegram") => step.get("text"),
            _ => None,
        };
        if let Some(name) = cmd
            .and_then(|c| c.as_str())
            .and_then(|c| c.split_whitespace().next())
            .filter(|n| n.starts_with('/'))
        {
            out.insert(name.to_string());
        }
        if step.get("kind").and_then(|k| k.as_str()) == Some("rpc")
            && let Some(m) = step.get("method").and_then(|m| m.as_str())
        {
            out.insert(format!("rpc:{m}"));
        }
    }
    let name = regex(&NAME, r#""name"\s*:\s*"([A-Za-z0-9_]+)""#);
    for line in s.model.lines().filter(|l| l.contains("\"tool_calls\"")) {
        out.extend(name.captures_iter(line).map(|c| format!("outil:{}", &c[1])));
    }
    Ok(out)
}

/// Lit `scenario.toml` et `model.jsonl` de chaque répertoire de `dir`.
pub fn read_scenarios(dir: &Path) -> Result<Vec<ScenarioFiles>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let spec = std::fs::read_to_string(path.join("scenario.toml"))
            .map_err(|err| format!("scénario {name} : scenario.toml : {err}"))?;
        let model = std::fs::read_to_string(path.join("model.jsonl")).unwrap_or_default();
        out.push(ScenarioFiles { name, spec, model });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Couverture : les surfaces du catalogue, et celles qu'aucun scénario n'exerce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub catalog: BTreeSet<String>,
    pub uncovered: BTreeSet<String>,
}

pub fn coverage(snap: &Snapshot, scenarios: &[ScenarioFiles]) -> Result<Coverage, String> {
    let catalog = catalog(snap);
    let mut played = BTreeSet::new();
    for s in scenarios {
        played.extend(exercised(s)?);
    }
    let uncovered = catalog.difference(&played).cloned().collect();
    Ok(Coverage { catalog, uncovered })
}

/// La couverture du workspace réel.
pub fn workspace_coverage(snap: &Snapshot) -> Result<Coverage, String> {
    coverage(
        snap,
        &read_scenarios(&crate::workspace_root().join(SCENARIOS_DIR))?,
    )
}

/// `[scenarios].missing` d'un `budget.toml` ; une table absente est une liste vide.
pub fn missing_in(raw: &str) -> Result<BTreeSet<String>, String> {
    let doc: toml::Value = raw
        .parse()
        .map_err(|e| format!("{BUDGET_FILE} : TOML invalide : {e}"))?;
    let Some(list) = doc.get("scenarios").and_then(|s| s.get("missing")) else {
        return Ok(BTreeSet::new());
    };
    list.as_array()
        .ok_or_else(|| format!("{BUDGET_FILE} [scenarios].missing : liste attendue"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{BUDGET_FILE} [scenarios].missing : chaîne attendue"))
        })
        .collect()
}

/// `[scenarios].missing` du workspace, après le resserrement d'`UPDATE_BUDGET`.
pub fn workspace_missing() -> Result<BTreeSet<String>, String> {
    Budget::load()?;
    let raw =
        std::fs::read_to_string(Budget::path()).map_err(|e| format!("{BUDGET_FILE} : {e}"))?;
    missing_in(&raw)
}

fn describe(surface: &str) -> String {
    if let Some(name) = surface.strip_prefix("outil:") {
        format!("outil natif {name}")
    } else if let Some(name) = surface.strip_prefix("rpc:") {
        format!("méthode RPC {name}")
    } else {
        format!("commande Telegram {surface}")
    }
}

/// Les refus de R10 : une surface sans scénario absente de la liste, une entrée de la
/// liste qui a désormais un scénario ou n'existe plus.
pub fn violations(cov: &Coverage, missing: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for s in cov.uncovered.difference(missing) {
        out.push(format!(
            "{} sans scénario de composition ({SCENARIOS_DIR}/) et absente de \
             budget.toml [scenarios].missing",
            describe(s)
        ));
    }
    for s in missing.difference(&cov.uncovered) {
        let why = if cov.catalog.contains(s) {
            "a maintenant un scénario"
        } else {
            "n'existe plus dans le code"
        };
        out.push(format!(
            "budget.toml [scenarios].missing : « {s} » {why} ; retirer l'entrée, ou \
             UPDATE_BUDGET=1 cargo test -p penelope-archtest"
        ));
    }
    out
}

/// Resserre `[scenarios].missing` : ne garde que les surfaces encore sans scénario.
/// N'ajoute jamais d'entrée et ne crée pas la table.
pub fn tighten(raw: &str, uncovered: &BTreeSet<String>) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|e| format!("{BUDGET_FILE} : TOML invalide : {e}"))?;
    if let Some(a) = doc
        .get_mut("scenarios")
        .and_then(|s| s.get_mut("missing"))
        .and_then(|x| x.as_array_mut())
    {
        a.retain(|v| v.as_str().is_none_or(|s| uncovered.contains(s)));
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests;
