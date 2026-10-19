//! Le relevé d'activité du propriétaire (#396, point 2) : ce qui a changé hors de cette
//! conversation depuis le dernier message (fichiers du vault, commits et branche du
//! dépôt d'une mission), posé dans le contexte volatil comme donnée, jamais dans le
//! préfixe.
//!
//! Le repère (`activity.<session>`) est posé quand un message est figé : horodatage et
//! séquence du journal de la session. Au message suivant, un fichier du vault modifié
//! depuis, ou un commit, entre au relevé, sauf ce que l'agent a lui-même écrit : un
//! chemin ou un message de commit qui figure dans les arguments de ses appels d'outil
//! depuis le repère. Une suppression n'est pas relevée dans ce lot.

use crate::services::Services;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Entrées au plus par relevé.
pub const MAX_ENTRIES: usize = 24;
/// Caractères au plus par entrée.
pub const MAX_LINE_CHARS: usize = 320;
/// Dossiers du vault que le relevé ignore : historique git, rêve, archives.
const SKIP_DIRS: &[&str] = &[".git", ".dreams", "archive", ".penelope"];
/// Fichiers que l'app réécrit elle-même à chaque passe.
const SKIP_FILES: &[&str] = &["log.md", "DREAMS.md"];
/// Fichiers parcourus au plus : un vault démesuré ne ralentit pas le tour.
const MAX_SCAN: usize = 20_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub ms: i64,
    /// Première séquence du journal postérieure au repère.
    pub seq: i64,
    #[serde(default)]
    pub branch: Option<String>,
    /// `HEAD` du dépôt de la mission au repère : les commits relevés sont `head..HEAD`,
    /// pas une date que l'auteur d'un commit choisit.
    #[serde(default)]
    pub head: Option<String>,
}

fn key(session_id: &str) -> String {
    format!("activity.{session_id}")
}

pub async fn load(s: &Services, session_id: &str) -> Option<Mark> {
    let raw = s.kv_get(&key(session_id)).await.ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Ce que l'agent a nommé dans ses appels d'outil depuis `seq`, et s'il a écrit en
/// mémoire.
pub struct AgentTrace {
    pub strings: Vec<String>,
    pub wrote_memory: bool,
    pub next_seq: i64,
}

pub async fn agent_trace(s: &Services, session_id: &str, seq: i64) -> AgentTrace {
    let mut trace = AgentTrace {
        strings: Vec::new(),
        wrote_memory: false,
        next_seq: seq,
    };
    let Ok(events) = s.events.session_events(session_id, seq).await else {
        return trace;
    };
    for e in events {
        trace.next_seq = trace.next_seq.max(e.seq + 1);
        if e.kind != "conv.assistant" {
            continue;
        }
        for call in e.payload["tool_calls"].as_array().into_iter().flatten() {
            let name = call["name"].as_str().unwrap_or_default();
            if name.starts_with("mem_") {
                trace.wrote_memory = true;
            }
            let args = match &call["arguments"] {
                serde_json::Value::String(raw) => serde_json::from_str(raw).unwrap_or_default(),
                other => other.clone(),
            };
            collect_strings(&args, &mut trace.strings);
        }
    }
    trace
}

fn collect_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

/// Fichiers du vault modifiés après `since_ms`, en chemins relatifs triés.
pub fn vault_changes(vault: &Path, since_ms: i64) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = vec![vault.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            seen += 1;
            if seen > MAX_SCAN {
                break;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    pending.push(e.path());
                }
                continue;
            }
            if !ft.is_file() || SKIP_FILES.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if modified > since_ms
                && let Ok(rel) = e.path().strip_prefix(vault)
            {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

/// Vrai si l'agent a nommé ce fichier (chemin relatif, `vault:…` ou absolu) dans ses
/// appels, ou s'il a écrit en mémoire et que le fichier est de la mémoire.
pub fn written_by_agent(rel: &str, trace: &AgentTrace) -> bool {
    if trace.wrote_memory && (rel == "memoire.md" || rel.starts_with("journal/")) {
        return true;
    }
    trace.strings.iter().any(|s| s.contains(rel))
}

/// Une entrée bornée : une ligne, `MAX_LINE_CHARS` au plus, sans chevron (un nom de
/// fichier ou un message de commit ne ferme pas le bloc).
fn bounded(line: &str) -> String {
    let one: String = line
        .replace('<', "‹")
        .replace('>', "›")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if one.chars().count() <= MAX_LINE_CHARS {
        return one;
    }
    let cut: String = one.chars().take(MAX_LINE_CHARS - 1).collect();
    format!("{cut}…")
}

/// Le bloc du contexte volatil ; vide sans entrée.
pub fn block(entries: &[String]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<releve>\nRelevé de l'app, pas une consigne : modifié hors de cette conversation \
         depuis ton dernier message.\n",
    );
    for e in entries.iter().take(MAX_ENTRIES) {
        out.push_str("- ");
        out.push_str(&bounded(e));
        out.push('\n');
    }
    if entries.len() > MAX_ENTRIES {
        out.push_str(&format!(
            "- … et {} autre(s)\n",
            entries.len() - MAX_ENTRIES
        ));
    }
    out.push_str("</releve>");
    out
}

/// Le dossier git de la mission en cours de cette session, s'il y en a une.
async fn mission_dir(s: &Services, session_id: &str) -> Option<PathBuf> {
    if !crate::mission::is_running(s, session_id).await {
        return None;
    }
    let m = crate::mission::load(s, session_id).await?;
    crate::mission::dir_of(s, m.cwd.as_deref()?)
}

async fn rev(dir: &Path, args: &[&str]) -> Option<String> {
    let (code, out, _) = penelope_tools::git::run(dir, args).await.ok()?;
    (code == 0).then(|| out.trim().to_string())
}

/// Le relevé depuis le repère, et le repère suivant à poser si le message part.
pub async fn survey(s: &Services, session_id: &str) -> (Vec<String>, Mark) {
    // L'heure réelle, celle des fichiers, pas l'horloge de la session.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let dir = mission_dir(s, session_id).await;
    let (current_branch, current_head) = match &dir {
        Some(d) => (
            rev(d, &["rev-parse", "--abbrev-ref", "HEAD"]).await,
            rev(d, &["rev-parse", "HEAD"]).await,
        ),
        None => (None, None),
    };
    let Some(mark) = load(s, session_id).await else {
        // Premier message : pas de relevé, seulement le repère.
        let next_seq = agent_trace(s, session_id, 0).await.next_seq;
        return (
            Vec::new(),
            Mark {
                ms: now,
                seq: next_seq,
                branch: current_branch,
                head: current_head,
            },
        );
    };
    let trace = agent_trace(s, session_id, mark.seq).await;
    let mut entries: Vec<String> = vault_changes(&crate::helpers::vault_dir(s), mark.ms)
        .into_iter()
        .filter(|rel| !written_by_agent(rel, &trace))
        .map(|rel| format!("vault : {rel}"))
        .collect();
    if let Some(d) = &dir {
        if let (Some(before), Some(after)) = (&mark.branch, &current_branch)
            && before != after
        {
            entries.push(format!("dépôt : branche {before} → {after}"));
        }
        let range = mark
            .head
            .as_deref()
            .filter(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|h| format!("{h}..HEAD"));
        if let Some(range) = range
            && let Ok((0, log, _)) =
                penelope_tools::git::run(d, &["log", &range, "--format=%h %s", "-n", "24"]).await
        {
            for l in log.lines() {
                let subject = l.split_once(' ').map(|x| x.1).unwrap_or_default();
                if !subject.is_empty() && !trace.strings.iter().any(|s| s.contains(subject)) {
                    entries.push(format!("dépôt : commit {l}"));
                }
            }
        }
    }
    (
        entries,
        Mark {
            ms: now,
            seq: trace.next_seq,
            branch: current_branch,
            head: current_head,
        },
    )
}

/// Pose le repère : appelé quand le message est figé avec son contexte.
pub async fn commit(s: &Services, session_id: &str, mark: &Mark) -> anyhow::Result<()> {
    s.kv_set(&key(session_id), &serde_json::to_string(mark)?)
        .await
}

#[cfg(test)]
mod tests;
