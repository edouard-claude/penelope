//! `ci_variable_set` (#350) : une variable CI posée depuis un secret du magasin, par son
//! nom ; la valeur passe sur l'entrée standard de `glab` ou `gh`, jamais en argument,
//! jamais dans ce que voit le modèle.

use crate::error::{ToolError, ToolResult};
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Variables d'environnement qu'un appel peut fixer : l'hôte de la forge (celui d'un
/// remote du dépôt, vérifié) et un relais **local** (le GitLab APNL passe par un relais
/// Tailscale sur 127.0.0.1).
pub const ALLOWED_ENV: &[&str] = &["GITLAB_HOST", "GH_HOST", "HTTPS_PROXY", "HTTP_PROXY"];

/// Seuls les secrets rangés par un collage (`/secrets`, `secret import`) partent en CI :
/// jamais un secret interne (`openrouter_api_key`, `codex.oauth`…), quel que soit le nom
/// que donne le modèle.
pub fn check_secret_name(name: &str) -> ToolResult<()> {
    let prefix = penelope_platform::secrets::import::PREFIX;
    if !name.starts_with(prefix) {
        return Err(ToolError::Denied(format!(
            "`{name}` : seuls les secrets rangés par `/secrets <projet>` (`{prefix}…`) partent \
             en CI"
        )));
    }
    Ok(())
}

/// Un relais n'est accepté que sur la machine : un relais distant verrait passer la
/// requête.
fn local_proxy(url: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| {
            url == *p || url.starts_with(&format!("{p}:")) || url.starts_with(&format!("{p}/"))
        })
}

/// Les hôtes des remotes git de `cwd` : `https://hôte/…`, `ssh://git@hôte/…`,
/// `git@hôte:…`.
pub fn remote_hosts(remotes: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in remotes.split_whitespace() {
        let rest = url.split_once("://").map_or(url, |(_, r)| r);
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let host = rest
            .split(['/', ':'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if host.contains('.') && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// Refuse un hôte de forge qui n'est pas celui d'un remote du dépôt `cwd` : le modèle ne
/// choisit pas où part la valeur.
pub async fn check_hosts(req: &Request, cwd: &Path) -> ToolResult<()> {
    let wanted: Vec<&str> = req
        .env
        .iter()
        .filter(|(k, _)| k == "GITLAB_HOST" || k == "GH_HOST")
        .map(|(_, v)| v.as_str())
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let (_, remotes, _) = crate::git::run(cwd, &["remote", "-v"]).await?;
    let hosts = remote_hosts(&remotes);
    for h in wanted {
        if !hosts.iter().any(|r| r.eq_ignore_ascii_case(h)) {
            return Err(ToolError::Denied(format!(
                "hôte `{h}` : ce n'est l'hôte d'aucun remote du dépôt `{}` ({})",
                cwd.display(),
                if hosts.is_empty() {
                    "aucun remote".to_string()
                } else {
                    hosts.join(", ")
                }
            )));
        }
    }
    Ok(())
}

/// Ce qu'il faut poser.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// `gitlab` (défaut) ou `github`.
    pub forge: String,
    pub key: String,
    /// Environnement (`dev`, `qa`) ; absent : tous.
    pub scope: Option<String>,
    /// `groupe/projet` ; absent : celui du dossier `cwd`.
    pub repo: Option<String>,
    pub masked: bool,
    pub protected: bool,
    pub env: Vec<(String, String)>,
}

impl Request {
    /// Lit les arguments de l'outil ; refuse une clé ou une variable d'environnement
    /// hors liste.
    pub fn from_args(args: &Value) -> ToolResult<Request> {
        let text = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
        };
        let key = text("key").ok_or_else(|| ToolError::Invalid("`key` manquant".into()))?;
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(ToolError::Invalid(format!(
                "`key` : lettres, chiffres et `_` seulement ({key})"
            )));
        }
        let forge = text("forge").unwrap_or_else(|| "gitlab".into());
        if !matches!(forge.as_str(), "gitlab" | "github") {
            return Err(ToolError::Invalid(format!("forge inconnue : {forge}")));
        }
        let mut env = Vec::new();
        if let Some(map) = args.get("env").and_then(|v| v.as_object()) {
            for (k, v) in map {
                if !ALLOWED_ENV.contains(&k.as_str()) {
                    return Err(ToolError::Invalid(format!(
                        "`env.{k}` : seuls {} sont permis",
                        ALLOWED_ENV.join(", ")
                    )));
                }
                let v = v.as_str().unwrap_or_default().to_string();
                if k.ends_with("_PROXY") && !local_proxy(&v) {
                    return Err(ToolError::Invalid(format!(
                        "`env.{k}` : un relais local seulement (http://127.0.0.1:…)"
                    )));
                }
                env.push((k.clone(), v));
            }
        }
        let flag = |k: &str, d: bool| args.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        Ok(Request {
            forge,
            key,
            scope: text("scope"),
            repo: text("repo"),
            masked: flag("masked", true),
            protected: flag("protected", false),
            env,
        })
    }

    /// Le programme et ses arguments ; `update` : remplacer une variable qui existe.
    pub fn command(&self, update: bool) -> (&'static str, Vec<String>) {
        let mut a: Vec<String> = Vec::new();
        if self.forge == "github" {
            a.extend(["secret".into(), "set".into(), self.key.clone()]);
            if let Some(s) = &self.scope {
                a.extend(["--env".into(), s.clone()]);
            }
            if let Some(r) = &self.repo {
                a.extend(["--repo".into(), r.clone()]);
            }
            return ("gh", a);
        }
        let verb = if update { "update" } else { "set" };
        a.extend(["variable".into(), verb.into(), self.key.clone()]);
        if let Some(s) = &self.scope {
            a.extend(["--scope".into(), s.clone()]);
        }
        if self.masked {
            a.push("--masked".into());
        }
        if self.protected {
            a.push("--protected".into());
        }
        if let Some(r) = &self.repo {
            a.extend(["-R".into(), r.clone()]);
        }
        ("glab", a)
    }
}

/// Ce que l'outil rend : jamais la valeur.
#[derive(Debug)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub updated: bool,
}

async fn run_once(req: &Request, cwd: &Path, value: &str, update: bool) -> ToolResult<Outcome> {
    let (program, args) = req.command(update);
    let bin = penelope_platform::which(program)
        .ok_or_else(|| ToolError::Denied(format!("`{program}` est absent du PATH")))?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(&args)
        .current_dir(cwd)
        .env("PATH", penelope_platform::process::search_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| ToolError::Io(format!("{program} : {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(value.as_bytes())
            .await
            .map_err(|e| ToolError::Io(format!("{program} : entrée : {e}")))?;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| ToolError::Io(format!("{program} : {e}")))?;
    Ok(Outcome {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        updated: update,
    })
}

/// Pose la variable ; une variable GitLab qui existe déjà est mise à jour.
pub async fn set_variable(req: &Request, cwd: &Path, value: &str) -> ToolResult<Outcome> {
    let first = run_once(req, cwd, value, false).await?;
    let exists = first.stderr.contains("already been taken") || first.stderr.contains("exists");
    if first.code != 0 && req.forge == "gitlab" && exists {
        return run_once(req, cwd, value, true).await;
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gitlab_and_github_commands_take_the_value_on_stdin() {
        let r = Request::from_args(&json!({
            "key": "STORAGE_ACCESS_KEY", "scope": "dev", "repo": "mentaleco/mentaleco-app",
            "env": {"GITLAB_HOST": "gitlab.apnl.tech"}
        }))
        .unwrap();
        assert_eq!(
            r.command(false),
            (
                "glab",
                [
                    "variable",
                    "set",
                    "STORAGE_ACCESS_KEY",
                    "--scope",
                    "dev",
                    "--masked",
                    "-R",
                    "mentaleco/mentaleco-app"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        assert_eq!(r.command(true).1[1], "update");
        let g =
            Request::from_args(&json!({"key": "TOKEN", "forge": "github", "scope": "qa"})).unwrap();
        assert_eq!(
            g.command(false),
            (
                "gh",
                ["secret", "set", "TOKEN", "--env", "qa"]
                    .map(String::from)
                    .to_vec()
            )
        );
    }

    #[test]
    fn a_key_or_an_environment_variable_outside_the_list_is_refused() {
        assert!(Request::from_args(&json!({"key": "A; rm -rf /"})).is_err());
        assert!(Request::from_args(&json!({"key": "A", "env": {"LD_PRELOAD": "x"}})).is_err());
        assert!(Request::from_args(&json!({"key": "A", "forge": "bitbucket"})).is_err());
        assert!(Request::from_args(&json!({})).is_err());
        assert!(
            Request::from_args(
                &json!({"key": "A", "env": {"HTTPS_PROXY": "http://evil.example:80"}})
            )
            .is_err()
        );
        assert!(
            Request::from_args(
                &json!({"key": "A", "env": {"HTTPS_PROXY": "http://127.0.0.1:18765"}})
            )
            .is_ok()
        );
    }

    /// Seuls les secrets collés partent ; l'hôte est celui d'un remote du dépôt.
    #[tokio::test]
    async fn only_pasted_secrets_go_and_only_to_a_remote_of_the_repository() {
        assert!(check_secret_name("ci.mentaleco.dev.access_key").is_ok());
        for internal in ["openrouter_api_key", "codex.oauth", "mcp.clickup.oauth"] {
            assert!(check_secret_name(internal).is_err(), "{internal}");
        }
        assert_eq!(
            remote_hosts(
                "origin\thttps://gitlab.apnl.tech/mentaleco/app.git (fetch)\n\
                 gh\tgit@github.com:acme/app.git (push)\nx\tssh://git@Forge.Example.org:2222/a.git"
            ),
            ["gitlab.apnl.tech", "github.com", "forge.example.org"]
        );
        let dir = tempfile::tempdir().unwrap();
        crate::git::run(dir.path(), &["init", "-q"]).await.unwrap();
        crate::git::run(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://gitlab.apnl.tech/mentaleco/app.git",
            ],
        )
        .await
        .unwrap();
        let req = |host: &str| {
            Request::from_args(&json!({"key": "K", "env": {"GITLAB_HOST": host}})).unwrap()
        };
        check_hosts(&req("gitlab.apnl.tech"), dir.path())
            .await
            .unwrap();
        let e = check_hosts(&req("evil.example.org"), dir.path())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("aucun remote") || e.contains("gitlab.apnl.tech"),
            "{e}"
        );
    }
}
