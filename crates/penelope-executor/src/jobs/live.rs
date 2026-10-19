//! La sortie d'un job en direct (#397, point 3) : `shell_exec` lancé en arrière-plan
//! copie sa sortie brute dans `{state}/jobs/<id>.log` pendant qu'il tourne ;
//! `penelope job attach <id>` la suit en lecture seule, `job_status` en rend les
//! dernières lignes au modèle. Un fichier `<id>.done` dit que la commande est finie.
//! Une commande qui reçoit un secret collé (#352) n'est pas copiée : la valeur ne doit
//! jamais atterrir en clair sur le disque.

use std::path::{Path, PathBuf};

tokio::task_local! {
    static LOG: PathBuf;
}

/// Lignes de l'écran rendues au modèle.
pub const SCREEN_LINES: usize = 20;
/// Âge au-delà duquel un journal de job est effacé.
const KEEP_SECS: u64 = 24 * 3600;

pub fn dir(state: &Path) -> PathBuf {
    state.join("jobs")
}

/// Le journal du job `id` ; `None` pour un identifiant qui sortirait du dossier.
pub fn log_path(state: &Path, id: &str) -> Option<PathBuf> {
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| dir(state).join(format!("{id}.log")))
}

pub fn done_path(log: &Path) -> PathBuf {
    log.with_extension("done")
}

/// Exécute `f` avec le journal du job `id` comme destination de la copie : ce que
/// `shell_exec` lit par [`current`].
pub async fn scoped<F: std::future::Future>(state: &Path, id: &str, f: F) -> F::Output {
    let Some(path) = log_path(state, id) else {
        return f.await;
    };
    let _ = std::fs::create_dir_all(dir(state));
    prune(&dir(state));
    LOG.scope(path, f).await
}

/// Le journal du job en cours sur cette tâche, s'il y en a un.
pub fn current() -> Option<PathBuf> {
    LOG.try_with(|p| p.clone()).ok()
}

/// La commande est finie : `attach` s'arrête.
pub fn mark_done(log: &Path) {
    let _ = std::fs::write(done_path(log), "");
}

/// Les dernières lignes du journal, sans séquences de terminal.
pub fn screen(state: &Path, id: &str, lines: usize) -> Option<String> {
    let raw = std::fs::read(log_path(state, id)?).ok()?;
    let start = raw.len().saturating_sub(64 * 1024);
    let text = penelope_tools::shell::strip_ansi(&String::from_utf8_lossy(&raw[start..]));
    let all: Vec<&str> = text.lines().collect();
    Some(all[all.len().saturating_sub(lines)..].join("\n"))
}

fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > KEEP_SECS);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_log_is_scoped_to_the_job_and_its_screen_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        assert!(current().is_none());
        let seen = scoped(dir.path(), "j1", async { current() }).await.unwrap();
        assert_eq!(seen, dir.path().join("jobs/j1.log"));
        std::fs::write(&seen, "a\n\x1b[32mb\x1b[0m\nc\n").unwrap();
        assert_eq!(screen(dir.path(), "j1", 2).unwrap(), "b\nc");
        assert!(log_path(dir.path(), "../x").is_none());
    }
}
