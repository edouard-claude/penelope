//! `penelope dataset export` : les jeux de données locaux, dans un fichier (issue #233).
//!
//! `--kind approvals` écrit le jeu de décisions du juge d'approbation, une ligne JSON par
//! échantillon (`"v": 1`). La base est ouverte en **lecture seule**, comme pour
//! `approvals stats` : la commande tourne daemon lancé ou arrêté, sans socket. Le fichier
//! est créé en `0600` : il porte des lignes de commande, masquées mais personnelles.
//! Rien ne sort de la machine.

use super::*;
use chrono::NaiveDate;
use clap::Subcommand;
use rusqlite::Connection;
use std::io::Write;
use std::path::Path;

#[derive(Subcommand, Debug)]
pub enum DatasetCmd {
    /// Écrit un jeu de données local en JSONL. Lecture seule, sans daemon.
    Export {
        /// Le jeu : `approvals`, les décisions du juge d'approbation
        /// (`observability.dataset.approvals`).
        #[arg(long, value_parser = ["approvals"])]
        kind: String,
        /// Premier jour gardé, `AAAA-MM-JJ` ; sans : tout.
        #[arg(long)]
        since: Option<String>,
        /// Fichier écrit (remplacé s'il existe), en `0600`.
        #[arg(long)]
        out: PathBuf,
    },
}

pub(super) fn run(cli: &Cli, cmd: &DatasetCmd) -> CliResult<()> {
    let DatasetCmd::Export { since, out, .. } = cmd;
    let since = match since {
        Some(d) => Some(
            NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map_err(|_| CliError::Usage(format!("`--since {d}` : attendu AAAA-MM-JJ")))?
                .format("%Y-%m-%d")
                .to_string(),
        ),
        None => None,
    };
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let db = dirs.db_path();
    if !db.is_file() {
        return Err(CliError::Io(format!("aucune base à {}", db.display())));
    }
    let conn = approval_stats::open_read_only(&db).map_err(|e| CliError::Io(e.to_string()))?;
    let n = export_approvals(&conn, since.as_deref(), out)?;
    if cli.json {
        output::print(&json!({"kind": "approvals", "lines": n, "out": out}), true);
    } else {
        println!("{n} échantillon(s) écrit(s) dans {}", out.display());
    }
    Ok(())
}

/// Écrit les échantillons dans `out`, un par ligne ; rend leur nombre.
pub(super) fn export_approvals(
    conn: &Connection,
    since: Option<&str>,
    out: &Path,
) -> CliResult<usize> {
    let lines = penelope_hitl::samples::export(conn, since).map_err(|e| {
        CliError::Io(format!(
            "lecture du jeu de décisions : {e} (base pas encore migrée par le daemon ?)"
        ))
    })?;
    let io = |e: std::io::Error| CliError::Io(format!("{} : {e}", out.display()));
    let mut f = private_file(out).map_err(io)?;
    for line in &lines {
        writeln!(f, "{line}").map_err(io)?;
    }
    f.sync_all().map_err(io)?;
    Ok(lines.len())
}

/// Un fichier que seul le propriétaire lit : `0600` à la création, et remis à `0600`
/// s'il existait avec d'autres droits.
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(f)
}

#[cfg(test)]
mod tests;
