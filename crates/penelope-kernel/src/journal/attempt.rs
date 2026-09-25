//! `conv.attempt` : ce qu'un appel au modèle a coûté sans donner de réponse (#206).
//!
//! La forme du payload ; `penelope-context` la relit dans son pliage (seule la consigne
//! de relance entre dans la requête suivante), la boucle la remplit.

use serde::{Deserialize, Serialize};

/// Tokens facturés d'un appel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt: u64,
    #[serde(default)]
    pub completion: u64,
    #[serde(default)]
    pub cached: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub reasoning: u64,
}

/// Cause d'une tentative d'appel qui n'a pas donné de réponse (#206).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptCause {
    StreamCut,
    BeforeStream,
    EmptyAnswer,
    Fallback,
}

impl AttemptCause {
    /// Le nom écrit dans le journal, repris par les journaux du daemon.
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptCause::StreamCut => "stream_cut",
            AttemptCause::BeforeStream => "before_stream",
            AttemptCause::EmptyAnswer => "empty_answer",
            AttemptCause::Fallback => "fallback",
        }
    }
}

/// `conv.attempt` : hors surface ; seule sa consigne de relance entre dans la requête
/// suivante du même tour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    #[serde(default)]
    pub step: u32,
    pub cause: AttemptCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_request_id: Option<String>,
    /// Consigne ajoutée en dernier message utilisateur à la requête suivante.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_prompt: Option<String>,
}

impl AttemptPayload {
    /// Une tentative de cette cause, sans rien d'autre : l'appelant remplit ce qu'il sait.
    pub fn new(cause: AttemptCause) -> Self {
        AttemptPayload {
            turn: None,
            step: 0,
            cause,
            model: None,
            provider: None,
            upstream: None,
            error: None,
            partial_text: None,
            partial_reasoning: None,
            usage: None,
            cost_usd: None,
            llm_request_id: None,
            retry_prompt: None,
        }
    }
}
