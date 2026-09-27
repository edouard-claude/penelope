//! Trace de vie du daemon (issue #225) : au démarrage, un fichier d'état dit « en route » ;
//! à l'arrêt, il dit qui l'a demandé et pourquoi. Le démarrage suivant le relit : il
//! suit un arrêt demandé (lequel), ou un arrêt non propre (plantage, coupure, `kill -9`),
//! quand le fichier dit encore « en route ».

pub use crate::ports::Stop;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Dernier état connu du daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    version: String,
    /// Heure locale du démarrage, ou de l'arrêt quand `stopped`.
    at: String,
    stopped: bool,
    #[serde(default)]
    stop: Option<Stop>,
}

fn record_file(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon-run.json")
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn write(state_dir: &Path, r: &Record) {
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let tmp = state_dir.join("daemon-run.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(r)?)?;
        std::fs::rename(&tmp, record_file(state_dir))
    };
    if let Err(error) = write() {
        tracing::warn!(%error, "trace de vie du daemon non écrite");
    }
}

/// Démarrage : ce qu'on sait de l'arrêt précédent (`None` au tout premier démarrage),
/// puis « en route » à sa place.
pub fn on_start(state_dir: &Path, version: &str) -> Option<String> {
    let previous = std::fs::read_to_string(record_file(state_dir))
        .ok()
        .map(|raw| match serde_json::from_str::<Record>(&raw) {
            Ok(r) => describe(&r),
            Err(_) => "inconnu (trace de vie illisible)".to_string(),
        });
    write(
        state_dir,
        &Record {
            version: version.to_string(),
            at: now(),
            stopped: false,
            stop: None,
        },
    );
    previous
}

/// Arrêt propre, avec son origine quand elle est connue.
pub fn on_stop(state_dir: &Path, version: &str, stop: Option<&Stop>) {
    write(
        state_dir,
        &Record {
            version: version.to_string(),
            at: now(),
            stopped: true,
            stop: stop.cloned(),
        },
    );
}

fn describe(r: &Record) -> String {
    match (&r.stop, r.stopped) {
        (Some(stop), true) => format!("{stop}, version {}, le {}", r.version, r.at),
        (None, true) => format!(
            "arrêt sans demande (sortie de la boucle principale), version {}, le {}",
            r.version, r.at
        ),
        (_, false) => format!(
            "arrêt non propre : la version {} tournait depuis le {} (plantage, coupure ou \
             arrêt forcé)",
            r.version, r.at
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_start_has_no_previous_stop() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(on_start(dir.path(), "1.0.2"), None);
    }

    #[test]
    fn a_requested_restart_is_told_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        on_start(dir.path(), "1.0.1");
        let stop = Stop::new("telegram", "/restart", true);
        on_stop(dir.path(), "1.0.1", Some(&stop));
        let told = on_start(dir.path(), "1.0.1").unwrap();
        assert!(
            told.starts_with("redémarrage demandé par telegram (/restart), version 1.0.1, le "),
            "{told}"
        );
    }

    #[test]
    fn a_start_without_a_recorded_stop_follows_an_unclean_stop() {
        let dir = tempfile::tempdir().unwrap();
        on_start(dir.path(), "1.0.1");
        let told = on_start(dir.path(), "1.0.2").unwrap();
        assert!(
            told.starts_with("arrêt non propre : la version 1.0.1 tournait"),
            "{told}"
        );
        // Le second démarrage a réécrit « en route » : un troisième le dit aussi.
        assert!(on_start(dir.path(), "1.0.2").unwrap().contains("1.0.2"));
    }

    #[test]
    fn a_stop_without_origin_is_clean_but_unexplained() {
        let dir = tempfile::tempdir().unwrap();
        on_stop(dir.path(), "1.0.1", None);
        assert!(
            on_start(dir.path(), "1.0.1")
                .unwrap()
                .starts_with("arrêt sans demande")
        );
    }
}
