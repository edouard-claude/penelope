//! L'invite des commandes interactives de la sauvegarde (#328, #329) : une question, une
//! valeur masquée, le magasin de secrets de la machine. Le terminal en vrai ; un double
//! scripté dans les tests, qui ne touchent jamais le vrai trousseau.

use super::*;

pub(super) trait Console {
    /// Une ligne lue à l'invite, `default` quand elle est vide.
    fn ask(&mut self, question: &str, default: &str) -> CliResult<String>;
    /// Une valeur lue sans écho, nettoyée de ses blancs ; vide : refusée.
    fn secret(&mut self, prompt: &str) -> CliResult<String>;
    /// Le magasin de secrets de cette machine, quand le daemon ne répond pas.
    fn store(
        &self,
        dirs: &dyn penelope_platform::Directories,
    ) -> CliResult<Box<dyn penelope_platform::SecretStore>>;
}

/// Le terminal et le magasin de la plateforme (trousseau sur macOS).
pub(super) struct Terminal;

impl Console for Terminal {
    fn ask(&mut self, question: &str, default: &str) -> CliResult<String> {
        if default.is_empty() {
            eprint!("{question} : ");
        } else {
            eprint!("{question} [{default}] : ");
        }
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
            .map_err(|e| CliError::Io(e.to_string()))?;
        let line = line.trim();
        Ok(if line.is_empty() { default } else { line }.to_string())
    }

    fn secret(&mut self, prompt: &str) -> CliResult<String> {
        let v = penelope_platform::terminal::read_secret(prompt)
            .map_err(|e| CliError::Io(e.to_string()))?;
        let v = v.trim().to_string();
        if v.is_empty() {
            return Err(CliError::Usage(
                "valeur vide : rien n'a été enregistré".into(),
            ));
        }
        Ok(v)
    }

    fn store(
        &self,
        dirs: &dyn penelope_platform::Directories,
    ) -> CliResult<Box<dyn penelope_platform::SecretStore>> {
        penelope_platform::backend::secret_store(dirs).map_err(|e| CliError::Io(e.to_string()))
    }
}
