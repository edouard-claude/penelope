//! Confirmation de `penelope session purge` (issue #46, arbitrage 3 de la V1).
//!
//! Avant d'effacer, la CLI lit ce que la purge emporterait (`session.purge_preview`) : une
//! session dont d'autres sont nées par fork leur retire leur début, et le propriétaire
//! veut l'entendre avant d'agir. Sans terminal pour répondre, pas d'effacement sans
//! `--yes`.

use super::*;
use std::io::{BufRead, IsTerminal, Write};

/// Vrai si la purge peut partir : la session est lue, l'avertissement affiché, la
/// réponse lue sur l'entrée standard.
pub(super) async fn confirm(cli: &Cli, session: &str) -> CliResult<bool> {
    let socket = socket_path(cli.home.clone())?;
    let preview = call(
        &socket,
        m::SESSION_PURGE_PREVIEW,
        json!({"session": session}),
    )
    .await?;
    let interactive = std::io::stdin().is_terminal();
    decide(
        &preview,
        session,
        interactive,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
    )
}

/// La décision, hors des flux du processus pour être testée.
pub(super) fn decide(
    preview: &Value,
    session: &str,
    interactive: bool,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> CliResult<bool> {
    let io = |e: std::io::Error| CliError::Io(e.to_string());
    if let Some(warning) = preview["avertissement"].as_str() {
        writeln!(out, "{warning}").map_err(io)?;
    }
    if !interactive {
        return Err(CliError::Usage(
            "aucun terminal pour confirmer la purge : rien n'a été effacé. Relancer avec \
             `--yes` pour effacer sans confirmation."
                .into(),
        ));
    }
    let label = preview["title"].as_str().unwrap_or(session);
    write!(
        out,
        "Effacer définitivement le contenu de la session « {label} » (messages, résumés, \
         artefacts) ? La chaîne d'audit garde ses lignes, sans leur contenu. \
         [o]ui / [n]on : "
    )
    .map_err(io)?;
    out.flush().map_err(io)?;
    let mut answer = String::new();
    input.read_line(&mut answer).map_err(io)?;
    if matches!(
        answer.trim().to_lowercase().as_str(),
        "o" | "oui" | "y" | "yes"
    ) {
        return Ok(true);
    }
    writeln!(out, "Rien n'a été effacé.").map_err(io)?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(preview: &Value, interactive: bool, answer: &str) -> (CliResult<bool>, String) {
        let mut out = Vec::new();
        let r = decide(
            preview,
            "s_1",
            interactive,
            &mut answer.as_bytes(),
            &mut out,
        );
        (r, String::from_utf8(out).unwrap())
    }

    fn with_forks() -> Value {
        json!({
            "session": "s_1", "title": "Mère (24/09)",
            "forks": [{"id": "s_2", "title": "Fille (24/09)"}],
            "avertissement": "Cette session a un fork, il perdra son début : s_2 « Fille (24/09) ».",
        })
    }

    #[test]
    fn a_session_with_forks_is_announced_before_the_question() {
        let (r, out) = run(&with_forks(), true, "o\n");
        assert!(r.unwrap());
        let warning = out.find("il perdra son début").expect("avertissement");
        let question = out.find("Effacer définitivement").expect("question");
        assert!(warning < question, "{out}");
        assert!(out.contains("« Mère (24/09) »"), "{out}");
    }

    #[test]
    fn a_session_without_forks_asks_without_warning() {
        let preview = json!({"session": "s_1", "title": "Seule", "forks": []});
        let (r, out) = run(&preview, true, "n\n");
        assert!(!r.unwrap());
        assert!(!out.contains("fork"), "{out}");
        assert!(out.contains("Rien n'a été effacé."), "{out}");
    }

    #[test]
    fn without_a_terminal_the_purge_is_refused_after_the_warning() {
        let (r, out) = run(&with_forks(), false, "o\n");
        let Err(CliError::Usage(msg)) = r else {
            panic!("refus attendu : {r:?}")
        };
        assert!(msg.contains("--yes"), "{msg}");
        assert!(out.contains("il perdra son début"), "{out}");
        assert!(!out.contains("Effacer définitivement"), "{out}");
    }

    #[test]
    fn yes_skips_the_confirmation_and_purges_directly() {
        let c = Cli::try_parse_from(["penelope", "session", "purge", "s_1", "--yes"]).unwrap();
        let Command::Session(SessionCmd::Purge { yes, .. }) = &c.command else {
            panic!("session purge attendu")
        };
        assert!(*yes);
        assert_eq!(route(&c.command).unwrap().0, m::SESSION_PURGE);
    }
}
