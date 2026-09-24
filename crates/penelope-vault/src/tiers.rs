//! Tuiles du prompt d'une session (§5.2, §6.3) : T0 à T2 stables (âme, consignes, outils,
//! skills, workflows, machine, instantanés mémoire), T4 volatile (date, état du run, notes
//! de travail, rappel déclenché par le message). Venues de `conversation.rs` du daemon
//! (épopée #208, T22) : elles ne lisent que le vault, la mémoire et `Services`.

use crate::snapshot::{fresh_snapshot, frozen_snapshot, snapshot_uids};
use penelope_app::helpers::{local_now, vault_dir};
use penelope_app::services::Services;
use penelope_context::tiers::{Tiers, TiersBuilder, volatile_header};

/// Assemble les tuiles du prompt d'une session, instantanés mémoire recalculés.
pub async fn build_tiers(
    s: &Services,
    user_text: &str,
    mcp_lines: &[String],
    run_state: Option<&str>,
) -> Tiers {
    build_tiers_in(s, user_text, mcp_lines, run_state, None, None).await
}

/// Comme [`build_tiers`], avec les instantanés T2 figés pour l'épisode `(session, n)` :
/// une écriture de profil n'altère pas le préfixe avant l'épisode suivant (§6.6, CA 6).
pub async fn build_tiers_in(
    s: &Services,
    user_text: &str,
    mcp_lines: &[String],
    run_state: Option<&str>,
    episode: Option<(&str, i64)>,
    query_vector: Option<Vec<f32>>,
) -> Tiers {
    build_turn_prompt(s, user_text, mcp_lines, run_state, episode, query_vector)
        .await
        .0
}

/// Comme [`build_tiers_in`], avec les uid des souvenirs que le rappel automatique a
/// servis : le tour les compte et juge leur usage sur la réponse (issue #105).
pub async fn build_turn_prompt(
    s: &Services,
    user_text: &str,
    mcp_lines: &[String],
    run_state: Option<&str>,
    episode: Option<(&str, i64)>,
    query_vector: Option<Vec<f32>>,
) -> (Tiers, Vec<String>) {
    let cfg = s.config.config();
    let vault = vault_dir(s);

    let mut b = TiersBuilder::new();
    if let Ok(soul) = std::fs::read_to_string(vault.join("SOUL.md")) {
        b = b.soul(strip_frontmatter(&soul));
    }
    if let Ok(agents) = std::fs::read_to_string(vault.join("AGENTS.md")) {
        b = b.agents_md(strip_frontmatter(&agents));
    }

    // Conversation : les outils rares sont nommés, pas décrits (#104).
    if episode.is_some() {
        b = b.on_demand(penelope_tools::ON_DEMAND);
    }
    // T1 : méta-outils MCP seulement s'il y a des serveurs, skills, serveurs connectés.
    if !mcp_lines.is_empty() {
        for (name, desc, _) in penelope_mcp::registry::ToolRegistry::meta_tools() {
            b = b.meta_tool(name, desc);
        }
        for l in mcp_lines {
            b = b.mcp_server(l.clone());
        }
    }
    for sk in s.skills.all() {
        b = b.skill(sk.name.clone(), sk.description.clone());
    }
    // Workflows : le modèle sait qu'ils existent et ce qu'ils attendent (issue #34).
    for e in s.workflows.all() {
        let m = &e.workflow.metadata;
        let required: Vec<&str> = m
            .parameters
            .iter()
            .filter(|p| p.required)
            .map(|p| p.id.as_str())
            .collect();
        let role = m.description.lines().next().unwrap_or_default().trim();
        let line = if required.is_empty() {
            role.to_string()
        } else {
            format!("{role} (paramètres requis : {})", required.join(", "))
        };
        b = b.workflow(m.id.clone(), line);
    }

    // Ce que la machine sait faire, en une ligne stable (issue #156) : lue du dernier
    // inventaire, jamais sondée ici — un tour de conversation ne lance pas de processus.
    if let Some(inv) = penelope_app::machine::cached(s).await {
        b = b.machine(inv.prompt_line());
    }

    // T2 : instantanés mémoire, figés par épisode quand il y en a un.
    let [profile, core, project] = match episode {
        Some((session_id, n)) => frozen_snapshot(s, session_id, n, user_text).await,
        None => fresh_snapshot(s, &crate::session_project::Scope::All).await,
    };
    b = b.memory_snapshot(profile, core, project);

    // T4 : date locale, état du run, rappel mémoire déclenché par le message.
    b = b.volatile(volatile_header(
        &local_now(s),
        &cfg.owner.timezone,
        run_state,
    ));
    // Notes de travail de la session, à jour à chaque tour : elles survivent aux
    // compactions (issue #32).
    if let Some((session_id, _)) = episode
        && let Some(notes) = crate::session_notes::prompt_block(s, session_id).await
    {
        b = b.volatile(notes);
    }
    let mut recalled = Vec::new();
    if !user_text.trim().is_empty() {
        // Voie 1 avec ce qu'il faut pour être utile : les pratiques du vault et le
        // contexte du tour, sans quoi aucune règle défaisable n'est rappelée et le
        // facteur « projet actif » reste inopérant (issue #58).
        let practices = practices_of(&vault);
        let mut ctx = current_context(s, user_text, episode.map(|(sid, _)| sid)).await;
        let scope = match episode {
            Some((sid, _)) => crate::session_project::Scope::Session(
                crate::session_project::of_session(s, sid).await.0,
            ),
            None => crate::session_project::Scope::All,
        };
        ctx.injected_uids = snapshot_uids(s, &scope).await;
        let recall = penelope_memory::Recall::new(
            &s.memory,
            penelope_memory::RecallParams::from_config(&cfg.memory),
        )
        .path1(user_text, &ctx, query_vector, &practices)
        .await;
        // Retour d'usage (issues #37 et #105) : servis, comptés par le tour qui les a
        // demandés, et utiles seulement si la réponse s'en sert.
        recalled = recall
            .triggered
            .iter()
            .map(|t| t.entry.uid.clone())
            .collect();
        // Vue sans être retenue : le dénominateur du retrait proposé (issue #86).
        let _ = s.memory.record_seen(&recall.seen).await;
        let rendered = recall.render();
        if !rendered.trim().is_empty() {
            b = b.volatile(rendered);
        }
    }
    (b.build(), recalled)
}

/// Pratiques du vault, relues seulement quand un fichier a changé (issue #58).
fn practices_of(vault: &std::path::Path) -> Vec<penelope_memory::vault::Practice> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    type Cache = HashMap<std::path::PathBuf, (std::time::SystemTime, Option<PracticeEntry>)>;
    static CACHE: std::sync::OnceLock<Mutex<Cache>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());

    let dir = vault.join("pratiques");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for e in read.flatten() {
        let path = e.path();
        let Some(stem) = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .filter(|f| f.ends_with(".md"))
            .map(|f| f.trim_end_matches(".md").to_string())
        else {
            continue;
        };
        seen.push(path.clone());
        let mtime = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let fresh = guard.get(&path).map(|(t, _)| *t == mtime).unwrap_or(false);
        if !fresh {
            let parsed = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| penelope_memory::vault::Practice::parse(&raw, &stem).ok());
            guard.insert(path.clone(), (mtime, parsed));
        }
        if let Some((_, Some(p))) = guard.get(&path) {
            out.push(p.clone());
        }
    }
    guard.retain(|k, _| seen.contains(k));
    out
}

type PracticeEntry = penelope_memory::vault::Practice;

/// Contexte du tour : projet actif du workspace de la session, type de tâche déduit du
/// message. Ce que la voie 1 évalue pour décider d'une exception (§6.7, issue #58).
async fn current_context(
    s: &Services,
    user_text: &str,
    session_id: Option<&str>,
) -> penelope_memory::recall::CurrentContext {
    let mut ctx = penelope_memory::recall::CurrentContext {
        tache: penelope_memory::recall::classify_task(user_text),
        ..Default::default()
    };
    if let Some(sid) = session_id
        && let Ok(Some(sess)) = s.sessions.get(sid).await
        && let Some(ws) = sess.workspace.filter(|w| !w.is_empty())
    {
        let key = penelope_memory::recall::project_key(None, &ws);
        ctx.touch_project(&key);
    }
    ctx
}

fn strip_frontmatter(raw: &str) -> String {
    penelope_kernel::frontmatter::parse(raw)
        .map(|fm| fm.body)
        .unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests;
