//! Saisie au terminal (§13.1) : un secret tapé ou collé ne s'affiche jamais.
//!
//! En SSH, `pbpaste` lit le presse-papiers de la machine distante : la seule façon
//! fiable de passer un secret est de le coller dans le terminal, sans écho.

use std::io::{BufRead, IsTerminal, Read, Write};

/// Lit une valeur secrète.
///
/// - entrée standard redirigée (`pbpaste | …`, `< fichier`) : tout le flux ;
/// - terminal interactif : invite sur la sortie d'erreur, une ligne, **sans écho**.
pub fn read_secret(prompt: &str) -> std::io::Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut raw = String::new();
        stdin.lock().read_to_string(&mut raw)?;
        return Ok(raw);
    }

    let mut err = std::io::stderr();
    write!(err, "{prompt}")?;
    err.flush()?;

    let echo_off = set_echo(false);
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if echo_off {
        set_echo(true);
    }
    writeln!(err)?;
    read?;
    Ok(line)
}

/// Active ou coupe l'écho du terminal. Vrai si le réglage a pu être appliqué.
#[cfg(unix)]
fn set_echo(on: bool) -> bool {
    std::process::Command::new("/bin/stty")
        .arg(if on { "echo" } else { "-echo" })
        .stdin(std::process::Stdio::inherit())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn set_echo(_on: bool) -> bool {
    false
}
