//! Déposer et retirer un plugin (#393) : `penelope plugin add <source>` copie un dossier
//! ou clone un dépôt git dans `{data}/plugins/<nom>`, `remove` l'efface. Déposer n'active
//! rien qui exécute : ses serveurs MCP et ses hooks attendent la politique du propriétaire
//! (`plugins.allowed_mcp_servers`, `plugins.allowed_hooks`) ; ses skills et ses
//! sous-agents sont lus comme ceux de `{data}/skills` et `{data}/agents`.

use std::path::{Path, PathBuf};

/// Fichier qui garde l'origine d'un plugin déposé.
pub const SOURCE_FILE: &str = ".penelope-source";

/// Le nom tiré d'une source : son dernier segment, sans `.git`.
pub fn name_of(source: &str) -> String {
    source
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_lowercase()
}

fn is_git(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("git@") || source.ends_with(".git")
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        if name == ".git" {
            continue;
        }
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_dir(&e.path(), &to.join(&name))?;
        } else if ty.is_file() {
            std::fs::copy(e.path(), to.join(&name))?;
        }
        // Un lien symbolique n'est pas suivi : il pourrait viser hors du plugin.
    }
    Ok(())
}

/// Dépose `source` (dossier local ou dépôt git) sous `{data}/plugins/<nom>` ; rend le
/// chemin du plugin.
pub fn add(data: &Path, source: &str, name: Option<&str>) -> Result<PathBuf, String> {
    let name = name.map(str::to_string).unwrap_or_else(|| name_of(source));
    penelope_platform::validate_slug(&name).map_err(|_| {
        format!("nom de plugin `{name}` : lettres, chiffres, `-` et `_` (`--name`)")
    })?;
    let dest = data.join("plugins").join(&name);
    if dest.exists() {
        return Err(format!(
            "le plugin `{name}` existe déjà : `penelope plugin remove {name}` d'abord"
        ));
    }
    std::fs::create_dir_all(data.join("plugins")).map_err(|e| e.to_string())?;
    let local = Path::new(source);
    if local.is_dir() {
        copy_dir(local, &dest).map_err(|e| e.to_string())?;
    } else if is_git(source) {
        if source.starts_with('-') {
            return Err("source refusée".into());
        }
        let git = penelope_platform::which("git").ok_or("`git` est absent du PATH")?;
        let out = std::process::Command::new(git)
            .args(["clone", "--depth", "1", "--", source])
            .arg(&dest)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(format!(
                "git clone : {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let _ = std::fs::remove_dir_all(dest.join(".git"));
    } else {
        return Err(format!(
            "source `{source}` : ni dossier, ni dépôt git (https://, git@, .git)"
        ));
    }
    std::fs::write(dest.join(SOURCE_FILE), source).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Retire le plugin `name`.
pub fn remove(data: &Path, name: &str) -> Result<(), String> {
    penelope_platform::validate_slug(name)
        .map_err(|_| format!("nom de plugin `{name}` invalide"))?;
    let dir = data.join("plugins").join(name);
    if !dir.is_dir() {
        return Err(format!("aucun plugin `{name}`"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_folder_is_copied_named_and_removed() {
        let src = tempfile::tempdir().unwrap();
        let root = src.path().join("Revue-PR");
        std::fs::create_dir_all(root.join("skills/x")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join("skills/x/SKILL.md"),
            "---\nname: x\ndescription: d\n---\nb\n",
        )
        .unwrap();
        let data = tempfile::tempdir().unwrap();
        let dest = add(data.path(), root.to_str().unwrap(), None).unwrap();
        assert_eq!(dest, data.path().join("plugins/revue-pr"));
        assert!(dest.join("skills/x/SKILL.md").exists());
        assert!(!dest.join(".git").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join(SOURCE_FILE)).unwrap(),
            root.to_str().unwrap()
        );
        assert!(
            add(data.path(), root.to_str().unwrap(), None)
                .unwrap_err()
                .contains("existe déjà")
        );
        assert!(add(data.path(), "rien", None).is_err());
        assert!(add(data.path(), root.to_str().unwrap(), Some("../x")).is_err());
        remove(data.path(), "revue-pr").unwrap();
        assert!(!dest.exists());
        assert!(remove(data.path(), "..").is_err());
    }

    #[test]
    fn a_name_comes_from_the_last_segment() {
        assert_eq!(name_of("https://github.com/acme/revue-pr.git"), "revue-pr");
        assert_eq!(name_of("git@github.com:acme/outils.git"), "outils");
        assert_eq!(name_of("/plugins/notes/"), "notes");
    }
}
