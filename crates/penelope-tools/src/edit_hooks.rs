//! Hooks après édition (#366) : le dépôt dit quelle commande relancer quand un fichier
//! change (format, lint, test ciblé), et l'erreur revient dans le résultat de l'édition au
//! lieu d'attendre l'étape de test.
//!
//! ```text
//!  fs_edit src/rapport.rs ──► .penelope/after-edit le plus proche, sans sortir du workspace
//!                               « cargo fmt -- {file} && cargo check -q »
//!                         ──► lancé dans le dossier qui porte `.penelope/`, résultat joint
//! ```
//!
//! Une seule ligne de commande (les lignes vides et `#` sont ignorées) ; `{file}` devient le
//! chemin du fichier édité, relatif à ce dossier et cité pour le shell. Le fichier vit avec
//! le dépôt, comme son CLAUDE.md : aucun réglage de Pénélope à tenir à jour par projet.

use std::path::{Path, PathBuf};

/// Le fichier du hook, relatif à la racine du projet.
pub const FILE: &str = ".penelope/after-edit";

/// Le hook qui s'applique à `path` : le dossier où le lancer et la commande prête. La
/// recherche remonte les parents de `path` sans quitter le workspace qui le contient.
pub fn after_edit_for(path: &Path, workspaces: &[PathBuf]) -> Option<(PathBuf, String)> {
    let root = workspaces.iter().find(|w| path.starts_with(w))?;
    let mut dir = path.parent()?;
    loop {
        if let Ok(raw) = std::fs::read_to_string(dir.join(FILE))
            && let Some(command) = first_command(&raw)
        {
            let relative = path.strip_prefix(dir).unwrap_or(path);
            let file = quote(&relative.to_string_lossy());
            return Some((dir.to_path_buf(), command.replace("{file}", &file)));
        }
        if dir == root.as_path() {
            return None;
        }
        dir = dir.parent()?;
    }
}

fn first_command(raw: &str) -> Option<String> {
    raw.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
}

/// Cite un argument pour `sh` : entre apostrophes, une apostrophe devient `'\''`.
pub fn quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_hook_inside_the_workspace_applies() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        let project = ws.join("atelier");
        std::fs::create_dir_all(project.join("src/cli")).unwrap();
        std::fs::create_dir_all(project.join(".penelope")).unwrap();
        std::fs::write(
            project.join(FILE),
            "# formatage puis vérification\n\ncargo fmt -- {file} && cargo check -q\n",
        )
        .unwrap();
        let edited = project.join("src/cli/rapport d'été.rs");
        let (run_in, command) = after_edit_for(&edited, std::slice::from_ref(&ws)).unwrap();
        assert_eq!(run_in, project);
        assert_eq!(
            command,
            r"cargo fmt -- 'src/cli/rapport d'\''été.rs' && cargo check -q"
        );

        // Hors de tout workspace, ou sans fichier jusqu'à la racine du workspace : rien.
        assert!(after_edit_for(&edited, &[dir.path().join("ailleurs")]).is_none());
        let other = ws.join("autre/src/a.rs");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        assert!(after_edit_for(&other, &[ws.clone()]).is_none());
        // Un hook au-dessus du workspace n'est pas lu.
        std::fs::create_dir_all(dir.path().join(".penelope")).unwrap();
        std::fs::write(dir.path().join(FILE), "echo dehors").unwrap();
        assert!(after_edit_for(&other, &[ws]).is_none());
    }
}
