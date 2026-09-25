//! Ce qu'un appel au modèle dit de lui-même, sans son contenu (épopée #208, lot K).
//!
//! La boucle le remplit à chaque réponse ; `penelope-context` le verse dans le
//! `conv.assistant` du message écrit (`AssistantPayload::of_call`), dont le contenu vient
//! toujours du message lui-même.

use super::TokenUsage;

/// L'appel qui a produit une réponse : modèle, usage, coût, empreintes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallRecord {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub upstream: Option<String>,
    pub generation_id: Option<String>,
    pub finish: Option<String>,
    pub usage: Option<TokenUsage>,
    pub cost_usd: Option<f64>,
    pub estimated: bool,
    pub system_hash: Option<String>,
    pub tools_hash: Option<String>,
    pub request_hash: Option<String>,
    /// Réponse partielle gardée après `/stop`.
    pub interrupted: bool,
}
