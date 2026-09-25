//! `penelope logs` : journaux du daemon filtrés par tour ou par session, sans daemon.

use super::*;

/// `penelope logs` : lit les journaux JSON du jour et de la veille, sans daemon, et garde
/// les lignes d'un tour ou d'une session (champ du span ou de l'événement, issue #103).
pub(super) fn logs(
    cli: &Cli,
    turn: Option<&str>,
    session: Option<&str>,
    keep: usize,
) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dirs.logs())
        .map_err(|e| CliError::Io(format!("{} : {e}", dirs.logs().display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.starts_with("penelope-") && n.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    let recent = files.split_off(files.len().saturating_sub(2));
    let out = filter_log_lines(&recent, turn, session, keep);
    for l in &out {
        println!("{l}");
    }
    if out.is_empty() {
        eprintln!("aucune ligne ne correspond dans {}", dirs.logs().display());
    }
    Ok(())
}

/// Lignes JSON dont le span ou les champs portent ce tour ou cette session.
pub(super) fn filter_log_lines(
    files: &[PathBuf],
    turn: Option<&str>,
    session: Option<&str>,
    keep: usize,
) -> Vec<String> {
    let matches = |v: &Value, key: &str, want: &str| {
        v["span"][key].as_str() == Some(want) || v["fields"][key].as_str() == Some(want)
    };
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        for line in text.lines() {
            let keep_it = match (turn, session) {
                (None, None) => true,
                _ => match serde_json::from_str::<Value>(line) {
                    Ok(v) => {
                        turn.is_some_and(|t| matches(&v, "turn", t))
                            || session.is_some_and(|s| matches(&v, "session", s))
                    }
                    Err(_) => false,
                },
            };
            if keep_it {
                out.push(line.to_string());
            }
        }
    }
    let skip = out.len().saturating_sub(keep.max(1));
    out.split_off(skip)
}
