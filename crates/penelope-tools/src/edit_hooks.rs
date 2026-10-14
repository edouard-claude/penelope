//! Hooks après édition (#366) : la commande que le propriétaire a donnée au lancement
//! d'une mission (`--hook`) est relancée quand un fichier change (format, lint, test
//! ciblé), et son erreur revient dans le résultat de l'édition au lieu d'attendre l'étape
//! de test.
//!
//! ```text
//!  /mission … --hook "cargo fmt -- {file} && cargo check -q"
//!  fs_edit src/rapport.rs ──► commande rendue (`{file}` cité) ──► lancée dans le dépôt
//! ```
//!
//! La commande vient du propriétaire, jamais d'un fichier du dépôt : l'agent pourrait
//! écrire ce fichier, et sa commande échapperait à la politique du shell. `{file}` n'est
//! remplacé que par un chemin sans caractère spécial du shell : cité ou non, entre
//! guillemets doubles ou pas, il ne peut rien exécuter.

/// La commande du hook pour un fichier édité (`relative` : son chemin depuis le dossier
/// où le hook tourne) ; `None` si ce chemin porte un caractère que le shell interprète.
pub fn render(command: &str, relative: &str) -> Option<String> {
    if !command.contains("{file}") {
        return Some(command.to_string());
    }
    safe_path(relative).then(|| command.replace("{file}", &quote(relative)))
}

/// Un chemin sans rien que `sh` interprète, même entre guillemets doubles.
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('-')
        && path.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@' | ',' | ' ')
        })
}

/// Cite un argument pour `sh` : entre apostrophes, une apostrophe devient `'\''`.
pub fn quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_safe_path_is_quoted_and_a_shell_character_refuses_the_hook() {
        assert_eq!(
            render(
                "cargo fmt -- {file} && cargo check -q",
                "src/cli/rapport été.rs"
            )
            .as_deref(),
            Some("cargo fmt -- 'src/cli/rapport été.rs' && cargo check -q")
        );
        assert_eq!(render("make lint", "a.rs").as_deref(), Some("make lint"));
        for evil in [
            "$(touch x).rs",
            "a`id`.rs",
            "a\".rs",
            "a'b.rs",
            "a;b.rs",
            "-rf",
            "a\nb",
            "",
        ] {
            assert!(render("echo \"{file}\"", evil).is_none(), "{evil:?}");
        }
    }
}
