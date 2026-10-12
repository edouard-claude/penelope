//! `ci_variable_set` (#350) : une variable CI posée depuis un secret du magasin, par son
//! nom ; la valeur passe sur l'entrée standard de `glab` ou `gh`, jamais en argument,
//! jamais dans ce que voit le modèle.

use crate::error::{ToolError, ToolResult};
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Variables d'environnement qu'un appel peut fixer : l'hôte de la forge et un relais
/// (le GitLab APNL passe par un relais Tailscale local).
pub const ALLOWED_ENV: &[&str] = &["GITLAB_HOST", "GH_HOST", "HTTPS_PROXY", "HTTP_PROXY"];

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
                env.push((k.clone(), v.as_str().unwrap_or_default().to_string()));
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
    }
}
