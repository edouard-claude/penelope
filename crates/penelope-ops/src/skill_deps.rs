//! Dépendances déclarées par une skill (issue #146).
//!
//! Les skills documentaires supposent un socle : paquets npm, modules Python, binaires en
//! ligne de commande. Rien de tout cela n'est installé par Pénélope — c'est la machine du
//! propriétaire. Ce module se contente de **dire ce qui manque**, et avec quelle commande
//! le poser.
//!
//! Format, dans le frontmatter : `requires: [pip:openpyxl, npm:docx, bin:pandoc]`.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Délai d'une sonde : un interpréteur qui ne répond pas en trois secondes est un
/// problème de machine, pas une dépendance manquante.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Missing {
    /// La déclaration telle qu'elle est écrite dans la skill.
    pub requirement: String,
    /// La commande qui la pose, à passer par le propriétaire.
    pub how: String,
}

/// Type et nom d'une déclaration, si elle est lisible et sans surprise.
fn split(req: &str) -> Option<(&str, &str)> {
    let (kind, name) = req.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(is_name_char) {
        return None;
    }
    matches!(kind, "pip" | "npm" | "bin").then_some((kind, name))
}

/// Caractères admis dans un nom de paquet. Ces noms finissent dans une ligne `import` ou
/// un `require.resolve` : tout le reste est refusé plutôt qu'échappé.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '/')
}

/// Ce qui manque, parmi ces déclarations. Les doublons sont fondus : dix skills qui
/// veulent `pandoc` ne donnent qu'une ligne.
pub async fn missing_for(requirements: &[String]) -> Vec<Missing> {
    let mut wanted: Vec<String> = requirements.to_vec();
    wanted.sort();
    wanted.dedup();

    let mut out = Vec::new();
    // Chemin des paquets npm globaux, lu une fois : `require.resolve` ne le connaît pas
    // tout seul.
    let mut node_path: Option<Option<String>> = None;

    for req in wanted {
        let Some((kind, name)) = split(&req) else {
            out.push(Missing {
                requirement: req.clone(),
                how: "déclaration illisible : attendu `pip:`, `npm:` ou `bin:` suivi d'un \
                      nom de paquet"
                    .into(),
            });
            continue;
        };
        let present = match kind {
            "bin" => in_path(name),
            "pip" => probe("python3", &["-c".into(), format!("import {name}")], None).await,
            "npm" => {
                let path = match &node_path {
                    Some(p) => p.clone(),
                    None => {
                        let p = npm_root().await;
                        node_path = Some(p.clone());
                        p
                    }
                };
                probe(
                    "node",
                    &["-e".into(), format!("require.resolve('{name}')")],
                    path.as_deref(),
                )
                .await
            }
            _ => true,
        };
        if !present {
            out.push(Missing {
                requirement: req.clone(),
                how: how_to_install(kind, name),
            });
        }
    }
    out
}

pub fn how_to_install(kind: &str, name: &str) -> String {
    match kind {
        "pip" => format!("pip install {name} (venv dédié si le pip système refuse, PEP 668)"),
        "npm" => format!("npm install -g {name}, puis NODE_PATH=$(npm root -g)"),
        _ => format!("installer `{name}` et le mettre dans le PATH"),
    }
}

/// Exécutable présent dans le `PATH` ? Lecture de répertoires seulement, aucun processus.
fn in_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(name);
        p.is_file() && is_executable(&p)
    })
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

/// Lance une sonde et dit si elle a réussi. Une commande absente vaut « manquant », pas
/// une erreur : c'est le sens de la question.
async fn probe(program: &str, args: &[String], node_path: Option<&str>) -> bool {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(p) = node_path {
        cmd.env("NODE_PATH", p);
    }
    let Ok(child) = cmd.spawn() else {
        return false;
    };
    match tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o.status.success(),
        _ => false,
    }
}

/// `npm root -g`, pour que `require.resolve` voie les paquets globaux (friction relevée
/// lors du portage du 20/09).
async fn npm_root() -> Option<String> {
    let out = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::process::Command::new("npm")
            .args(["root", "-g"])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #146 : une déclaration se lit en type et nom, et tout ce qui pourrait s'échapper
    /// dans une ligne `import` est refusé plutôt qu'échappé.
    #[test]
    fn a_requirement_is_read_or_refused() {
        assert_eq!(split("pip:openpyxl"), Some(("pip", "openpyxl")));
        assert_eq!(split("npm:@scope/pkg"), Some(("npm", "@scope/pkg")));
        assert_eq!(split("bin: pandoc"), Some(("bin", "pandoc")));
        for bad in [
            "openpyxl",
            "cargo:serde",
            "pip:",
            "pip:os'); import shutil",
            "npm:a b",
            "bin:x;rm -rf /",
        ] {
            assert!(split(bad).is_none(), "`{bad}` devrait être refusée");
        }
    }

    /// #146 : ce qui manque est dit avec la commande qui le pose, et les doublons fondus.
    #[tokio::test]
    async fn what_is_missing_is_said_once_with_its_command() {
        let missing = missing_for(&[
            "bin:un-binaire-qui-n-existe-pas".into(),
            "bin:un-binaire-qui-n-existe-pas".into(),
            "npm-sans-deux-points".into(),
        ])
        .await;
        assert_eq!(missing.len(), 2, "{missing:?}");
        assert!(missing[0].how.contains("PATH"), "{:?}", missing[0]);
        assert!(missing[1].how.contains("illisible"), "{:?}", missing[1]);

        // `sh` existe partout où Pénélope tourne : rien à signaler.
        assert!(missing_for(&["bin:sh".into()]).await.is_empty());
    }
}
