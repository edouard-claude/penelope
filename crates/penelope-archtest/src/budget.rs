//! `budget.toml` : la liste de référence du gel de la dette (#209, #210, #211, #213, #214).
//!
//! Un test compare l'état courant au fichier ; `UPDATE_BUDGET=1 cargo test -p
//! penelope-archtest` réécrit le fichier **vers le bas uniquement** (`tighten`), sur le
//! modèle d'`UPDATE_CA_MATRIX` côté `penelope-evals`. Le remonter demande le trailer
//! « Dérogation-budget: #N » (`scripts/check-budget.sh`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::freeze::measure;
use crate::snapshot::workspace_snapshot;

/// Chemin du budget, relatif à la racine du dépôt.
pub const BUDGET_FILE: &str = "crates/penelope-archtest/budget.toml";

/// Le budget lu depuis `budget.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Budget {
    /// `[files].ceiling` : plafond d'un fichier source, tests inline compris (R1).
    pub ceiling: usize,
    /// `[files].test_ceiling` : plafond d'un fichier de tests (R2).
    pub test_ceiling: usize,
    /// `[files].test_modules` : modules de `src/` entièrement `#[cfg(test)]`.
    pub test_modules: BTreeSet<String>,
    /// `[files.oversized]` : fichiers autorisés à dépasser, chacun avec sa borne (R3).
    pub oversized: BTreeMap<String, usize>,
    /// `[crates]` : plafond de lignes de `src/` par crate (R4).
    pub crates: BTreeMap<String, usize>,
    /// `[daemon].modules` : liste blanche des modules du daemon (R5).
    pub daemon_modules: BTreeSet<String>,
    /// `[daemon].impl_daemon` : fichiers où un bloc `impl Daemon` est admis (R6).
    pub impl_daemon: BTreeSet<String>,
    /// `[daemon.daemon_users]` : occurrences du type `Daemon` admises par fichier (R6).
    pub daemon_users: BTreeMap<String, usize>,
    /// `[lints].allow_too_many_lines` : nombre d'allows admis (R7).
    pub allow_too_many_lines: usize,
    /// `[ca].required` : critères d'acceptation qui doivent exister (R8).
    pub ca_required: BTreeSet<String>,
    /// `[channel.allowed]` : mentions du canal admises par fichier (R8 du découpage).
    pub channel_allowed: BTreeMap<String, usize>,
}

impl Budget {
    pub fn path() -> PathBuf {
        crate::workspace_root().join(BUDGET_FILE)
    }

    /// Lit `budget.toml`. Si `UPDATE_BUDGET` est posé, le resserre d'abord (une fois par
    /// processus) : chaque test de règle lit ainsi le fichier déjà réécrit, quel que soit
    /// l'ordre d'exécution.
    pub fn load() -> Result<Self, String> {
        static UPDATED: OnceLock<Result<(), String>> = OnceLock::new();
        if std::env::var_os("UPDATE_BUDGET").is_some() {
            UPDATED.get_or_init(update_file).clone()?;
        }
        let raw =
            std::fs::read_to_string(Self::path()).map_err(|e| format!("{BUDGET_FILE} : {e}"))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let doc: toml::Value = raw
            .parse()
            .map_err(|e| format!("{BUDGET_FILE} : TOML invalide : {e}"))?;
        let files = table(&doc, "files")?;
        let daemon = table(&doc, "daemon")?;
        Ok(Self {
            ceiling: int(files, "ceiling", "[files]")?,
            test_ceiling: int(files, "test_ceiling", "[files]")?,
            test_modules: str_list(files, "test_modules", "[files]")?,
            oversized: int_map(table(files, "oversized")?, "[files.oversized]")?,
            crates: int_map(table(&doc, "crates")?, "[crates]")?,
            daemon_modules: str_list(daemon, "modules", "[daemon]")?,
            impl_daemon: str_list(daemon, "impl_daemon", "[daemon]")?,
            daemon_users: int_map(table(daemon, "daemon_users")?, "[daemon.daemon_users]")?,
            allow_too_many_lines: int(table(&doc, "lints")?, "allow_too_many_lines", "[lints]")?,
            ca_required: str_list(table(&doc, "ca")?, "required", "[ca]")?,
            channel_allowed: int_map(
                table(table(&doc, "channel")?, "allowed")?,
                "[channel.allowed]",
            )?,
        })
    }
}

fn table<'a>(v: &'a toml::Value, key: &str) -> Result<&'a toml::Value, String> {
    v.get(key)
        .filter(|t| t.is_table())
        .ok_or_else(|| format!("{BUDGET_FILE} : table « {key} » absente"))
}

fn int(t: &toml::Value, key: &str, section: &str) -> Result<usize, String> {
    t.get(key)
        .and_then(|v| v.as_integer())
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("{BUDGET_FILE} {section} : entier « {key} » absent"))
}

fn str_list(t: &toml::Value, key: &str, section: &str) -> Result<BTreeSet<String>, String> {
    let arr = t
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{BUDGET_FILE} {section} : liste « {key} » absente"))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{BUDGET_FILE} {section}.{key} : chaîne attendue"))
        })
        .collect()
}

fn int_map(t: &toml::Value, section: &str) -> Result<BTreeMap<String, usize>, String> {
    let Some(t) = t.as_table() else {
        return Err(format!("{BUDGET_FILE} {section} : table attendue"));
    };
    t.iter()
        .map(|(k, v)| {
            v.as_integer()
                .and_then(|n| usize::try_from(n).ok())
                .map(|n| (k.clone(), n))
                .ok_or_else(|| format!("{BUDGET_FILE} {section} : « {k} » doit être un entier"))
        })
        .collect()
}

/// L'état courant du workspace, tel que `tighten` le confronte au budget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Measures {
    /// Fichiers au-dessus de leur plafond, avec leur taille.
    pub oversized: BTreeMap<String, usize>,
    /// Modules déclarés (`mod x;`) dans le daemon, hors `tests`.
    pub daemon_modules: BTreeSet<String>,
    /// Fichiers du daemon portant un bloc `impl Daemon`.
    pub impl_daemon: BTreeSet<String>,
    /// Occurrences du type `Daemon` par fichier du daemon (seulement les comptes > 0).
    pub daemon_users: BTreeMap<String, usize>,
    /// `#[allow(clippy::too_many_lines)]` dans les sources.
    pub allow_too_many_lines: usize,
    /// Critères d'acceptation présents dans les sources.
    pub ca_present: BTreeSet<String>,
    /// Mentions du canal par fichier des crates agnostiques (comptes > 0).
    pub channel: BTreeMap<String, usize>,
}

/// Resserre `raw` (le contenu de `budget.toml`) sur `m`, vers le bas seulement :
/// chaque entrée descend à `min(inscrit, courant)`, une entrée passée sous son plafond
/// disparaît, un critère d'acceptation nouveau est ajouté ; rien ne monte jamais, et
/// aucune entrée n'est ajoutée aux listes de référence. Les commentaires du fichier sont
/// conservés (c'est pour eux que la réécriture passe par `toml_edit`).
pub fn tighten(raw: &str, m: &Measures) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|e| format!("{BUDGET_FILE} : TOML invalide : {e}"))?;

    if let Some(t) = doc
        .get_mut("files")
        .and_then(|f| f.get_mut("oversized"))
        .and_then(|o| o.as_table_mut())
    {
        lower_table(t, &m.oversized);
    }
    if let Some(d) = doc.get_mut("daemon") {
        if let Some(a) = d.get_mut("modules").and_then(|x| x.as_array_mut()) {
            retain_array(a, &m.daemon_modules);
        }
        if let Some(a) = d.get_mut("impl_daemon").and_then(|x| x.as_array_mut()) {
            retain_array(a, &m.impl_daemon);
        }
        if let Some(t) = d.get_mut("daemon_users").and_then(|x| x.as_table_mut()) {
            lower_table(t, &m.daemon_users);
        }
    }
    if let Some(i) = doc
        .get_mut("lints")
        .and_then(|l| l.get_mut("allow_too_many_lines"))
    {
        lower_item(i, m.allow_too_many_lines);
    }
    if let Some(a) = doc
        .get_mut("ca")
        .and_then(|c| c.get_mut("required"))
        .and_then(|x| x.as_array_mut())
    {
        extend_sorted_array(a, &m.ca_present);
    }
    if let Some(t) = doc
        .get_mut("channel")
        .and_then(|c| c.get_mut("allowed"))
        .and_then(|x| x.as_table_mut())
    {
        lower_table(t, &m.channel);
    }
    Ok(doc.to_string())
}

/// Abaisse chaque entrée à la valeur courante si elle est plus basse ; retire les entrées
/// dont la valeur courante est nulle ou absente. N'ajoute jamais d'entrée.
fn lower_table(t: &mut toml_edit::Table, current: &BTreeMap<String, usize>) {
    let keys: Vec<String> = t.iter().map(|(k, _)| k.to_string()).collect();
    for k in keys {
        match current.get(&k) {
            None | Some(0) => {
                t.remove(&k);
            }
            Some(&cur) => {
                if let Some(item) = t.get_mut(&k) {
                    lower_item(item, cur);
                }
            }
        }
    }
}

/// Remplace un entier par `new` s'il est plus bas, en gardant le commentaire de la ligne.
fn lower_item(item: &mut toml_edit::Item, new: usize) {
    if let Some(toml_edit::Value::Integer(f)) = item.as_value_mut() {
        let old = usize::try_from(*f.value()).unwrap_or(0);
        if new < old {
            let decor = f.decor().clone();
            *f = toml_edit::Formatted::new(i64::try_from(new).unwrap_or(i64::MAX));
            *f.decor_mut() = decor;
        }
    }
}

fn retain_array(a: &mut toml_edit::Array, keep: &BTreeSet<String>) {
    a.retain(|v| v.as_str().is_none_or(|s| keep.contains(s)));
}

/// Ajoute à la liste les noms de `all` qui n'y sont pas, en la réécrivant triée, un nom
/// par ligne. Ne retire rien.
fn extend_sorted_array(a: &mut toml_edit::Array, all: &BTreeSet<String>) {
    let present: BTreeSet<String> = a
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if all.is_subset(&present) {
        return;
    }
    let union: BTreeSet<&String> = present.iter().chain(all.iter()).collect();
    a.clear();
    for name in union {
        a.push_formatted(toml_edit::Value::from(name.as_str()).decorated("\n    ", ""));
    }
    a.set_trailing_comma(true);
    a.set_trailing("\n");
}

fn update_file() -> Result<(), String> {
    let path = Budget::path();
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{BUDGET_FILE} : {e}"))?;
    let budget = Budget::parse(&raw)?;
    let m = measure(workspace_snapshot(), &budget);
    let new = tighten(&raw, &m)?;
    let cov = crate::scenarios::workspace_coverage(workspace_snapshot())?;
    let new = crate::scenarios::tighten(&new, &cov.uncovered)?;
    if new != raw {
        std::fs::write(&path, new).map_err(|e| format!("{BUDGET_FILE} : {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# en-tête conservé
[files]
ceiling = 1000
test_ceiling = 1500
test_modules = ["crates/x/src/e2e.rs"]

[files.oversized]
"crates/x/src/big.rs" = 1200
"crates/x/src/gone.rs" = 1100 # ce commentaire disparaît avec l'entrée
"crates/x/src/small.rs" = 1050

[crates]
"penelope-daemon" = 80000

[daemon]
modules = ["alpha", "beta"]
impl_daemon = ["runtime.rs", "engine.rs"]

[daemon.daemon_users]
"a.rs" = 5
"b.rs" = 2

[lints]
allow_too_many_lines = 3

[ca]
required = [
    "ca_1_1_first",
]

[channel.allowed]
"crates/x/src/a.rs" = 4 # permanent : raison
"crates/x/src/b.rs" = 1
"#;

    fn measures() -> Measures {
        Measures {
            oversized: [("crates/x/src/big.rs".to_string(), 1150)].into(),
            daemon_modules: ["alpha".to_string()].into(),
            impl_daemon: ["runtime.rs".to_string()].into(),
            daemon_users: [("a.rs".to_string(), 9), ("c.rs".to_string(), 1)].into(),
            allow_too_many_lines: 2,
            ca_present: ["ca_1_1_first".to_string(), "ca_0_9_added".to_string()].into(),
            channel: [("crates/x/src/a.rs".to_string(), 3)].into(),
        }
    }

    #[test]
    fn the_sample_budget_parses() {
        let b = Budget::parse(SAMPLE).unwrap();
        assert_eq!(b.ceiling, 1000);
        assert_eq!(b.oversized["crates/x/src/big.rs"], 1200);
        assert_eq!(b.daemon_users["a.rs"], 5);
        assert!(b.ca_required.contains("ca_1_1_first"));
        assert_eq!(b.channel_allowed["crates/x/src/a.rs"], 4);
        assert!(b.test_modules.contains("crates/x/src/e2e.rs"));
    }

    #[test]
    fn a_missing_section_is_named() {
        let err = Budget::parse("[files]\nceiling = 1\n[daemon]\n").unwrap_err();
        assert!(err.contains("« test_ceiling » absent"), "{err}");
        let err = Budget::parse("[files]\nceiling = 1\n").unwrap_err();
        assert!(err.contains("« daemon » absente"), "{err}");
    }

    #[test]
    fn tightening_only_goes_down() {
        let out = tighten(SAMPLE, &measures()).unwrap();
        let b = Budget::parse(&out).unwrap();
        // Abaissé, jamais remonté.
        assert_eq!(b.oversized["crates/x/src/big.rs"], 1150);
        assert_eq!(b.daemon_users["a.rs"], 5, "9 > 5 : l'entrée ne monte pas");
        assert_eq!(b.allow_too_many_lines, 2);
        assert_eq!(b.channel_allowed["crates/x/src/a.rs"], 3);
        // Les entrées passées sous le plafond ou à zéro disparaissent.
        assert!(!b.oversized.contains_key("crates/x/src/gone.rs"));
        assert!(!b.oversized.contains_key("crates/x/src/small.rs"));
        assert!(!b.daemon_users.contains_key("b.rs"));
        assert!(!b.channel_allowed.contains_key("crates/x/src/b.rs"));
        // Aucune entrée nouvelle dans les listes de référence.
        assert!(!b.daemon_users.contains_key("c.rs"));
        // Les listes blanches raccourcissent, les critères s'ajoutent.
        assert_eq!(b.daemon_modules, ["alpha".to_string()].into());
        assert_eq!(b.impl_daemon, ["runtime.rs".to_string()].into());
        assert!(b.ca_required.contains("ca_0_9_added"));
        assert!(b.ca_required.contains("ca_1_1_first"));
        // Ce qui n'est pas mesuré ne bouge pas.
        assert_eq!(b.crates["penelope-daemon"], 80000);
        assert_eq!(b.ceiling, 1000);
    }

    #[test]
    fn tightening_keeps_comments_and_formatting() {
        let out = tighten(SAMPLE, &measures()).unwrap();
        assert!(out.starts_with("# en-tête conservé\n"));
        assert!(
            out.contains("\"crates/x/src/a.rs\" = 3 # permanent : raison"),
            "{out}"
        );
        assert!(out.contains("required = [\n    \"ca_0_9_added\",\n    \"ca_1_1_first\",\n]"));
        assert!(!out.contains("ce commentaire disparaît"));
    }

    #[test]
    fn tightening_an_up_to_date_budget_changes_nothing() {
        let raw = tighten(SAMPLE, &measures()).unwrap();
        let again = tighten(&raw, &measures()).unwrap();
        assert_eq!(raw, again);
    }
}
