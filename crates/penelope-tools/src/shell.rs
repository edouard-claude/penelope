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

/// Une ligne de commande qui ne fait que lire (issue #111). La classification vit dans
/// `penelope_hitl::cmdline`, avec le découpage : une liste `&&` doit poser la même
/// question à chacune de ses étapes (issue #150), et deux listes divergeraient.
pub fn is_read_command(command: &str) -> bool {
    penelope_hitl::cmdline::is_read_command(command)
}

/// Même question sur une ligne déjà découpée.
pub fn is_read_pipeline(line: &penelope_hitl::cmdline::Pipeline) -> bool {
    penelope_hitl::cmdline::is_read(line)
}

/// Préfixe `cd <répertoire> && ` d'une ligne de commande (issue #123) : le modèle se place
/// ainsi dans un dépôt avant d'y lire. Renvoie le répertoire et la commande qui suit quand
/// le préfixe est seul de son espèce : `cd` en tête, un chemin sans variable, substitution,
/// joker, tilde ni échappement (entre guillemets ou non), puis `&&`. La suite est rendue
/// telle quelle : c'est elle qui se classe, et un second enchaînement la laisse composée.
pub fn split_cd_prefix(command: &str) -> Option<(String, String)> {
    const UNSAFE: &[char] = &[
        '$', '`', '\\', ';', '&', '|', '<', '>', '(', ')', '{', '}', '*', '?', '[', ']', '~', '!',
        '#', '\'', '"', '\n', '\r',
    ];
    let rest = command.trim_start().strip_prefix("cd")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    let (dir, after) = match rest.chars().next()? {
        q @ ('\'' | '"') => {
            let body = &rest[1..];
            let end = body.find(q)?;
            (&body[..end], &body[end + 1..])
        }
        _ => {
            let end = rest.find([' ', '\t']).unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        }
    };
    if dir.is_empty() || dir == "-" || dir.contains(UNSAFE) {
        return None;
    }
    let after = after.trim_start_matches([' ', '\t']).strip_prefix("&&")?;
    if after.starts_with('&') {
        return None;
    }
    let next = after.trim();
    (!next.is_empty()).then(|| (dir.to_string(), next.to_string()))
}

/// Une commande qui peut détruire, ou dont on ne peut pas le dire (issue #111) :
/// enchaînement ou substitution (ce qui suit peut être n'importe quoi), suppression,
/// écrasement, élévation de droits, git qui réécrit ou efface. Le mode « tout sauf le
/// destructif » la fait toujours approuver.
pub fn may_destroy(command: &str) -> bool {
    let line = command.trim();
    if line.contains(['|', ';', '&', '`', '$', '>', '(', ')', '{', '}', '\n', '\r']) {
        return true;
    }
    let words: Vec<String> = line
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '\'' || c == '"').to_string())
        .collect();
    let Some(program) = words
        .first()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
    else {
        return false;
    };
    let has = |bad: &[&str]| words[1..].iter().any(|w| bad.contains(&w.as_str()));
    match program.as_str() {
        "rm" | "rmdir" | "dd" | "shred" | "truncate" | "mkfs" | "sudo" | "doas" | "su"
        | "chown" | "kill" | "killall" | "pkill" | "launchctl" | "diskutil" | "xargs" => true,
        "chmod" | "mv" | "cp" => has(&["-R", "-r", "-f", "--force"]),
        "find" => has(&["-delete", "-exec", "-execdir"]),
        "git" => {
            has(&[
                "--force",
                "-f",
                "--hard",
                "-D",
                "--delete",
                "--force-with-lease",
            ]) || matches!(
                words.get(1).map(String::as_str),
                Some("clean" | "filter-branch")
            )
        }
        _ => false,
    }
}

/// Note ajoutée à l'échec d'une commande lancée sans réseau quand elle en avait
/// vraisemblablement besoin (issue #106) : l'agent sait quoi changer au lieu de relancer
/// la même commande.
pub const NETWORK_OFF_NOTE: &str = "Réseau coupé pour cette commande par le bac à sable : \
elle n'a pas demandé `network: true` (sandbox.shell_network est fermé). Si elle a besoin du \
réseau, relance-la avec \"network\": true ; le propriétaire approuvera l'accès.";

/// Un échec qui ressemble à un réseau coupé : message de résolution ou de connexion, ou
/// commande qui ne vit que du réseau (`curl`, `git push`, `gh`, installation de paquets).
pub fn looks_like_network_failure(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> bool {
    if exit_code == 0 {
        return false;
    }
    const SIGNS: &[&str] = &[
        "could not resolve",
        "couldn't resolve",
        "couldn't connect",
        "failed to connect",
        "could not connect",
        "name or service not known",
        "nodename nor servname",
        "temporary failure in name resolution",
        "network is unreachable",
        "no route to host",
        "getaddrinfo",
        "enotfound",
        "eai_again",
        "econnrefused",
        "dns error",
        "error sending request",
        "operation not permitted (os error 1)",
        "connect: operation not permitted",
        "unable to access 'http",
        "could not read from remote repository",
    ];
    let text = format!("{stdout}\n{stderr}").to_lowercase();
    if SIGNS.iter().any(|s| text.contains(s)) {
        return true;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    matches!(
        words.as_slice(),
        [
            "curl" | "wget" | "nc" | "ssh" | "scp" | "rsync" | "gh" | "ping",
            ..
        ] | ["git", "push" | "pull" | "fetch" | "clone" | "ls-remote", ..]
            | [
                "npm" | "pnpm" | "yarn" | "pip" | "pip3" | "brew",
                "install" | "add" | "i" | "update",
                ..
            ]
            | ["cargo", "install" | "fetch" | "update" | "publish", ..]
    )
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

    // Étiquette PID : ce qu'un crash du daemon laisse derrière est ramassé au démarrage
    // suivant (`reap_orphans`, issue #65).
    spec = spec.pid_tag(format!("shell-{}", penelope_kernel::ids::Ulid::new()));

    let started = std::time::Instant::now();
    let mut child = host
        .spawn(spec, profile)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    // Sorties lues en continu sous plafond : une commande bavarde ne fait plus monter la
    // mémoire du daemon de la taille de sa sortie (issue #65).
    let out_pipe = child.inner.stdout.take();
    let err_pipe = child.inner.stderr.take();
    let out_task = tokio::spawn(read_capped(out_pipe, max_output_bytes));
    let err_task = tokio::spawn(read_capped(err_pipe, max_output_bytes / 4));

    let pid = child.pid;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let tick = tokio::time::Instant::now() + CANCEL_POLL;
        tokio::select! {
            r = child.inner.wait() => {
                match r {
                    Ok(status) => {
                        let (stdout, t1) = out_task.await.unwrap_or_default().render(max_output_bytes);
                        let (stderr, t2) = err_task.await.unwrap_or_default().render(max_output_bytes / 4);
                        return Ok(ShellOutput {
                            exit_code: status.code().unwrap_or(-1),
                            stdout,
                            stderr,
                            truncated: t1 || t2,
                            duration_ms: started.elapsed().as_millis() as u64,
                        });
                    }
                    Err(e) => return Err(ToolError::Io(e.to_string())),
                }
            }
            _ = tokio::time::sleep_until(tick.min(deadline)) => {
                let stop = cancel.is_some_and(|c| c.is_cancelled());
                if stop || tokio::time::Instant::now() >= deadline {
                    // Le groupe de processus est terminé : sans cela la commande survit au
                    // délai et le modèle la relance (issue #65).
                    penelope_platform::process::terminate_group(pid, GRACE).await;
                    let _ = tokio::time::timeout(GRACE, child.inner.wait()).await;
                    out_task.abort();
                    err_task.abort();
                    if stop {
                        return Err(ToolError::Cancelled);
                    }
                    return Err(ToolError::Timeout(timeout.as_millis() as u64));
                }
            }
        }
    }
}

/// Sortie lue sous plafond : la tête, la queue, et le nombre total d'octets vus.
#[derive(Default)]
struct Capped {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: usize,
}

impl Capped {
    /// Rend la sortie telle que le modèle la voit, et dit si elle a été coupée.
    fn render(&self, max_bytes: usize) -> (String, bool) {
        if self.total <= max_bytes {
            let mut all = self.head.clone();
            all.extend_from_slice(&self.tail);
            return (String::from_utf8_lossy(&all).to_string(), false);
        }
        let elided = self.total - self.head.len() - self.tail.len();
        (
            format!(
                "{}\n[… {elided} octets élidés, sortie complète en artefact …]\n{}",
                String::from_utf8_lossy(&self.head),
                String::from_utf8_lossy(&self.tail)
            ),
            true,
        )
    }
}

/// Lit un tuyau jusqu'à sa fermeture en ne gardant que la tête et la queue : le reste est
/// compté puis jeté, sans jamais tenir en mémoire (issue #65).
async fn read_capped<R>(pipe: Option<R>, max_bytes: usize) -> Capped
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut out = Capped::default();
    let Some(mut pipe) = pipe else {
        return out;
    };
    let head_cap = (max_bytes * 6 / 10).max(1);
    let tail_cap = max_bytes.saturating_sub(head_cap).max(1);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        out.total += n;
        let mut rest = &buf[..n];
        if out.head.len() < head_cap {
            let take = (head_cap - out.head.len()).min(rest.len());
            out.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        if !rest.is_empty() {
            out.tail.extend_from_slice(rest);
            if out.tail.len() > tail_cap {
                out.tail.drain(..out.tail.len() - tail_cap);
            }
        }
    }
    out
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
    profile_with_denied_reads(default_profile, workspace, network, &[])
}

/// Comme [`profile_for`], avec les chemins dont la lecture est refusée (issue #68).
pub fn profile_with_denied_reads(
    default_profile: &str,
    workspace: &Path,
    network: bool,
    deny_read: &[PathBuf],
) -> penelope_platform::Profile {
    let mut p = match default_profile {
        "readonly" => penelope_platform::Profile::read_only(),
        "full" => penelope_platform::Profile::full(),
        _ => penelope_platform::Profile::workspace_write(workspace.to_path_buf()),
    };
    p = p.with_network(network);
    // Un workspace sous un chemin refusé resterait lisible : le refus ne vise que ce qui
    // n'est pas un répertoire de travail.
    p.deny_read = deny_read
        .iter()
        .filter(|d| !workspace.starts_with(d))
        .cloned()
        .collect();
    p
}

/// Workspaces par défaut d'une session.
pub fn default_workspaces(state_dir: &Path, extra: &[String]) -> Vec<PathBuf> {
    let mut v = vec![state_dir.to_path_buf()];
    v.extend(extra.iter().map(PathBuf::from));
    v
}

#[cfg(test)]
mod tests;
