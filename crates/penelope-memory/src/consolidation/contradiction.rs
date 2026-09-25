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
    let contradicts = negates(existing_text, &candidate.text);
    if !contradicts {
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
/// Deux entrées se contredisent : directives de polarité opposée sur le même sujet.
pub fn contradicts(a: &str, b: &str) -> bool {
    negates(a, b)
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

fn negates(a: &str, b: &str) -> bool {
    // Deux règles, pas un dossier : au-delà de la borne d'une entrée, ou d'un rapport de
    // longueur de trois, la comparaison n'a pas de sens (issue #145).
    let (la, lb) = (a.chars().count(), b.chars().count());
    if la == 0
        || lb == 0
        || la > crate::quality::MAX_ENTRY_CHARS
        || lb > crate::quality::MAX_ENTRY_CHARS
        || la.max(lb) as f64 / la.min(lb) as f64 > MAX_LENGTH_RATIO
    {
        return false;
    }
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
