//! Ce que la livraison demande à git : le remote, la branche de travail, son commit, et
//! le push. `git` est lancé comme exécutable, jamais par un shell, sans invite
//! d'identifiants : un push qui en voudrait une échoue au lieu d'attendre.

use std::path::Path;
use std::time::Duration;

const QUICK: Duration = Duration::from_secs(20);
const PUSH: Duration = Duration::from_secs(180);

async fn git(dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("git {} : délai de {} s dépassé", args[0], timeout.as_secs()))?
        .map_err(|e| format!("git introuvable : {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!(
            "git {} : {}",
            args[0],
            penelope_observe::redact(&err)
        ))
    }
}

/// L'URL d'un remote, s'il existe.
pub async fn remote_url(dir: &Path, remote: &str) -> Option<String> {
    git(dir, &["remote", "get-url", "--", remote], QUICK)
        .await
        .ok()
        .filter(|u| !u.is_empty())
}

/// La branche de travail ; une tête détachée n'en a pas.
pub async fn current_branch(dir: &Path) -> Result<String, String> {
    git(dir, &["symbolic-ref", "--short", "-q", "HEAD"], QUICK)
        .await
        .map_err(|_| "tête détachée : le travail n'est sur aucune branche".to_string())
}

pub async fn head_sha(dir: &Path) -> Result<String, String> {
    git(dir, &["rev-parse", "HEAD"], QUICK).await
}

/// Des changements suivis non commités : ils ne partiraient pas avec le push. Les
/// fichiers non suivis ne comptent pas.
pub async fn dirty(dir: &Path) -> Result<bool, String> {
    Ok(!git(
        dir,
        &["status", "--porcelain", "--untracked-files=no"],
        QUICK,
    )
    .await?
    .is_empty())
}

/// Pousse `branch` sur `remote`, sous le même nom.
pub async fn push(dir: &Path, remote: &str, branch: &str) -> Result<(), String> {
    let spec = format!("refs/heads/{branch}:refs/heads/{branch}");
    git(dir, &["push", "--", remote, &spec], PUSH)
        .await
        .map(|_| ())
}
