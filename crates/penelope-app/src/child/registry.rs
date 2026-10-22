//! Les enfants lancés (#395, reprise) : une fiche par enfant dans `{state}/children`, son
//! socket à côté. Après un redémarrage, le daemon y retrouve les enfants vivants et les
//! jobs qu'ils servent ; une fiche dont l'enfant est mort et sans résultat est effacée.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// La fiche d'un enfant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub pid: u32,
    pub session_id: String,
    /// Le job d'outil que l'enfant sert ; `None` pour un sous-agent au premier plan.
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub effect_id: Option<String>,
    /// Ce que le résultat porte en plus du texte (`model`, `kind`).
    #[serde(default)]
    pub extra: serde_json::Value,
    pub started_ms: i64,
}

pub fn dir(state: &Path) -> PathBuf {
    state.join("children")
}

fn valid(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Le socket de l'enfant `id`.
pub fn socket(state: &Path, id: &str) -> Option<PathBuf> {
    valid(id).then(|| dir(state).join(format!("{id}.sock")))
}

fn entry_path(state: &Path, id: &str) -> Option<PathBuf> {
    valid(id).then(|| dir(state).join(format!("{id}.json")))
}

/// Écrit la fiche (dossier en `0700`).
pub fn write(state: &Path, e: &Entry) -> std::io::Result<()> {
    let path =
        entry_path(state, &e.id).ok_or_else(|| std::io::Error::other("identifiant invalide"))?;
    std::fs::create_dir_all(dir(state))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir(state), std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(path, serde_json::to_vec(e)?)
}

/// Efface la fiche, le socket et le résultat.
pub fn remove(state: &Path, id: &str) {
    if let (Some(e), Some(s)) = (entry_path(state, id), socket(state, id)) {
        let _ = std::fs::remove_file(e);
        let _ = std::fs::remove_file(super::result_path(&s));
        let _ = std::fs::remove_file(s);
    }
}

/// Toutes les fiches.
pub fn list(state: &Path) -> Vec<Entry> {
    let Ok(entries) = std::fs::read_dir(dir(state)) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter(|e| !e.path().to_string_lossy().ends_with(".result.json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by_key(|e: &Entry| e.started_ms);
    out
}

/// Les jobs encore servis par un enfant : vivant, ou fini avec un résultat à lire. La
/// reprise des jobs au démarrage les laisse ouverts.
pub fn live_jobs(state: &Path) -> Vec<String> {
    list(state)
        .into_iter()
        .filter(|e| {
            penelope_platform::process::process_exists(e.pid)
                || socket(state, &e.id).is_some_and(|s| super::socket::read_result(&s).is_some())
        })
        .filter_map(|e| e.job_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_written_listed_and_removed() {
        let state = tempfile::tempdir().unwrap();
        let e = Entry {
            id: "c1".into(),
            pid: std::process::id(),
            session_id: "s".into(),
            job_id: Some("tj_1".into()),
            effect_id: None,
            extra: serde_json::json!({"kind": "explore"}),
            started_ms: 1,
        };
        write(state.path(), &e).unwrap();
        assert_eq!(list(state.path()), vec![e.clone()]);
        assert_eq!(
            live_jobs(state.path()),
            ["tj_1"],
            "le process de test est vivant"
        );
        remove(state.path(), "c1");
        assert!(list(state.path()).is_empty());
        assert!(socket(state.path(), "../x").is_none());
    }
}
