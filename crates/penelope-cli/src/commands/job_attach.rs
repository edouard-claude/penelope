//! `penelope job attach <id>` (#397, point 3) : la sortie d'un `shell_exec` lancé en
//! arrière-plan par l'agent, en direct et en lecture seule, depuis `{state}/jobs/<id>.log`.
//! S'arrête quand la commande est finie (`<id>.done`) ; Ctrl-C le quitte avant.
//!
//! `--control` prend la main sur un job `tty`, après confirmation dans le terminal : ce
//! qui est tapé part sur l'entrée de la commande par son tube (`<id>.in`), le terminal en
//! mode brut ; Ctrl-] rend la main. Tant qu'il l'a, `<id>.owner` le dit à l'agent
//! (`job_status`).

use super::*;
use clap::Subcommand;
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Subcommand, Debug)]
pub enum JobCmd {
    /// Suit la sortie d'un job en direct, en lecture seule ; `--control` prend la main
    /// sur un job `tty` après confirmation (Ctrl-] la rend).
    Attach {
        id: String,
        #[arg(long)]
        control: bool,
    },
}

/// Ctrl-] : rend la main, comme `telnet`.
const DETACH: u8 = 0x1d;

/// Le terminal en mode brut tant que le propriétaire a la main ; rétabli, et la marque
/// retirée, quand il la rend ou que la commande finit.
struct Control {
    owner: std::path::PathBuf,
    saved: Option<String>,
    detached: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn stty(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("stty")
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Control {
    fn take(keyboard: &std::path::Path, owner: std::path::PathBuf) -> CliResult<Control> {
        let mut fifo = std::fs::OpenOptions::new()
            .write(true)
            .open(keyboard)
            .map_err(|e| CliError::Io(format!("{} : {e}", keyboard.display())))?;
        let _ = std::fs::write(&owner, "");
        let saved = stty(&["-g"]);
        if saved.is_some() {
            let _ = stty(&["raw", "-echo"]);
        }
        let detached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = detached.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 256];
            loop {
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let bytes = &buf[..n];
                let cut = bytes.iter().position(|b| *b == DETACH);
                let send = &bytes[..cut.unwrap_or(n)];
                if (!send.is_empty() && fifo.write_all(send).is_err()) || cut.is_some() {
                    break;
                }
            }
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        Ok(Control {
            owner,
            saved,
            detached,
        })
    }

    fn detached(&self) -> bool {
        self.detached.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        if let Some(saved) = &self.saved {
            let _ = stty(&[saved]);
        }
        let _ = std::fs::remove_file(&self.owner);
        eprintln!("\r\n[main rendue à l'agent]\r");
    }
}

/// « Prendre la main ? » : seul « o » ou « oui » la prend.
fn confirm(id: &str) -> bool {
    eprint!(
        "Prendre la main sur le job `{id}` ? Ce que tu tapes ira à la commande ; Ctrl-] \
         la rend. [o/N] "
    );
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok()
        && matches!(line.trim().to_lowercase().as_str(), "o" | "oui")
}

pub(super) async fn run(cli: &Cli, cmd: &JobCmd) -> CliResult<()> {
    let JobCmd::Attach { id, control } = cmd;
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CliError::Usage(format!(
            "identifiant de job invalide : `{id}`"
        )));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let log = dirs.state().join("jobs").join(format!("{id}.log"));
    let done = log.with_extension("done");
    let mut waited = 0;
    while !log.exists() {
        if done.exists() || waited >= 50 {
            return Err(CliError::Usage(format!(
                "aucune sortie en direct pour le job `{id}` : seul un `shell_exec` en \
                 arrière-plan en a une, et pas quand un secret collé y passe (`penelope jobs`)"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }
    let keyboard = log.with_extension("in");
    let held = if *control {
        if !keyboard.exists() {
            return Err(CliError::Usage(format!(
                "le job `{id}` n'a pas de clavier : seul un `shell_exec` avec `tty: true` en \
                 arrière-plan en a un"
            )));
        }
        match confirm(id) {
            true => Some(Control::take(&keyboard, log.with_extension("owner"))?),
            false => {
                eprintln!("Lecture seule.");
                None
            }
        }
    } else {
        None
    };
    let mut file = std::fs::File::open(&log).map_err(|e| CliError::Io(e.to_string()))?;
    let mut offset = 0u64;
    let mut out = std::io::stdout();
    loop {
        let finished = done.exists();
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| CliError::Io(e.to_string()))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| CliError::Io(e.to_string()))?;
        offset += buf.len() as u64;
        if !buf.is_empty() {
            let _ = out.write_all(&buf);
            let _ = out.flush();
        }
        if (finished && buf.is_empty()) || held.as_ref().is_some_and(Control::detached) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
