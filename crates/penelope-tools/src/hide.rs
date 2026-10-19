//! Chemins masqués dans les dossiers accordés (#394) : des motifs comme `**/.env`,
//! `.git/config` ou `secrets/`, refusés aux outils de fichiers et, sous macOS, au bac à
//! sable du shell (règle Seatbelt `regex`). `deny_read` cache des chemins nommés ; ces
//! motifs cachent ce qui se trouve **dans** un workspace.

use std::path::Path;

/// Un motif de `sandbox.hide` en expression régulière, ancré sous `root` :
/// `**/` vaut n'importe quel nombre de dossiers, `*` un segment, un `/` final un dossier
/// et tout ce qu'il contient.
pub fn pattern_regex(root: &Path, pattern: &str) -> String {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    let (pattern, dir) = match pattern.strip_suffix('/') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let mut body = String::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("**/") {
            body.push_str("(.*/)?");
            rest = r;
        } else if let Some(r) = rest.strip_prefix('*') {
            body.push_str("[^/]*");
            rest = r;
        } else {
            let c = rest.chars().next().unwrap_or_default();
            if "\\.+?()[]{}|^$".contains(c) {
                body.push('\\');
            }
            body.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    let mut escaped_root = String::new();
    for c in root.chars() {
        if "\\.+?()[]{}|^$*".contains(c) {
            escaped_root.push('\\');
        }
        escaped_root.push(c);
    }
    format!(
        "^{escaped_root}/{body}{}",
        if dir { "(/.*)?$" } else { "$" }
    )
}

/// Chaque racine sous sa forme donnée et sous sa forme réelle (liens résolus, `/var` →
/// `/private/var` sous macOS) : un chemin résolu ne doit pas échapper au motif.
fn both_forms(roots: &[impl AsRef<Path>]) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for r in roots {
        let raw = r.as_ref().to_path_buf();
        if let Ok(real) = std::fs::canonicalize(&raw)
            && !out.contains(&real)
        {
            out.push(real);
        }
        if !out.contains(&raw) {
            out.push(raw);
        }
    }
    out
}

/// Les expressions de tous les motifs sous tous les dossiers accordés.
pub fn regexes(roots: &[impl AsRef<Path>], patterns: &[String]) -> Vec<String> {
    both_forms(roots)
        .iter()
        .flat_map(|r| patterns.iter().map(move |p| pattern_regex(r, p)))
        .collect()
}

/// Le motif qui masque ce chemin, s'il y en a un.
pub fn hidden(path: &Path, roots: &[impl AsRef<Path>], patterns: &[String]) -> Option<String> {
    let path = path.to_string_lossy();
    for root in both_forms(roots) {
        for p in patterns {
            let re = pattern_regex(&root, p);
            if regex::Regex::new(&re).is_ok_and(|r| r.is_match(&path)) {
                return Some(p.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn patterns_hide_inside_a_granted_folder() {
        let roots = [PathBuf::from("/ws/app")];
        let pats: Vec<String> = ["**/.env", "**/.env.*", ".git/config", "secrets/", "*.pem"]
            .map(String::from)
            .to_vec();
        for (path, hit) in [
            ("/ws/app/.env", Some("**/.env")),
            ("/ws/app/api/config/.env", Some("**/.env")),
            ("/ws/app/.env.local", Some("**/.env.*")),
            ("/ws/app/.git/config", Some(".git/config")),
            ("/ws/app/secrets", Some("secrets/")),
            ("/ws/app/secrets/prod/key.json", Some("secrets/")),
            ("/ws/app/server.pem", Some("*.pem")),
            ("/ws/app/src/main.rs", None),
            ("/ws/app/.envrc", None),
            ("/ws/app/docs/secrets.md", None),
            ("/ailleurs/.env", None),
        ] {
            assert_eq!(
                hidden(Path::new(path), &roots, &pats).as_deref(),
                hit,
                "{path}"
            );
        }
        assert_eq!(
            pattern_regex(Path::new("/ws/a.b"), "**/.env"),
            "^/ws/a\\.b/(.*/)?\\.env$"
        );
    }
}
