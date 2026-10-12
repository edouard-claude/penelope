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

/// `https://hôte/g/p.git`, `ssh://git@hôte:22/g/p.git`, `git@hôte:g/p.git` → (hôte, `g/p`).
pub fn parse_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next()?.to_string(), path.to_string())
    } else {
        let rest = url.rsplit_once('@').map_or(url, |(_, r)| r);
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string())
    };
    let path = path
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_string();
    (host.contains('.') && path.contains('/')).then(|| (host.to_ascii_lowercase(), path))
}

/// Un projet `groupe/projet` (sous-groupes permis), sans schéma, sans `@`, sans `..` :
/// `glab -R` et `gh --repo` accepteraient sinon un hôte caché dans le nom.
fn check_repo(repo: &str) -> ToolResult<()> {
    let ok = !repo.contains("..")
        && !repo.starts_with('/')
        && !repo.ends_with('/')
        && repo.split('/').count() >= 2
        && repo.split('/').all(|seg| !seg.is_empty())
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'));
    if ok {
        Ok(())
    } else {
        Err(ToolError::Invalid(format!(
            "`repo` : `groupe/projet` attendu, sans hôte ni schéma ({repo})"
        )))
    }
}

/// Où part la valeur, résolu **une fois** : l'hôte (dans `tools.ci_hosts`) et le projet.
/// La forge est ensuite appelée hors du dépôt, avec l'un et l'autre explicites : elle ne
/// relit aucun remote qu'un autre appel aurait pu changer entre-temps.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub host: String,
    pub repo: String,
}

/// Résout l'hôte (`GITLAB_HOST`/`GH_HOST`, sinon celui du remote `origin` de `cwd`) et le
/// projet (`repo`, sinon celui d'`origin` sur ce même hôte), puis refuse un hôte hors de
/// `allowed` (`tools.ci_hosts`, fixé par le propriétaire).
pub async fn resolve(req: &Request, cwd: &Path, allowed: &[String]) -> ToolResult<Target> {
    let explicit = req
        .env
        .iter()
        .find(|(k, _)| k == "GITLAB_HOST" || k == "GH_HOST")
        .map(|(_, v)| v.trim().to_ascii_lowercase());
    let (_, url, _) = crate::git::run(cwd, &["remote", "get-url", "origin"]).await?;
    let origin = parse_remote(&url);
    let host = explicit
        .or_else(|| origin.as_ref().map(|(h, _)| h.clone()))
        .ok_or_else(|| {
            ToolError::Denied(
                "aucun hôte de forge : lance l'outil depuis le dépôt (`cwd`) ou donne \
                 `GITLAB_HOST`"
                    .into(),
            )
        })?;
    if !allowed.iter().any(|a| a.trim().eq_ignore_ascii_case(&host)) {
        return Err(ToolError::Denied(format!(
            "hôte `{host}` hors de `tools.ci_hosts` ({}) : seul le propriétaire l'ajoute",
            allowed.join(", ")
        )));
    }
    let repo = match &req.repo {
        Some(r) => {
            check_repo(r)?;
            r.clone()
        }
        None => match origin {
            Some((h, path)) if h == host => {
                check_repo(&path)?;
                path
            }
            _ => {
                return Err(ToolError::Invalid(format!(
                    "`repo` manquant : aucun remote `origin` sur `{host}` dans le dépôt"
                )));
            }
        },
    };
    Ok(Target { host, repo })
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

    /// Le programme et ses arguments, vers `target` ; `update` : remplacer une variable qui
    /// existe.
    pub fn command(&self, target: &Target, update: bool) -> (&'static str, Vec<String>) {
        let mut a: Vec<String> = Vec::new();
        if self.forge == "github" {
            a.extend(["secret".into(), "set".into(), self.key.clone()]);
            if let Some(s) = &self.scope {
                a.extend(["--env".into(), s.clone()]);
            }
            a.extend(["--repo".into(), format!("{}/{}", target.host, target.repo)]);
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
        a.extend(["-R".into(), target.repo.clone()]);
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

async fn run_once(
    req: &Request,
    target: &Target,
    value: &str,
    update: bool,
) -> ToolResult<Outcome> {
    let (program, args) = req.command(target, update);
    let bin = penelope_platform::which(program)
        .ok_or_else(|| ToolError::Denied(format!("`{program}` est absent du PATH")))?;
    let mut cmd = tokio::process::Command::new(bin);
    // Hors de tout dépôt : la forge ne lit que l'hôte et le projet donnés.
    cmd.args(&args)
        .current_dir(std::env::temp_dir())
        .env("PATH", penelope_platform::process::search_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in req.env.iter().filter(|(k, _)| k.ends_with("_PROXY")) {
        cmd.env(k, v);
    }
    cmd.env(
        if req.forge == "github" {
            "GH_HOST"
        } else {
            "GITLAB_HOST"
        },
        &target.host,
    );
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

/// Pose la variable vers `target` ; une variable GitLab qui existe déjà est mise à jour.
pub async fn set_variable(req: &Request, target: &Target, value: &str) -> ToolResult<Outcome> {
    let first = run_once(req, target, value, false).await?;
    let exists = first.stderr.contains("already been taken") || first.stderr.contains("exists");
    if first.code != 0 && req.forge == "gitlab" && exists {
        return run_once(req, target, value, true).await;
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target() -> Target {
        Target {
            host: "gitlab.apnl.tech".into(),
            repo: "mentaleco/mentaleco-app".into(),
        }
    }

    #[test]
    fn gitlab_and_github_commands_name_host_and_project_explicitly() {
        let r = Request::from_args(&json!({"key": "STORAGE_ACCESS_KEY", "scope": "dev"})).unwrap();
        assert_eq!(
            r.command(&target(), false),
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
        assert_eq!(r.command(&target(), true).1[1], "update");
        let g =
            Request::from_args(&json!({"key": "TOKEN", "forge": "github", "scope": "qa"})).unwrap();
        let t = Target {
            host: "github.com".into(),
            repo: "acme/app".into(),
        };
        assert_eq!(
            g.command(&t, false),
            (
                "gh",
                [
                    "secret",
                    "set",
                    "TOKEN",
                    "--env",
                    "qa",
                    "--repo",
                    "github.com/acme/app"
                ]
                .map(String::from)
                .to_vec()
            )
        );
    }

    #[test]
    fn a_key_an_environment_variable_or_a_remote_proxy_outside_the_rules_is_refused() {
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

    #[test]
    fn remotes_are_parsed_in_their_three_forms() {
        for url in [
            "https://gitlab.apnl.tech/mentaleco/mentaleco-app.git",
            "git@gitlab.apnl.tech:mentaleco/mentaleco-app.git",
            "ssh://git@GitLab.apnl.tech:2222/mentaleco/mentaleco-app",
        ] {
            assert_eq!(
                parse_remote(url),
                Some(("gitlab.apnl.tech".into(), "mentaleco/mentaleco-app".into())),
                "{url}"
            );
        }
        assert_eq!(parse_remote("/chemin/local"), None);
    }

    /// Seuls les secrets collés partent ; l'hôte doit être dans la liste du propriétaire,
    /// et un projet ne cache pas d'hôte.
    #[tokio::test]
    async fn only_pasted_secrets_go_and_only_to_an_allowed_host() {
        assert!(check_secret_name("ci.mentaleco.dev.access_key").is_ok());
        for internal in ["openrouter_api_key", "codex.oauth", "mcp.clickup.oauth"] {
            assert!(check_secret_name(internal).is_err(), "{internal}");
        }
        let dir = tempfile::tempdir().unwrap();
        crate::git::run(dir.path(), &["init", "-q"]).await.unwrap();
        crate::git::run(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://gitlab.apnl.tech/mentaleco/mentaleco-app.git",
            ],
        )
        .await
        .unwrap();
        let allowed = vec!["gitlab.apnl.tech".to_string()];
        let req = |v: Value| Request::from_args(&v).unwrap();
        assert_eq!(
            resolve(&req(json!({"key": "K"})), dir.path(), &allowed)
                .await
                .unwrap(),
            target()
        );
        for bad in [
            json!({"key": "K", "env": {"GITLAB_HOST": "evil.example.org"}}),
            json!({"key": "K", "repo": "https://evil.example.org/a/b"}),
            json!({"key": "K", "repo": "evil.example.org/a/b@x"}),
            json!({"key": "K", "repo": "a/../b"}),
        ] {
            assert!(
                resolve(&req(bad.clone()), dir.path(), &allowed)
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        assert!(
            resolve(&req(json!({"key": "K"})), dir.path(), &[])
                .await
                .is_err()
        );
        let empty = tempfile::tempdir().unwrap();
        crate::git::run(empty.path(), &["init", "-q"])
            .await
            .unwrap();
        assert!(
            resolve(&req(json!({"key": "K"})), empty.path(), &allowed)
                .await
                .is_err()
        );
    }
}
