//! Un sujet de conversation nommé par le canal est un projet (#301).
//!
//! ```text
//!  canal ── « cette conversation s'appelle Dose » ──► adopt : fiche projets/dose.md,
//!             (création, renommage, démarrage)         sessions du sujet rattachées (how = sujet)
//!  démarrage ── tous les sujets nommés ──────────────► migrate : idempotente, compte rendu
//!                                                       pour le digest du lendemain
//! ```
//!
//! Le cœur ne sait pas d'où vient le nom : le canal lui donne un [`Subject`], nom et
//! sessions, hors sujet général et foyer, qu'il tranche de son côté. Un rattachement
//! `explicite` (`/projet`) n'est jamais repris ; un rattachement déduit du titre ou du
//! premier message l'est, le sujet prime. Le préfixe retenu d'une session rattachée est
//! libéré une fois, par `session.project` au journal, pas à chaque tour.

use super::{assign, normalize, store, stored};
use penelope_app::services::Services;
use penelope_kernel::frontmatter::{self, FmValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Dossier des fiches de projet, relatif au vault.
pub const DIR: &str = "projets";
/// Compte rendu de la dernière migration qui a changé quelque chose, lu par le digest.
const REPORT_KEY: &str = "projects.subjects_migration";
/// Fichiers de mémoire dont les annotations `projet` et les sections suivent un renommage.
const MEMORY_FILES: [&str; 2] = ["memoire.md", "projets.md"];

/// Un sujet de conversation que le canal nomme, et ses sessions, présentes et passées.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// Le nom tel que le canal le donne (« Dose »).
    pub name: String,
    pub sessions: Vec<String>,
}

/// Ce que l'adoption d'un sujet a changé.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adoption {
    /// Le projet, forme normalisée du nom.
    pub project: String,
    /// Fiche `projets/<slug>.md` créée.
    pub created: bool,
    /// Fiche déplacée depuis l'ancien nom (renommage).
    pub moved: bool,
    /// Sessions sans projet, rattachées.
    pub attached: usize,
    /// Sessions dont le rattachement déduit (titre, message, ancien nom) est remplacé.
    pub corrected: usize,
    /// Sessions à rattachement explicite, laissées telles quelles.
    pub kept: usize,
}

/// Compte rendu de la migration au démarrage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    pub day: String,
    pub subjects: usize,
    pub created: usize,
    pub attached: usize,
    pub corrected: usize,
    pub kept: usize,
}

impl MigrationReport {
    /// Vrai si quelque chose a été écrit : un second passage rend faux.
    pub fn changed(&self) -> bool {
        self.created + self.attached + self.corrected > 0
    }

    /// La ligne du digest.
    pub fn line(&self) -> String {
        let mut t = format!(
            "📁 Un sujet, un projet : {} sujet(s), {} projet(s), {} fiche(s) créée(s), {} \
             session(s) rattachée(s), {} rattachement(s) corrigé(s)",
            self.subjects, self.subjects, self.created, self.attached, self.corrected
        );
        if self.kept > 0 {
            t.push_str(&format!(" ; {} choix explicite(s) conservé(s)", self.kept));
        }
        t.push('.');
        t
    }
}

/// Chemin de la fiche d'un projet, relatif au vault.
pub fn note_path(project: &str) -> String {
    format!("{DIR}/{project}.md")
}

/// Projets qui ont une fiche : les slugs de `projets/*.md`, triés.
pub fn notes(vault: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(vault.join(DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let slug = name.strip_suffix(".md")?;
            (!slug.is_empty() && !slug.starts_with('_')).then(|| slug.to_string())
        })
        .collect();
    out.sort();
    out
}

/// Un nom tel qu'il peut figurer dans un titre, une propriété ou un wikilink.
fn clean(name: &str) -> String {
    name.replace(['\n', '\r', '[', ']', '|'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fiche d'un projet, au gabarit du wiki : propriétés en tête, titre, d'où il vient. Rend
/// vrai si elle a été créée ; une fiche existante n'est jamais réécrite.
pub fn ensure_note(vault: &Path, name: &str, project: &str, day: &str) -> Result<bool, String> {
    let rel = note_path(project);
    if vault.join(&rel).exists() {
        return Ok(false);
    }
    let name = clean(name);
    let mut fields = BTreeMap::new();
    fields.insert("type".to_string(), FmValue::Str("projet".into()));
    fields.insert("nom".to_string(), FmValue::Str(name.clone()));
    let aliases = if name != project {
        vec![name.clone()]
    } else {
        Vec::new()
    };
    fields.insert("aliases".to_string(), FmValue::List(aliases));
    fields.insert("tags".to_string(), FmValue::List(vec!["projets".into()]));
    fields.insert("created".to_string(), FmValue::Str(day.into()));
    fields.insert("updated".to_string(), FmValue::Str(day.into()));
    let body = format!(
        "\n# {name}\n\nProjet né du sujet « {name} » de la conversation : chaque session de \
         ce sujet lui appartient et reçoit d'office les entrées annotées `projet: {project}` \
         et la section « {name} » de [[projets]]. Les autres restent au rappel et à \
         `mem_search`.\n"
    );
    crate::vault_ops::save_note(vault, &rel, &frontmatter::render(&fields, &body), day)?;
    Ok(true)
}

/// Les commentaires `<!-- projet: X -->` d'une ligne dont `X` vaut `old` prennent le nom
/// nouveau ; le reste de la ligne ne bouge pas.
fn rewrite_project_comments(line: &str, old: &str, new_name: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find("<!--") {
        let Some(len) = rest[start..].find("-->") else {
            break;
        };
        let inner = &rest[start + 4..start + len];
        out.push_str(&rest[..start]);
        match inner.trim().split_once(':') {
            Some((k, v)) if k.trim().eq_ignore_ascii_case("projet") && normalize(v) == old => {
                out.push_str(&format!("<!-- projet: {new_name} -->"));
            }
            _ => out.push_str(&rest[start..start + len + 3]),
        }
        rest = &rest[start + len + 3..];
    }
    out.push_str(rest);
    out
}

/// Un fichier de mémoire après le renommage d'un projet : annotations `projet` et, pour
/// `projets.md`, la section `## Ancien nom`. Les identifiants de bloc ne bougent pas.
fn renamed_memory(raw: &str, old: &str, new_name: &str, sections: bool) -> String {
    let mut out: Vec<String> = raw
        .lines()
        .map(|line| {
            if sections
                && let Some(h) = line.strip_prefix("## ")
                && normalize(h) == old
            {
                return format!("## {new_name}");
            }
            rewrite_project_comments(line, old, new_name)
        })
        .collect();
    if raw.ends_with('\n') {
        out.push(String::new());
    }
    out.join("\n")
}

/// Le projet suit le sujet renommé : la fiche est déplacée vers le nouveau slug (ancien
/// nom gardé en alias, wikilinks réécrits), les annotations et la section de la mémoire
/// prennent le nouveau nom, l'index est refait. Jamais de doublon : une fiche déjà là
/// sous le nouveau nom reste, l'ancienne aussi. Rend vrai si la fiche a été déplacée.
async fn rename(
    s: &Services,
    vault: &Path,
    old_name: &str,
    old: &str,
    new_name: &str,
    new: &str,
    day: &str,
) -> Result<bool, String> {
    let (from, to) = (note_path(old), note_path(new));
    let mut moved = false;
    if vault.join(&from).exists() && !vault.join(&to).exists() {
        penelope_memory::wiki::rename_note(vault, &from, &to).map_err(|e| e.to_string())?;
        let (old_name, new_name) = (old_name.to_string(), new_name.to_string());
        crate::vault_ops::update_note(vault, &to, None, day, |raw| {
            let fm = frontmatter::parse(raw).map_err(|e| e.to_string())?;
            let mut fields = fm.fields.clone();
            let mut aliases = penelope_memory::wiki::aliases_of(&fm);
            for a in [old_name.clone(), old.to_string()] {
                if a != new_name && a != new && !aliases.contains(&a) {
                    aliases.push(a);
                }
            }
            fields.insert("aliases".into(), FmValue::List(aliases));
            fields.insert("nom".into(), FmValue::Str(new_name.clone()));
            fields.insert("updated".into(), FmValue::Str(day.into()));
            let body = fm
                .body
                .replacen(&format!("# {old_name}\n"), &format!("# {new_name}\n"), 1);
            Ok(frontmatter::render(&fields, &body))
        })?;
        crate::vault_ops::log(
            vault,
            day,
            "projet",
            &new_name,
            &[format!(
                "fiche déplacée de {from} vers {to}, alias « {old_name} » conservé"
            )],
        )?;
        moved = true;
    }
    let mut touched = false;
    for rel in MEMORY_FILES {
        if !vault.join(rel).exists() {
            continue;
        }
        let sections = rel == "projets.md";
        let new_name = new_name.to_string();
        let written = crate::vault_ops::update_note(vault, rel, None, day, |raw| {
            Ok(renamed_memory(raw, old, &new_name, sections))
        })?;
        touched |= written.is_some();
    }
    if touched {
        crate::vault_ops::reindex(s, vault).await?;
    }
    Ok(moved)
}

/// Un sujet nommé par le canal, à sa création, à son renommage (`previous` : l'ancien
/// nom) ou au démarrage : sa fiche existe, ses sessions lui appartiennent. Un
/// rattachement explicite est conservé ; un rattachement déduit (titre, message, ancien
/// nom) est remplacé, une fois, avec `session.project` au journal pour les sessions
/// actives ; une session déjà rattachée au bon projet par son sujet n'est pas touchée.
pub async fn adopt(
    s: &Services,
    vault: &Path,
    subject: &Subject,
    previous: Option<&str>,
) -> Adoption {
    let project = normalize(&subject.name);
    let mut a = Adoption {
        project: project.clone(),
        ..Default::default()
    };
    if project.is_empty() {
        return a;
    }
    let day = crate::vault_ops::day(s);
    if let Some(old_name) = previous {
        let old = normalize(old_name);
        if !old.is_empty() && old != project {
            match rename(s, vault, old_name, &old, &subject.name, &project, &day).await {
                Ok(moved) => a.moved = moved,
                Err(e) => {
                    tracing::warn!(error = %e, from = %old, to = %project, "projet non renommé")
                }
            }
        }
    }
    match ensure_note(vault, &subject.name, &project, &day) {
        Ok(created) => {
            a.created = created;
            if created
                && let Err(e) = crate::vault_ops::log(
                    vault,
                    &day,
                    "projet",
                    &subject.name,
                    &[format!(
                        "fiche {} créée depuis le sujet « {} »",
                        note_path(&project),
                        clean(&subject.name)
                    )],
                )
            {
                tracing::warn!(error = %e, "log.md non mis à jour");
            }
        }
        Err(e) => tracing::warn!(error = %e, project = %project, "fiche de projet non créée"),
    }
    for sid in &subject.sessions {
        let active = s
            .sessions
            .get(sid)
            .await
            .ok()
            .flatten()
            .is_some_and(|x| x.state == "active");
        match stored(s, sid).await {
            Some((_, how)) if how == "explicite" => a.kept += 1,
            Some((Some(p), how)) if p == project => {
                // Le bon projet, trouvé par le titre ou le message : il vient du sujet.
                if how != "sujet" {
                    store(s, sid, Some(&project), "sujet").await;
                }
            }
            was => {
                if active {
                    assign(s, sid, Some(&project), "sujet").await;
                } else {
                    store(s, sid, Some(&project), "sujet").await;
                }
                if was.is_some() {
                    a.corrected += 1;
                } else {
                    a.attached += 1;
                }
            }
        }
    }
    a
}

/// Au démarrage : chaque sujet nommé est adopté. Idempotente, un second passage ne change
/// rien ; le compte rendu est gardé pour le digest du lendemain quand quelque chose a
/// changé.
pub async fn migrate(s: &Services, vault: &Path, subjects: &[Subject]) -> MigrationReport {
    let mut r = MigrationReport {
        day: crate::vault_ops::day(s),
        subjects: subjects.len(),
        ..Default::default()
    };
    for subject in subjects {
        let a = adopt(s, vault, subject, None).await;
        r.created += usize::from(a.created);
        r.attached += a.attached;
        r.corrected += a.corrected;
        r.kept += a.kept;
    }
    if r.changed() {
        tracing::info!(
            subjects = r.subjects,
            created = r.created,
            attached = r.attached,
            corrected = r.corrected,
            kept = r.kept,
            "sujets devenus projets"
        );
        if let Ok(raw) = serde_json::to_string(&r)
            && let Err(e) = s.kv_set(REPORT_KEY, &raw).await
        {
            tracing::warn!(error = %e, "compte rendu de la migration des sujets non gardé");
        }
    }
    r
}

/// La ligne du digest : le compte rendu d'une migration d'aujourd'hui ou d'hier.
pub async fn digest_note(s: &Services) -> Option<String> {
    let raw = s.kv_get(REPORT_KEY).await.ok().flatten()?;
    let r: MigrationReport = serde_json::from_str(&raw).ok()?;
    let today = crate::vault_ops::day(s);
    let yesterday = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.pred_opt())
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    (r.day == today || r.day == yesterday).then(|| r.line())
}
