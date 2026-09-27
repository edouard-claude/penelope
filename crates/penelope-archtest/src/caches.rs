//! Les caches de la conversation ne s'écrivent que dans `penelope-context` (épopée #208,
//! T16 ; `design/v1/source-de-verite.md` §4.4).
//!
//! La conversation vit dans le journal d'événements ; `messages`, `messages_fts`,
//! `message_context`, `lcm_nodes`, `lcm_edges`, `prompt_snapshots` et
//! `projections_session` en sont des caches, que le projecteur écrit dans la seconde
//! transaction de chaque événement et que `penelope history reindex` refait. Une écriture
//! SQL sur ces tables ailleurs rouvrirait le chemin direct de la 0.17 : une ligne que le
//! journal ne connaît pas, que `verify` signalerait et que `reindex` effacerait.
//!
//! La règle lit le code hors commentaires et hors tests (fichiers de tests, et ce qui suit
//! un `#[cfg(test)]`) : un test fabrique une base 0.17 ou altère un cache pour prouver
//! qu'il est vu. Un `INSERT`, `REPLACE`, `UPDATE` ou `DELETE` qui nomme une de ces tables,
//! sur une ou plusieurs lignes, est une violation.

use crate::{Violation, crates, snapshot, sources};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

/// Les tables que le projecteur tient depuis le journal.
pub const CACHE_TABLES: &[&str] = &[
    "messages",
    "messages_fts",
    "message_context",
    "lcm_nodes",
    "lcm_edges",
    "prompt_snapshots",
    "projections_session",
];

/// La seule crate qui écrit ces tables : le projecteur et les écritures journalisées de
/// `HistoryStore` et `ContextEngine`.
pub const CACHE_WRITER: &str = "penelope-context";

/// Fichiers hors de `penelope-context` autorisés à écrire un cache, avec la raison.
pub const CACHE_WRITE_EXEMPT: &[(&str, &str)] = &[(
    "crates/penelope-evals/src/scenario/harness/journal.rs",
    "le harnais des scénarios travaille sur une base temporaire : il efface les caches \
     pour prouver que `reindex` les redonne à l'identique (CA 4.6)",
)];

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let tables = CACHE_TABLES.join("|");
        Regex::new(&format!(
            r"(?i)\b(?:INSERT(?:\s+OR\s+\w+)?\s+INTO|REPLACE\s+INTO|UPDATE(?:\s+OR\s+\w+)?|DELETE\s+FROM)\s+({tables})\b"
        ))
        .unwrap_or_else(|e| panic!("motif des caches invalide : {e}"))
    })
}

/// Les écritures de cache hors de `penelope-context`, dans tout le workspace.
pub fn cache_write_violations() -> Vec<Violation> {
    let mut out = Vec::new();
    for c in crates() {
        for file in sources(&c) {
            let Ok(raw) = std::fs::read_to_string(&file) else {
                continue;
            };
            out.extend(cache_writes_in(&c.name, &file, &raw));
        }
    }
    out
}

/// Les écritures de cache d'un fichier. Commentaires et tests sont blanchis (lignes
/// vides, pour garder les numéros) avant la recherche, qui franchit les retours à la
/// ligne d'une requête écrite sur plusieurs lignes.
pub fn cache_writes_in(crate_name: &str, file: &Path, raw: &str) -> Vec<Violation> {
    let rel = snapshot::relative_to_root(file);
    if crate_name == CACHE_WRITER
        || snapshot::is_test_path(&rel)
        || CACHE_WRITE_EXEMPT.iter().any(|(path, _)| *path == rel)
    {
        return Vec::new();
    }
    let mut code = String::with_capacity(raw.len());
    for line in raw.lines() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        if !line.trim_start().starts_with("//") {
            code.push_str(line);
        }
        code.push('\n');
    }
    pattern()
        .find_iter(&code)
        .map(|m| {
            let line = code[..m.start()].matches('\n').count();
            Violation {
                crate_name: crate_name.to_string(),
                file: file.to_path_buf(),
                line: line + 1,
                rule: "écriture d'un cache de la conversation hors de penelope-context",
                text: raw.lines().nth(line).unwrap_or_default().to_string(),
            }
        })
        .collect()
}
