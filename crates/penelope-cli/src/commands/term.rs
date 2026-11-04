//! `penelope term` (#397, point 3) : un shell du propriétaire que l'agent peut lire avec
//! `read_terminal`, sur son accord donné au lancement (`--share` le donne d'avance).
//! `script` copie l'écran dans `{state}/terms/<id>.log` ; à la fermeture, la copie et la
//! fiche sont effacées. Refusé, le shell s'ouvre sans copie. L'agent n'y écrit jamais.

use super::*;

/// La commande qui lance `shell` sous `script`, copie dans `log` vidée au fil de l'eau.
fn script_argv(shell: &str, log: &std::path::Path) -> Vec<String> {
    let log = log.to_string_lossy().to_string();
    if cfg!(target_os = "macos") {
        vec!["-q".into(), "-F".into(), log, shell.into()]
    } else {
        vec!["-q".into(), "-f".into(), "-c".into(), shell.into(), log]
    }
}

fn accepted() -> bool {
    eprint!(
        "Pénélope peut lire ce terminal (lecture seule, copie effacée à la fermeture) ? [o/N] "
    );
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok()
        && matches!(line.trim().to_lowercase().as_str(), "o" | "oui")
}

pub(super) fn run(cli: &Cli, share: bool) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let status = if share || accepted() {
        let state = dirs.state();
        let id = penelope_kernel::ids::Ulid::new().to_string().to_lowercase();
        let id = id[id.len() - 8..].to_string();
        let title = std::path::Path::new(&shell)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let log = penelope_executor::terminals::register(
            &state,
            &id,
            &title,
            &chrono::Utc::now().to_rfc3339(),
        )
        .map_err(|e| CliError::Io(e.to_string()))?;
        eprintln!("Terminal `{id}` partagé avec Pénélope (`read_terminal`) ; `exit` le ferme.");
        let status = std::process::Command::new("script")
            .args(script_argv(&shell, &log))
            .env("PENELOPE_TERM", &id)
            .status();
        penelope_executor::terminals::unregister(&state, &id);
        eprintln!("Terminal `{id}` fermé, sa copie effacée.");
        status
    } else {
        eprintln!("Non partagé : Pénélope ne lira pas ce terminal.");
        std::process::Command::new(&shell).status()
    };
    status
        .map(|_| ())
        .map_err(|e| CliError::Io(format!("{shell} : {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_copies_the_screen_into_the_log() {
        let argv = script_argv("/bin/zsh", std::path::Path::new("/s/terms/t.log"));
        assert!(argv.contains(&"/s/terms/t.log".to_string()));
        assert!(argv.contains(&"/bin/zsh".to_string()));
        assert_eq!(argv[0], "-q");
    }
}
