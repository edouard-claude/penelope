//! Phrase du propriétaire derrière un candidat (issue #245).
//!
//! « Retiens que nos réunions d'équipe ont lieu le mardi matin à 9 h » devenait un
//! candidat d'origine `agent` dès que l'agent le notait sans recopier la phrase mot pour
//! mot : le tri le jugeait « ni dit ni confirmé par le propriétaire » et le fait se
//! perdait. Ici, un recouvrement déterministe retrouve la phrase du message du
//! propriétaire que le candidat reprend, même reformulé à la troisième personne.
//!
//! Le recouvrement se mesure sur les mots pleins (mots vides, pronoms et verbes de
//! mémorisation retirés, accents et pluriels ramenés) :
//!
//! ```text
//!   phrase du propriétaire ─► mots P        candidat ─► mots C
//!   retenue si  |P ∩ C| ≥ 3,  |P ∩ C| / |P| ≥ 0,75,  |P ∩ C| / |C| ≥ 0,7
//!               et même polarité (négation présente des deux côtés ou d'aucun)
//! ```
//!
//! Le premier seuil exige que la phrase soit presque entière dans le candidat, le second
//! qu'il n'y ajoute pas une déduction de l'agent. La mesure ne porte que sur le texte que
//! l'appelant lui donne : c'est à lui de n'y mettre que les messages du propriétaire du
//! tour, jamais un résultat d'outil ni un contenu transféré.

use std::collections::BTreeSet;

/// Mots pleins communs minimum entre la phrase et le candidat.
pub const MIN_SHARED: usize = 3;
/// Part des mots de la phrase du propriétaire retrouvés dans le candidat.
pub const MIN_OWNER_COVERAGE: f64 = 0.75;
/// Part des mots du candidat venus de la phrase du propriétaire.
pub const MIN_CANDIDATE_COVERAGE: f64 = 0.7;

/// Mots sans contenu propre : articles, pronoms, auxiliaires, formules de mémorisation
/// (« retiens que », « note bien ») et les noms que l'agent donne au propriétaire.
const STOP: &[&str] = &[
    "les",
    "des",
    "une",
    "aux",
    "que",
    "qui",
    "quoi",
    "dont",
    "pour",
    "par",
    "dans",
    "sur",
    "avec",
    "chez",
    "vers",
    "mais",
    "donc",
    "car",
    "est",
    "sont",
    "ont",
    "etait",
    "sera",
    "etre",
    "avoir",
    "fait",
    "faire",
    "nos",
    "vos",
    "mes",
    "tes",
    "ses",
    "notre",
    "votre",
    "leur",
    "leurs",
    "mon",
    "ton",
    "son",
    "moi",
    "toi",
    "lui",
    "elle",
    "eux",
    "nous",
    "vous",
    "ils",
    "elles",
    "cela",
    "ceci",
    "cette",
    "ces",
    "tout",
    "tous",
    "toute",
    "toutes",
    "bien",
    "aussi",
    "retien",
    "retenir",
    "retenu",
    "note",
    "noter",
    "souvien",
    "souvenir",
    "rappelle",
    "rappel",
    "oublie",
    "oublier",
    "proprietaire",
    "utilisateur",
    "desormai",
    "stp",
    "merci",
];

/// Marques de négation : une phrase niée et son contraire ne se recouvrent pas.
const NEGATION: &[&str] = &[
    "pas", "jamais", "aucun", "aucune", "rien", "non", "sans", "ni",
];

fn fold(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' => 'i',
        'ô' | 'ö' => 'o',
        'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        c => c,
    }
}

/// Mots d'un texte, minuscules et sans accents ; lettres et chiffres collés (« 9h »)
/// sont séparés.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digit = false;
    for c in text.to_lowercase().chars().map(fold) {
        if c.is_alphanumeric() {
            if !cur.is_empty() && c.is_ascii_digit() != digit {
                out.push(std::mem::take(&mut cur));
            }
            digit = c.is_ascii_digit();
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Pluriel ramené au singulier, grossièrement mais des deux côtés pareil.
fn stem(w: &str) -> String {
    if w.chars().count() > 3 && (w.ends_with('s') || w.ends_with('x')) {
        w[..w.len() - 1].to_string()
    } else {
        w.to_string()
    }
}

/// Mots pleins et polarité (vrai si le texte porte une négation).
fn content(text: &str) -> (BTreeSet<String>, bool) {
    let mut words = BTreeSet::new();
    let mut negated = false;
    for t in tokens(text) {
        if NEGATION.contains(&t.as_str()) {
            negated = true;
            continue;
        }
        let digits = t.chars().all(|c| c.is_ascii_digit());
        if !digits && t.chars().count() < 3 {
            continue;
        }
        let s = stem(&t);
        if STOP.contains(&s.as_str()) || STOP.contains(&t.as_str()) {
            continue;
        }
        words.insert(s);
    }
    (words, negated)
}

/// Phrases d'un message : fins de phrase, points-virgules, deux-points et retours à la
/// ligne. Le préambule `<contexte>` du tour n'en est pas une.
fn sentences(message: &str) -> impl Iterator<Item = &str> {
    let body = match message.find("</contexte>") {
        Some(i) => &message[i + "</contexte>".len()..],
        None => message,
    };
    body.split(['.', '!', '?', ';', ':', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Recouvrement d'une phrase du propriétaire par un candidat : la part de la phrase
/// retrouvée, ou `None` si les seuils ne sont pas tenus.
fn overlap(sentence: &str, candidate: &(BTreeSet<String>, bool)) -> Option<f64> {
    let (owner, owner_neg) = content(sentence);
    let (cand, cand_neg) = candidate;
    if owner.is_empty() || cand.is_empty() || owner_neg != *cand_neg {
        return None;
    }
    let shared = owner.intersection(cand).count();
    let owner_cov = shared as f64 / owner.len() as f64;
    let cand_cov = shared as f64 / cand.len() as f64;
    (shared >= MIN_SHARED && owner_cov >= MIN_OWNER_COVERAGE && cand_cov >= MIN_CANDIDATE_COVERAGE)
        .then_some(owner_cov)
}

/// Phrase de `message` que `candidate` reprend, si le recouvrement est fort : la
/// meilleure, recopiée telle que le propriétaire l'a écrite.
pub fn owner_statement(candidate: &str, message: &str) -> Option<String> {
    let cand = content(candidate);
    sentences(message)
        .filter_map(|s| overlap(s, &cand).map(|score| (score, s)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, s)| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEETINGS: &str = "Retiens que nos réunions d'équipe ont lieu le mardi matin à 9 h.";

    /// Le cas de la passe réelle : l'agent reformule et ajoute le fuseau.
    #[test]
    fn a_reworded_owner_fact_is_found() {
        for c in [
            "Les réunions d'équipe ont lieu le mardi matin à 9 h (fuseau Indian/Reunion).",
            "Les réunions d'équipe ont lieu le mardi matin à 9h.",
            "Réunions d'équipe : mardi matin, 9 h.",
        ] {
            assert_eq!(
                owner_statement(c, MEETINGS).as_deref(),
                Some("Retiens que nos réunions d'équipe ont lieu le mardi matin à 9 h"),
                "{c}"
            );
        }
    }

    /// Troisième personne, négation des deux côtés, préambule du tour ignoré.
    #[test]
    fn a_third_person_rule_matches_its_sentence() {
        let m = "<contexte>\nDate et heure : jeudi.\n</contexte>\n\nretiens que je veux mes \
                 devis en PDF, jamais en Word ; et note ceci : relance les clients le mardi";
        assert_eq!(
            owner_statement("Le propriétaire veut ses devis en PDF, jamais en Word.", m).as_deref(),
            Some("retiens que je veux mes devis en PDF, jamais en Word")
        );
    }

    /// Ce qui doit rester à l'agent : une déduction, une phrase niée ou retournée, un
    /// sujet voisin, un texte trop court pour prouver quoi que ce soit.
    #[test]
    fn an_agent_inference_is_not_the_owners_word() {
        for c in [
            "Le propriétaire n'est pas disponible le mardi matin.",
            "Les réunions d'équipe n'ont pas lieu le mardi matin à 9 h.",
            "Les réunions d'équipe ont lieu le mardi matin à 9 h, donc aucun rendez-vous \
             client ne se prend le mardi avant midi.",
            "Les réunions d'équipe ont lieu le mardi matin à 9 h, donc le propriétaire est \
             indisponible avant midi.",
            "Les réunions de direction ont lieu le jeudi après-midi.",
            "Mardi.",
        ] {
            assert_eq!(owner_statement(c, MEETINGS), None, "{c}");
        }
        assert_eq!(owner_statement("Les réunions ont lieu le mardi.", ""), None);
    }
}
