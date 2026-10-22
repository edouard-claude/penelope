//! L'exécution d'un outil sur un hôte distant (#395, point 4) : ce que `penelope run
//! --stdio` fait d'un run `tool`, lancé par `ssh` depuis le daemon.
//!
//! Le daemon a déjà tranché politique et approbation ; ici, l'outil seul. Le chemin est
//! résolu **sur l'hôte**, dans la racine de la mission (rien n'en sort), les motifs
//! masqués s'appliquent, et le shell tourne sous le profil demandé. L'hôte rend la valeur
//! brute (`fs::*`, sortie du shell) : le post-traitement (masquage des secrets, digest des
//! tests, artefacts) reste au daemon.

use crate::error::{ToolError, ToolResult};
use penelope_llm::CancelToken;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Les outils servis à distance.
pub const TOOLS: &[&str] = &[
    "fs_read",
    "fs_list",
    "fs_search",
    "fs_write",
    "fs_edit",
    "shell_exec",
];

/// Un appel d'outil à faire sur l'hôte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub tool: String,
    pub args: Value,
    /// La racine de la mission sur l'hôte, absolue.
    pub root: PathBuf,
    #[serde(default)]
    pub hide: Vec<String>,
    /// `full`, `workspace-write` ou `readonly`.
    pub profile: String,
    #[serde(default)]
    pub network: bool,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    /// Variables d'environnement de la commande (secrets collés, `TERM`).
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Lectures refusées (`sandbox.deny_read`) : `~/…` est développé sur l'hôte, une entrée
    /// qui nomme un dossier de Pénélope (`{data}`…) ne vaut que sur sa machine.
    #[serde(default)]
    pub deny_read: Vec<String>,
}

/// Les lectures refusées, en chemins de cet hôte.
fn denied(req: &ToolRequest) -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    req.deny_read
        .iter()
        .filter(|d| !d.contains('{'))
        .map(|d| match d.strip_prefix("~/") {
            Some(rest) if !home.is_empty() => Path::new(&home).join(rest),
            _ => PathBuf::from(d),
        })
        .filter(|p| p.is_absolute())
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect()
}

fn str_arg<'a>(args: &'a Value, k: &str) -> ToolResult<&'a str> {
    args.get(k)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::Invalid(format!("`{k}` manquant")))
}

fn u_arg(args: &Value, k: &str, default: usize) -> usize {
    args.get(k)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default)
}

/// Un chemin de l'appel, résolu dans la racine et hors des motifs masqués.
fn resolve(req: &ToolRequest, raw: &str) -> ToolResult<PathBuf> {
    if raw.starts_with(crate::fs::VAULT_PREFIX) {
        return Err(ToolError::Denied(
            "le vault reste sur la machine de Pénélope".into(),
        ));
    }
    let p = crate::fs::resolve_in(raw, std::slice::from_ref(&req.root), None)?;
    if let Some(d) = denied(req).into_iter().find(|d| p.starts_with(d)) {
        return Err(ToolError::Denied(format!(
            "`{raw}` est sous une lecture refusée (`sandbox.deny_read` : {})",
            d.display()
        )));
    }
    if let Some(pattern) = crate::hide::hidden(&p, std::slice::from_ref(&req.root), &req.hide) {
        return Err(ToolError::Denied(format!(
            "`{raw}` est masqué (`sandbox.hide` : `{pattern}`)"
        )));
    }
    Ok(p)
}

/// Exécute l'outil sur cet hôte.
pub async fn run(
    host: &penelope_platform::UnixProcessHost,
    req: &ToolRequest,
    cancel: &CancelToken,
) -> ToolResult<Value> {
    if !req.root.is_absolute() {
        return Err(ToolError::Invalid(
            "la racine distante doit être un chemin absolu".into(),
        ));
    }
    let args = &req.args;
    match req.tool.as_str() {
        "fs_read" => crate::fs::read(
            &resolve(req, str_arg(args, "path")?)?,
            u_arg(args, "offset", 0),
            u_arg(args, "limit", 400),
        ),
        "fs_list" => crate::fs::list(
            &resolve(req, str_arg(args, "path")?)?,
            args.get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            u_arg(args, "max_entries", 500),
        ),
        "fs_search" => {
            let root = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) if !p.is_empty() => resolve(req, p)?,
                _ => req.root.clone(),
            };
            crate::fs::search(
                &root,
                str_arg(args, "pattern")?,
                args.get("glob").and_then(|v| v.as_str()),
                u_arg(args, "max_results", 100),
            )
        }
        "fs_write" => crate::fs::write(
            &resolve(req, str_arg(args, "path")?)?,
            str_arg(args, "content")?,
        ),
        "fs_edit" => crate::fs::edit(
            &resolve(req, str_arg(args, "path")?)?,
            str_arg(args, "old")?,
            str_arg(args, "new")?,
            args.get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        ),
        "shell_exec" => {
            let command = str_arg(args, "command")?;
            let cwd = match args.get("cwd").and_then(|v| v.as_str()) {
                Some(c) if !c.is_empty() => resolve(req, c)?,
                _ => req.root.clone(),
            };
            let mut profile = crate::shell::profile_with_denied_reads(
                &req.profile,
                &cwd,
                req.network,
                &denied(req),
            );
            profile.hide = crate::hide::regexes(std::slice::from_ref(&req.root), &req.hide);
            let full = req.profile == "full";
            let out = crate::shell::exec(
                host,
                command,
                crate::shell::ExecOptions {
                    profile: (!full).then_some(&profile),
                    cwd: Some(&cwd),
                    timeout: std::time::Duration::from_millis(req.timeout_ms.max(1000)),
                    max_output_bytes: req.max_output_bytes,
                    cancel: Some(cancel),
                    env: req.env.clone(),
                    ..Default::default()
                },
            )
            .await?;
            Ok(out.to_json())
        }
        other => Err(ToolError::Unknown(other.to_string())),
    }
}

/// La racine sur l'hôte existe et est un dossier.
pub fn check_root(root: &Path) -> ToolResult<()> {
    if root.is_dir() {
        Ok(())
    } else {
        Err(ToolError::Invalid(format!(
            "racine distante `{}` absente",
            root.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(root: &Path, tool: &str, args: Value) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            args,
            root: root.to_path_buf(),
            hide: vec!["**/id_ed25519".into()],
            profile: "full".into(),
            network: false,
            timeout_ms: 10_000,
            max_output_bytes: 64 * 1024,
            env: Vec::new(),
            deny_read: Vec::new(),
        }
    }

    #[tokio::test]
    async fn the_host_runs_files_and_shell_inside_its_root_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let host = penelope_platform::UnixProcessHost::new(root.join(".pids"));
        let c = CancelToken::new();
        run(
            &host,
            &req(
                &root,
                "fs_write",
                serde_json::json!({"path": "src/a.txt", "content": "bonjour\n"}),
            ),
            &c,
        )
        .await
        .unwrap();
        let read = run(
            &host,
            &req(&root, "fs_read", serde_json::json!({"path": "src/a.txt"})),
            &c,
        )
        .await
        .unwrap();
        assert!(read.to_string().contains("bonjour"), "{read}");
        let sh = run(
            &host,
            &req(
                &root,
                "shell_exec",
                serde_json::json!({"command": "cat src/a.txt; pwd"}),
            ),
            &c,
        )
        .await
        .unwrap();
        assert!(sh["stdout"].as_str().unwrap().contains("bonjour"), "{sh}");
        assert!(
            sh["stdout"]
                .as_str()
                .unwrap()
                .contains(root.to_str().unwrap()),
            "dans la racine : {sh}"
        );
        for (tool, args) in [
            ("fs_read", serde_json::json!({"path": "../dehors"})),
            ("fs_read", serde_json::json!({"path": "/etc/hosts"})),
            ("fs_read", serde_json::json!({"path": "vault:memoire.md"})),
        ] {
            assert!(
                run(&host, &req(&root, tool, args.clone()), &c)
                    .await
                    .is_err(),
                "{args}"
            );
        }
        std::fs::write(root.join("id_ed25519"), "CLE").unwrap();
        let e = run(
            &host,
            &req(&root, "fs_read", serde_json::json!({"path": "id_ed25519"})),
            &c,
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("masqué"), "{e}");
        assert!(
            run(
                &host,
                &req(
                    Path::new("relatif"),
                    "fs_list",
                    serde_json::json!({"path": "."})
                ),
                &c
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_denied_read_is_denied_on_the_host_too() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let host = penelope_platform::UnixProcessHost::new(root.join(".pids"));
        std::fs::create_dir_all(root.join("cles")).unwrap();
        std::fs::write(root.join("cles/a"), "x").unwrap();
        let mut r = req(&root, "fs_read", serde_json::json!({"path": "cles/a"}));
        r.deny_read = vec![
            root.join("cles").to_string_lossy().to_string(),
            "{data}/penelope.db".into(),
        ];
        let e = run(&host, &r, &CancelToken::new()).await.unwrap_err();
        assert!(e.to_string().contains("lecture refusée"), "{e}");
    }
}
