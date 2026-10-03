//! Sujet de travail d'une session (issue #119) : il filtre la mémoire injectée d'office.
//!
//! ```text
//!  projet de la session ─ explicite (/projet), sinon le sujet de la conversation que le
//!                         canal nomme (un sujet = un projet, #301), sinon le titre ou le
//!                         premier message quand ils nomment un projet connu du vault
//!  instantané T2 ──────── Profil toujours ; Cœur et Projets : entrées sans projet, et celles
//!                         du projet de la session ; les autres restent au rappel et à mem_search
//! ```
//!
//! Le projet se fixe quand l'instantané de l'épisode est figé : le préfixe reste identique
//! d'un tour à l'autre. Le changer à la main refige l'instantané au tour suivant. Les
//! sujets nommés par le canal deviennent des projets (fiche `projets/<slug>.md`, sessions
//! rattachées) dans [`subjects`].

use penelope_app::services::Services;
use penelope_memory::{IndexedEntry, Level};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub mod subjects;
#[cfg(test)]
mod tests;

/// Portée d'un instantané mémoire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Tout ce qui est injectable (sessions hors conversation, rêve).
    All,
    /// Une conversation et son sujet, s'il en a un.
    Session(Option<String>),
}

/// Forme comparable d'un nom de projet : minuscules, sans accents, mots joints par `-`.
pub fn normalize(name: &str) -> String {
    let folded: String = name
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join("-")
}

/// Projet d'une entrée : son annotation `projet`, sinon, pour `projets.md`, la section où
/// elle est rangée.
pub fn entry_project(e: &IndexedEntry) -> Option<String> {
    e.projet
        .as_deref()
        .or(if e.level == Level::Projet {
            e.anchor.as_deref()
        } else {
            None
        })
        .map(normalize)
        .filter(|p| !p.is_empty())
}

/// Vrai si l'entrée entre dans l'instantané de cette portée. Une entrée sans projet entre
/// partout ; le profil aussi.
pub fn keeps(scope: &Scope, e: &IndexedEntry) -> bool {
    match scope {
        Scope::All => true,
        Scope::Session(project) => {
            e.level == Level::Profil
                || match entry_project(e) {
                    None => true,
                    Some(p) => project.as_deref() == Some(p.as_str()),
                }
        }
    }
}

/// Projets que le vault connaît : annotations, sections de `projets.md` et fiches
/// `projets/<slug>.md` (#301).
pub async fn known(s: &Services) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for level in [Level::Coeur, Level::Projet] {
        for e in s.memory.by_level(level).await.unwrap_or_default() {
            if let Some(p) = entry_project(&e) {
                out.insert(p);
            }
        }
    }
    out.extend(subjects::notes(&penelope_app::helpers::vault_dir(s)));
    out
}

fn key(session_id: &str) -> String {
    format!("session.project.{session_id}")
}

/// Projet enregistré d'une session : `None` s'il n'a jamais été fixé, `Some(None)` pour
/// « aucun projet » choisi.
async fn stored(s: &Services, session_id: &str) -> Option<(Option<String>, String)> {
    let k = key(session_id);
    let raw = s
        .store
        .read(move |c| penelope_store::kv_get(c, &k))
        .await
        .ok()
        .flatten()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some((
        v["project"].as_str().map(String::from),
        v["how"].as_str().unwrap_or("explicite").to_string(),
    ))
}

async fn store(s: &Services, session_id: &str, project: Option<&str>, how: &str) {
    let (k, v) = (
        key(session_id),
        json!({"project": project, "how": how}).to_string(),
    );
    let _ = s
        .store
        .write(move |tx| penelope_store::kv_set(tx, &k, &v))
        .await;
}

/// Projet d'une session et comment il a été fixé (`explicite`, `sujet`, `titre`,
/// `message`), s'il l'a été.
pub async fn of_session(s: &Services, session_id: &str) -> (Option<String>, Option<String>) {
    match stored(s, session_id).await {
        Some((p, how)) => (p, Some(how)),
        None => (None, None),
    }
}

/// Enregistre le projet d'une session déjà en cours et le dit au journal : l'instantané de
/// l'épisode est refigé au tour suivant, et `session.project` libère le préfixe retenu,
/// même à cache chaud (`penelope_context::store::PREFIX_RELEASES`, T16). Une fois par
/// changement, jamais à chaque tour.
async fn assign(s: &Services, session_id: &str, project: Option<&str>, how: &str) {
    store(s, session_id, project, how).await;
    crate::episodes::refresh_snapshot(s, session_id).await;
    let draft = penelope_kernel::event::EventDraft::new(
        penelope_context::store::KIND_SESSION_PROJECT,
        json!({"project": project, "how": how}),
    )
    .session(session_id);
    if let Err(e) = s.events.append(draft).await {
        tracing::warn!(session = session_id, error = %e, "projet non journalisé");
    }
}

/// Fixe le projet d'une session à la main (`None` : aucun). Un choix explicite n'est
/// jamais repris par un sujet ni par une migration.
pub async fn set(s: &Services, session_id: &str, project: Option<&str>) {
    let project = project.map(normalize).filter(|p| !p.is_empty());
    assign(s, session_id, project.as_deref(), "explicite").await;
}

/// Projet à appliquer à l'instantané qu'on fige : l'enregistré, sinon le sujet de la
/// conversation que le canal nomme (il n'a pas besoin d'être connu du vault : un sujet est
/// un projet, #301), sinon celui que nomment le titre ou le message parmi les projets
/// connus. Un projet déduit est enregistré, pour que le préfixe reste stable.
pub async fn resolve(s: &Services, session_id: &str, user_text: &str) -> Option<String> {
    if let Some((p, _)) = stored(s, session_id).await {
        return p;
    }
    if let Some(name) = s.channel.subject(session_id).await {
        let p = normalize(&name);
        if !p.is_empty() {
            store(s, session_id, Some(&p), "sujet").await;
            return Some(p);
        }
    }
    let projects = known(s).await;
    if projects.is_empty() {
        return None;
    }
    let title = s
        .sessions
        .get(session_id)
        .await
        .ok()
        .flatten()
        .and_then(|x| x.title);
    for (text, how) in [(title.as_deref(), "titre"), (Some(user_text), "message")] {
        let Some(text) = text else { continue };
        let words = normalize(text);
        let words: Vec<&str> = words.split('-').collect();
        let found = projects.iter().find(|p| {
            let parts: Vec<&str> = p.split('-').collect();
            words.windows(parts.len()).any(|w| w == parts.as_slice())
        });
        if let Some(p) = found {
            store(s, session_id, Some(p), how).await;
            return Some(p.clone());
        }
    }
    None
}
