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

mod tools;
pub use tools::{binary_version, create_tar_gz, extract_tar_gz, git_commit_push, git_sync_repo};

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

/// Emplacements usuels des exécutables installés par l'utilisateur.
///
/// Un service système démarre avec un PATH minimal : sous `launchd`, seulement
/// `/usr/bin:/bin:/usr/sbin:/sbin`. Sans ces emplacements, `npx`, `uvx`, `docker` ou
/// `cargo` seraient introuvables pour le daemon alors qu'ils marchent dans un terminal.
pub fn extra_bin_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        v.extend(
            [
                "/opt/homebrew/bin",
                "/opt/homebrew/sbin",
                "/usr/local/bin",
                "/usr/local/sbin",
                "/Applications/Docker.app/Contents/Resources/bin",
                "/opt/local/bin",
            ]
            .map(PathBuf::from),
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        v.extend(
            [
                "/usr/local/bin",
                "/home/linuxbrew/.linuxbrew/bin",
                "/snap/bin",
            ]
            .map(PathBuf::from),
        );
    }
    // `go install` : `$GOPATH/bin`, `~/go/bin` par défaut.
    if let Some(gopath) = std::env::var_os("GOPATH").filter(|g| !g.is_empty()) {
        for p in std::env::split_paths(&gopath) {
            v.push(p.join("bin"));
        }
    }
    if let Some(h) = home {
        for rel in [
            ".local/bin",
            ".cargo/bin",
            "go/bin",
            ".bun/bin",
            ".deno/bin",
            ".volta/bin",
            ".orbstack/bin",
        ] {
            v.push(h.join(rel));
        }
        // nvm : la version de Node la plus récente installée.
        if let Some(bin) = newest_nvm_bin(&h.join(".nvm/versions/node")) {
            v.push(bin);
        }
    }
    #[cfg(unix)]
    {
        v.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    }
    v
}

fn newest_nvm_bin(root: &Path) -> Option<PathBuf> {
    let parse = |name: &str| -> Option<Vec<u64>> {
        name.strip_prefix('v')?
            .split('.')
            .map(|p| p.parse().ok())
            .collect()
    };
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(ver) = parse(&name) else { continue };
        let bin = e.path().join("bin");
        if bin.is_dir() && best.as_ref().map(|(b, _)| ver > *b).unwrap_or(true) {
            best = Some((ver, bin));
        }
    }
    best.map(|(_, p)| p)
}

/// PATH hérité, complété des emplacements usuels qui existent, sans doublon et dans
/// l'ordre : ce que l'utilisateur a choisi passe toujours en premier.
pub fn merge_paths(current: Option<&std::ffi::OsStr>, extras: &[PathBuf]) -> std::ffi::OsString {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<PathBuf> = Vec::new();
    let inherited: Vec<PathBuf> = current
        .map(|c| std::env::split_paths(c).collect())
        .unwrap_or_default();
    for p in inherited.into_iter().chain(extras.iter().cloned()) {
        if p.as_os_str().is_empty() {
            continue;
        }
        let is_extra = extras.contains(&p);
        if is_extra && !p.is_dir() {
            continue;
        }
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    std::env::join_paths(out).unwrap_or_default()
}

/// Sortie d'une sonde : code de retour et flux, sans interprétation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Le programme a rendu 0.
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Probe {
    /// Le flux qui porte le message : `stdout` s'il dit quelque chose, `stderr` sinon.
    /// `gh auth status` a écrit sur l'un puis sur l'autre selon les versions.
    pub fn text(&self) -> &str {
        if self.stdout.trim().is_empty() {
            self.stderr.trim()
        } else {
            self.stdout.trim()
        }
    }
}

/// Lance `<programme> <arguments>` sans entrée et renvoie sa sortie, ou `None` s'il ne
/// démarre pas ou ne répond pas dans le délai.
///
/// L'entrée est fermée : un programme qui réclame une saisie (`gh auth login`) meurt au
/// délai au lieu de retenir l'appelant. Le délai est court par construction — ces sondes
/// tournent au démarrage et dans `doctor`, jamais dans un tour de conversation.
pub fn probe_command(program: &Path, args: &[&str], timeout: std::time::Duration) -> Option<Probe> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .env("PATH", search_path())
        // Une sonde ne doit jamais ouvrir de pager : `gh` en lance un dès que la sortie
        // ressemble à un terminal.
        .env("PAGER", "cat")
        .env("GH_PAGER", "cat")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().ok()?;
                return Some(Probe {
                    ok: status.success(),
                    stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                });
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Lance `<programme> --version` et renvoie la première ligne, ou `None` s'il ne répond
/// pas dans le délai. Sert au diagnostic : présent dans le PATH ne veut pas dire utilisable.
pub fn probe_version(program: &Path, timeout: std::time::Duration) -> Option<String> {
    let p = probe_command(program, &["--version"], timeout)?;
    let first = p.text().lines().next().unwrap_or("").trim().to_string();
    (p.ok && !first.is_empty()).then_some(first)
}

/// PATH effectif de Pénélope et de tout ce qu'elle lance.
pub fn search_path() -> std::ffi::OsString {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    merge_paths(
        std::env::var_os("PATH").as_deref(),
        &extra_bin_dirs(home.as_deref()),
    )
}

pub fn which(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = search_path();
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
        // PATH hérité = PATH effectif : l'enfant trouve ce que le daemon trouve.
        if k == "PATH" {
            out.insert(k.clone(), search_path().to_string_lossy().to_string());
            continue;
        }
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
                    .args(group_kill_args("KILL", pid))
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

/// Arguments de `/bin/kill` pour signaler tout le groupe dont `pid` est le leader.
///
/// La forme POSIX `-s SIG -- -<pgid>` : sans `--`, le `kill` de procps-ng (Linux) lit
/// `-<pgid>` comme une option et ne signale rien, le groupe survit au délai.
fn group_kill_args(sig: &str, pid: u32) -> [String; 4] {
    ["-s".into(), sig.into(), "--".into(), format!("-{pid}")]
}

async fn signal_group(pid: u32, sig: &str) -> Result<()> {
    let status = tokio::process::Command::new("/bin/kill")
        .args(group_kill_args(sig, pid))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(PlatformError::Process(format!("kill -{sig} : {e}"))),
    }
}

/// Arrête un groupe de processus : `SIGTERM`, puis `SIGKILL` après la grâce. Sert quand
/// le processus a été consommé par `wait_with_output` et qu'il ne reste que son pid
/// (issue #57).
pub async fn terminate_group(pid: u32, grace: std::time::Duration) {
    if pid == 0 {
        return;
    }
    let _ = signal_group(pid, "TERM").await;
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !process_exists(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let _ = signal_group(pid, "KILL").await;
}

/// Comment un processus a fini (issue #114) : code de sortie, ou signal qui l'a tué.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitInfo {
    pub fn of(status: &std::process::ExitStatus) -> ExitInfo {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitInfo {
                code: status.code(),
                signal: status.signal(),
            }
        }
        #[cfg(not(unix))]
        {
            ExitInfo {
                code: status.code(),
                signal: None,
            }
        }
    }

    /// « sorti avec le code 1 », « tué par le signal 9 (SIGKILL) ».
    pub fn describe(&self) -> String {
        match (self.code, self.signal) {
            (Some(c), _) => format!("sorti avec le code {c}"),
            (None, Some(sig)) => {
                let name = match sig {
                    1 => " (SIGHUP)",
                    2 => " (SIGINT)",
                    6 => " (SIGABRT)",
                    9 => " (SIGKILL)",
                    11 => " (SIGSEGV)",
                    13 => " (SIGPIPE)",
                    15 => " (SIGTERM)",
                    _ => "",
                };
                format!("tué par le signal {sig}{name}")
            }
            (None, None) => "arrêté".into(),
        }
    }
}

impl Child {
    /// Attend la fin du processus, au plus `timeout` : `None` s'il tourne encore.
    pub async fn wait_exit(&mut self, timeout: std::time::Duration) -> Option<ExitInfo> {
        match tokio::time::timeout(timeout, self.inner.wait()).await {
            Ok(Ok(status)) => Some(ExitInfo::of(&status)),
            _ => None,
        }
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
mod tests;
