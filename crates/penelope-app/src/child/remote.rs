//! L'exécuteur distant (#395, point 4) : un appel d'outil d'une mission `--host` part sur
//! l'hôte par `ssh`, sur un canal exec, avec le même protocole JSONL que l'enfant local.
//! Aucun port n'est ouvert ni transféré ; la ligne distante est fixe
//! (`penelope run --stdio`) et tout le reste passe en JSON sur l'entrée standard.
//!
//! `@local` désigne cette machine : le même protocole sans `ssh`, pour les essais et la
//! suite rejouable.

use super::{Cause, Limits, ToChild, supervise};
use crate::services::Services;
use penelope_llm::CancelToken;
use penelope_platform::ProcessSpec;
use penelope_tools::remote::ToolRequest;
use serde_json::Value;
use std::time::Duration;

/// Le genre de run d'un appel d'outil distant.
pub const KIND_TOOL: &str = "tool";

/// L'hôte local, par le même protocole.
pub const LOCAL: &str = "@local";

/// Un nom de connexion SSH acceptable : celui de `~/.ssh/config`, jamais une option.
pub fn valid_host(host: &str) -> bool {
    host == LOCAL
        || (!host.is_empty()
            && !host.starts_with('-')
            && host.len() <= 253
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c)))
}

/// La commande qui joint l'hôte.
pub fn spec_for(s: &Services, host: &str) -> Result<ProcessSpec, String> {
    if !valid_host(host) {
        return Err(format!(
            "hôte `{host}` refusé : un nom de connexion de ~/.ssh/config"
        ));
    }
    if host == LOCAL {
        // `runners.local_executor` : un autre binaire pour `@local` (suites d'essai, où le
        // binaire courant est celui des tests).
        let configured = s.config.config().runners.local_executor.clone();
        let exe = if configured.is_empty() {
            crate::helpers::running_binary()?
        } else {
            std::path::PathBuf::from(configured)
        };
        return Ok(ProcessSpec::new(exe.to_string_lossy().to_string())
            .arg("run")
            .arg("--stdio"));
    }
    let remote = s.config.config().runners.remote_binary.clone();
    if remote.is_empty()
        || remote
            .chars()
            .any(|c| c.is_whitespace() || ";|&$`'\"<>".contains(c))
    {
        return Err(format!(
            "`runners.remote_binary` = `{remote}` refusé : un chemin sans espace"
        ));
    }
    let mut spec = ProcessSpec::new("ssh");
    for a in [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "RemoteCommand=none",
        "-o",
        "RequestTTY=no",
        "-o",
        "PermitLocalCommand=no",
        "--",
        host,
    ] {
        spec = spec.arg(a);
    }
    spec = spec.arg(remote).arg("run").arg("--stdio");
    // L'agent SSH du propriétaire, s'il en a un : la clé ne quitte pas l'agent.
    if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
        spec = spec.env("SSH_AUTH_SOCK", sock);
    }
    Ok(spec)
}

/// Exécute `req` sur `host` et rend la valeur brute de l'outil.
pub async fn call(
    s: &Services,
    host: &str,
    req: &ToolRequest,
    cancel: &CancelToken,
) -> Result<Value, String> {
    let spec = spec_for(s, host)?;
    let limits = Limits {
        max_rss_kb: None,
        // L'outil a son délai ; le canal a une marge pour la connexion.
        max_duration: Duration::from_millis(req.timeout_ms) + Duration::from_secs(30),
        ..Limits::default()
    };
    let payload = serde_json::to_value(req).map_err(|e| e.to_string())?;
    let init = ToChild::init(&req.tool, KIND_TOOL, payload);
    let report = supervise(&s.platform.processes, spec, init, &limits, cancel, None).await;
    match report.cause {
        Cause::Done => Ok(report.result.unwrap_or(Value::Null)),
        Cause::Failed { error } => Err(error),
        _ => Err(format!("hôte `{host}` : {}", report.describe())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_a_connection_name_never_an_option() {
        for ok in ["lab", "build-01.example.org", "moi@lab", LOCAL] {
            assert!(valid_host(ok), "{ok}");
        }
        for bad in ["-oProxyCommand=sh", "", "lab;rm", "lab host", "lab$(x)"] {
            assert!(!valid_host(bad), "{bad}");
        }
    }
}
