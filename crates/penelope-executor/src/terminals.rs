//! Les terminaux que le propriétaire partage avec l'agent (#397, point 3) :
//! `penelope term` ouvre son shell sous `script`, qui copie l'écran dans
//! `{state}/terms/<id>.log` ; une fiche `<id>.json` dit qu'il est ouvert. L'outil
//! `read_terminal` en rend les dernières lignes au modèle, nettoyées des séquences de
//! terminal et citées comme données. Il ne fait que lire : rien n'écrit jamais dans un
//! terminal du propriétaire.
//!
//! L'accord se donne au lancement (`penelope term` le demande, `--share` le donne
//! d'avance) ; sans lui, le shell tourne sans copie. À la fermeture du terminal, fiche et
//! copie sont effacées : un terminal fermé ne se relit plus.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Lignes rendues par défaut, et au plus.
pub const DEFAULT_LINES: usize = 40;
pub const MAX_LINES: usize = 200;
/// Une fiche sans écriture depuis un jour est celle d'un terminal tombé sans se fermer.
const STALE_SECS: u64 = 24 * 3600;

pub fn dir(state: &Path) -> PathBuf {
    state.join("terms")
}

fn valid(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn log_path(state: &Path, id: &str) -> Option<PathBuf> {
    valid(id).then(|| dir(state).join(format!("{id}.log")))
}

fn card_path(state: &Path, id: &str) -> Option<PathBuf> {
    valid(id).then(|| dir(state).join(format!("{id}.json")))
}

/// Ouvre la fiche d'un terminal partagé : dossier 0700, fiche et copie 0600. Rend le
/// chemin de la copie, que `script` remplit.
pub fn register(state: &Path, id: &str, title: &str, started_at: &str) -> std::io::Result<PathBuf> {
    let (Some(log), Some(card)) = (log_path(state, id), card_path(state, id)) else {
        return Err(std::io::Error::other(format!(
            "identifiant invalide : {id}"
        )));
    };
    std::fs::create_dir_all(dir(state))?;
    std::fs::write(&log, "")?;
    std::fs::write(
        &card,
        json!({"id": id, "title": title, "started_at": started_at}).to_string(),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir(state), std::fs::Permissions::from_mode(0o700))?;
        for p in [&log, &card] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(log)
}

/// Le terminal est fermé : sa fiche et sa copie disparaissent.
pub fn unregister(state: &Path, id: &str) {
    for p in [log_path(state, id), card_path(state, id)]
        .into_iter()
        .flatten()
    {
        let _ = std::fs::remove_file(p);
    }
}

fn age_secs(p: &Path) -> Option<u64> {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

/// Les terminaux ouverts, le plus récent d'abord. Les fiches tombées sont effacées.
pub fn list(state: &Path) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(dir(state)) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        let log = log_path(state, &id);
        let stale = log
            .as_deref()
            .and_then(age_secs)
            .is_none_or(|a| a > STALE_SECS);
        if stale {
            unregister(state, &id);
            continue;
        }
        if let Some(card) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
        {
            out.push(card);
        }
    }
    out.sort_by(|a, b| b["started_at"].as_str().cmp(&a["started_at"].as_str()));
    out
}

/// Les `lines` dernières lignes du terminal `id`, sans séquences de terminal.
pub fn screen(state: &Path, id: &str, lines: usize) -> Option<String> {
    let raw = std::fs::read(log_path(state, id)?).ok()?;
    let start = raw.len().saturating_sub(256 * 1024);
    let text = penelope_tools::shell::strip_ansi(&String::from_utf8_lossy(&raw[start..]));
    let all: Vec<&str> = text.lines().collect();
    Some(all[all.len().saturating_sub(lines)..].join("\n"))
}

/// `read_terminal` : l'écran d'un terminal partagé, ou la liste quand il n'est pas
/// désigné et qu'il n'y en a pas exactement un.
pub fn tool(state: &Path, args: &Value) -> Result<Value, String> {
    let lines = args["lines"]
        .as_u64()
        .map_or(DEFAULT_LINES, |n| (n as usize).clamp(1, MAX_LINES));
    let open = list(state);
    let id = match args["terminal"].as_str().filter(|t| !t.is_empty()) {
        Some(id) => id.to_string(),
        None if open.len() == 1 => open[0]["id"].as_str().unwrap_or_default().to_string(),
        None if open.is_empty() => {
            return Ok(json!({
                "terminals": [],
                "note": "aucun terminal partagé : le propriétaire en ouvre un avec \
                         `penelope term` et accepte qu'il soit lu",
            }));
        }
        None => {
            return Ok(json!({
                "terminals": open,
                "note": "plusieurs terminaux partagés : désigne-le par `terminal`",
            }));
        }
    };
    if !open.iter().any(|t| t["id"] == id) {
        return Err(format!(
            "terminal `{id}` introuvable ou fermé (ouverts : {})",
            open.iter()
                .filter_map(|t| t["id"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let text = screen(state, &id, lines).unwrap_or_default();
    Ok(json!({
        "terminal": id,
        "lines": lines,
        "screen": penelope_observe::injection::wrap_untrusted(
            &format!("terminal {id} du propriétaire"),
            &text,
        ),
    }))
}

#[cfg(test)]
mod tests;
