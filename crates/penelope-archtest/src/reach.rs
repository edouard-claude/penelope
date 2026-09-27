//! Ce qu'une crate atteint d'une autre par les modules d'une troisième (épopée #208,
//! lot K).
//!
//! La règle de dépendances ne voit que les `Cargo.toml` : `penelope-agent` ne déclare pas
//! `penelope-context`, mais il l'atteignait par les réexports de `penelope_app::journal`.
//! Le graphe cargo, lui, ne peut pas être coupé tant que `penelope-app` porte `Services`
//! (et son `ContextEngine`) avec les ports. Ce module suit donc le chemin au grain des
//! modules : les modules de `penelope-app` que la boucle nomme, puis ceux qu'ils nomment
//! par `crate::`, jusqu'au point fixe ; aucun ne doit nommer `penelope_context`.

use crate::Crate;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Un module de premier niveau de `penelope-app` nommé par un chemin, et ce qu'il nomme.
fn named(cell: &'static OnceLock<Regex>, pattern: &str, raw: &str) -> BTreeSet<String> {
    let re = cell.get_or_init(|| Regex::new(pattern).expect("motif valide"));
    code(raw)
        .flat_map(|line| {
            re.captures_iter(line)
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Les lignes de code, tests compris : un test de la boucle ne doit pas plus atteindre
/// le moteur de contexte que son code.
fn code(raw: &str) -> impl Iterator<Item = &str> {
    raw.lines().filter(|l| !l.trim_start().starts_with("//"))
}

/// Les fichiers d'un module de premier niveau : `src/<m>.rs`, `src/<m>/mod.rs` et tout
/// ce qui est sous `src/<m>/`.
fn module_files(src: &Path, module: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let file = src.join(format!("{module}.rs"));
    if file.is_file() {
        out.push(file);
    }
    let dir = src.join(module);
    if dir.is_dir() {
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

fn read_all(files: &[PathBuf]) -> String {
    files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Les modules de `via` que `from` atteint (directement ou par `crate::`), chacun avec
/// ses lignes qui nomment `target` (`penelope_context`, par exemple). Vide : aucun
/// chemin.
pub fn leaks(from: &Crate, via: &Crate, target: &str) -> BTreeMap<String, Vec<String>> {
    static VIA: OnceLock<Regex> = OnceLock::new();
    static LOCAL: OnceLock<Regex> = OnceLock::new();
    let via_path = via.name.replace('-', "_");
    let from_raw = read_all(&crate::snapshot::all_sources(from));
    let pattern = format!(r"\b{via_path}::(\w+)");
    let mut todo: Vec<String> = named(&VIA, &pattern, &from_raw).into_iter().collect();
    let mut seen = BTreeSet::new();
    let mut out = BTreeMap::new();
    let src = via.dir.join("src");
    while let Some(m) = todo.pop() {
        if !seen.insert(m.clone()) {
            continue;
        }
        let files = module_files(&src, &m);
        if files.is_empty() {
            // Un item de la racine (`penelope_app::Services`) : `lib.rs` le réexporte.
            continue;
        }
        let raw = read_all(&files);
        let hits: Vec<String> = code(&raw)
            .filter(|l| l.contains(target))
            .map(|l| l.trim().to_string())
            .collect();
        if !hits.is_empty() {
            out.insert(m.clone(), hits);
        }
        todo.extend(named(&LOCAL, r"\bcrate::(\w+)", &raw));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(name: &str) -> Crate {
        crate::crates()
            .into_iter()
            .find(|c| c.name == name)
            .expect(name)
    }

    /// Lot K : la boucle ne voit aucun type de `penelope-context`, ni directement ni par
    /// les modules de `penelope-app` qu'elle nomme (ports, vocabulaire du journal), ni
    /// par ceux qu'ils nomment à leur tour. Les types purs sont dans le noyau.
    #[test]
    fn the_agent_crate_reaches_no_context_type_through_its_ports() {
        let agent = find("penelope-agent");
        let direct: Vec<String> = code(&read_all(&crate::snapshot::all_sources(&agent)))
            .filter(|l| l.contains("penelope_context"))
            .map(|l| l.trim().to_string())
            .collect();
        assert!(
            direct.is_empty(),
            "penelope-agent nomme penelope_context : {direct:?}"
        );
        let leaks = leaks(&agent, &find("penelope-app"), "penelope_context");
        assert!(
            leaks.is_empty(),
            "penelope-agent atteint penelope-context par ces modules de penelope-app \
             (faire descendre les types purs dans penelope_kernel::journal, ou les passer \
             par un port) : {leaks:#?}"
        );
    }

    /// Le chemin est bien suivi : le daemon nomme `penelope_app::journal`
    /// (`JournalAttempts`) et `penelope_app::services`, qui parlent au moteur de contexte.
    #[test]
    fn a_module_that_names_the_target_is_found() {
        let leaks = leaks(
            &find("penelope-daemon"),
            &find("penelope-app"),
            "penelope_context",
        );
        assert!(leaks.contains_key("journal"), "{leaks:?}");
        assert!(leaks.contains_key("services"), "{leaks:?}");
    }
}
