//! `shell_exec` (§11) : bac à sable, délai, sortie tronquée, commandes interdites.

use crate::error::{ToolError, ToolResult};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Vérifie une commande avant exécution (§11).
///
/// Ce n'est pas la protection principale (c'est le bac à sable), mais un garde-fou qui
/// donne un message clair au modèle plutôt qu'un échec opaque. La liste des commandes
/// interdites vit dans `penelope-platform` : elle dépend de l'OS.
pub fn check_command(command: &str) -> ToolResult<()> {
    if command.trim().is_empty() {
        return Err(ToolError::Invalid("commande vide".into()));
    }
    let normalised = command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    for p in penelope_platform::process::PRIVILEGE_PREFIXES {
        if normalised.starts_with(&p.to_lowercase())
            || normalised.contains(&format!("| {}", p.trim()))
        {
            return Err(ToolError::Denied(format!(
                "`{}` est interdit : Pénélope n'élève jamais ses privilèges",
                p.trim()
            )));
        }
    }
    for f in penelope_platform::process::forbidden_commands() {
        if normalised.contains(f) {
            return Err(ToolError::Denied(format!("commande interdite : `{f}`")));
        }
    }
    Ok(())
}

/// Sortie d'une exécution.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Vrai si la sortie a été tronquée et doit partir en artefact.
    pub truncated: bool,
    pub duration_ms: u64,
}

impl ShellOutput {
    pub fn to_json(&self) -> Value {
        json!({
            "exitCode": self.exit_code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "truncated": self.truncated,
            "durationMs": self.duration_ms,
            "success": self.exit_code == 0,
        })
    }
}

/// Variables héritées en plus par le shell du propriétaire : emplacements de
/// configuration et agent SSH, pour que `gh`, `git` ou `ssh` retrouvent ceux du terminal.
/// Jamais de jeton (`GH_TOKEN`, `GITHUB_TOKEN`…) : un `env` le montrerait au modèle.
pub const SHELL_EXTRA_ENV: &[&str] = &[
    "LOGNAME",
    "SSH_AUTH_SOCK",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "GH_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
];

/// Exécute une commande sous le profil de bac à sable donné.
/// Intervalle de vérification de l'arrêt demandé pendant qu'un processus tourne.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// Grâce laissée au groupe de processus entre `SIGTERM` et `SIGKILL`.
const GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Paramètres d'une exécution : bac à sable, répertoire, délai, troncature, shell et
/// jeton d'arrêt (issue #57).
pub struct ExecOptions<'a> {
    pub profile: Option<&'a penelope_platform::Profile>,
    pub cwd: Option<&'a Path>,
    pub timeout: std::time::Duration,
    pub max_output_bytes: usize,
    pub shell: Option<(String, Vec<String>)>,
    pub cancel: Option<&'a penelope_llm::CancelToken>,
}

impl Default for ExecOptions<'_> {
    fn default() -> Self {
        ExecOptions {
            profile: None,
            cwd: None,
            timeout: std::time::Duration::from_secs(120),
            max_output_bytes: 256 * 1024,
            shell: None,
            cancel: None,
        }
    }
}

pub async fn exec(
    host: &penelope_platform::UnixProcessHost,
    command: &str,
    opts: ExecOptions<'_>,
) -> ToolResult<ShellOutput> {
    let ExecOptions {
        profile,
        cwd,
        timeout,
        max_output_bytes,
        shell,
        cancel,
    } = opts;
    use penelope_platform::ProcessHost;

    check_command(command)?;
    let (program, mut args) = shell.unwrap_or_else(penelope_platform::process::default_shell);
    args.push(command.to_string());

    let mut spec = penelope_platform::ProcessSpec::new(program).args(args);
    spec.inherit_env
        .extend(SHELL_EXTRA_ENV.iter().map(|k| k.to_string()));
    spec.stdin = false;
    if let Some(d) = cwd {
        spec = spec.cwd(d);
    }

    let started = std::time::Instant::now();
    let child = host
        .spawn(spec, profile)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    // Le processus est suivi par son pid : arrêt demandé ou délai dépassé, son groupe est
    // terminé (`SIGTERM` puis `SIGKILL`) au lieu de continuer en orphelin (issue #57).
    let pid = child.pid;
    let waiting = child.inner.wait_with_output();
    tokio::pin!(waiting);
    let deadline = tokio::time::Instant::now() + timeout;
    let output = loop {
        let tick = tokio::time::Instant::now() + CANCEL_POLL;
        tokio::select! {
            r = &mut waiting => break Ok(r),
            _ = tokio::time::sleep_until(tick.min(deadline)) => {
                if cancel.is_some_and(|c| c.is_cancelled()) {
                    penelope_platform::process::terminate_group(pid, GRACE).await;
                    let _ = tokio::time::timeout(GRACE, &mut waiting).await;
                    return Err(ToolError::Cancelled);
                }
                if tokio::time::Instant::now() >= deadline {
                    penelope_platform::process::terminate_group(pid, GRACE).await;
                    let _ = tokio::time::timeout(GRACE, &mut waiting).await;
                    break Err(());
                }
            }
        }
    };
    let duration_ms = started.elapsed().as_millis() as u64;

    match output {
        Ok(Ok(o)) => {
            let (stdout, t1) = truncate(&String::from_utf8_lossy(&o.stdout), max_output_bytes);
            let (stderr, t2) = truncate(&String::from_utf8_lossy(&o.stderr), max_output_bytes / 4);
            Ok(ShellOutput {
                exit_code: o.status.code().unwrap_or(-1),
                stdout,
                stderr,
                truncated: t1 || t2,
                duration_ms,
            })
        }
        Ok(Err(e)) => Err(ToolError::Io(e.to_string())),
        Err(()) => Err(ToolError::Timeout(timeout.as_millis() as u64)),
    }
}

/// Tronque une sortie en gardant la tête **et** la queue : c'est la fin qui porte
/// l'erreur, le début qui porte la commande.
pub fn truncate(s: &str, max_bytes: usize) -> (String, bool) {
    if s.len() <= max_bytes {
        return (s.to_string(), false);
    }
    let head_bytes = max_bytes * 6 / 10;
    let tail_bytes = max_bytes - head_bytes;
    let head_end = floor_char_boundary(s, head_bytes);
    let tail_start = ceil_char_boundary(s, s.len() - tail_bytes);
    (
        format!(
            "{}\n[… {} octets élidés, sortie complète en artefact …]\n{}",
            &s[..head_end],
            s.len() - head_end - (s.len() - tail_start),
            &s[tail_start..]
        ),
        true,
    )
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Profil de bac à sable pour une commande, d'après la configuration.
pub fn profile_for(
    default_profile: &str,
    workspace: &Path,
    network: bool,
) -> penelope_platform::Profile {
    let mut p = match default_profile {
        "readonly" => penelope_platform::Profile::read_only(),
        "full" => penelope_platform::Profile::full(),
        _ => penelope_platform::Profile::workspace_write(workspace.to_path_buf()),
    };
    p = p.with_network(network);
    p
}

/// Workspaces par défaut d'une session.
pub fn default_workspaces(state_dir: &Path, extra: &[String]) -> Vec<PathBuf> {
    let mut v = vec![state_dir.to_path_buf()];
    v.extend(extra.iter().map(PathBuf::from));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_env_never_carries_tokens() {
        for k in SHELL_EXTRA_ENV {
            let u = k.to_uppercase();
            assert!(
                !u.contains("TOKEN") && !u.contains("KEY") && !u.contains("SECRET"),
                "{k}"
            );
        }
        assert!(profile_for("workspace-write", Path::new("/w"), true).allow_network);
    }

    /// #57 : `/stop` pendant une commande longue termine le processus et son groupe,
    /// au lieu d'attendre le délai.
    #[tokio::test]
    async fn a_cancelled_command_is_terminated_quickly() {
        let dir = tempfile::tempdir().unwrap();
        let host = penelope_platform::UnixProcessHost::new(dir.path().join("pids"));
        let cancel = penelope_llm::CancelToken::new();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            stopper.cancel();
        });
        let started = std::time::Instant::now();
        let e = exec(
            &host,
            "sleep 60",
            ExecOptions {
                timeout: std::time::Duration::from_secs(60),
                max_output_bytes: 4096,
                shell: Some(("/bin/sh".into(), vec!["-c".into()])),
                cancel: Some(&cancel),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(e, ToolError::Cancelled), "{e}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "arrêt trop lent : {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn forbidden_commands_are_refused() {
        for c in ["rm -rf /", "sudo rm x", "  rm   -rf   /  "] {
            let e = check_command(c).unwrap_err();
            assert!(
                e.to_string().contains("interdit"),
                "commande acceptée à tort : {c} → {e}"
            );
        }
        assert!(check_command("").is_err());
    }

    #[test]
    fn ordinary_commands_pass() {
        for c in [
            "cargo test",
            "git status",
            "rm -rf target/debug",
            "npm run build",
        ] {
            check_command(c).unwrap_or_else(|e| panic!("refusée à tort : {c} → {e}"));
        }
    }

    #[test]
    fn sudo_in_a_pipeline_is_caught() {
        assert!(check_command("echo x | sudo tee /etc/hosts").is_err());
    }

    #[test]
    fn truncation_keeps_head_and_tail() {
        let s = format!("{}ERREUR FINALE", "a".repeat(10_000));
        let (out, truncated) = truncate(&s, 1000);
        assert!(truncated);
        assert!(out.starts_with("aaaa"));
        assert!(
            out.ends_with("ERREUR FINALE"),
            "la fin doit survivre : {}",
            &out[out.len() - 40..]
        );
        assert!(out.contains("octets élidés"));
    }

    #[test]
    fn short_output_is_untouched() {
        let (out, truncated) = truncate("court", 1000);
        assert_eq!(out, "court");
        assert!(!truncated);
    }

    #[test]
    fn truncation_never_splits_utf8() {
        let s = "é".repeat(5000);
        let (out, _) = truncate(&s, 1000);
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn profiles_follow_the_configuration() {
        let ws = Path::new("/tmp/ws");
        assert_eq!(
            profile_for("readonly", ws, false).kind,
            penelope_platform::ProfileKind::ReadOnly
        );
        assert_eq!(
            profile_for("workspace-write", ws, false).kind,
            penelope_platform::ProfileKind::WorkspaceWrite
        );
        assert!(profile_for("workspace-write", ws, true).allow_network);
        assert!(!profile_for("full", ws, false).enforced());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn timeout_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let host = penelope_platform::UnixProcessHost::new(dir.path());
        let e = exec(
            &host,
            "sleep 30",
            ExecOptions {
                timeout: std::time::Duration::from_millis(200),
                max_output_bytes: 4096,
                shell: Some(("/bin/sh".into(), vec!["-c".into()])),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(e, ToolError::Timeout(_)), "{e}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn exit_code_and_streams_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let host = penelope_platform::UnixProcessHost::new(dir.path());
        let out = exec(
            &host,
            "echo bonjour; echo souci >&2; exit 3",
            ExecOptions {
                timeout: std::time::Duration::from_secs(10),
                max_output_bytes: 4096,
                shell: Some(("/bin/sh".into(), vec!["-c".into()])),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(out.exit_code, 3);
        assert!(out.stdout.contains("bonjour"));
        assert!(out.stderr.contains("souci"));
        assert_eq!(out.to_json()["success"], false);
    }
}
