//! Ce que le serveur lit dans son environnement : l'adresse CalDAV, le compte, le mot de
//! passe d'application, le fuseau par défaut et, au choix, les calendriers à servir.
//!
//! Pénélope développe `${SECRET:agenda_password}` dans `env` de la déclaration avant de
//! lancer le processus : le mot de passe n'existe que dans l'environnement du serveur.

use chrono_tz::Tz;
use std::time::Duration;

/// Variables lues ; les trois premières sont obligatoires.
pub const URL: &str = "AGENDA_URL";
pub const USER: &str = "AGENDA_USER";
pub const PASSWORD: &str = "AGENDA_PASSWORD";
pub const TIMEZONE: &str = "AGENDA_TIMEZONE";
pub const CALENDARS: &str = "AGENDA_CALENDARS";
pub const TIMEOUT: &str = "AGENDA_TIMEOUT";

/// Délai d'une requête CalDAV par défaut, en secondes.
const DEFAULT_TIMEOUT_S: u64 = 20;

#[derive(Debug, Clone)]
pub struct Settings {
    /// Adresse de découverte : `https://caldav.icloud.com`, `https://caldav.fastmail.com/dav/`,
    /// `https://cloud.exemple.fr/remote.php/dav`.
    pub url: String,
    pub user: String,
    pub password: String,
    /// Fuseau des réponses quand l'appel n'en donne pas.
    pub timezone: Tz,
    /// Noms des calendriers servis ; vide : tous ceux qui portent des événements.
    pub calendars: Vec<String>,
    pub timeout: Duration,
}

impl Settings {
    /// Lit l'environnement du processus.
    pub fn from_env() -> Result<Settings, String> {
        Settings::from_vars(|k| std::env::var(k).ok())
    }

    /// Lit les variables par une fonction : l'environnement, ou une table dans les tests.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Settings, String> {
        let read = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let missing: Vec<&str> = [URL, USER, PASSWORD]
            .into_iter()
            .filter(|k| read(k).is_none())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "variable(s) manquante(s) : {} ; les déclarer dans `env` de mcp.d/agenda.toml, \
                 le mot de passe par `${{SECRET:agenda_password}}` (`penelope secret set \
                 agenda_password`)",
                missing.join(", ")
            ));
        }
        let timezone = match read(TIMEZONE) {
            None => Tz::UTC,
            Some(name) => name
                .parse::<Tz>()
                .map_err(|_| format!("{TIMEZONE} : fuseau inconnu « {name} »"))?,
        };
        let timeout =
            match read(TIMEOUT) {
                None => DEFAULT_TIMEOUT_S,
                Some(s) => s.parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| {
                    format!("{TIMEOUT} : un nombre de secondes attendu, lu « {s} »")
                })?,
            };
        let calendars = read(CALENDARS)
            .map(|v| {
                v.split(',')
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Settings {
            url: read(URL).unwrap_or_default(),
            user: read(USER).unwrap_or_default(),
            password: read(PASSWORD).unwrap_or_default(),
            timezone,
            calendars,
            timeout: Duration::from_secs(timeout),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn table(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn missing_variables_are_all_named_at_once() {
        let err = Settings::from_vars(table(&[(USER, "anne")])).unwrap_err();
        assert!(err.contains("AGENDA_URL, AGENDA_PASSWORD"), "{err}");
        assert!(err.contains("SECRET:agenda_password"), "{err}");
        let err =
            Settings::from_vars(table(&[(URL, "x"), (USER, " "), (PASSWORD, "p")])).unwrap_err();
        assert!(err.contains("AGENDA_USER"), "{err}");
    }

    #[test]
    fn defaults_and_optional_variables() {
        let s = Settings::from_vars(table(&[
            (URL, "https://caldav.icloud.com"),
            (USER, "anne@exemple.fr"),
            (PASSWORD, "xxxx-xxxx"),
        ]))
        .unwrap();
        assert_eq!(s.timezone, Tz::UTC);
        assert!(s.calendars.is_empty());
        assert_eq!(s.timeout, Duration::from_secs(20));

        let s = Settings::from_vars(table(&[
            (URL, "https://caldav.icloud.com"),
            (USER, "anne@exemple.fr"),
            (PASSWORD, "xxxx-xxxx"),
            (TIMEZONE, "Indian/Reunion"),
            (CALENDARS, "Perso, Famille,,"),
            (TIMEOUT, "5"),
        ]))
        .unwrap();
        assert_eq!(s.timezone, chrono_tz::Indian::Reunion);
        assert_eq!(s.calendars, vec!["Perso", "Famille"]);
        assert_eq!(s.timeout, Duration::from_secs(5));
    }

    #[test]
    fn a_bad_timezone_or_timeout_is_refused_by_name() {
        let base = [
            (URL, "https://caldav.icloud.com"),
            (USER, "anne"),
            (PASSWORD, "p"),
        ];
        let mut v = base.to_vec();
        v.push((TIMEZONE, "Mars/Olympus"));
        let err = Settings::from_vars(table(&v)).unwrap_err();
        assert!(
            err.contains("AGENDA_TIMEZONE") && err.contains("Mars/Olympus"),
            "{err}"
        );
        let mut v = base.to_vec();
        v.push((TIMEOUT, "0"));
        let err = Settings::from_vars(table(&v)).unwrap_err();
        assert!(err.contains("AGENDA_TIMEOUT"), "{err}");
    }
}
