//! `penelope job attach <id>` (#397, point 3) : la sortie d'un `shell_exec` lancé en
//! arrière-plan par l'agent, en direct et en lecture seule, depuis `{state}/jobs/<id>.log`.
//! S'arrête quand la commande est finie (`<id>.done`) ; Ctrl-C le quitte avant.

use super::*;
use clap::Subcommand;
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Subcommand, Debug)]
pub enum JobCmd {
    /// Suit la sortie d'un job en direct, en lecture seule.
    Attach { id: String },
}

pub(super) async fn run(cli: &Cli, cmd: &JobCmd) -> CliResult<()> {
    let JobCmd::Attach { id } = cmd;
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
        if finished && buf.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
