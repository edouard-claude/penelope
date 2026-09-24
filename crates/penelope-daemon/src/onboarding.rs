//! Entretien d'accueil (issue #21) : ce que le propriétaire dit de lui (rôle, projets,
//! outils, style, limites) n'attend pas les occurrences répétées du rêve nocturne.
//!
//! Les questions sont écrites dans le vault (`accueil/accueil-AAAA-MM-JJ.md`) avant d'être posées,
//! chaque réponse se range sous sa question : la séance se relit et se reprend après une
//! interruption. À la clôture, un récapitulatif montre ce qui change dans `profil.md` et
//! `memoire.md` ; rien n'est écrit sans validation, et chaque entrée garde sa provenance
//! vers la question d'où elle vient.

use crate::runtime::Daemon;
use penelope_memory::{Level, Provenance};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Partie de l'entretien, rejouable seule (`/accueil limites`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Part {
    Profil,
    Outils,
    Style,
    Limites,
}

impl Part {
    pub fn parse(s: &str) -> Option<Part> {
        Some(match s.trim().to_lowercase().as_str() {
            "profil" | "activité" | "activite" => Part::Profil,
            "outils" | "sources" => Part::Outils,
            "style" | "ton" => Part::Style,
            "limites" | "limite" | "interdits" => Part::Limites,
            _ => return None,
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Part::Profil => "profil",
            Part::Outils => "outils",
            Part::Style => "style",
            Part::Limites => "limites",
        }
    }
}

pub struct Question {
    pub n: u32,
    pub part: Part,
    pub text: &'static str,
    pub hint: &'static str,
    /// Réponses proposées en boutons ; vide : texte libre.
    pub choices: &'static [&'static str],
    /// Une réponse par ligne (projets, limites).
    pub list: bool,
}

pub const QUESTIONS: &[Question] = &[
    Question {
        n: 1,
        part: Part::Profil,
        text: "Quel est ton rôle ou ton métier ?",
        hint: "Par exemple : développeur indépendant, directrice d'agence.",
        choices: &[],
        list: false,
    },
    Question {
        n: 2,
        part: Part::Profil,
        text: "Pour qui travailles-tu : entreprise, clients principaux ?",
        hint: "Une ligne suffit.",
        choices: &[],
        list: false,
    },
    Question {
        n: 3,
        part: Part::Profil,
        text: "Quels projets sont en cours ?",
        hint: "Un projet par ligne.",
        choices: &[],
        list: true,
    },
    Question {
        n: 4,
        part: Part::Outils,
        text: "Quels outils et sources utilises-tu au quotidien ?",
        hint: "Messagerie, gestion de tickets, stockage, agenda…",
        choices: &[],
        list: false,
    },
    Question {
        n: 5,
        part: Part::Style,
        text: "Tutoiement ou vouvoiement ?",
        hint: "",
        choices: &["Tutoiement", "Vouvoiement"],
        list: false,
    },
    Question {
        n: 6,
        part: Part::Style,
        text: "Quelle longueur de réponse préfères-tu ?",
        hint: "",
        choices: &["Courtes", "Détaillées quand c'est utile"],
        list: false,
    },
    Question {
        n: 7,
        part: Part::Style,
        text: "Dans quelle langue dois-je répondre ?",
        hint: "",
        choices: &["Français", "Anglais", "La langue du message"],
        list: false,
    },
    Question {
        n: 8,
        part: Part::Limites,
        text: "Qu'est-ce que je ne dois jamais faire ?",
        hint: "Une limite par ligne, par exemple : écrire en mon nom, supprimer sans demander, \
               inventer un chiffre.",
        choices: &[],
        list: true,
    },
    Question {
        n: 9,
        part: Part::Limites,
        text: "Et ce que je dois toujours faire, préférer ou éviter ?",
        hint: "Une règle par ligne, commencée par « toujours », « préférer » ou « éviter ».",
        choices: &[],
        list: true,
    },
];

pub fn question(n: u32) -> Option<&'static Question> {
    QUESTIONS.iter().find(|q| q.n == n)
}

/// Réponse rangée sous sa question.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Answer {
    Pending,
    Skipped,
    Given(String),
}

const PENDING: &str = "_en attente_";
const SKIPPED: &str = "_passée_";

/// Séance d'accueil : son fichier dans le vault et les réponses qu'il contient.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sitting {
    pub rel: String,
    pub part: Option<Part>,
    pub answers: BTreeMap<u32, Answer>,
}

impl Sitting {
    /// Prochaine question sans réponse.
    pub fn next(&self) -> Option<&'static Question> {
        self.answers
            .iter()
            .find(|(_, a)| **a == Answer::Pending)
            .and_then(|(n, _)| question(*n))
    }

    pub fn position(&self, n: u32) -> (usize, usize) {
        let i = self.answers.keys().position(|k| *k == n).unwrap_or(0);
        (i + 1, self.answers.len())
    }

    fn render(&self, date: &str) -> String {
        let scope = match self.part {
            Some(p) => format!(" · {}", p.as_str()),
            None => String::new(),
        };
        let mut out = format!(
            "# Accueil du {date}{scope}\n\nChaque réponse est rangée sous sa question ; \
             `/accueil` reprend à la première question sans réponse.\n"
        );
        for (n, a) in &self.answers {
            let Some(q) = question(*n) else { continue };
            let body = match a {
                Answer::Pending => PENDING.to_string(),
                Answer::Skipped => SKIPPED.to_string(),
                Answer::Given(t) => t.trim().to_string(),
            };
            out.push_str(&format!("\n## {n}. {}\n\n{body}\n", q.text));
        }
        out
    }
}

/// Réponses d'un fichier de séance.
pub fn parse(raw: &str) -> BTreeMap<u32, Answer> {
    let mut out = BTreeMap::new();
    let mut current: Option<(u32, Vec<&str>)> = None;
    let flush = |cur: Option<(u32, Vec<&str>)>, out: &mut BTreeMap<u32, Answer>| {
        if let Some((n, lines)) = cur {
            let body = lines.join("\n").trim().to_string();
            let a = match body.as_str() {
                PENDING | "" => Answer::Pending,
                SKIPPED => Answer::Skipped,
                _ => Answer::Given(body),
            };
            out.insert(n, a);
        }
    };
    for line in raw.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            flush(current.take(), &mut out);
            if let Some(n) = head.split('.').next().and_then(|n| n.trim().parse().ok()) {
                current = Some((n, Vec::new()));
            }
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    flush(current, &mut out);
    out
}

fn kv_current() -> &'static str {
    "onboard.current"
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

fn vault(d: &Daemon) -> std::path::PathBuf {
    crate::conversation::vault_dir(&d.services)
}

/// Séance en cours, sinon une nouvelle (toutes les questions, ou une partie). Le fichier
/// est écrit avant la première question.
pub async fn start(d: &Daemon, part: Option<Part>) -> anyhow::Result<Sitting> {
    if let Some(rel) = d
        .services
        .kv_get(kv_current())
        .await?
        .filter(|r| !r.is_empty())
        && let Some(s) = load(d, &rel)
        && (part.is_none() || s.part == part)
    {
        return Ok(s);
    }
    let date = today(d);
    // Nom unique dans tout le vault : `accueil-AAAA-MM-JJ`, jamais le nom d'une note du
    // journal (issue #29).
    let rel = match part {
        Some(p) => format!("accueil/accueil-{date}-{}.md", p.as_str()),
        None => format!("accueil/accueil-{date}.md"),
    };
    let sitting = Sitting {
        rel: rel.clone(),
        part,
        answers: QUESTIONS
            .iter()
            .filter(|q| part.is_none_or(|p| q.part == p))
            .map(|q| (q.n, Answer::Pending))
            .collect(),
    };
    save(d, &sitting)?;
    d.services.kv_set(kv_current(), &rel).await?;
    Ok(sitting)
}

/// Relit une séance depuis son fichier.
pub fn load(d: &Daemon, rel: &str) -> Option<Sitting> {
    let raw = std::fs::read_to_string(vault(d).join(rel)).ok()?;
    let answers = parse(&raw);
    if answers.is_empty() {
        return None;
    }
    let part = rel
        .trim_end_matches(".md")
        .rsplit('-')
        .next()
        .and_then(Part::parse);
    Some(Sitting {
        rel: rel.to_string(),
        part,
        answers,
    })
}

fn save(d: &Daemon, s: &Sitting) -> anyhow::Result<()> {
    let vault = vault(d);
    let date = s
        .rel
        .trim_start_matches("accueil/")
        .trim_start_matches("accueil-")
        .chars()
        .take(10)
        .collect::<String>();
    let current = std::fs::read_to_string(vault.join(&s.rel)).unwrap_or_default();
    let content = penelope_memory::wiki::replace_body(&current, &s.render(&date));
    crate::vault_ops::save_note(&vault, &s.rel, &content, &today(d)).map_err(anyhow::Error::msg)?;
    Ok(())
}

/// Range une réponse (`None` : question passée) et rend la séance à jour. Une réponse à
/// choix doit correspondre à un choix.
pub async fn answer(d: &Daemon, rel: &str, n: u32, text: Option<&str>) -> anyhow::Result<Sitting> {
    let mut s = load(d, rel).ok_or_else(|| anyhow::anyhow!("séance d'accueil introuvable"))?;
    let q = question(n).ok_or_else(|| anyhow::anyhow!("question {n} inconnue"))?;
    let a = match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Answer::Skipped,
        Some(t) if !q.choices.is_empty() => Answer::Given(
            choice_of(q, t)
                .ok_or_else(|| anyhow::anyhow!("réponds par : {}", q.choices.join(", ")))?
                .to_string(),
        ),
        Some(t) => Answer::Given(t.to_string()),
    };
    s.answers.insert(n, a);
    save(d, &s)?;
    Ok(s)
}

fn choice_of(q: &Question, text: &str) -> Option<&'static str> {
    let t = text.trim().to_lowercase();
    let head: String = t.chars().take(3).collect();
    q.choices
        .iter()
        .find(|c| c.to_lowercase() == t)
        .or_else(|| {
            q.choices
                .iter()
                .find(|c| !head.is_empty() && c.to_lowercase().starts_with(&head))
        })
        .copied()
}

/// Entrée de mémoire tirée d'une réponse.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub level: &'static str,
    pub text: String,
    pub question: u32,
}

fn level_of(p: &Proposal) -> Level {
    if p.level == "profil" {
        Level::Profil
    } else {
        Level::Coeur
    }
}

fn lines(t: &str) -> Vec<String> {
    t.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['-', '*', '•'])
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Directive « Jamais », « Toujours », « Préférer » ou « Éviter » d'une ligne.
fn directive(line: &str, default: &str) -> String {
    let lower = line.to_lowercase();
    for (prefixes, word) in [
        (&["ne jamais ", "jamais "][..], "Jamais"),
        (&["toujours "][..], "Toujours"),
        (&["préférer ", "preferer ", "privilégier "][..], "Préférer"),
        (&["éviter ", "eviter "][..], "Éviter"),
    ] {
        for p in prefixes {
            if lower.starts_with(p) {
                return format!("{word} {}", line[p.len()..].trim());
            }
        }
    }
    let rest = line
        .strip_prefix("ne pas ")
        .or_else(|| line.strip_prefix("Ne pas "))
        .unwrap_or(line);
    let first = rest
        .chars()
        .next()
        .map(|c| c.to_lowercase().to_string())
        .unwrap_or_default();
    format!(
        "{default} {first}{}",
        rest.chars().skip(1).collect::<String>()
    )
}

/// Ce que les réponses font écrire.
pub fn proposals(s: &Sitting) -> Vec<Proposal> {
    let mut out = Vec::new();
    let mut push = |level: &'static str, text: String, question: u32| {
        out.push(Proposal {
            level,
            text: capitalised(text.trim()),
            question,
        })
    };
    for (n, a) in &s.answers {
        let Answer::Given(t) = a else { continue };
        match n {
            1 => push("memoire", format!("Rôle du propriétaire : {t}"), *n),
            2 => push(
                "memoire",
                format!("Le propriétaire travaille pour ou avec : {t}"),
                *n,
            ),
            3 => {
                for l in lines(t) {
                    push(
                        "memoire",
                        format!("Projet en cours du propriétaire : {l}"),
                        *n,
                    );
                }
            }
            4 => push(
                "memoire",
                format!("Outils et sources du propriétaire : {t}"),
                *n,
            ),
            5 => push(
                "profil",
                if t.starts_with("Vouv") {
                    "Toujours vouvoyer le propriétaire".into()
                } else {
                    "Toujours tutoyer le propriétaire".into()
                },
                *n,
            ),
            6 => push(
                "profil",
                if t.starts_with("Courtes") {
                    "Préférer des réponses courtes".into()
                } else {
                    "Préférer des réponses détaillées quand c'est utile".into()
                },
                *n,
            ),
            7 => push(
                "profil",
                match t.as_str() {
                    "Anglais" => "Toujours répondre en anglais".into(),
                    "La langue du message" => "Toujours répondre dans la langue du message".into(),
                    _ => "Toujours répondre en français".into(),
                },
                *n,
            ),
            8 => {
                for l in lines(t) {
                    push("profil", directive(&l, "Jamais"), *n);
                }
            }
            9 => {
                for l in lines(t) {
                    push("profil", directive(&l, "Toujours"), *n);
                }
            }
            _ => {}
        }
    }
    out
}

/// Ce que la validation changera.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub add: Vec<Proposal>,
    /// Entrée d'un accueil précédent pour la même question, remplacée : (uid, texte).
    pub replace: Vec<(String, String, Proposal)>,
    pub keep: Vec<Proposal>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.replace.is_empty()
    }
}

/// Compare les propositions à la mémoire actuelle.
pub async fn plan(d: &Daemon, s: &Sitting) -> anyhow::Result<Plan> {
    let services = &d.services;
    let mut plan = Plan::default();
    for p in proposals(s) {
        let existing: Vec<(String, String, Option<String>)> = {
            let level = level_of(&p).as_str().to_string();
            services
                .store
                .read(move |c| {
                    let mut st = c.prepare(
                        "SELECT e.uid, e.text, p.source_ref FROM mem_entries e
                         LEFT JOIN mem_provenance p ON p.uid = e.uid
                         WHERE e.level = ?1 AND e.statut != 'retiree'",
                    )?;
                    let rows = st.query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await?
        };
        if existing
            .iter()
            .any(|(_, t, _)| t.trim().eq_ignore_ascii_case(p.text.trim()))
        {
            plan.keep.push(p);
            continue;
        }
        let single = question(p.question).is_some_and(|q| !q.list);
        let tag = format!("#q{}", p.question);
        let previous = existing.iter().find(|(_, _, r)| {
            r.as_deref()
                .is_some_and(|r| r.starts_with("accueil/") && r.ends_with(&tag))
        });
        match previous {
            Some((uid, text, _)) if single => {
                plan.replace.push((uid.clone(), text.clone(), p));
            }
            _ => plan.add.push(p),
        }
    }
    Ok(plan)
}

/// Récapitulatif lisible.
pub fn plan_text(plan: &Plan) -> String {
    let mut by_file: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let file = |p: &Proposal| {
        if p.level == "profil" {
            "profil.md"
        } else {
            "memoire.md"
        }
    };
    for p in &plan.add {
        by_file
            .entry(file(p))
            .or_default()
            .push(format!("+ {}", p.text));
    }
    for (_, old, p) in &plan.replace {
        by_file
            .entry(file(p))
            .or_default()
            .push(format!("- {old}\n+ {}", p.text));
    }
    for p in &plan.keep {
        by_file
            .entry(file(p))
            .or_default()
            .push(format!("= {} (déjà retenu)", p.text));
    }
    if by_file.is_empty() {
        return "Aucune réponse à retenir.".into();
    }
    let mut t = String::from("📋 Récapitulatif de l'accueil, à valider avant écriture :\n");
    for (f, lines) in by_file {
        t.push_str(&format!("\n{f}\n```\n{}\n```\n", lines.join("\n")));
    }
    t
}

/// Écrit le plan validé ; rend (ajouts, remplacements).
pub async fn write(d: &Daemon, s: &Sitting, session_id: &str) -> anyhow::Result<(usize, usize)> {
    let services = &d.services;
    let plan = plan(d, s).await?;
    let vault = vault(d);
    let prov = |p: &Proposal| {
        Provenance::owner(session_id, "accueil", &services.clock.now_rfc3339())
            .with_source(format!("{}#q{}", s.rel, p.question))
    };
    for p in &plan.add {
        crate::vault_ops::remember_with(services, &vault, level_of(p), &p.text, prov(p))
            .await
            .map_err(anyhow::Error::msg)?;
    }
    for (uid, _, p) in &plan.replace {
        crate::vault_ops::forget(services, &vault, uid)
            .await
            .map_err(anyhow::Error::msg)?;
        crate::vault_ops::remember_with(services, &vault, level_of(p), &p.text, prov(p))
            .await
            .map_err(anyhow::Error::msg)?;
    }
    services.kv_set(kv_current(), "").await?;
    let sitting = s.rel.trim_start_matches("accueil/").trim_end_matches(".md");
    if let Err(e) = crate::vault_ops::log(
        &vault,
        &today(d),
        "accueil",
        &format!(
            "{} ajout(s), {} remplacement(s)",
            plan.add.len(),
            plan.replace.len()
        ),
        &[format!("[[{sitting}]]")],
    ) {
        tracing::warn!(error = %e, "log.md non mis à jour");
    }
    Ok((plan.add.len(), plan.replace.len()))
}

/// Abandonne la séance en cours (le fichier reste dans le vault).
pub async fn cancel(d: &Daemon) -> anyhow::Result<()> {
    d.services.kv_set(kv_current(), "").await
}

/// Vrai quand le profil n'a encore aucune entrée : l'accueil est proposé.
pub async fn profile_is_empty(d: &Daemon) -> bool {
    d.services
        .memory
        .by_level(Level::Profil)
        .await
        .map(|e| e.is_empty())
        .unwrap_or(false)
}

/// Question à poser, pour la CLI et les tests : numéro, position, texte, choix.
pub fn question_json(s: &Sitting, q: &Question) -> Value {
    let (i, total) = s.position(q.n);
    json!({
        "rel": s.rel,
        "n": q.n,
        "position": i,
        "total": total,
        "text": q.text,
        "hint": q.hint,
        "choices": q.choices,
        "list": q.list,
    })
}

/// La question, plus ce que la machine sait déjà (issue #156).
///
/// La question « quels outils utilises-tu ? » partait d'une page blanche alors que
/// Pénélope avait l'inventaire sous la main. Ce qui est détecté est **proposé**, pas
/// écrit : le propriétaire confirme ou complète, et ce qu'il déclare reste prioritaire
/// sur ce qui est détecté (§ accueil, issue #21).
pub async fn question_payload(d: &Daemon, s: &Sitting, q: &Question) -> Value {
    let mut v = question_json(s, q);
    if q.part == Part::Outils
        && let Some(inv) = crate::machine::cached(&d.services).await
    {
        let installed: Vec<&str> = inv.present.iter().map(|t| t.name.as_str()).collect();
        if !installed.is_empty() {
            v["detected"] = json!(installed);
            v["detected_hint"] = json!(format!(
                "Sur cette machine je vois : {}. Dis-moi ce que tu utilises vraiment, et \
                 ce qui manque.",
                installed.join(", ")
            ));
        }
    }
    v
}

/// RPC `onboard.*`.
pub async fn rpc(d: &std::sync::Arc<Daemon>, method: &str, p: &Value) -> anyhow::Result<Value> {
    use penelope_kernel::api::method as m;
    let part = p.get("part").and_then(|v| v.as_str()).and_then(Part::parse);
    match method {
        m::ONBOARD_NEXT => {
            let s = start(d, part).await?;
            Ok(match s.next() {
                Some(q) => json!({"question": question_payload(d, &s, q).await}),
                None => {
                    let plan = plan(d, &s).await?;
                    json!({"done": true, "rel": s.rel, "plan": plan, "text": plan_text(&plan)})
                }
            })
        }
        m::ONBOARD_ANSWER => {
            let rel = p["rel"].as_str().unwrap_or_default();
            let n = p["n"].as_u64().unwrap_or(0) as u32;
            let s = answer(d, rel, n, p.get("answer").and_then(|a| a.as_str())).await?;
            let next = match s.next() {
                Some(q) => Some(question_payload(d, &s, q).await),
                None => None,
            };
            Ok(json!({ "next": next }))
        }
        _ => {
            let rel = p["rel"].as_str().unwrap_or_default();
            let s = load(d, rel).ok_or_else(|| anyhow::anyhow!("séance d'accueil introuvable"))?;
            let session = d.chat_session_for(&crate::bus::Origin::Cli).await?;
            let (added, replaced) = write(d, &s, &session).await?;
            Ok(json!({"added": added, "replaced": replaced}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_directives() {
        let mut answers = BTreeMap::new();
        answers.insert(5, Answer::Given("Vouvoiement".into()));
        answers.insert(
            8,
            Answer::Given("- écrire en mon nom\nNe pas supprimer sans demander".into()),
        );
        answers.insert(
            9,
            Answer::Given(
                "éviter le jargon\npréférer les listes courtes\nciter les sources".into(),
            ),
        );
        answers.insert(1, Answer::Skipped);
        let s = Sitting {
            rel: "accueil/2026-09-17.md".into(),
            part: None,
            answers,
        };
        let texts: Vec<String> = proposals(&s).into_iter().map(|p| p.text).collect();
        assert_eq!(
            texts,
            vec![
                "Toujours vouvoyer le propriétaire",
                "Jamais écrire en mon nom",
                "Jamais supprimer sans demander",
                "Éviter le jargon",
                "Préférer les listes courtes",
                "Toujours citer les sources",
            ]
        );
        let raw = s.render("2026-09-17");
        assert_eq!(parse(&raw), s.answers);
        assert_eq!(choice_of(question(5).unwrap(), "vous"), Some("Vouvoiement"));
        assert_eq!(choice_of(question(7).unwrap(), "fr"), Some("Français"));
        assert_eq!(choice_of(question(5).unwrap(), "peut-être"), None);
        assert_eq!(
            choice_of(question(7).unwrap(), "français"),
            Some("Français")
        );
    }
}
