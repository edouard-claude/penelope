//! Un worktree git par mission (#397) : deux missions sur le même dépôt ne se marchent
//! plus dessus. Le worktree vit dans le dépôt, sous `.penelope/worktrees/<mission>` (donc
//! dans le même workspace autorisé), sur une branche `penelope/<mission>` ; il est exclu du
//! dépôt par `.git/info/exclude`. À la fin de la mission, il est retiré s'il est propre ;
//! la branche et ses commits restent, pour la PR.

use crate::services::Services;
use std::path::{Path, PathBuf};

/// Dossier des worktrees de missions, relatif au dépôt.
pub const DIR: &str = ".penelope/worktrees";

/// Le nom court d'une mission : les huit derniers caractères de sa session.
pub fn short(session_id: &str) -> String {
    let id: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    id[id.len().saturating_sub(8)..].to_string()
}

async fn git(s: &Services, dir: &Path, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let out = penelope_tools::shell::exec_argv(
        &s.platform.processes,
        "git",
        &args,
        penelope_tools::shell::ExecOptions {
            cwd: Some(dir),
            timeout: std::time::Duration::from_secs(60),
            max_output_bytes: 64 * 1024,
            ..Default::default()
        },
    )
    .await
    .map_err(|e| format!("git {} : {e}", args.join(" ")))?;
    if out.exit_code == 0 {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {} : code {} {}",
            args.join(" "),
            out.exit_code,
            out.stderr.trim()
        ))
    }
}

/// Ouvre le worktree d'une mission dans le dépôt `repo` : rend son chemin et sa branche.
pub async fn open(
    s: &Services,
    repo: &Path,
    session_id: &str,
) -> Result<(PathBuf, String), String> {
    let top = git(s, repo, &["rev-parse", "--show-toplevel"]).await?;
    // Le dépôt entier doit être dans un workspace autorisé : un dossier de mission au
    // fond d'un dépôt plus large ne doit pas ouvrir un worktree hors du périmètre.
    let top = crate::mission::dir_of(s, top.trim()).ok_or_else(|| {
        format!(
            "la racine du dépôt `{}` n'est dans aucun workspace autorisé",
            top.trim()
        )
    })?;
    let name = short(session_id);
    let branch = format!("penelope/mission-{name}");
    let git_dir = git(s, &top, &["rev-parse", "--git-common-dir"]).await?;
    exclude_worktrees(s, &top.join(git_dir.trim()));
    // Le dossier parent est créé et vérifié **avant** git : un lien posé dans `.penelope`
    // ne peut pas envoyer le worktree hors du périmètre.
    let parent = top.join(DIR);
    std::fs::create_dir_all(&parent).map_err(|e| format!("{} : {e}", parent.display()))?;
    let parent = crate::mission::dir_of(s, &parent.to_string_lossy())
        .ok_or_else(|| format!("`{}` sort des workspaces autorisés", parent.display()))?;
    let p = parent.join(&name).to_string_lossy().to_string();
    git(s, &top, &["worktree", "add", "-b", &branch, &p, "HEAD"]).await?;
    match crate::mission::dir_of(s, &p) {
        Some(path) => Ok((path, branch)),
        None => {
            let _ = git(s, &top, &["worktree", "remove", "--force", &p]).await;
            let _ = git(s, &top, &["branch", "-D", &branch]).await;
            Err(format!(
                "le worktree `{p}` n'est dans aucun workspace autorisé"
            ))
        }
    }
}

/// Ajoute le dossier des worktrees à `.git/info/exclude`, seulement si ce fichier est
/// dans le périmètre et n'est pas un lien.
fn exclude_worktrees(s: &Services, git_dir: &Path) {
    let Some(git_dir) = crate::mission::dir_of(s, &git_dir.to_string_lossy()) else {
        return;
    };
    let info = git_dir.join("info");
    if std::fs::create_dir_all(&info).is_err()
        || crate::mission::dir_of(s, &info.to_string_lossy()).as_deref() != Some(info.as_path())
    {
        return;
    }
    let exclude = info.join("exclude");
    if std::fs::symlink_metadata(&exclude).is_ok_and(|m| m.file_type().is_symlink()) {
        return;
    }
    let line = format!("/{DIR}/");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    if current.lines().any(|l| l.trim() == line) {
        return;
    }
    let sep = if current.ends_with('\n') || current.is_empty() {
        ""
    } else {
        "\n"
    };
    let _ = std::fs::write(&exclude, format!("{current}{sep}{line}\n"));
}

/// Retire le worktree d'une mission finie s'il est propre ; sinon le garde, et le dit.
pub async fn release(s: &Services, repo: &Path, path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok("worktree déjà retiré".into());
    }
    let dirty = git(s, path, &["status", "--porcelain"]).await?;
    if !dirty.trim().is_empty() {
        return Ok(format!(
            "worktree `{}` gardé : des modifications n'y sont pas commitées",
            path.display()
        ));
    }
    let p = path.to_string_lossy().to_string();
    git(s, repo, &["worktree", "remove", &p]).await?;
    Ok("worktree retiré, la branche reste".into())
}

#[cfg(test)]
mod tests;
