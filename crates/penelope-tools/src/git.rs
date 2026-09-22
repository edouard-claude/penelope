//! Outils git (§11). `git` est invoqué comme **exécutable**, jamais via un shell.
//!
//! `git_push` est classé `external` et passe donc toujours par une approbation (§11).

use crate::error::{ToolError, ToolResult};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;

/// Accepte uniquement une source Git explicite, ou le raccourci `owner/repo` de GitHub.
/// La validation précède toute création de dossier ou invocation de `git` (#160).
pub fn normalize_clone_url(source: &str) -> ToolResult<String> {
    let valid_component = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    };
    if let Some((owner, repo)) = source.split_once('/')
        && valid_component(owner)
        && valid_component(repo)
    {
        return Ok(format!("https://github.com/{owner}/{repo}.git"));
    }
    if source.contains(char::is_whitespace) || source.chars().any(char::is_control) {
        return Err(ToolError::Invalid(
            "source Git invalide : utiliser https://, ssh://, git://, file:// ou user@hôte:chemin"
                .into(),
        ));
    }
    if let Ok(url) = url::Url::parse(source)
        && matches!(url.scheme(), "https" | "ssh" | "git" | "file")
        && (url.scheme() == "file" || url.host_str().is_some())
        && !url.path().is_empty()
    {
        return Ok(source.to_string());
    }
    if let Some((user_host, path)) = source.split_once(':')
        && let Some((user, host)) = user_host.split_once('@')
        && !user.is_empty()
        && !host.is_empty()
        && !host.contains('/')
        && !path.is_empty()
        && !path.starts_with('-')
    {
        return Ok(source.to_string());
    }
    Err(ToolError::Invalid(format!(
        "source Git `{source}` invalide : utiliser https://, ssh://, git://, file://, \
         user@hôte:chemin ou owner/repo (GitHub)"
    )))
}

fn remote_identity(source: &str) -> Option<(String, String)> {
    let (host, path) = if let Ok(url) = url::Url::parse(source) {
        (url.host_str()?.to_ascii_lowercase(), url.path().to_string())
    } else {
        let (user_host, path) = source.split_once(':')?;
        let (_, host) = user_host.split_once('@')?;
        (host.to_ascii_lowercase(), path.to_string())
    };
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = if host == "github.com" {
        path.to_ascii_lowercase()
    } else {
        path.to_string()
    };
    (!path.is_empty()).then_some((host, path))
}

/// Cherche un dépôt existant sans suivre les liens ni parcourir indéfiniment le workspace.
async fn existing_clone(workspace: &Path, source: &str) -> Option<(String, String)> {
    let wanted = remote_identity(source)?;
    let mut pending = vec![(workspace.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = pending.pop() {
        seen += 1;
        if seen > 512 {
            break;
        }
        if dir.join(".git").exists()
            && let Some(origin) = origin_remote(&dir).await
            && remote_identity(&origin) == Some(wanted.clone())
        {
            return Some((dir.to_string_lossy().to_string(), origin));
        }
        if depth >= 4 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && !entry.file_name().to_string_lossy().starts_with('.')
                    && entry.file_name() != "target"
            })
            .map(|entry| entry.path())
            .collect();
        children.sort();
        pending.extend(children.into_iter().rev().map(|path| (path, depth + 1)));
    }
    None
}

/// Exécute une commande git et renvoie (code, stdout, stderr).
pub async fn run(cwd: &Path, args: &[&str]) -> ToolResult<(i32, String, String)> {
    let git = penelope_platform::which("git")
        .ok_or_else(|| ToolError::Denied("`git` est absent du PATH".into()))?;
    let out = tokio::process::Command::new(git)
        .args(args)
        .current_dir(cwd)
        // Aucune invite interactive : un mot de passe demandé bloquerait le daemon.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        // Les aides de git (`git-lfs`, gestionnaires d'identifiants) vivent souvent hors
        // du PATH minimal d'un service.
        .env("PATH", penelope_platform::process::search_path())
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| ToolError::Io(format!("git {} : {e}", args.join(" "))))?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

fn ok_or_err(code: i32, stdout: String, stderr: String, extra: Value) -> ToolResult<Value> {
    if code != 0 {
        return Err(ToolError::Other(format!(
            "git a échoué (code {code}) : {}",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        )));
    }
    let mut v = json!({"stdout": stdout, "ok": true});
    if let (Some(a), Some(b)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in b {
            a.insert(k.clone(), val.clone());
        }
    }
    Ok(v)
}

pub async fn status(cwd: &Path) -> ToolResult<Value> {
    let (code, out, err) = run(cwd, &["status", "--porcelain=v1", "--branch"]).await?;
    let branch = out
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("## "))
        .map(|b| b.split(['.', ' ']).next().unwrap_or(b).to_string())
        .unwrap_or_default();
    let files: Vec<Value> = out
        .lines()
        .skip(1)
        .filter(|l| l.len() > 3)
        .map(|l| json!({"state": l[..2].trim(), "path": l[3..].to_string()}))
        .collect();
    ok_or_err(
        code,
        out.clone(),
        err,
        json!({"branch": branch, "files": files, "clean": files.is_empty()}),
    )
}

pub async fn diff(cwd: &Path, against: Option<&str>, staged: bool) -> ToolResult<Value> {
    let mut args = vec!["diff", "--no-color"];
    if staged {
        args.push("--staged");
    }
    if let Some(a) = against {
        args.push(a);
    }
    let (code, out, err) = run(cwd, &args).await?;
    let (plus, minus) = crate::fs::diff_stats(&out);
    ok_or_err(
        code,
        out.clone(),
        err,
        json!({"added": plus, "removed": minus, "lines": out.lines().count()}),
    )
}

pub async fn branch(cwd: &Path, name: &str, create: bool) -> ToolResult<Value> {
    validate_ref(name)?;
    let args: Vec<&str> = if create {
        vec!["checkout", "-b", name]
    } else {
        vec!["checkout", name]
    };
    let (code, out, err) = run(cwd, &args).await?;
    ok_or_err(code, out, err, json!({"branch": name, "created": create}))
}

pub async fn commit(cwd: &Path, message: &str, all: bool) -> ToolResult<Value> {
    if message.trim().is_empty() {
        return Err(ToolError::Invalid("message de commit vide".into()));
    }
    if all {
        let (c, o, e) = run(cwd, &["add", "-A"]).await?;
        if c != 0 {
            return Err(ToolError::Other(format!(
                "git add : {}{}",
                o.trim(),
                e.trim()
            )));
        }
    }
    // « Rien à valider » n'est pas une erreur. Le test porte sur l'index, pas sur le
    // message de git, qui est traduit selon la locale de l'utilisateur.
    let (_, porcelain, _) = run(cwd, &["status", "--porcelain=v1"]).await?;
    if porcelain.trim().is_empty() {
        return Ok(json!({"ok": true, "committed": false, "reason": "rien à valider"}));
    }
    let (code, out, err) = run(cwd, &["commit", "-m", message]).await?;
    let sha = run(cwd, &["rev-parse", "HEAD"])
        .await
        .ok()
        .map(|(_, s, _)| s.trim().to_string())
        .unwrap_or_default();
    ok_or_err(code, out, err, json!({"committed": true, "sha": sha}))
}

pub async fn clone(url: &str, dest: &Path, depth: Option<u32>) -> ToolResult<Value> {
    let parent = dest
        .parent()
        .ok_or_else(|| ToolError::Invalid("destination sans répertoire parent".into()))?;
    clone_in(url, dest, &[parent.to_path_buf()], depth).await
}

/// Variante appelée par le daemon avec toutes les racines de workspace autorisées.
pub async fn clone_in(
    url: &str,
    dest: &Path,
    workspaces: &[std::path::PathBuf],
    depth: Option<u32>,
) -> ToolResult<Value> {
    let url = normalize_clone_url(url)?;
    let parent = dest
        .parent()
        .ok_or_else(|| ToolError::Invalid("destination sans répertoire parent".into()))?;
    for workspace in workspaces {
        if workspace.exists()
            && let Some((existing, origin)) = existing_clone(workspace, &url).await
        {
            return Ok(json!({
                "ok": true, "already": true, "dest": existing, "url": url, "origin": origin,
                "stdout": "dépôt déjà présent dans le workspace"
            }));
        }
    }
    std::fs::create_dir_all(parent).map_err(|e| ToolError::Io(e.to_string()))?;
    let depth_s = depth.unwrap_or(50).to_string();
    let dest_s = dest.to_string_lossy().to_string();
    let args = vec![
        "clone",
        "--depth",
        depth_s.as_str(),
        url.as_str(),
        dest_s.as_str(),
    ];
    let (code, out, err) = run(parent, &args).await?;
    ok_or_err(
        code,
        out,
        err,
        json!({"dest": dest_s, "url": url, "already": false}),
    )
}

pub async fn push(cwd: &Path, remote: &str, branch_name: &str) -> ToolResult<Value> {
    validate_ref(branch_name)?;
    let (code, out, err) = run(cwd, &["push", "-u", remote, branch_name]).await?;
    ok_or_err(
        code,
        out,
        err.clone(),
        json!({"remote": remote, "branch": branch_name, "stderr": err}),
    )
}

/// Point de reprise avant une écriture (§11 : « checkpoint git si workspace git »).
pub async fn checkpoint(cwd: &Path, label: &str) -> ToolResult<Option<String>> {
    if !cwd.join(".git").exists() {
        return Ok(None);
    }
    let (code, out, _) = run(cwd, &["stash", "create", label]).await?;
    if code != 0 {
        return Ok(None);
    }
    let sha = out.trim().to_string();
    Ok((!sha.is_empty()).then_some(sha))
}

/// Remote `origin` normalisé, clé de projet du rappel contextuel (§6.7).
pub async fn origin_remote(cwd: &Path) -> Option<String> {
    let (code, out, _) = run(cwd, &["remote", "get-url", "origin"]).await.ok()?;
    (code == 0)
        .then(|| out.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Valide un nom de branche ou de référence : pas d'option déguisée, pas d'espace.
pub fn validate_ref(name: &str) -> ToolResult<()> {
    if name.is_empty() {
        return Err(ToolError::Invalid("nom de référence vide".into()));
    }
    if name.starts_with('-') {
        return Err(ToolError::Invalid(format!(
            "`{name}` commence par `-` : refusé pour éviter qu'un nom se fasse passer \
             pour une option"
        )));
    }
    if name.contains(char::is_whitespace) || name.contains("..") || name.contains('~') {
        return Err(ToolError::Invalid(format!(
            "nom de référence invalide : `{name}`"
        )));
    }
    Ok(())
}

/// Nom de branche par défaut pour un ticket (§12.10, `penelope/<ticket>-<slug>`).
pub fn branch_name_for(ticket: &str, title: &str) -> String {
    let slug = penelope_platform::slugify(title);
    let ticket = penelope_platform::slugify(ticket);
    let mut name = format!("penelope/{ticket}-{slug}");
    if name.len() > 100 {
        name.truncate(100);
        while name.ends_with('-') {
            name.pop();
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repo() -> Option<tempfile::TempDir> {
        let d = tempfile::tempdir().ok()?;
        let (code, _, _) = run(d.path(), &["init", "-q"]).await.ok()?;
        if code != 0 {
            return None;
        }
        run(d.path(), &["config", "user.email", "test@example.com"])
            .await
            .ok()?;
        run(d.path(), &["config", "user.name", "Test"]).await.ok()?;
        Some(d)
    }

    #[test]
    fn ref_validation_blocks_option_injection() {
        validate_ref("penelope/4312-corrige-tva").unwrap();
        assert!(validate_ref("--upload-pack=evil").is_err());
        assert!(validate_ref("a b").is_err());
        assert!(validate_ref("a..b").is_err());
        assert!(validate_ref("").is_err());
    }

    #[test]
    fn branch_names_are_slugified_and_bounded() {
        let b = branch_name_for("#4312", "Corrige la TVA sur les factures — urgent");
        assert!(b.starts_with("penelope/4312-corrige-la-tva"));
        assert!(!b.contains(' '));
        let long = branch_name_for("T-1", &"mot ".repeat(80));
        assert!(long.len() <= 100);
        assert!(!long.ends_with('-'));
    }

    #[tokio::test]
    async fn status_reports_branch_and_files() {
        let Some(d) = repo().await else {
            eprintln!("git indisponible : test ignoré");
            return;
        };
        std::fs::write(d.path().join("a.txt"), "contenu").unwrap();
        let s = status(d.path()).await.unwrap();
        assert_eq!(s["clean"], false);
        let files = s["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f["path"] == "a.txt"));
    }

    #[tokio::test]
    async fn commit_then_diff() {
        let Some(d) = repo().await else { return };
        std::fs::write(d.path().join("a.txt"), "une\n").unwrap();
        let c = commit(d.path(), "premier commit", true).await.unwrap();
        assert_eq!(c["committed"], true);
        assert!(!c["sha"].as_str().unwrap_or_default().is_empty());

        std::fs::write(d.path().join("a.txt"), "deux\n").unwrap();
        let diff = diff(d.path(), None, false).await.unwrap();
        assert_eq!(diff["added"], 1);
        assert_eq!(diff["removed"], 1);
    }

    #[tokio::test]
    async fn empty_commit_is_not_an_error() {
        let Some(d) = repo().await else { return };
        std::fs::write(d.path().join("a.txt"), "une\n").unwrap();
        commit(d.path(), "premier", true).await.unwrap();
        let second = commit(d.path(), "rien à faire", true).await.unwrap();
        assert_eq!(second["committed"], false);
    }

    #[tokio::test]
    async fn branch_creation_and_checkout() {
        let Some(d) = repo().await else { return };
        std::fs::write(d.path().join("a.txt"), "x").unwrap();
        commit(d.path(), "init", true).await.unwrap();
        let b = branch(d.path(), "penelope/test", true).await.unwrap();
        assert_eq!(b["created"], true);
        let s = status(d.path()).await.unwrap();
        assert_eq!(s["branch"], "penelope/test");
    }

    #[tokio::test]
    async fn checkpoint_is_none_outside_a_repo() {
        let d = tempfile::tempdir().unwrap();
        assert!(
            checkpoint(d.path(), "avant écriture")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn commit_refuses_an_empty_message() {
        let d = tempfile::tempdir().unwrap();
        assert!(commit(d.path(), "   ", false).await.is_err());
    }

    #[test]
    fn clone_sources_accept_remotes_and_expand_a_github_shortcut() {
        assert_eq!(
            normalize_clone_url("Fidelatoo/imap").unwrap(),
            "https://github.com/Fidelatoo/imap.git"
        );
        assert_eq!(
            normalize_clone_url("git@github.com:o/r.git").unwrap(),
            "git@github.com:o/r.git"
        );
        assert_eq!(
            normalize_clone_url("https://gitlab.apnl.tech/g/p.git").unwrap(),
            "https://gitlab.apnl.tech/g/p.git"
        );
        for source in ["../autre", "/tmp/x", "imap", "-option", "Fidelatoo/../imap"] {
            assert!(
                matches!(normalize_clone_url(source), Err(ToolError::Invalid(_))),
                "{source}"
            );
        }
    }

    #[tokio::test]
    async fn clone_reuses_an_existing_workspace_repo_with_an_equivalent_origin() {
        let workspace = tempfile::tempdir().unwrap();
        let existing = workspace.path().join("Fidelatoo/imap");
        std::fs::create_dir_all(&existing).unwrap();
        run(&existing, &["init", "-q"]).await.unwrap();
        run(
            &existing,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:Fidelatoo/imap.git",
            ],
        )
        .await
        .unwrap();
        let dest = workspace.path().join("imap-src");
        let result = clone("Fidelatoo/imap", &dest, None).await.unwrap();
        assert_eq!(result["already"], true);
        assert_eq!(result["url"], "https://github.com/Fidelatoo/imap.git");
        assert_eq!(result["dest"], existing.to_string_lossy().as_ref());
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn clone_searches_the_workspace_even_when_destination_is_nested() {
        let workspace = tempfile::tempdir().unwrap();
        let existing = workspace.path().join("Fidelatoo/imap");
        std::fs::create_dir_all(&existing).unwrap();
        run(&existing, &["init", "-q"]).await.unwrap();
        run(
            &existing,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:Fidelatoo/imap.git",
            ],
        )
        .await
        .unwrap();
        let dest = workspace.path().join("other/imap-src");
        let result = clone_in(
            "Fidelatoo/imap",
            &dest,
            &[workspace.path().to_path_buf()],
            None,
        )
        .await
        .unwrap();
        assert_eq!(result["already"], true);
        assert_eq!(result["dest"], existing.to_string_lossy().as_ref());
        assert!(!dest.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn clone_rejects_local_paths_before_starting_git() {
        let workspace = tempfile::tempdir().unwrap();
        let dest = workspace.path().join("new/dest");
        for source in ["../autre", "/tmp/x", "imap"] {
            assert!(matches!(
                clone(source, &dest, None).await,
                Err(ToolError::Invalid(_))
            ));
            assert!(!workspace.path().join("new").exists());
        }
    }

    #[tokio::test]
    async fn an_explicit_file_url_clones_and_reports_the_actual_source() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        run(&source, &["init", "-q"]).await.unwrap();
        let url = url::Url::from_file_path(&source).unwrap().to_string();
        let dest = workspace.path().join("cloned");
        let result = clone(&url, &dest, None).await.unwrap();
        assert_eq!(result["url"], url);
        assert_eq!(result["already"], false);
        assert!(dest.join(".git").exists());
    }
}
