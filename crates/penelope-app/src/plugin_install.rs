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

pub fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
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

/// Copie `source` (dossier local, ou dépôt git cloné dans `tmp` puis recopié) vers
/// `dest`, sans `.git` ni lien symbolique. `dest` est effacé si la copie échoue.
pub fn fetch(source: &str, dest: &Path, tmp: &Path) -> Result<(), String> {
    fetch_with(source, dest, tmp, false)
}

/// [`fetch`] pour une source qu'un tiers a écrite (une marketplace) : un dépôt distant
/// seulement, `https://` ou `git@`, jamais un chemin ni le protocole `file`.
pub fn fetch_remote(source: &str, dest: &Path, tmp: &Path) -> Result<(), String> {
    if !(source.starts_with("https://") || source.starts_with("git@")) {
        return Err(format!(
            "source `{source}` : un dépôt distant (https:// ou git@) attendu"
        ));
    }
    fetch_with(source, dest, tmp, true)
}

fn fetch_with(source: &str, dest: &Path, tmp: &Path, remote_only: bool) -> Result<(), String> {
    let local = Path::new(source);
    if !remote_only && local.is_dir() {
        return copy_dir(local, dest).map_err(|e| {
            let _ = std::fs::remove_dir_all(dest);
            e.to_string()
        });
    }
    if !is_git(source) {
        return Err(format!(
            "source `{source}` : ni dossier, ni dépôt git (https://, git@, .git)"
        ));
    }
    if source.starts_with('-') {
        return Err("source refusée".into());
    }
    // Cloné à part, puis recopié par la même règle qu'un dossier : un lien symbolique du
    // dépôt (vers ~/.ssh, par exemple) n'arrive jamais à destination.
    let git = penelope_platform::which("git").ok_or("`git` est absent du PATH")?;
    let _ = std::fs::remove_dir_all(tmp);
    let out = std::process::Command::new(git)
        .args([
            "-c",
            "protocol.file.allow=never",
            "clone",
            "--depth",
            "1",
            "--",
            source,
        ])
        .arg(tmp)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| e.to_string());
    let copied = match out {
        Ok(o) if o.status.success() => copy_dir(tmp, dest).map_err(|e| e.to_string()),
        Ok(o) => Err(format!(
            "git clone : {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_dir_all(tmp);
    if copied.is_err() {
        let _ = std::fs::remove_dir_all(dest);
    }
    copied
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
    fetch(
        source,
        &dest,
        &data.join("plugins").join(format!(".clone-{name}")),
    )?;
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

    #[cfg(unix)]
    #[test]
    fn a_symlink_never_enters_the_plugin() {
        let src = tempfile::tempdir().unwrap();
        let root = src.path().join("piege");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(src.path().join("secret"), "S").unwrap();
        std::os::unix::fs::symlink(src.path().join("secret"), root.join("lien")).unwrap();
        std::fs::write(root.join("ok.md"), "x").unwrap();
        let data = tempfile::tempdir().unwrap();
        let dest = add(data.path(), root.to_str().unwrap(), None).unwrap();
        assert!(dest.join("ok.md").exists());
        assert!(std::fs::symlink_metadata(dest.join("lien")).is_err());
    }

    #[test]
    fn a_name_comes_from_the_last_segment() {
        assert_eq!(name_of("https://github.com/acme/revue-pr.git"), "revue-pr");
        assert_eq!(name_of("git@github.com:acme/outils.git"), "outils");
        assert_eq!(name_of("/plugins/notes/"), "notes");
    }
}
