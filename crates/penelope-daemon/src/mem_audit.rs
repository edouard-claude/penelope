//! Audit de la mémoire noté sur 100 (issue #23) : ce que Pénélope sait de son propriétaire
//! et de son contexte, ce qui manque, et pour chaque axe la prochaine action la plus
//! rentable. Le barème est fixe et versionné, pour que deux audits se comparent ; chaque
//! audit est historisé dans `audits/audit-AAAA-MM-JJ.md` avec l'écart depuis le précédent.

use crate::runtime::Daemon;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Version du barème : un changement de pondération la fait monter. v2 : le lint du wiki
/// compte avec les liens morts (issue #29).
pub const SCALE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Axis {
    pub name: String,
    pub score: u32,
    pub max: u32,
    pub why: String,
    /// Prochaine action la plus rentable ; vide quand l'axe est au maximum.
    pub next: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Audit {
    pub version: u32,
    pub date: String,
    pub total: u32,
    pub axes: Vec<Axis>,
    /// Écart avec l'audit précédent, s'il existe.
    pub delta: Option<i64>,
    pub previous_date: Option<String>,
}

/// Mesures brutes, lues en base.
#[derive(Debug, Default, Clone)]
struct Facts {
    profile: i64,
    never: i64,
    style: i64,
    dated: i64,
    mcp_ready: i64,
    sources: i64,
    projects: i64,
    skills: i64,
    procedures_pending: i64,
    intents: i64,
    schedules: i64,
    runs_30d: i64,
    dream_48h: bool,
    entries: i64,
    with_provenance: i64,
    dead_links: i64,
    /// Problèmes du lint du wiki (liens, identifiants de bloc, propriétés, noms).
    lint_problems: i64,
    undefined_concepts: i64,
    contradictions: i64,
    vectors: i64,
}

async fn facts(d: &Daemon) -> anyhow::Result<Facts> {
    let s = &d.services;
    let now = s.clock.now_utc();
    let month_ago = (now - chrono::Duration::days(30)).to_rfc3339();
    let two_days_ago = (now - chrono::Duration::days(2)).to_rfc3339();
    let mut f = s
        .store
        .read(move |c| {
            let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
            let n1 = |sql: &str, p: &str| c.query_row(sql, [p], |r| r.get::<_, i64>(0));
            Ok(Facts {
                profile: n("SELECT COUNT(*) FROM mem_entries WHERE level = 'profil' AND statut != 'retiree'")?,
                never: n("SELECT COUNT(*) FROM mem_entries WHERE level = 'profil' AND statut != 'retiree'
                          AND (text LIKE 'Jamais%' OR text LIKE 'Ne jamais%')")?,
                style: n("SELECT COUNT(*) FROM mem_entries WHERE level = 'profil' AND statut != 'retiree'
                          AND (text LIKE '%tutoy%' OR text LIKE '%vouvoy%' OR text LIKE '%répondre en%'
                               OR text LIKE '%réponses courtes%' OR text LIKE '%réponses détaillées%')")?,
                dated: n("SELECT COUNT(*) FROM mem_entries WHERE level = 'profil' AND statut != 'retiree'
                          AND depuis IS NOT NULL AND depuis != ''")?,
                sources: n("SELECT COUNT(DISTINCT slug) FROM mem_entries WHERE file LIKE 'sources/%'
                            AND statut != 'retiree'")?,
                projects: n("SELECT COUNT(*) FROM mem_entries WHERE statut != 'retiree'
                             AND (level = 'projet' OR text LIKE 'Projet en cours%')")?,
                skills: n("SELECT COUNT(*) FROM skills WHERE valid = 1")?,
                procedures_pending: n("SELECT COUNT(*) FROM mem_candidates
                                       WHERE ctype = 'procedure_candidate' AND state IN ('new', 'grouped')")?,
                intents: n("SELECT COUNT(*) FROM intents WHERE etat = 'armee'")?,
                schedules: n("SELECT COUNT(*) FROM schedules WHERE state = 'active'")?,
                runs_30d: n1("SELECT COUNT(*) FROM workflow_runs WHERE started_at >= ?1", &month_ago)?,
                dream_48h: n1(
                    "SELECT COUNT(*) FROM dream_runs WHERE phase = 'done' AND started_at >= ?1",
                    &two_days_ago,
                )? > 0,
                entries: n("SELECT COUNT(*) FROM mem_entries WHERE statut != 'retiree'
                            AND file NOT LIKE 'sources/%'")?,
                with_provenance: n("SELECT COUNT(*) FROM mem_entries e JOIN mem_provenance p ON p.uid = e.uid
                                    WHERE e.statut != 'retiree' AND e.file NOT LIKE 'sources/%'")?,
                dead_links: n("SELECT COUNT(*) FROM mem_links l WHERE NOT EXISTS
                               (SELECT 1 FROM mem_entries e WHERE e.slug = l.to_slug)")?,
                contradictions: n("SELECT COUNT(*) FROM approval_requests WHERE kind = 'memory_proposal'
                                   AND state = 'pending'")?,
                vectors: n("SELECT COUNT(*) FROM mem_vec v JOIN mem_entries e ON e.uid = v.uid
                            WHERE e.statut != 'retiree'")?,
                ..Default::default()
            })
        })
        .await?;
    if let Some(sup) = d.hooks.mcp_supervisor() {
        f.mcp_ready = sup
            .statuses()
            .await
            .iter()
            .filter(|st| st.state == penelope_mcp::supervisor::ServerState::Ready)
            .count() as i64;
    }
    let vault = crate::helpers::vault_dir(s);
    f.lint_problems = penelope_memory::wiki::lint(&vault).problems() as i64;
    let pending = vault.join("concepts/_a-definir.md");
    f.undefined_concepts = std::fs::read_to_string(pending)
        .map(|raw| {
            penelope_memory::wiki::body_of(&raw)
                .lines()
                .filter(|l| l.trim_start().starts_with("- "))
                .count() as i64
        })
        .unwrap_or(0);
    Ok(f)
}

fn pts(have: i64, full: i64, max: u32) -> u32 {
    if full <= 0 {
        return max;
    }
    ((have.max(0) as f64 / full as f64).min(1.0) * max as f64).round() as u32
}

fn axes(f: &Facts) -> Vec<Axis> {
    // Connaissance du propriétaire : profil rempli, limites, style, directives datées.
    let owner_score = pts(f.profile, 8, 8)
        + pts(f.never, 2, 6)
        + pts(f.style, 3, 4)
        + pts(f.dated, f.profile.max(1), 2);
    let owner_next = if f.profile == 0 {
        "Lancer `/accueil` : neuf questions pour remplir le profil".to_string()
    } else if f.never == 0 {
        "`/accueil limites` : dire ce que Pénélope ne doit jamais faire".into()
    } else if f.style < 3 {
        "`/accueil style` : tutoiement, longueur et langue des réponses".into()
    } else if f.profile < 8 {
        "`/accueil profil` : compléter rôle, clients et projets".into()
    } else {
        String::new()
    };
    let owner = Axis {
        name: "Connaissance du propriétaire".into(),
        score: owner_score.min(20),
        max: 20,
        why: format!(
            "{} directive(s) au profil, dont {} limite(s) « Jamais » et {} sur le style",
            f.profile, f.never, f.style
        ),
        next: owner_next,
    };

    // Portée : serveurs MCP prêts, sources ingérées, projets décrits.
    let reach_score = pts(f.mcp_ready, 2, 7) + pts(f.sources, 5, 6) + pts(f.projects, 2, 7);
    let reach_next = if f.projects == 0 {
        "Décrire les projets en cours (`/accueil profil`)".to_string()
    } else if f.mcp_ready == 0 {
        "Brancher un serveur MCP (`/mcp add`) sur un outil du quotidien".into()
    } else if f.sources < 5 {
        "Envoyer les documents de référence (PDF, notes) dans la conversation".into()
    } else if f.mcp_ready < 2 {
        "Brancher un second serveur MCP".into()
    } else {
        String::new()
    };
    let reach = Axis {
        name: "Portée".into(),
        score: reach_score.min(20),
        max: 20,
        why: format!(
            "{} serveur(s) MCP prêt(s), {} source(s) ingérée(s), {} projet(s) décrit(s)",
            f.mcp_ready, f.sources, f.projects
        ),
        next: reach_next,
    };

    // Savoir-faire : skills actives, procédures candidates traitées.
    let skill_score = pts(f.skills, 3, 15) + if f.procedures_pending == 0 { 5 } else { 2 };
    let skill = Axis {
        name: "Savoir-faire".into(),
        score: skill_score.min(20),
        max: 20,
        why: format!(
            "{} skill(s) active(s), {} procédure(s) candidate(s) en attente",
            f.skills, f.procedures_pending
        ),
        next: if f.procedures_pending > 0 {
            "Relire les procédures candidates au prochain digest".into()
        } else if f.skills < 3 {
            "Décrire une tâche récurrente pour qu'elle devienne une skill".into()
        } else {
            String::new()
        },
    };

    // Autonomie : intentions, planifications, workflows récents, dernier rêve.
    let auto_score = pts(f.intents, 1, 5)
        + pts(f.schedules, 1, 5)
        + pts(f.runs_30d, 1, 5)
        + if f.dream_48h { 5 } else { 0 };
    let autonomy = Axis {
        name: "Autonomie".into(),
        score: auto_score.min(20),
        max: 20,
        why: format!(
            "{} intention(s) armée(s), {} planification(s), {} run(s) sur 30 jours, rêve {}",
            f.intents,
            f.schedules,
            f.runs_30d,
            if f.dream_48h {
                "réussi depuis moins de 48 h"
            } else {
                "absent depuis 48 h"
            }
        ),
        next: if !f.dream_48h {
            "Lancer `/dream` : la consolidation n'a pas réussi depuis 48 h".into()
        } else if f.schedules == 0 {
            "Demander un rappel ou une veille planifiée".into()
        } else if f.intents == 0 {
            "Confier une intention (« rappelle-moi quand… »)".into()
        } else if f.runs_30d == 0 {
            "Lancer un workflow (`/wf`)".into()
        } else {
            String::new()
        },
    };

    // Qualité : provenance, liens morts, concepts à définir, contradictions, vecteurs.
    // Une mémoire vide n'a ni provenance ni vecteurs à faire valoir.
    let quality_score = if f.entries == 0 {
        0
    } else {
        pts(f.with_provenance, f.entries, 6) + pts(f.vectors, f.entries, 4)
    } + if f.dead_links == 0 && f.lint_problems == 0 {
        4
    } else {
        1
    } + if f.undefined_concepts == 0 { 3 } else { 1 }
        + if f.contradictions == 0 { 3 } else { 0 };
    let quality = Axis {
        name: "Qualité".into(),
        score: quality_score.min(20),
        max: 20,
        why: format!(
            "{}/{} entrée(s) avec provenance, {} lien(s) mort(s), {} problème(s) de lint, \
             {} concept(s) à définir, {} contradiction(s) ouverte(s), {}/{} vecteur(s)",
            f.with_provenance,
            f.entries,
            f.dead_links,
            f.lint_problems,
            f.undefined_concepts,
            f.contradictions,
            f.vectors,
            f.entries
        ),
        next: if f.contradictions > 0 {
            "Trancher les contradictions en attente (`/approvals`)".into()
        } else if f.entries > 0 && f.vectors < f.entries {
            "`penelope mem reindex --embeddings` : vecteurs manquants".into()
        } else if f.dead_links > 0 || f.lint_problems > 0 {
            "Corriger ce que signale `penelope vault lint` (liens, blocs, propriétés)".into()
        } else if f.undefined_concepts > 0 {
            "Définir les concepts de `concepts/_a-definir.md`".into()
        } else {
            String::new()
        },
    };
    vec![owner, reach, skill, autonomy, quality]
}

fn today(d: &Daemon) -> String {
    let s = &d.services;
    let cfg = s.config.config();
    let utc = chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default();
    match cfg.owner.timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => utc.with_timezone(&tz).format("%Y-%m-%d").to_string(),
        Err(_) => utc.format("%Y-%m-%d").to_string(),
    }
}

const LAST_KEY: &str = "mem.audit.last";

/// Audite, historise et rend l'audit.
pub async fn run(d: &Daemon) -> anyhow::Result<Audit> {
    let f = facts(d).await?;
    let axes = axes(&f);
    let total = axes.iter().map(|a| a.score).sum();
    let previous: Option<Audit> = d
        .services
        .kv_get(LAST_KEY)
        .await?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .filter(|p: &Audit| p.version == SCALE_VERSION);
    let date = today(d);
    let audit = Audit {
        version: SCALE_VERSION,
        date: date.clone(),
        total,
        delta: previous.as_ref().map(|p| total as i64 - p.total as i64),
        previous_date: previous.map(|p| p.date),
        axes,
    };
    d.services
        .kv_set(LAST_KEY, &serde_json::to_string(&audit)?)
        .await?;
    // `audit-AAAA-MM-JJ` : un nom unique dans tout le vault (issue #29).
    let vault = crate::helpers::vault_dir(&d.services);
    let rel = format!("audits/audit-{date}.md");
    let current = std::fs::read_to_string(vault.join(&rel)).unwrap_or_default();
    crate::vault_ops::save_note(
        &vault,
        &rel,
        &penelope_memory::wiki::replace_body(&current, &render(&audit)),
        &date,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(audit)
}

/// Axe dont la prochaine action rapporte le plus de points ; à écart égal, l'ordre du
/// barème départage (le propriétaire d'abord).
pub fn best_next(a: &Audit) -> Option<&Axis> {
    a.axes
        .iter()
        .rev()
        .filter(|x| !x.next.is_empty())
        .max_by_key(|x| x.max - x.score)
}

pub fn render(a: &Audit) -> String {
    let delta = match (a.delta, &a.previous_date) {
        (Some(d), Some(p)) => format!(" ({d:+} depuis le {p})"),
        _ => String::new(),
    };
    let mut t = format!(
        "# Audit de la mémoire du {}\n\n**{}/100**{delta}, barème v{}.\n\n| Axe | Score | Pourquoi | Prochaine action |\n|---|---|---|---|\n",
        a.date, a.total, a.version
    );
    for x in &a.axes {
        t.push_str(&format!(
            "| {} | {}/{} | {} | {} |\n",
            x.name,
            x.score,
            x.max,
            x.why,
            if x.next.is_empty() {
                "rien".to_string()
            } else {
                x.next.clone()
            }
        ));
    }
    if let Some(best) = best_next(a) {
        t.push_str(&format!("\nLa plus rentable : {}.\n", best.next));
    }
    t
}

/// Texte court pour Telegram et le digest.
pub fn summary(a: &Audit) -> String {
    let delta = match (a.delta, &a.previous_date) {
        (Some(d), Some(p)) => format!(" ({d:+} depuis le {p})"),
        _ => String::new(),
    };
    let mut t = format!("📈 **Mémoire : {}/100**{delta}\n", a.total);
    for x in &a.axes {
        t.push_str(&format!(
            "\n- {} : {}/{} · {}",
            x.name, x.score, x.max, x.why
        ));
        if !x.next.is_empty() {
            t.push_str(&format!("\n  → {}", x.next));
        }
    }
    if let Some(best) = best_next(a) {
        t.push_str(&format!("\n\nLa plus rentable : {}.", best.next));
    }
    t
}

pub fn to_json(a: &Audit) -> serde_json::Value {
    json!({"audit": a, "text": summary(a), "best_next": best_next(a).map(|x| x.next.clone())})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Origin;
    use penelope_kernel::clock::TestClock;
    use penelope_memory::{Level, Provenance};
    use std::sync::Arc;

    /// Issue #23 : un vault vide obtient un score bas et « lancer /accueil » ; après un
    /// accueil, la connaissance du propriétaire monte et l'action proposée change.
    #[tokio::test]
    async fn an_onboarding_raises_the_score_and_changes_the_next_action() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let empty = run(&d).await.unwrap();
        assert!(empty.total < 30, "{empty:?}");
        assert!(
            best_next(&empty).unwrap().next.contains("/accueil"),
            "{empty:?}"
        );
        assert_eq!(empty.delta, None);

        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        let vault = crate::helpers::vault_dir(&s);
        for text in [
            "Toujours tutoyer le propriétaire",
            "Préférer des réponses courtes",
            "Toujours répondre en français",
            "Jamais écrire en mon nom",
            "Jamais supprimer sans demander",
        ] {
            let prov = Provenance::owner(&sid, "accueil", &s.clock.now_rfc3339())
                .with_source("accueil/2026-01-01.md#q5");
            crate::vault_ops::remember_with(&s, &vault, Level::Profil, text, prov)
                .await
                .unwrap();
        }
        clock.advance_days(1);
        let after = run(&d).await.unwrap();
        assert!(after.axes[0].score > empty.axes[0].score, "{after:?}");
        assert!(after.delta.unwrap() > 0);
        assert!(
            !after.axes[0].next.contains("Lancer `/accueil`"),
            "{after:?}"
        );
        let file =
            std::fs::read_to_string(vault.join(format!("audits/audit-{}.md", after.date))).unwrap();
        assert!(file.contains(&format!("**{}/100**", after.total)), "{file}");
        assert!(file.contains("depuis le"), "{file}");
    }
}
