//! Découpage d'une entrée fourre-tout (issue #145).
//!
//! Les sessions d'introspection ont écrit des dossiers entiers dans la mémoire de fond :
//! six entrées `projet` de 1 557 à 3 950 caractères, mêlant identité, clients, montants,
//! solde bancaire et salaire. Une telle entrée fausse la consolidation — elle « contredit »
//! tout ce qu'elle approche — et déborde dans le digest.
//!
//! La passe **ne s'exécute jamais toute seule** : le digest la propose, le propriétaire la
//! lance (`penelope mem split <uid>`), le modèle propose un découpage en faits courts, et
//! une carte `memory_proposal` demande le dernier mot. Les données financières
//! personnelles (soldes, salaire, épargne) restent hors de la mémoire de fond (#25).

use crate::runtime::{Daemon, Services};
use penelope_kernel::event::EventDraft;
use penelope_kernel::risk::RiskClass;
use penelope_llm::catalog::strip_provider;
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::{ChatMessage, ChatRequest};
use penelope_memory::{IndexedEntry, Level};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(120);

/// Consigne de découpage. Elle dit la borne, le format, et ce qui ne rentre pas.
const PROMPT: &str = "Tu découpes une entrée de mémoire trop longue en faits courts.\n\
    Règles :\n\
    - un fait par ligne, préfixé de « - », sans titre ni commentaire ;\n\
    - 300 caractères au plus par ligne, une seule idée par ligne ;\n\
    - garder la formulation d'origine quand elle tient, ne rien inventer ;\n\
    - dater ce qui est daté dans le texte, laisser le reste tel quel ;\n\
    - **écarter** les données financières personnelles (solde bancaire, salaire, épargne, \
    chiffre d'affaires, montants de facture) et les identifiants administratifs : ils n'ont \
    pas leur place en mémoire de fond ;\n\
    - au plus 12 lignes : ce qui ne tient pas est du détail, il reste dans le vault.\n\
    Répondre par les seules lignes de faits.";

/// Niveaux injectés automatiquement : ce sont eux que la borne concerne.
pub const LEVELS: &[Level] = &[
    Level::Instruction,
    Level::Profil,
    Level::Coeur,
    Level::Projet,
];

/// Entrées actives au-delà de la borne d'une entrée, du plus long au plus court.
pub async fn oversized(s: &Services) -> Vec<IndexedEntry> {
    let hidden = s.memory.hidden_uids().await.unwrap_or_default();
    let max = penelope_memory::quality::MAX_ENTRY_CHARS;
    let mut out = Vec::new();
    for level in LEVELS {
        for e in s.memory.by_level(*level).await.unwrap_or_default() {
            if e.text.chars().count() > max && !hidden.contains(&e.uid) {
                out.push(e);
            }
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.text.chars().count()));
    out
}

/// Propose le découpage d'une entrée : une carte `memory_proposal`, jamais une écriture.
/// Rend l'identifiant de la demande.
pub async fn propose(d: &Arc<Daemon>, uid: &str) -> anyhow::Result<String> {
    let s = &d.services;
    let Some(entry) = s.memory.get(uid).await? else {
        anyhow::bail!("entrée `{uid}` introuvable");
    };
    let max = penelope_memory::quality::MAX_ENTRY_CHARS;
    if entry.text.chars().count() <= max {
        anyhow::bail!("`{uid}` tient déjà en {max} caractères : rien à découper",);
    }
    let facts = cut(d, &entry.text).await?;
    if facts.is_empty() {
        anyhow::bail!("le modèle n'a proposé aucun fait pour `{uid}`");
    }
    let req = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::MemoryProposal,
            "mémoire",
            RiskClass::Write,
            json!({
                "split": true,
                "uid": entry.uid,
                "level": entry.level.as_str(),
                "file": entry.file,
                "source": entry.file,
                "items": facts,
                "question": format!(
                    "Découper « {} » ({} caractères) en {} fait(s) ?",
                    short(&entry.text),
                    entry.text.chars().count(),
                    facts.len()
                ),
            }),
            vec!["Découper".into(), "Ignorer".into()],
            None,
            None,
            false,
        )
        .await?;
    s.events
        .append(EventDraft::new(
            "memory.split_proposed",
            json!({"approval": req.id.as_str(), "uid": entry.uid, "facts": facts.len()}),
        ))
        .await?;
    Ok(req.id.as_str().to_string())
}

/// Les faits proposés par le modèle, bornés et nettoyés.
async fn cut(d: &Arc<Daemon>, text: &str) -> anyhow::Result<Vec<String>> {
    let s = &d.services;
    let cfg = s.config.config();
    let alias = cfg.role_alias("memory_review");
    let model = cfg
        .alias_model(&alias)
        .ok_or_else(|| anyhow::anyhow!("aucun modèle pour l'alias `{alias}`"))?
        .to_string();
    // Travail de fond : jamais l'abonnement du propriétaire (issue #142).
    let model = crate::codex_scope::background(&d.services, &model, "découpage de mémoire").await;
    let provider = d
        .provider_for(&model)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let info = s.catalog.get(strip_provider(&model));
    tokio::time::timeout(TIMEOUT, async {
        let mut last_quality = ProposalQuality::Empty;
        for attempt in 0..2 {
            let prompt = if attempt == 0 {
                PROMPT.to_string()
            } else {
                format!(
                    "{PROMPT}\nLa réponse précédente était vide ou trop partielle. \
                     Donne plusieurs faits distincts sous forme de puces, sans introduction."
                )
            };
            let request = ChatRequest {
                model: model.clone(),
                messages: vec![
                    ChatMessage::system(prompt),
                    ChatMessage::user(format!("<entree>\n{text}\n</entree>")),
                ],
                stream: true,
                max_tokens: Some(if attempt == 0 { 2_000 } else { 3_000 }),
                reasoning_effort: info.as_ref().and_then(|i| i.lightest_effort()),
                ..Default::default()
            };
            let rx = provider
                .chat_stream(request, CancelToken::new())
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let response = collect_stream(rx, &model, provider.name(), &s.catalog)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let _ = s
                .budget
                .record(penelope_kernel::budget::UsageRecord {
                    model: response.model.clone(),
                    provider: response.provider.clone(),
                    role: Some("memory_review".into()),
                    prompt: response.usage.prompt,
                    completion: response.usage.completion,
                    cost_usd: response.cost_usd,
                    estimated: response.cost_estimated,
                    ..Default::default()
                })
                .await;
            let raw = response.message.text();
            let facts = parse_facts(&raw);
            last_quality = proposal_quality(text.chars().count(), &raw, &facts);
            if last_quality == ProposalQuality::Ready {
                return Ok(facts);
            }
        }
        anyhow::bail!(
            "découpage refusé après deux essais : {}",
            last_quality.reason()
        )
    })
    .await
    .map_err(|_| anyhow::anyhow!("le découpage a pris trop de temps"))?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProposalQuality {
    Ready,
    Empty,
    NoFacts,
    TooSparse,
}

impl ProposalQuality {
    fn reason(self) -> &'static str {
        match self {
            Self::Ready => "proposition suffisante",
            Self::Empty => "réponse vide du modèle",
            Self::NoFacts => "aucun fait exploitable dans la réponse du modèle",
            Self::TooSparse => "un seul fait proposé pour une longue entrée",
        }
    }
}

fn proposal_quality(source_chars: usize, raw: &str, facts: &[String]) -> ProposalQuality {
    // Une entrée de plus de deux fois la borne ne tient pas dans un seul fait :
    // une carte avec une seule ligne donnerait une fausse impression de couverture.
    if raw.trim().is_empty() {
        ProposalQuality::Empty
    } else if facts.is_empty() {
        ProposalQuality::NoFacts
    } else if source_chars > 2 * penelope_memory::quality::MAX_ENTRY_CHARS && facts.len() < 2 {
        ProposalQuality::TooSparse
    } else {
        ProposalQuality::Ready
    }
}

/// Lignes de faits d'une réponse : puces, bornées, sans doublon.
pub fn parse_facts(raw: &str) -> Vec<String> {
    let max = penelope_memory::quality::MAX_ENTRY_CHARS;
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let Some(t) = bullet_text(line) else {
            continue;
        };
        let parts: Vec<&str> = if t.chars().count() > max {
            // Un modèle regroupe parfois plusieurs phrases dans une seule puce.
            // Ne couper qu'aux fins de phrase, jamais au milieu d'un fait.
            split_sentences(t)
        } else {
            vec![t]
        };
        for part in parts {
            let fact = part.trim();
            if fact.is_empty()
                || fact.chars().count() > max
                || fact.ends_with(':')
                || private_detail(fact)
            {
                continue;
            }
            if !out.iter().any(|existing| existing == fact) {
                out.push(fact.to_string());
            }
        }
    }
    out.truncate(12);
    out
}

fn split_sentences(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (at, mark) in text.char_indices() {
        if !matches!(mark, '.' | ';') {
            continue;
        }
        let end = at + mark.len_utf8();
        if text[end..].chars().next().is_none_or(char::is_whitespace) {
            parts.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

/// Seules les lignes explicitement présentées comme faits sont acceptées.
fn bullet_text(line: &str) -> Option<&str> {
    let t = line.trim();
    if let Some(rest) = t.strip_prefix(['-', '*', '•']) {
        return rest.starts_with(char::is_whitespace).then(|| rest.trim());
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 2 {
        return None;
    }
    let rest = &t[digits..];
    let rest = rest.strip_prefix(['.', ')'])?;
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}

/// Garde-fou local : le modèle ne suffit pas à exclure les données privées.
fn private_detail(fact: &str) -> bool {
    let lower = fact.to_lowercase();
    [
        "€",
        "iban",
        "siren",
        "siret",
        "salaire",
        "solde bancaire",
        "épargne",
        "chiffre d'affaires",
        "montant de facture",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

fn short(text: &str) -> String {
    let t: String = text.chars().take(80).collect();
    if text.chars().count() > 80 {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #145 : la réponse du modèle se lit en puces ; titre, doublon et pavé au-delà de la
    /// borne sont écartés.
    #[test]
    fn facts_are_read_as_bullets() {
        let long = "x".repeat(penelope_memory::quality::MAX_ENTRY_CHARS + 1);
        let raw = format!(
            "Voici les faits :\n\
             - Yobbu est une marketplace de services.\n\
             * Yobbu est une marketplace de services.\n\
             - {long}\n\
             \u{20} - Le 14 ouvre en 2027.\n"
        );
        assert_eq!(
            parse_facts(&raw),
            [
                "Yobbu est une marketplace de services.",
                "Le 14 ouvre en 2027.",
            ]
        );
    }

    #[test]
    fn an_apology_or_intro_is_never_a_memory_fact() {
        assert!(parse_facts("Je ne peux pas découper cette entrée.").is_empty());
        assert_eq!(
            parse_facts("Voici les faits proposés :\n1. Le projet est en cours."),
            ["Le projet est en cours."]
        );
    }

    #[test]
    fn a_long_bullet_is_split_at_sentence_boundaries() {
        let first = format!("Le projet utilise {}.", "Rust ".repeat(55));
        let second = "Les tests passent sur macOS et Linux.";
        let raw = format!("- {first} {second}");
        assert!(raw.chars().count() > penelope_memory::quality::MAX_ENTRY_CHARS);
        assert_eq!(parse_facts(&raw), [first.trim(), second]);
    }

    #[test]
    fn splitting_a_long_bullet_keeps_dots_inside_urls() {
        let first = format!("Consulter https://example.org pour {}.", "Rust ".repeat(50));
        let second = "Les tests passent sur macOS et Linux.";
        let raw = format!("- {first} {second}");
        assert!(raw.chars().count() > penelope_memory::quality::MAX_ENTRY_CHARS);
        assert_eq!(parse_facts(&raw), [first.trim(), second]);
    }

    #[test]
    fn financial_and_administrative_details_are_not_proposed() {
        let raw = "- Le salaire est de 5 000 € par mois.\n\
                   - Le projet utilise Rust.\n\
                   - Son IBAN est FR7612345678901234567890123.";
        assert_eq!(parse_facts(raw), ["Le projet utilise Rust."]);
    }

    #[test]
    fn a_silent_or_sparse_model_answer_requires_another_attempt() {
        assert_eq!(proposal_quality(2_000, "", &[]), ProposalQuality::Empty);
        assert_eq!(
            proposal_quality(2_000, "Je ne peux pas découper cette entrée.", &[]),
            ProposalQuality::NoFacts
        );
        assert_eq!(
            proposal_quality(2_000, "- Un fait court.", &["Un fait court.".into()]),
            ProposalQuality::TooSparse
        );
        assert_eq!(
            proposal_quality(2_000, "- A.\n- B.", &["A.".into(), "B.".into()]),
            ProposalQuality::Ready
        );
        assert_eq!(
            proposal_quality(450, "- A.", &["A.".into()]),
            ProposalQuality::Ready
        );
    }
}
