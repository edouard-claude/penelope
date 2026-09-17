//! Notes de travail d'une session (issue #32).
//!
//! ```text
//! session_notes(update_section) ─► vault/notes/<titre>-<id>.md  (sections fixes)
//! chaque tour ─────────────────► bloc borné en fin de prompt (T4) : survit aux compactions
//! /fork ───────────────────────► notes recopiées pour le fork
//! rêve ────────────────────────► décisions nouvelles en candidats, jamais réécrites
//! ```

use crate::runtime::Services;
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
    let vault = crate::conversation::vault_dir(s);
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
    let vault = crate::conversation::vault_dir(s);
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
pub async fn harvest(s: &Services) -> anyhow::Result<Vec<(String, String)>> {
    let vault = crate::conversation::vault_dir(s);
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
            let key = format!(
                "notes.harvested.{}",
                penelope_kernel::canonical::sha256_hex(format!("{session}|{text}").as_bytes())
            );
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
            s.store
                .write(move |tx| {
                    tx.execute("INSERT OR IGNORE INTO kv(k, v) VALUES(?1, '1')", [key])?;
                    Ok(())
                })
                .await?;
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
    use crate::bus::Origin;
    use crate::compaction::{Trigger, compact};
    use crate::runtime::Daemon;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::MockProvider;
    use penelope_llm::types::ChatMessage;
    use std::sync::Arc;

    async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        (dir, d, p)
    }

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

    /// Issue #32 : après une compaction, le prompt contient toujours les notes à jour ; un
    /// fork en reçoit sa propre copie ; le rêve relève les décisions une seule fois.
    #[tokio::test]
    async fn notes_survive_compaction_are_copied_by_fork_and_harvested_once() {
        let (_dir, d, p) = daemon().await;
        let s = &d.services;
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        s.sessions
            .set_title(&sid, "Refonte facturation", false)
            .await
            .unwrap();
        let h = &s.context.history;
        for i in 0..20 {
            let q = format!("question {i} : {}", "détail ".repeat(340));
            let a = format!("réponse {i} : {}", "analyse ".repeat(300));
            h.append(&sid, &ChatMessage::user(q), 600, 0, false, None)
                .await
                .unwrap();
            h.append(&sid, &ChatMessage::assistant(a), 600, 0, false, None)
                .await
                .unwrap();
        }

        // Session résumée sans notes : le harnais le rappelle.
        p.reply(r#"{"objectif": "migration", "contraintes_et_preferences": "", "fait": "schéma", "en_cours": "migration", "bloque": "", "decisions_cles": "PostgreSQL 17", "fichiers_et_ressources": "", "prochaines_etapes": "migrer", "contexte_critique": ""}"#);
        compact(&d, &sid, Trigger::Manual, None).await.unwrap();
        let reminder = prompt_block(s, &sid).await.expect("rappel");
        assert!(reminder.contains("session_notes"), "{reminder}");

        tool(s, &sid, &json!({"action": "update_section", "section": "Objectif", "content": "Migrer la facturation vers PostgreSQL 17"})).await.unwrap();
        tool(s, &sid, &json!({"action": "update_section", "section": "Décisions", "content": "- Garder les montants en centimes entiers"})).await.unwrap();
        tool(s, &sid, &json!({"action": "update_section", "section": "Décisions", "content": "- Migrer table par table", "mode": "append"})).await.unwrap();

        let tiers =
            crate::conversation::build_tiers_in(s, "on continue", &[], None, Some((&sid, 0)), None)
                .await;
        assert!(
            tiers
                .volatile
                .contains("Migrer la facturation vers PostgreSQL 17"),
            "{}",
            tiers.volatile
        );
        assert!(
            tiers.volatile.contains("centimes entiers")
                && tiers.volatile.contains("table par table")
        );

        let rel = file_of(s, &sid).await.unwrap().expect("fichier de notes");
        assert!(rel.starts_with("notes/refonte-facturation-"), "{rel}");
        let raw = std::fs::read_to_string(crate::conversation::vault_dir(s).join(&rel)).unwrap();
        assert!(
            raw.contains("type: session") && raw.contains(&format!("session: {sid}")),
            "{raw}"
        );

        // Nouvelle compaction : les notes, lues à part, restent dans le prompt.
        for i in 0..10 {
            h.append(
                &sid,
                &ChatMessage::user(format!("encore {i} {}", "x ".repeat(600))),
                600,
                0,
                false,
                None,
            )
            .await
            .unwrap();
            h.append(
                &sid,
                &ChatMessage::assistant(format!("ok {i} {}", "y ".repeat(600))),
                600,
                0,
                false,
                None,
            )
            .await
            .unwrap();
        }
        p.reply(r#"{"objectif": "migration", "contraintes_et_preferences": "", "fait": "schéma", "en_cours": "migration", "bloque": "", "decisions_cles": "PostgreSQL 17", "fichiers_et_ressources": "", "prochaines_etapes": "migrer", "contexte_critique": ""}"#);
        compact(&d, &sid, Trigger::Manual, None).await.unwrap();
        tool(s, &sid, &json!({"action": "update_section", "section": "Prochaine étape", "content": "Écrire la migration des avoirs"})).await.unwrap();
        let tiers = crate::conversation::build_tiers_in(
            s,
            "et maintenant ?",
            &[],
            None,
            Some((&sid, 0)),
            None,
        )
        .await;
        assert!(
            tiers.volatile.contains("Écrire la migration des avoirs"),
            "{}",
            tiers.volatile
        );
        assert!(tiers.volatile.contains("centimes entiers"));

        // Fork : copie propre, modifiable sans toucher l'original.
        let fork = crate::session_ops::fork(&d, &sid, None).await.unwrap();
        let fork = fork["session"].as_str().unwrap().to_string();
        let fork_file = file_of(s, &fork).await.unwrap().expect("notes du fork");
        assert_ne!(fork_file, rel);
        tool(s, &fork, &json!({"action": "update_section", "section": "Objectif", "content": "Variante sans avoirs"})).await.unwrap();
        let original = parse(&read(s, &sid).await.unwrap().unwrap());
        assert_eq!(
            original["Objectif"],
            "Migrer la facturation vers PostgreSQL 17"
        );

        // Rêve : décisions relevées une seule fois.
        let first = harvest(s).await.unwrap();
        assert!(
            first
                .iter()
                .any(|(session, t)| session == &sid
                    && t == "Garder les montants en centimes entiers"),
            "{first:?}"
        );
        let again = harvest(s).await.unwrap();
        assert!(again.is_empty(), "{again:?}");

        let err = tool(
            s,
            &sid,
            &json!({"action": "update_section", "section": "Divers", "content": "x"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("section inconnue"));
    }
}
