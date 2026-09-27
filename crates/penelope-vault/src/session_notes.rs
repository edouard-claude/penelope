//! Notes de travail d'une session (issue #32).
//!
//! ```text
//! session_notes(update_section) ─► vault/notes/<titre>-<id>.md  (sections fixes)
//! chaque tour ─────────────────► bloc borné en fin de prompt (T4) : survit aux compactions
//! /fork ───────────────────────► notes recopiées pour le fork
//! rêve ────────────────────────► décisions nouvelles en candidats, jamais réécrites
//! ```

use penelope_app::services::Services;
use penelope_kernel::session::MetadataOp;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Sections d'un fichier de notes, dans l'ordre.
pub const SECTIONS: [&str; 6] = [
    "Objectif",
    "Plan",
    "Décisions",
    "Fichiers touchés",
    "Points ouverts",
    "Prochaine étape",
];
pub const DIR: &str = "notes";
/// Clé de métadonnées de session qui porte le fichier de notes.
pub const META_KEY: &str = "notes_file";
/// Borne du bloc injecté dans le prompt (environ 1 500 tokens).
pub const PROMPT_CHARS: usize = 6_000;

fn section_of(name: &str) -> Option<&'static str> {
    let wanted = name.trim().to_lowercase();
    SECTIONS
        .iter()
        .find(|s| s.to_lowercase() == wanted)
        .copied()
}

/// Sections d'un fichier de notes, contenu sans les titres.
pub fn parse(raw: &str) -> BTreeMap<&'static str, String> {
    let body = penelope_memory::wiki::body_of(raw);
    let mut out: BTreeMap<&'static str, String> = BTreeMap::new();
    let mut current: Option<&'static str> = None;
    for line in body.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            current = section_of(h);
            continue;
        }
        if let Some(sec) = current {
            let entry = out.entry(sec).or_default();
            entry.push_str(line);
            entry.push('\n');
        }
    }
    for v in out.values_mut() {
        *v = v.trim().to_string();
    }
    out
}

/// Rend un fichier de notes complet.
pub fn render(title: &str, sections: &BTreeMap<&'static str, String>) -> String {
    let mut t = format!("# Notes de travail · {title}\n");
    for sec in SECTIONS {
        t.push_str(&format!("\n## {sec}\n\n"));
        if let Some(c) = sections.get(sec).filter(|c| !c.is_empty()) {
            t.push_str(c);
            t.push('\n');
        }
    }
    t
}

/// Fichier de notes d'une session, s'il existe déjà.
pub async fn file_of(s: &Services, session_id: &str) -> anyhow::Result<Option<String>> {
    Ok(s.sessions
        .get(session_id)
        .await?
        .and_then(|x| x.metadata[META_KEY].as_str().map(String::from)))
}

/// Titre nu d'une session, sans la date que porte son libellé.
fn plain_title(sess: &penelope_kernel::session::Session) -> String {
    sess.title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("session")
        .to_string()
}

/// Chemin d'un nouveau fichier de notes : titre de la session et fin de son identifiant.
async fn new_file(s: &Services, session_id: &str) -> anyhow::Result<(String, String)> {
    let sess = s.sessions.require(session_id).await?;
    let title = plain_title(&sess);
    let slug = penelope_memory::ingest::slugify(&title.replace(['/', '\\', '.'], " "));
    let suffix: String = session_id
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>()
        .to_lowercase();
    Ok((format!("{DIR}/{slug}-{suffix}.md"), title))
}

/// Contenu brut des notes d'une session, `None` si elles n'existent pas.
pub async fn read(s: &Services, session_id: &str) -> anyhow::Result<Option<String>> {
    let Some(rel) = file_of(s, session_id).await? else {
        return Ok(None);
    };
    let vault = penelope_app::helpers::vault_dir(s);
    Ok(std::fs::read_to_string(vault.join(rel)).ok())
}

/// Remplace (ou complète) une section ; le fichier est créé au premier appel.
pub async fn update(
    s: &Services,
    session_id: &str,
    section: &str,
    content: &str,
    append: bool,
) -> Result<String, String> {
    let sec = section_of(section)
        .ok_or_else(|| format!("section inconnue `{section}` : {}", SECTIONS.join(", ")))?;
    crate::vault_ops::write_filter_block(content)?;
    let vault = penelope_app::helpers::vault_dir(s);
    let (rel, title) = match file_of(s, session_id).await.map_err(|e| e.to_string())? {
        Some(rel) => {
            let sess = s
                .sessions
                .require(session_id)
                .await
                .map_err(|e| e.to_string())?;
            (rel, plain_title(&sess))
        }
        None => {
            let (rel, title) = new_file(s, session_id).await.map_err(|e| e.to_string())?;
            s.sessions
                .metadata(session_id, MetadataOp::Set, META_KEY, json!(rel))
                .await
                .map_err(|e| e.to_string())?;
            (rel, title)
        }
    };
    let day = crate::vault_ops::day(s);
    let session = session_id.to_string();
    crate::vault_ops::update_note(&vault, &rel, None, &day, |raw| {
        let mut sections = parse(raw);
        let entry = sections.entry(sec).or_default();
        if append && !entry.trim().is_empty() {
            entry.push('\n');
            entry.push_str(content.trim());
        } else {
            *entry = content.trim().to_string();
        }
        let body = render(&title, &sections);
        let with_session = penelope_memory::wiki::touch(
            &penelope_memory::wiki::replace_body(raw, &body),
            "session",
            &day,
            false,
            &[("session", &session)],
        );
        Ok(with_session)
    })?;
    Ok(rel)
}

/// Bloc des notes pour la fin du prompt, borné ; un rappel quand elles manquent alors que
/// la conversation a déjà été résumée.
pub async fn prompt_block(s: &Services, session_id: &str) -> Option<String> {
    let raw = read(s, session_id).await.ok().flatten();
    let sections = raw.as_deref().map(parse).unwrap_or_default();
    let filled: Vec<(&str, &String)> = SECTIONS
        .iter()
        .filter_map(|sec| {
            sections
                .get(sec)
                .filter(|c| !c.trim().is_empty())
                .map(|c| (*sec, c))
        })
        .collect();
    if filled.is_empty() {
        let compacted = s
            .context
            .lcm
            .active_nodes(session_id)
            .await
            .map(|n| !n.is_empty())
            .unwrap_or(false);
        return compacted.then(|| {
            "[Harnais : la conversation a été résumée et cette session n'a pas de notes de \
             travail. Consigne l'objectif, le plan, les décisions et la prochaine étape avec \
             `session_notes`.]"
                .to_string()
        });
    }
    let mut t = String::from(
        "Notes de travail de cette session (tiens-les à jour avec `session_notes` aux étapes \
         clés) :\n",
    );
    for (sec, content) in filled {
        t.push_str(&format!("\n## {sec}\n{content}\n"));
    }
    if t.chars().count() > PROMPT_CHARS {
        let kept: String = t.chars().take(PROMPT_CHARS).collect();
        t = format!("{kept}\n[… notes coupées : `session_notes` action `read` pour tout lire]");
    }
    Some(t)
}

/// Recopie les notes d'une session pour une autre (`/fork`, reprise sur le même sujet).
pub async fn copy(s: &Services, from: &str, to: &str) -> anyhow::Result<bool> {
    let Some(raw) = read(s, from).await? else {
        return Ok(false);
    };
    let sections = parse(&raw);
    for sec in SECTIONS {
        if let Some(c) = sections.get(sec).filter(|c| !c.is_empty()) {
            update(s, to, sec, c, false)
                .await
                .map_err(anyhow::Error::msg)?;
        }
    }
    Ok(true)
}

/// Notes d'autres sessions au titre proche : (session, titre).
pub async fn similar(s: &Services, session_id: &str, title: &str) -> Vec<(String, String)> {
    let words = |t: &str| -> std::collections::BTreeSet<String> {
        crate::concepts::normalize(t)
            .split_whitespace()
            .filter(|w| w.chars().count() > 3)
            .map(String::from)
            .collect()
    };
    let wanted = words(title);
    if wanted.is_empty() {
        return Vec::new();
    }
    let sessions = s
        .sessions
        .list(Some(penelope_kernel::session::SessionKind::Chat), 200)
        .await
        .unwrap_or_default();
    sessions
        .into_iter()
        .filter(|x| x.id.as_str() != session_id && x.metadata[META_KEY].is_string())
        .filter_map(|x| {
            let label = plain_title(&x);
            let theirs = words(&label);
            let shared = wanted.intersection(&theirs).count();
            (shared * 2 >= wanted.len().max(1)).then(|| (x.id.to_string(), label))
        })
        .take(3)
        .collect()
}

/// Décisions des notes modifiées depuis `since`, pas encore relevées : candidats du rêve.
/// Marque une décision comme récoltée : à appeler seulement quand son candidat a été
/// enregistré (issue #61).
pub async fn mark_harvested(s: &Services, session: &str, text: &str) -> anyhow::Result<()> {
    let key = harvest_key(session, text);
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO kv(k, v, ts)
                 VALUES(?1, '1', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [key],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn harvest_key(session: &str, text: &str) -> String {
    format!(
        "notes.harvested.{}",
        penelope_kernel::canonical::sha256_hex(format!("{session}|{text}").as_bytes())
    )
}

pub async fn harvest(s: &Services) -> anyhow::Result<Vec<(String, String)>> {
    let vault = penelope_app::helpers::vault_dir(s);
    let mut out = Vec::new();
    for e in std::fs::read_dir(vault.join(DIR))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(raw) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let session = penelope_kernel::frontmatter::parse(&raw)
            .map(|fm| fm.string("session"))
            .unwrap_or_default();
        if session.is_empty() {
            continue;
        }
        let Some(decisions) = parse(&raw).remove("Décisions") else {
            continue;
        };
        for line in decisions.lines() {
            let text = line.trim().trim_start_matches(['-', '*']).trim();
            if text.chars().count() < 8 {
                continue;
            }
            let key = harvest_key(&session, text);
            let seen = s
                .store
                .read({
                    let key = key.clone();
                    move |c| {
                        Ok(
                            c.query_row("SELECT 1 FROM kv WHERE k = ?1", [key], |_| Ok(()))
                                .is_ok(),
                        )
                    }
                })
                .await?;
            if seen {
                continue;
            }
            // Le marqueur « récoltée » est posé par l'appelant, une fois le candidat
            // accepté : sinon une décision disparaît sans jamais devenir candidat
            // (issue #61).
            out.push((session.clone(), text.to_string()));
        }
    }
    Ok(out)
}

/// Réponse de l'outil `session_notes`.
pub async fn tool(s: &Services, session_id: &str, args: &Value) -> Result<Value, String> {
    match args["action"].as_str().unwrap_or("read") {
        "read" => {
            let raw = read(s, session_id).await.map_err(|e| e.to_string())?;
            let sections = raw.as_deref().map(parse).unwrap_or_default();
            Ok(json!({
                "file": file_of(s, session_id).await.map_err(|e| e.to_string())?,
                "sections": SECTIONS.iter().map(|sec| json!({
                    "section": sec,
                    "content": sections.get(sec).cloned().unwrap_or_default(),
                })).collect::<Vec<_>>(),
            }))
        }
        "update_section" => {
            let section = args["section"].as_str().ok_or("`section` manquant")?;
            let content = args["content"].as_str().ok_or("`content` manquant")?;
            let append = args["mode"].as_str() == Some("append");
            let rel = update(s, session_id, section, content, append).await?;
            Ok(
                json!({"file": rel, "section": section, "mode": if append { "append" } else { "replace" }}),
            )
        }
        other => Err(format!(
            "action inconnue `{other}` : read ou update_section"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_round_trip_in_order() {
        let mut sections = BTreeMap::new();
        sections.insert("Plan", "1. lire\n2. corriger".to_string());
        sections.insert("Objectif", "Réparer la facturation".to_string());
        let raw = render("PROJ-7", &sections);
        assert!(raw.find("## Objectif").unwrap() < raw.find("## Plan").unwrap());
        let back = parse(&raw);
        assert_eq!(back["Plan"], "1. lire\n2. corriger");
        assert_eq!(back["Décisions"], "");
    }
}
