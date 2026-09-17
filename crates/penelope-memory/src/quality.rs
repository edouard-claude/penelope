//! Porte de qualité avant promotion (issue #25) : une entrée de mémoire est un fait
//! complet, court, au sujet identifiable ; un état passager part en projet avec une date
//! d'expiration ; une donnée client, financière ou de sécurité est marquée sensible et
//! n'est jamais injectée d'office. Les faits sur Pénélope elle-même ne sont pas retenus :
//! la configuration effective (`self_status`) fait foi.

use regex::Regex;
use std::sync::OnceLock;

/// Longueur maximale d'une entrée : au-delà, une entrée par fait.
pub const MAX_ENTRY_CHARS: usize = 300;
/// Durée de vie par défaut d'un état passager.
pub const TEMPORAL_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Keep,
    /// Trop long, découpé en faits ; `dropped` : morceaux écartés (tronqués, sans sujet).
    Split {
        parts: Vec<String>,
        dropped: Vec<(String, String)>,
    },
    Reject(String),
}

const PRONOUNS: &[&str] = &[
    "il", "elle", "ils", "elles", "ce", "cela", "ça", "celui", "celle", "ceux", "celles", "lui",
];
const DANGLING: &[&str] = &[
    "et", "ou", "de", "du", "des", "à", "au", "aux", "le", "la", "les", "un", "une", "non", "pour",
    "avec", "sur", "que", "qui",
];

/// Défaut d'un texte court, s'il en a un.
fn flaw(text: &str) -> Option<String> {
    let t = text.trim();
    if t.ends_with("...") || t.ends_with('…') {
        return Some("texte tronqué".into());
    }
    let last = t
        .trim_end_matches(['.', '!', '?'])
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .to_lowercase();
    if t.ends_with([',', ':', ';', '(']) || DANGLING.contains(&last.as_str()) {
        return Some("phrase incomplète".into());
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.len() < 3 {
        return Some("trop court pour être un fait".into());
    }
    let first = words[0]
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if PRONOUNS.contains(&first.as_str()) {
        return Some("sans sujet identifiable : nommer de qui ou de quoi il s'agit".into());
    }
    None
}

/// Découpe en phrases.
fn sentences(text: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"(?:[.!?;])\s+").expect("regex valide"));
    let mut out = Vec::new();
    let mut last = 0;
    for m in re.find_iter(text) {
        let end = m.start() + 1;
        out.push(text[last..end].trim().to_string());
        last = m.end();
    }
    out.push(text[last..].trim().to_string());
    out.retain(|s| !s.is_empty());
    out
}

/// Verdict sur le texte d'une entrée.
pub fn check_text(text: &str) -> Verdict {
    let t = text.trim();
    if t.chars().count() <= MAX_ENTRY_CHARS {
        return match flaw(t) {
            Some(why) => Verdict::Reject(why),
            None => Verdict::Keep,
        };
    }
    let mut parts = Vec::new();
    let mut dropped = Vec::new();
    for s in sentences(t) {
        if s.chars().count() > MAX_ENTRY_CHARS {
            return Verdict::Reject(format!(
                "entrée trop longue ({} caractères, maximum {MAX_ENTRY_CHARS}) : une entrée par fait",
                t.chars().count()
            ));
        }
        match flaw(&s) {
            Some(why) => dropped.push((s, why)),
            None => parts.push(s),
        }
    }
    if parts.is_empty() {
        return Verdict::Reject("aucun fait exploitable une fois découpé".into());
    }
    Verdict::Split { parts, dropped }
}

/// État passager : vrai aujourd'hui, faux dans quelques jours.
pub fn is_temporal(text: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)\b(en cours|non lue?s?|pas encore|en attente|à venir|prochainement|cette semaine|ce mois|demain|aujourd'hui|hier|en cadrage|arbitrage|à relancer|deal|propale|rendez-vous|rdv)\b",
        )
        .expect("regex valide")
    })
    .is_match(text)
}

/// Donnée client, financière ou de sécurité.
pub fn is_sensitive(text: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)(\d[\d\s.,]*\s?(k?€|\$|(euros?|usd|eur)\b)|\b(factur[ée]e?s?|chiffre d'affaires|marge|salaire|iban|rib|faille|vulnérabilit|cve-\d|injection sql|mot de passe|confidentiel))",
        )
        .expect("regex valide")
    })
    .is_match(text)
}

/// Fait sur la configuration de Pénélope elle-même.
pub fn is_about_penelope(text: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let lower = text.to_lowercase();
    (lower.contains("pénélope") || lower.contains("penelope"))
        && R.get_or_init(|| {
            Regex::new(r"(?i)\b(dreaming|rêve|consolidation|cron|digest|budget|modèle|alias|compaction|tous les|toutes les|chaque nuit)\b")
                .expect("regex valide")
        })
        .is_match(text)
}

/// Date d'expiration d'un état passager.
pub fn expiry_from(today: &str) -> String {
    chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .map(|d| {
            (d + chrono::Duration::days(TEMPORAL_DAYS))
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|_| today.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_or_subjectless_texts_are_rejected() {
        assert!(matches!(
            check_text("L'adresse IP de la base de données est 127.0.0.1 et non 10..."),
            Verdict::Reject(r) if r.contains("tronqué")
        ));
        assert!(matches!(
            check_text("Il préfère le matin."),
            Verdict::Reject(_)
        ));
        assert!(matches!(
            check_text("Le client ACME paie à 30 jours et"),
            Verdict::Reject(_)
        ));
        assert_eq!(
            check_text("On pousse toujours sur dev d'abord."),
            Verdict::Keep
        );
    }

    #[test]
    fn a_long_paragraph_is_split_into_facts() {
        let paragraph = "Le propriétaire dirige une agence web à Saint-Denis. ".repeat(4)
            + &"Ses clients principaux sont des commerces de proximité. ".repeat(4)
            + &"Il a facturé 12 000 € en juin. ".repeat(6)
            + &"L'agence travaille surtout en Rust et en TypeScript. ".repeat(10);
        assert!(paragraph.chars().count() > 1_000);
        match check_text(&paragraph) {
            Verdict::Split { parts, dropped } => {
                assert!(parts.iter().all(|p| p.chars().count() <= MAX_ENTRY_CHARS));
                assert!(parts.iter().any(|p| p.contains("agence web")));
                assert!(dropped.iter().any(|(t, _)| t.starts_with("Il a facturé")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn temporal_sensitive_and_self_facts_are_recognised() {
        assert!(is_temporal("Deal ACME en cadrage, propale non lue"));
        assert!(!is_temporal("Le propriétaire code en Rust"));
        assert!(is_sensitive("Le client a payé 4 500 € la refonte"));
        assert!(is_sensitive(
            "Faille d'injection SQL découverte chez le prospect"
        ));
        assert!(!is_sensitive("Le client préfère les réunions le mardi"));
        assert!(is_about_penelope(
            "Penelope utilise un dreaming tous les 3h30"
        ));
        assert!(!is_about_penelope(
            "Le propriétaire utilise Obsidian tous les jours"
        ));
        assert_eq!(expiry_from("2026-09-17"), "2026-10-17");
    }
}
