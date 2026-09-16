//! Types d'échange avec les providers (§10).
//!
//! Le format interne est proche de l'API « chat completions », qui est le dénominateur
//! commun d'OpenRouter et de tous les endpoints OpenAI-compatibles (llama.cpp,
//! mistral.rs, Ollama).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "system" => Role::System,
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => return None,
        })
    }
}

/// Bloc de contenu. Le texte est le cas courant ; les images arrivent de Telegram (§10.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    /// Image en `data:` URI ou URL distante.
    ImageUrl {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Audio encodé (entrée vocale transcrite en amont, gardé pour les modèles audio).
    InputAudio {
        data: String,
        format: String,
    },
}

impl Content {
    pub fn text(s: impl Into<String>) -> Self {
        Content::Text { text: s.into() }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Arguments décodés. Un JSON illisible est conservé sous `{"__raw": "..."}` pour
    /// que le modèle puisse se corriger (§8.4, `isError` renvoyé au modèle).
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Pour un message `tool` : identifiant de l'appel auquel il répond.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Marqueur de cache (`cache_control`) pour les modèles Anthropic via OpenRouter.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cache_marker: bool,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self::simple(Role::System, text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::simple(Role::User, text)
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::simple(Role::Assistant, text)
    }

    pub fn simple(role: Role, text: impl Into<String>) -> Self {
        ChatMessage {
            role,
            content: vec![Content::text(text)],
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            cache_marker: false,
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        ChatMessage {
            role: Role::Tool,
            content: vec![Content::text(text)],
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
            cache_marker: false,
        }
    }

    pub fn with_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = calls;
        self
    }

    pub fn cached(mut self) -> Self {
        self.cache_marker = true;
        self
    }

    /// Concatène le texte des blocs.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn has_images(&self) -> bool {
        self.content
            .iter()
            .any(|c| matches!(c, Content::ImageUrl { .. }))
    }
}

/// Définition d'un outil exposé au modèle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// Schéma de sortie, quand le serveur MCP en fournit un (§8.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl ToolDef {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        ToolDef {
            name: name.into(),
            description: description.into(),
            parameters,
            output_schema: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Effort de raisonnement transmis quand le modèle le supporte (§10.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Sortie structurée exigée (classifieur, sub-agents avec `outputSchema`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
    Cancelled,
}

impl FinishReason {
    pub fn parse(s: &str) -> FinishReason {
        match s {
            "stop" | "end_turn" => FinishReason::Stop,
            "length" | "max_tokens" => FinishReason::Length,
            "tool_calls" | "tool_use" | "function_call" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt: u64,
    pub completion: u64,
    /// Tokens servis depuis le cache de préfixe du provider.
    pub cached: u64,
    pub reasoning: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.prompt + self.completion
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub provider: String,
    pub message: ChatMessage,
    pub finish: FinishReason,
    pub usage: Usage,
    /// Coût lu dans la réponse quand le provider l'expose, sinon calculé du catalogue.
    pub cost_usd: f64,
    pub cost_estimated: bool,
    /// Texte de raisonnement, quand il est exposé séparément.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
}

/// Fragment reçu en streaming SSE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamChunk {
    /// Les en-têtes sont arrivés : l'appel est réputé parti (§4.3 `response_started`).
    Started {
        id: String,
        model: String,
    },
    Delta {
        text: String,
    },
    Reasoning {
        text: String,
    },
    /// Un appel d'outil complet, reconstitué à partir des fragments.
    ToolCall(ToolCall),
    Usage(Usage),
    Done {
        finish: FinishReason,
    },
    Error {
        message: String,
        retryable: bool,
    },
}

/// Erreurs des providers, classées pour le routage de repli (§10.3 point 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmErrorKind {
    /// 5xx, timeout, coupure : le repli d'alias est légitime.
    Transient,
    /// 429 persistant.
    RateLimited,
    /// La requête dépasse la fenêtre : déclenche la compaction d'urgence (§5.4 niveau 4).
    ContextLength,
    /// 401/403 : clé invalide.
    Auth,
    /// 400 : requête malformée, inutile de réessayer.
    BadRequest,
    /// Modèle inconnu du catalogue.
    UnknownModel,
    Cancelled,
    Other,
}

impl LlmErrorKind {
    pub fn is_retryable(&self) -> bool {
        matches!(self, LlmErrorKind::Transient | LlmErrorKind::RateLimited)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{kind:?} : {message}")]
pub struct LlmError {
    pub kind: LlmErrorKind,
    pub message: String,
    pub status: Option<u16>,
    /// Vrai si le provider a pu facturer l'appel malgré l'erreur (§4.3).
    pub maybe_billed: bool,
}

impl LlmError {
    pub fn new(kind: LlmErrorKind, message: impl Into<String>) -> Self {
        LlmError {
            kind,
            message: message.into(),
            status: None,
            maybe_billed: false,
        }
    }
    pub fn transient(m: impl Into<String>) -> Self {
        Self::new(LlmErrorKind::Transient, m)
    }
    pub fn context_length(m: impl Into<String>) -> Self {
        Self::new(LlmErrorKind::ContextLength, m)
    }
    pub fn with_status(mut self, s: u16) -> Self {
        self.status = Some(s);
        self
    }
    pub fn billed(mut self) -> Self {
        self.maybe_billed = true;
        self
    }

    /// Classe une réponse HTTP.
    pub fn from_status(status: u16, body: &str) -> Self {
        let lower = body.to_lowercase();
        let kind = if lower.contains("context length")
            || lower.contains("context_length")
            || lower.contains("maximum context")
            || lower.contains("too many tokens")
            || lower.contains("prompt is too long")
        {
            LlmErrorKind::ContextLength
        } else {
            match status {
                401 | 403 => LlmErrorKind::Auth,
                404 => LlmErrorKind::UnknownModel,
                429 => LlmErrorKind::RateLimited,
                400 | 422 => LlmErrorKind::BadRequest,
                s if s >= 500 => LlmErrorKind::Transient,
                _ => LlmErrorKind::Other,
            }
        };
        LlmError {
            kind,
            message: body.chars().take(400).collect(),
            status: Some(status),
            maybe_billed: false,
        }
    }
}

pub type Result<T, E = LlmError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_text_concatenates_blocks() {
        let m = ChatMessage {
            role: Role::Assistant,
            content: vec![Content::text("bon"), Content::text("jour")],
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
            cache_marker: false,
        };
        assert_eq!(m.text(), "bonjour");
    }

    #[test]
    fn image_detection() {
        let mut m = ChatMessage::user("regarde");
        assert!(!m.has_images());
        m.content.push(Content::ImageUrl {
            url: "data:image/png;base64,AAA".into(),
            detail: None,
        });
        assert!(m.has_images());
    }

    #[test]
    fn finish_reason_aliases() {
        assert_eq!(FinishReason::parse("end_turn"), FinishReason::Stop);
        assert_eq!(FinishReason::parse("tool_use"), FinishReason::ToolCalls);
        assert_eq!(FinishReason::parse("max_tokens"), FinishReason::Length);
        assert_eq!(FinishReason::parse("inconnu"), FinishReason::Stop);
    }

    #[test]
    fn context_length_error_is_detected_whatever_the_status() {
        let e = LlmError::from_status(400, "This model's maximum context length is 128000 tokens");
        assert_eq!(e.kind, LlmErrorKind::ContextLength);
        let e = LlmError::from_status(400, "invalid tool schema");
        assert_eq!(e.kind, LlmErrorKind::BadRequest);
    }

    #[test]
    fn retryable_classification() {
        assert!(LlmError::from_status(503, "").kind.is_retryable());
        assert!(LlmError::from_status(429, "").kind.is_retryable());
        assert!(!LlmError::from_status(401, "").kind.is_retryable());
        assert!(!LlmError::from_status(400, "bad").kind.is_retryable());
    }

    #[test]
    fn serialisation_is_stable() {
        let r = ChatRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("salut")],
            tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
            stream: true,
            ..Default::default()
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: ChatRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn usage_total() {
        let u = Usage {
            prompt: 100,
            completion: 20,
            cached: 80,
            reasoning: 5,
        };
        assert_eq!(u.total(), 120);
    }
}
