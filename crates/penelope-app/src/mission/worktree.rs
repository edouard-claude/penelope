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
    let top = PathBuf::from(top.trim());
    let name = short(session_id);
    let branch = format!("penelope/mission-{name}");
    let path = top.join(DIR).join(&name);
    let git_dir = git(s, &top, &["rev-parse", "--git-common-dir"]).await?;
    let git_dir = top.join(git_dir.trim());
    let exclude = git_dir.join("info").join("exclude");
    let line = format!("/{DIR}/");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !current.lines().any(|l| l.trim() == line) {
        let _ = std::fs::create_dir_all(git_dir.join("info"));
        let _ = std::fs::write(
            &exclude,
            format!(
                "{current}{}{line}\n",
                if current.ends_with('\n') || current.is_empty() {
                    ""
                } else {
                    "\n"
                }
            ),
        );
    }
    let p = path.to_string_lossy().to_string();
    git(s, &top, &["worktree", "add", "-b", &branch, &p, "HEAD"]).await?;
    Ok((path, branch))
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
