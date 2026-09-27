use super::*;

/// Contradiction (§6.8) : un candidat `owner` qui contredit une entrée curée **sans**
/// signature de contexte distincte donne une question dans le digest, jamais un arbitrage.
#[derive(Debug, Clone, PartialEq)]
pub enum Contradiction {
    /// Contextes distincts : ce n'est pas une contradiction, c'est une exception.
    DistinctContext,
    /// Vraie contradiction : question au propriétaire.
    NeedsQuestion { existing: String, candidate: String },
}

pub fn detect_contradiction(
    candidate: &Candidate,
    existing_text: &str,
    existing_when: Option<&When>,
) -> Option<Contradiction> {
    if candidate.origin != Origin::Owner {
        return None;
    }
    // Une contradiction oppose deux **règles**. Un fait observé et un écart n'en sont
    // pas : ils ne se contredisent pas, ils se datent (issue #145).
    if matches!(candidate.ctype, CandidateType::Fait | CandidateType::Ecart) {
        return None;
    }
    if !contradicts(existing_text, &candidate.text) {
        return None;
    }
    match (&candidate.quand, existing_when) {
        (Some(a), Some(b)) if !a.compatible_with(b) => Some(Contradiction::DistinctContext),
        (Some(_), None) => Some(Contradiction::DistinctContext),
        _ => Some(Contradiction::NeedsQuestion {
            existing: existing_text.to_string(),
            candidate: candidate.text.clone(),
        }),
    }
}

/// Heuristique de négation : deux directives de **polarité opposée** portant sur le même
/// sujet.
///
/// La comparaison de sujet se fait par **containment** sur les mots significatifs (le plus
/// petit énoncé sert de dénominateur) : « Toujours répondre en anglais aux clients » et
/// « Jamais de réponse en anglais » se contredisent malgré des longueurs différentes.
/// Deux entrées se contredisent : directives de polarité opposée sur le même sujet, ou
/// une valeur mise à la place d'une autre (« tutoyer » contre « vouvoyer », issue #224).
pub fn contradicts(a: &str, b: &str) -> bool {
    comparable(a, b) && (negates(a, b) || substitutes(a, b))
}

/// Part de sujet commun exigée, en Jaccard (dénominateur : l'union). Le containment
/// d'avant prenait le plus petit énoncé comme dénominateur : face à un dossier de 3 000
/// caractères, deux mots communs suffisaient à « contredire » n'importe quoi (#145).
const SUBJECT_JACCARD: f64 = 0.4;
/// Écart de longueur admis entre deux énoncés comparés : un dossier n'est pas une règle.
const MAX_LENGTH_RATIO: f64 = 3.0;
/// Similarité d'embedding exigée avant de dire deux énoncés contradictoires : le Jaccard
/// dit le vocabulaire commun, la similarité dit le sujet commun, et les deux ensemble
/// écartent le voisin lointain qu'aucun seuil ne filtrait (#145). Elle ne s'applique que
/// si la similarité a pu être mesurée ; sans vecteur, le Jaccard décide seul.
pub const CONTRADICTION_SIMILARITY: f64 = 0.80;
/// Mots de tête où se lit la directive : « Toujours répondre… », « Ne jamais… ». Un
/// « toujours payé » au milieu d'un dossier n'est pas une polarité.
const DIRECTIVE_WORDS: usize = 6;

/// Deux règles, pas un dossier : au-delà de la borne d'une entrée, ou d'un rapport de
/// longueur de trois, la comparaison n'a pas de sens (issue #145).
fn comparable(a: &str, b: &str) -> bool {
    let (la, lb) = (a.chars().count(), b.chars().count());
    la > 0
        && lb > 0
        && la <= crate::quality::MAX_ENTRY_CHARS
        && lb <= crate::quality::MAX_ENTRY_CHARS
        && la.max(lb) as f64 / la.min(lb) as f64 <= MAX_LENGTH_RATIO
}

fn negates(a: &str, b: &str) -> bool {
    let (pa, pb) = (polarity(a), polarity(b));
    if pa == 0 || pb == 0 || pa == pb {
        return false;
    }
    let (wa, wb) = (significant_words(a), significant_words(b));
    if wa.is_empty() || wb.is_empty() {
        return false;
    }
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    inter / union >= SUBJECT_JACCARD
}

/// Valeurs qui s'excluent sur un même sujet : dire l'une, c'est renoncer à l'autre. Un
/// motif finissant par `*` est un radical (« tutoi* » : tutoie, tutoiement), sinon le
/// mot entier (« euro », pas « européen »). `true` : la famille est son propre sujet
/// (tutoyer ou vouvoyer, c'est toujours la façon de s'adresser au propriétaire) ; sinon
/// il faut en plus un mot de sujet commun hors de la famille, « facturer en dollars »
/// contre « facturer en euros ».
const EXCLUSIVE_VALUES: &[(bool, &[&[&str]])] = &[
    (true, &[&["tutoi*", "tutoy*"], &["vouvoi*", "vouvoy*"]]),
    (
        false,
        &[
            &["françai*", "francai*"],
            &["anglai*"],
            &["allemand*"],
            &["espagnol*"],
            &["italien*"],
        ],
    ),
    (false, &[&["euro", "euros"], &["dollar*"], &["sterling"]]),
    (
        false,
        &[
            &["lundi*"],
            &["mardi*"],
            &["mercredi*"],
            &["jeudi*"],
            &["vendredi*"],
            &["samedi*"],
            &["dimanche*"],
        ],
    ),
];

fn matches_value(word: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(stem) => word.starts_with(stem),
        None => word == pattern,
    }
}

/// Substitution (issue #224) : les deux énoncés nomment chacun une valeur d'une même
/// famille exclusive, et aucune valeur en commun. « Toujours tutoyer » et « je préfère
/// qu'on se vouvoie » n'ont ni polarité opposée ni vocabulaire commun : la négation ne
/// les voyait jamais. Deux énoncés qui citent les deux valeurs (« en dollars, pas en
/// euros ») ne se substituent pas : dans le doute, pas de question.
fn substitutes(a: &str, b: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(String::from)
            .collect()
    };
    let (wa, wb) = (words(a), words(b));
    let values = |ws: &[String], family: &[&[&str]]| -> std::collections::BTreeSet<usize> {
        family
            .iter()
            .enumerate()
            .filter(|(_, pats)| ws.iter().any(|w| pats.iter().any(|p| matches_value(w, p))))
            .map(|(i, _)| i)
            .collect()
    };
    EXCLUSIVE_VALUES.iter().any(|(own_subject, family)| {
        let (va, vb) = (values(&wa, family), values(&wb, family));
        if va.is_empty() || vb.is_empty() || !va.is_disjoint(&vb) {
            return false;
        }
        // Les mots de la famille ne font pas le sujet : « euros » et « dollars » ne
        // sont pas un vocabulaire commun.
        let subject = |s: &str| -> std::collections::BTreeSet<String> {
            significant_words(s)
                .into_iter()
                .filter(|w| {
                    !family.iter().flat_map(|pats| pats.iter()).any(|p| {
                        matches_value(w, p) || p.trim_end_matches('*').starts_with(w.as_str())
                    })
                })
                .collect()
        };
        *own_subject || !subject(a).is_disjoint(&subject(b))
    })
}

/// Polarité d'une **directive** : le marqueur se lit en tête de la première phrase, là où
/// une règle s'énonce. Ailleurs dans le texte, c'est une tournure, pas une consigne.
fn polarity(s: &str) -> i8 {
    let head: String = s
        .split(['.', ';', '\n'])
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .split_whitespace()
        .take(DIRECTIVE_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    if head.contains("jamais") || head.contains("éviter") || head.contains("ne pas") {
        -1
    } else if head.contains("toujours") || head.contains("préférer") {
        1
    } else {
        0
    }
}

/// Mots porteurs de sens : on écarte les marqueurs de polarité et les mots courts, sinon
/// « toujours » et « jamais » feraient croire à un sujet commun.
fn significant_words(s: &str) -> std::collections::BTreeSet<String> {
    const IGNORED: &[&str] = &[
        "toujours",
        "jamais",
        "eviter",
        "éviter",
        "preferer",
        "préférer",
        "pas",
        "plus",
        "aux",
        "les",
        "des",
        "une",
        "sans",
        "avec",
        "pour",
        "dans",
        "sur",
    ];
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| w.chars().count() > 3 && !IGNORED.contains(w))
        // Rapproche « réponse » et « répondre », « client » et « clientèle » : cinq
        // lettres suffisent, et le seuil de Jaccard fait le reste (issue #145).
        .map(|w| w.chars().take(5).collect::<String>())
        .collect()
}
