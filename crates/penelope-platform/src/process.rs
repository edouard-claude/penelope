//! Lancement et arrêt de processus enfants (§2.6).
//!
//! Couvre : environnement filtré, groupe de processus, arrêt gracieux puis forcé, fichiers
//! PID pour la récupération des orphelins au démarrage, résolution des exécutables via
//! PATH et PATHEXT.
//!
//! Aucune dépendance `libc` : le groupe de processus est posé par
//! `std::os::unix::process::CommandExt::process_group`, qui est `safe`, et l'arrêt de
//! l'arbre passe par `/bin/kill` (exécutable, jamais un shell).

use crate::{PlatformError, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Description d'un processus à lancer.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Environnement **explicite** : rien n'est hérité hors de `inherit_env`.
    pub env: BTreeMap<String, String>,
    /// Variables héritées du daemon (liste blanche).
    pub inherit_env: Vec<String>,
    pub stdin: bool,
    /// Identifiant pour le fichier PID (`{state}/mcp-pids/<tag>.pid`).
    pub pid_tag: Option<String>,
}

impl ProcessSpec {
    pub fn new(program: impl Into<String>) -> Self {
        ProcessSpec {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            inherit_env: default_inherited_env(),
            stdin: true,
            pid_tag: None,
        }
    }
    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }
    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(mut self, it: I) -> Self {
        self.args.extend(it.into_iter().map(|s| s.into()));
        self
    }
    pub fn cwd(mut self, p: impl Into<PathBuf>) -> Self {
        self.cwd = Some(p.into());
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }
    pub fn pid_tag(mut self, t: impl Into<String>) -> Self {
        self.pid_tag = Some(t.into());
        self
    }
}

/// Liste blanche d'environnement par défaut : rien de sensible ne fuit vers un serveur
/// MCP tiers (§13.2, « environnement filtré »).
pub fn default_inherited_env() -> Vec<String> {
    [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "TERM",
        "SHELL",
        "USER",
        "PATHEXT",
        "SystemRoot",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Résolution d'un exécutable via PATH (et PATHEXT sous Windows). Jamais de nom en dur :
/// `npx` devient `npx.cmd` sous Windows.
pub fn which(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Environnement effectif d'un processus enfant : `env_clear()`, puis la liste blanche
/// héritée, puis les variables explicites du `ProcessSpec`.
///
/// Fonction pure, pour que le filtrage soit testable sans lancer de processus.
pub fn effective_env(spec: &ProcessSpec) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for k in &spec.inherit_env {
        if let Some(v) = std::env::var_os(k) {
            out.insert(k.clone(), v.to_string_lossy().to_string());
        }
    }
    for (k, v) in &spec.env {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// Processus enfant supervisé.
pub struct Child {
    pub pid: u32,
    pub inner: tokio::process::Child,
    pid_file: Option<PathBuf>,
}

impl Child {
    pub fn stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.inner.stdin.take()
    }
    pub fn stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.inner.stdout.take()
    }
    pub fn stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.inner.stderr.take()
    }
}

#[async_trait::async_trait]
pub trait ProcessHost: Send + Sync {
    /// Lance un processus, éventuellement sous un profil de bac à sable.
    async fn spawn(
        &self,
        spec: ProcessSpec,
        sandbox: Option<&crate::sandbox::Profile>,
    ) -> Result<Child>;

    /// Arrêt gracieux puis forcé : fermeture de stdin, `SIGTERM` au groupe, `SIGKILL`
    /// après `grace`.
    async fn terminate(&self, child: &mut Child, grace: std::time::Duration) -> Result<()>;

    /// Récupère les orphelins laissés par un crash du daemon.
    fn reap_orphans(&self, pid_dir: &Path) -> Result<Vec<u32>>;
}

/// Implémentation Unix (macOS livré ; identique sur Linux si un backend y est activé).
pub struct UnixProcessHost {
    pub pid_dir: PathBuf,
}

impl UnixProcessHost {
    pub fn new(pid_dir: impl Into<PathBuf>) -> Self {
        UnixProcessHost {
            pid_dir: pid_dir.into(),
        }
    }

    fn build_command(
        &self,
        spec: &ProcessSpec,
        sandbox: Option<&crate::sandbox::Profile>,
    ) -> Result<tokio::process::Command> {
        let resolved = which(&spec.program).ok_or_else(|| {
            PlatformError::NotFound(format!(
                "exécutable introuvable dans PATH : {}",
                spec.program
            ))
        })?;

        let mut cmd = match sandbox {
            Some(profile) if profile.enforced() => {
                let wrapper = crate::backend::sandbox_wrapper(profile, &resolved, &spec.args)?;
                let mut c = tokio::process::Command::new(wrapper.program);
                c.args(wrapper.args);
                c
            }
            _ => {
                let mut c = tokio::process::Command::new(&resolved);
                c.args(&spec.args);
                c
            }
        };

        cmd.env_clear();
        for (k, v) in effective_env(spec) {
            cmd.env(k, v);
        }
        if let Some(d) = &spec.cwd {
            cmd.current_dir(d);
        }
        cmd.stdin(if spec.stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);

        #[cfg(unix)]
        {
            // Groupe de processus dédié : `kill -TERM -<pgid>` atteint tout l'arbre,
            // y compris les enfants de `npx`.
            cmd.process_group(0);
        }
        Ok(cmd)
    }
}

#[async_trait::async_trait]
impl ProcessHost for UnixProcessHost {
    async fn spawn(
        &self,
        spec: ProcessSpec,
        sandbox: Option<&crate::sandbox::Profile>,
    ) -> Result<Child> {
        let mut cmd = self.build_command(&spec, sandbox)?;
        let child = cmd.spawn().map_err(|e| {
            PlatformError::Process(format!("échec du lancement de `{}` : {e}", spec.program))
        })?;
        let pid = child.id().unwrap_or(0);

        let pid_file = match &spec.pid_tag {
            Some(tag) => {
                std::fs::create_dir_all(&self.pid_dir)?;
                let p = self.pid_dir.join(format!("{tag}.pid"));
                std::fs::write(&p, pid.to_string())?;
                Some(p)
            }
            None => None,
        };

        Ok(Child {
            pid,
            inner: child,
            pid_file,
        })
    }

    async fn terminate(&self, child: &mut Child, grace: std::time::Duration) -> Result<()> {
        // 1. Fermeture de stdin : la plupart des serveurs MCP stdio s'arrêtent seuls.
        drop(child.inner.stdin.take());

        // 2. SIGTERM au groupe de processus.
        let pid = child.pid;
        if pid > 0 {
            let _ = signal_group(pid, "TERM").await;
        }

        // 3. Attente, puis SIGKILL.
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            match child.inner.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if tokio::time::Instant::now() >= deadline {
                        if pid > 0 {
                            let _ = signal_group(pid, "KILL").await;
                        }
                        let _ = child.inner.kill().await;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(PlatformError::Process(e.to_string())),
            }
        }
        let _ = child.inner.wait().await;
        if let Some(p) = &child.pid_file {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }

    fn reap_orphans(&self, pid_dir: &Path) -> Result<Vec<u32>> {
        let mut killed = Vec::new();
        let Ok(entries) = std::fs::read_dir(pid_dir) else {
            return Ok(killed);
        };
        for e in entries.flatten() {
            if e.path().extension().and_then(|s| s.to_str()) != Some("pid") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let Ok(pid) = raw.trim().parse::<u32>() else {
                let _ = std::fs::remove_file(e.path());
                continue;
            };
            if process_exists(pid) {
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-KILL", &format!("-{pid}")])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                killed.push(pid);
            }
            let _ = std::fs::remove_file(e.path());
        }
        Ok(killed)
    }
}

async fn signal_group(pid: u32, sig: &str) -> Result<()> {
    // Le groupe porte le pid du leader : la cible est `-<pid>`.
    let status = tokio::process::Command::new("/bin/kill")
        .args([&format!("-{sig}"), &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(PlatformError::Process(format!("kill -{sig} : {e}"))),
    }
}

/// Vrai si le PID existe encore (`kill -0`).
pub fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Commandes interdites, **par OS** (§11).
///
/// Cette liste vit ici parce qu'elle est la seule chose de `shell_exec` qui dépende de
/// l'OS : les motifs Windows contiennent des chemins littéraux `C:\`, interdits partout
/// ailleurs par le test d'architecture.
/// Attend une demande d'arrêt : Ctrl-C, ou `SIGTERM` envoyé par `launchd` / `systemd`.
///
/// Les signaux Unix n'ont le droit d'exister que dans ce crate (§2.10) : le daemon
/// n'appelle que cette fonction.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub fn forbidden_commands() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        FORBIDDEN_WINDOWS
    }
    #[cfg(not(windows))]
    {
        FORBIDDEN_UNIX
    }
}

#[cfg_attr(windows, allow(dead_code))]
const FORBIDDEN_UNIX: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -fr /",
    ":(){:|:&};:",
    "mkfs",
    "dd if=/dev/zero of=/dev/",
    "shutdown",
    "reboot",
    "halt",
    "diskutil erasedisk",
    "chmod -r 777 /",
    "> /dev/sda",
];

#[cfg_attr(not(windows), allow(dead_code))]
const FORBIDDEN_WINDOWS: &[&str] = &[
    "format-volume",
    "remove-item -recurse c:\\",
    "rd /s /q c:\\",
    "stop-computer",
    "restart-computer",
];

/// Préfixes d'élévation de privilèges, refusés sur tous les OS.
pub const PRIVILEGE_PREFIXES: &[&str] = &["sudo ", "doas ", "su -", "runas "];

/// Shell par défaut pour `shell_exec` (§2.6) : `$SHELL`, sinon `/bin/sh` sur Unix,
/// `pwsh` puis `powershell.exe` sur Windows.
pub fn default_shell() -> (String, Vec<String>) {
    if cfg!(windows) {
        let prog = if which("pwsh").is_some() {
            "pwsh"
        } else {
            "powershell.exe"
        };
        (
            prog.to_string(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
            ],
        )
    } else {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        (sh, vec!["-c".into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_a_standard_binary() {
        assert!(which("sh").is_some() || which("cmd").is_some());
        assert!(which("ce-binaire-nexiste-vraiment-pas-42").is_none());
    }

    #[test]
    fn which_accepts_absolute_paths() {
        if cfg!(unix) {
            assert_eq!(which("/bin/sh"), Some(PathBuf::from("/bin/sh")));
        }
    }

    #[test]
    fn default_inherited_env_has_no_secrets() {
        let v = default_inherited_env();
        for k in &v {
            let up = k.to_uppercase();
            assert!(
                !up.contains("KEY") && !up.contains("TOKEN") && !up.contains("SECRET"),
                "variable sensible dans la liste blanche : {k}"
            );
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn spawn_and_terminate_a_child() {
        let dir = tempfile::tempdir().unwrap();
        let host = UnixProcessHost::new(dir.path());
        let spec = ProcessSpec::new("sleep").arg("30").pid_tag("test");
        let mut child = host.spawn(spec, None).await.unwrap();
        assert!(child.pid > 0);
        assert!(dir.path().join("test.pid").exists());

        host.terminate(&mut child, std::time::Duration::from_millis(500))
            .await
            .unwrap();
        assert!(
            !dir.path().join("test.pid").exists(),
            "le fichier PID doit être nettoyé"
        );
    }

    #[test]
    fn environment_is_filtered() {
        let spec = ProcessSpec::new("sleep").env("PENELOPE_VISIBLE", "1");
        let env = effective_env(&spec);
        assert_eq!(env.get("PENELOPE_VISIBLE").map(String::as_str), Some("1"));
        // Seules les variables de la liste blanche (plus celles posées explicitement)
        // peuvent apparaître : aucune clé d'API du daemon ne fuit vers un serveur MCP.
        for k in env.keys() {
            assert!(
                spec.inherit_env.contains(k) || spec.env.contains_key(k),
                "variable hors liste blanche transmise : {k}"
            );
        }
        assert!(!env.contains_key("OPENROUTER_API_KEY"));
    }

    #[test]
    fn explicit_env_overrides_inherited() {
        let mut spec = ProcessSpec::new("sleep");
        spec.inherit_env = vec!["PATH".into()];
        let spec = spec.env("PATH", "/chemin/impose");
        assert_eq!(
            effective_env(&spec).get("PATH").map(String::as_str),
            Some("/chemin/impose")
        );
    }

    #[test]
    fn default_shell_is_reasonable() {
        let (prog, args) = default_shell();
        assert!(!prog.is_empty());
        assert!(!args.is_empty());
    }

    #[test]
    fn reap_orphans_removes_stale_pid_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mort.pid"), "999999999").unwrap();
        std::fs::write(dir.path().join("pasunpid.pid"), "abc").unwrap();
        let host = UnixProcessHost::new(dir.path());
        host.reap_orphans(dir.path()).unwrap();
        assert!(!dir.path().join("mort.pid").exists());
        assert!(!dir.path().join("pasunpid.pid").exists());
    }
}
