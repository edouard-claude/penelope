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
    clone_in(
        url,
        dest,
        &[parent.to_path_buf()],
        depth,
        &CloneAuth::default(),
    )
    .await
}

/// Ce que `git_clone` ajoute à `git clone` (#305) : l'assistant d'identifiants d'un client
/// de forge connecté, et la configuration de la plateforme.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneAuth {
    /// Client connecté dont git emprunte les identifiants (`gh`, `glab`) : c'est lui que
    /// le résultat nomme.
    pub helper: Option<String>,
    /// Paires `-c clé=valeur`, dans l'ordre.
    pub config: Vec<(String, String)>,
}

impl CloneAuth {
    /// L'assistant d'identifiants de `program` pour `https://<host>` : git lui demande le
    /// jeton au moment de s'authentifier, par son entrée standard. Le jeton ne passe ni
    /// par l'adresse, ni par un argument, ni par une variable d'environnement : rien de ce
    /// que le journal, l'audit ou la trace relèvent ne le contient. La première entrée
    /// vide efface les assistants déjà déclarés pour cet hôte, comme `gh auth setup-git`.
    pub fn forge_helper(program: &str, host: &str) -> CloneAuth {
        let key = format!("credential.https://{host}.helper");
        CloneAuth {
            helper: Some(program.to_string()),
            config: vec![
                (key.clone(), String::new()),
                (key, format!("!{program} auth git-credential")),
            ],
        }
    }
}

/// L'hôte d'une source `https://`, en minuscules : seule cette voie passe par un
/// assistant d'identifiants ; `ssh://` et `user@hôte:chemin` ont leurs clés.
pub fn https_host(source: &str) -> Option<String> {
    let url = url::Url::parse(source).ok()?;
    (url.scheme() == "https")
        .then(|| url.host_str().map(str::to_ascii_lowercase))
        .flatten()
}

/// Variante appelée par le daemon avec toutes les racines de workspace autorisées.
pub async fn clone_in(
    url: &str,
    dest: &Path,
    workspaces: &[std::path::PathBuf],
    depth: Option<u32>,
    auth: &CloneAuth,
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
    fetch_clone(parent, &url, dest, Some(depth.unwrap_or(50)), auth).await
}

/// `git clone` lui-même, sans les contrôles de [`clone_in`].
async fn fetch_clone(
    parent: &Path,
    url: &str,
    dest: &Path,
    depth: Option<u32>,
    auth: &CloneAuth,
) -> ToolResult<Value> {
    let pairs: Vec<String> = auth
        .config
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let mut args: Vec<&str> = Vec::new();
    for pair in &pairs {
        args.extend(["-c", pair.as_str()]);
    }
    args.push("clone");
    let depth_s = depth.map(|d| d.to_string());
    if let Some(d) = &depth_s {
        args.extend(["--depth", d.as_str()]);
    }
    let dest_s = dest.to_string_lossy().to_string();
    args.extend([url, dest_s.as_str()]);
    let (code, out, err) = run(parent, &args).await?;
    let mut extra = json!({"dest": dest_s, "url": url, "already": false});
    if let Some(helper) = &auth.helper {
        extra["credentials"] = json!(helper);
    }
    ok_or_err(code, out, err, extra)
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
            &CloneAuth::default(),
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

    /// #305 : l'assistant d'identifiants est scopé à l'hôte, efface ceux déjà déclarés, et
    /// ne porte qu'une commande ; seule une source `https://` a un hôte à authentifier.
    #[test]
    fn a_forge_helper_is_a_command_scoped_to_its_host() {
        let auth = CloneAuth::forge_helper("gh", "github.com");
        assert_eq!(auth.helper.as_deref(), Some("gh"));
        assert_eq!(
            auth.config,
            vec![
                ("credential.https://github.com.helper".into(), String::new()),
                (
                    "credential.https://github.com.helper".into(),
                    "!gh auth git-credential".into()
                ),
            ]
        );
        assert_eq!(
            https_host("https://GitHub.com/o/r.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(https_host("git@github.com:o/r.git"), None);
        assert_eq!(https_host("ssh://git@github.com/o/r.git"), None);
        assert_eq!(https_host("file:///tmp/r"), None);
    }

    /// Un faux hôte git en HTTP « bête » qui exige une authentification Basic : 401 sans
    /// les bons identifiants, les fichiers sous `root` sinon. Rend l'adresse et le
    /// nombre de requêtes authentifiées.
    async fn private_forge(
        root: std::path::PathBuf,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        // `x-access-token:jeton-305`, ce que rend le faux client de forge.
        const GRANTED: &str = "Basic eC1hY2Nlc3MtdG9rZW46amV0b24tMzA1";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = served.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .trim_start_matches('/')
                    .to_string();
                let granted = head.lines().any(|l| {
                    l.split_once(':').is_some_and(|(k, v)| {
                        k.eq_ignore_ascii_case("authorization") && v.trim() == GRANTED
                    })
                });
                let (status, body) = if !granted {
                    ("401 Unauthorized", Vec::new())
                } else {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    match std::fs::read(root.join(&path)) {
                        Ok(b) if !path.contains("..") => ("200 OK", b),
                        _ => ("404 Not Found", Vec::new()),
                    }
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\
                     WWW-Authenticate: Basic realm=\"forge\"\r\n\
                     Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/"), served)
    }

    /// #305 : un dépôt privé se clone en un appel par l'assistant d'identifiants du client
    /// connecté ; sans lui, refus. Le jeton ne sort ni dans le résultat, ni dans l'erreur.
    #[tokio::test]
    async fn a_private_repo_clones_through_the_forge_helper_without_showing_the_token() {
        let root = tempfile::tempdir().unwrap();
        let bare = root.path().join("coffre.git");
        let work = root.path().join("travail");
        std::fs::create_dir_all(&work).unwrap();
        if run(root.path(), &["init", "-q", "--bare", "coffre.git"])
            .await
            .map(|(c, _, _)| c)
            .unwrap_or(-1)
            != 0
        {
            eprintln!("git indisponible : test ignoré");
            return;
        }
        run(&work, &["init", "-q"]).await.unwrap();
        std::fs::write(work.join("README.md"), "privé\n").unwrap();
        for args in [
            &["add", "README.md"][..],
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "-m",
                "départ",
            ],
            &["push", "-q", bare.to_str().unwrap(), "HEAD:refs/heads/main"],
        ] {
            assert_eq!(run(&work, args).await.unwrap().0, 0, "{args:?}");
        }
        run(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"])
            .await
            .unwrap();
        run(&bare, &["update-server-info"]).await.unwrap();

        // Le faux client de forge : il ne répond qu'à `auth git-credential get`.
        let fake = root.path().join("faux-gh");
        std::fs::write(
            &fake,
            "#!/bin/sh\ncase \"$*\" in\n  *get) echo username=x-access-token; \
             echo password=jeton-305 ;;\nesac\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (base, served) = private_forge(root.path().to_path_buf()).await;
        let url = format!("{base}coffre.git");
        let auth = CloneAuth {
            helper: Some("gh".into()),
            config: vec![
                ("credential.helper".into(), String::new()),
                (
                    "credential.helper".into(),
                    format!("!{} auth git-credential", fake.display()),
                ),
            ],
        };

        // Sans assistant : le dépôt est privé, git ne peut rien demander, refus.
        let refused = fetch_clone(
            root.path(),
            &url,
            &root.path().join("sans"),
            None,
            &CloneAuth {
                helper: None,
                config: vec![("credential.helper".into(), String::new())],
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(!refused.contains("jeton-305"), "{refused}");
        assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 0);

        let dest = root.path().join("coffre");
        let v = fetch_clone(root.path(), &url, &dest, None, &auth)
            .await
            .unwrap();
        assert_eq!(v["credentials"], "gh");
        assert_eq!(v["already"], false);
        assert!(!v.to_string().contains("jeton-305"), "{v}");
        assert_eq!(
            std::fs::read_to_string(dest.join("README.md")).unwrap(),
            "privé\n"
        );
        assert!(served.load(std::sync::atomic::Ordering::SeqCst) > 0);
        // Le jeton n'est pas non plus resté dans la configuration du clone.
        let config = std::fs::read_to_string(dest.join(".git/config")).unwrap();
        assert!(!config.contains("jeton-305"), "{config}");
    }
}
