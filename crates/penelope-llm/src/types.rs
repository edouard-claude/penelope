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
    /// Raisonnement en clair d'un message assistant (champ `reasoning`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Blocs de raisonnement structurés (`reasoning_details`), à renvoyer **tels quels**
    /// avec les résultats d'outils : sans eux, un modèle qui raisonne perd le fil entre
    /// l'appel et la suite, et répond souvent à vide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
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
            reasoning: None,
            reasoning_details: None,
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
            reasoning: None,
            reasoning_details: None,
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
    /// Clé de regroupement OpenRouter (`session_id`) : les appels d'une même session
    /// restent sur le même provider amont, donc sur un cache de préfixe chaud, et sont
    /// regroupés dans les journaux OpenRouter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Modèles de repli essayés par OpenRouter lui-même (`models`), avant le premier
    /// jeton : une panne en début de flux bascule sans erreur côté client.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
    /// Modalités de sortie demandées : `["image", "text"]` pour générer une image (§10.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modalities: Vec<String>,
    /// Fournisseur amont à garder (nom affiché, `provider` de la réponse précédente) : son
    /// cache de préfixe est chaud (issue #17). OpenRouter seulement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_upstream: Option<String>,
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
            "error" => FinishReason::Error,
            _ => FinishReason::Stop,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt: u64,
    pub completion: u64,
    /// Tokens servis depuis le cache de préfixe du provider.
    pub cached: u64,
    /// Tokens écrits dans le cache (modèles à cache explicite, facturés à part).
    #[serde(default)]
    pub cache_write: u64,
    pub reasoning: u64,
    /// Coût facturé, tel qu'annoncé par OpenRouter (`usage.cost`, en USD). Absent chez
    /// les endpoints qui ne le donnent pas : le coût est alors estimé du catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
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
    /// Provider amont qui a servi la réponse (`provider` des fragments OpenRouter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Raison d'arrêt brute du provider amont (`native_finish_reason`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_finish: Option<String>,
    /// Refus explicite du modèle (`refusal`), à montrer tel quel plutôt qu'un vide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// Résultat d'une transcription audio (`/audio/transcriptions`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Transcription {
    pub text: String,
    /// Durée de l'audio, quand le serveur la donne.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    /// Coût facturé (`usage.cost` chez OpenRouter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
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
    /// Fragment de `reasoning_details` : blocs partiels, fusionnés par `index`.
    ReasoningDetails(serde_json::Value),
    /// Refus du modèle (`delta.refusal`).
    Refusal {
        text: String,
    },
    /// Image produite par le modèle, en URI `data:` (`delta.images`).
    Image {
        url: String,
    },
    /// Provider amont et raison d'arrêt brute, dès qu'ils sont connus.
    Meta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upstream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_finish: Option<String>,
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
        /// Type d'erreur canonique d'OpenRouter (`error.metadata.error_type`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_type: Option<String>,
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
    /// 402 : crédits épuisés ou plafond de la clé atteint.
    PaymentRequired,
    /// Refus du modèle ou filtre de contenu (`refusal`, `content_policy_violation`).
    ContentFilter,
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
    /// Type d'erreur canonique d'OpenRouter, quand il est fourni.
    pub error_type: Option<String>,
    /// Délai demandé par `Retry-After`, en secondes (429, 503).
    pub retry_after: Option<u64>,
}

impl LlmError {
    pub fn new(kind: LlmErrorKind, message: impl Into<String>) -> Self {
        LlmError {
            kind,
            message: message.into(),
            status: None,
            maybe_billed: false,
            error_type: None,
            retry_after: None,
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
    ///
    /// OpenRouter renvoie `{"error": {"code", "message", "metadata": {"error_type",
    /// "provider_name", "raw"}}}` : le type canonique prime sur le code HTTP, et le
    /// message amont (`raw`) est gardé quand le message principal est générique.
    pub fn from_status(status: u16, body: &str) -> Self {
        let parsed = serde_json::from_str::<Value>(body).ok();
        let err = parsed.as_ref().and_then(|v| v.get("error"));
        let error_type = err
            .and_then(|e| e.get("metadata"))
            .and_then(|m| m.get("error_type"))
            .and_then(|t| t.as_str())
            .map(String::from);

        let message = match err {
            Some(e) => {
                let mut m = e
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("erreur du provider")
                    .to_string();
                let meta = e.get("metadata");
                if let Some(p) = meta
                    .and_then(|m| m.get("provider_name"))
                    .and_then(|p| p.as_str())
                {
                    m.push_str(&format!(" ({p})"));
                }
                if let Some(raw) = meta.and_then(|m| m.get("raw")) {
                    let raw = match raw {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if !raw.is_empty() {
                        m.push_str(" : ");
                        m.push_str(&raw.chars().take(300).collect::<String>());
                    }
                }
                m
            }
            None => body.chars().take(400).collect(),
        };

        let lower = body.to_lowercase();
        let kind = error_type
            .as_deref()
            .and_then(kind_for_error_type)
            .unwrap_or_else(|| {
                if lower.contains("context length")
                    || lower.contains("context_length")
                    || lower.contains("maximum context")
                    || lower.contains("too many tokens")
                    || lower.contains("prompt is too long")
                {
                    LlmErrorKind::ContextLength
                } else {
                    match status {
                        401 | 403 => LlmErrorKind::Auth,
                        402 => LlmErrorKind::PaymentRequired,
                        404 => LlmErrorKind::UnknownModel,
                        408 => LlmErrorKind::Transient,
                        413 => LlmErrorKind::ContextLength,
                        429 => LlmErrorKind::RateLimited,
                        400 | 422 => LlmErrorKind::BadRequest,
                        s if s >= 500 => LlmErrorKind::Transient,
                        _ => LlmErrorKind::Other,
                    }
                }
            });
        LlmError {
            kind,
            message,
            status: Some(status),
            maybe_billed: false,
            error_type,
            retry_after: None,
        }
    }

    /// Erreur survenue au milieu d'un flux (HTTP 200 déjà envoyé).
    pub fn mid_stream(message: String, retryable: bool, error_type: Option<String>) -> Self {
        let kind = error_type
            .as_deref()
            .and_then(kind_for_error_type)
            .unwrap_or(if retryable {
                LlmErrorKind::Transient
            } else {
                LlmErrorKind::Other
            });
        LlmError {
            kind,
            message,
            status: None,
            maybe_billed: true,
            error_type,
            retry_after: None,
        }
    }
}

/// Correspondance des types d'erreur canoniques d'OpenRouter (stables sur tous ses
/// formats d'API) vers nos catégories.
pub fn kind_for_error_type(t: &str) -> Option<LlmErrorKind> {
    Some(match t {
        "context_length_exceeded" | "string_too_long" | "payload_too_large" => {
            LlmErrorKind::ContextLength
        }
        "authentication" | "permission_denied" => LlmErrorKind::Auth,
        "payment_required" | "token_limit_exceeded" => LlmErrorKind::PaymentRequired,
        "rate_limit_exceeded" => LlmErrorKind::RateLimited,
        "provider_overloaded" | "provider_unavailable" | "timeout" | "server" | "unmapped" => {
            LlmErrorKind::Transient
        }
        "not_found" => LlmErrorKind::UnknownModel,
        "content_policy_violation" | "refusal" => LlmErrorKind::ContentFilter,
        "invalid_request"
        | "invalid_prompt"
        | "precondition_failed"
        | "unprocessable"
        | "max_tokens_exceeded"
        | "invalid_image"
        | "image_too_large"
        | "image_too_small"
        | "unsupported_image_format"
        | "image_not_found"
        | "image_download_failed" => LlmErrorKind::BadRequest,
        _ => return None,
    })
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
            reasoning: None,
            reasoning_details: None,
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
            ..Default::default()
        };
        assert_eq!(u.total(), 120);
    }

    #[test]
    fn openrouter_error_bodies_are_classified_by_their_canonical_type() {
        let body = json!({"error": {
            "code": 400,
            "message": "Provider returned error",
            "metadata": {
                "error_type": "context_length_exceeded",
                "provider_name": "DeepInfra",
                "raw": "maximum context length is 131072 tokens"
            }
        }})
        .to_string();
        let e = LlmError::from_status(400, &body);
        assert_eq!(e.kind, LlmErrorKind::ContextLength);
        assert_eq!(e.error_type.as_deref(), Some("context_length_exceeded"));
        assert!(e.message.contains("DeepInfra"), "{}", e.message);
        assert!(e.message.contains("131072"), "{}", e.message);

        let credits = json!({"error": {"code": 402, "message": "Insufficient credits"}});
        let e = LlmError::from_status(402, &credits.to_string());
        assert_eq!(e.kind, LlmErrorKind::PaymentRequired);
        assert_eq!(e.message, "Insufficient credits");

        // Le type canonique l'emporte sur le code HTTP.
        let refusal = json!({"error": {"code": 403, "message": "refused",
            "metadata": {"error_type": "refusal"}}});
        let e = LlmError::from_status(403, &refusal.to_string());
        assert_eq!(e.kind, LlmErrorKind::ContentFilter);
    }

    #[test]
    fn mid_stream_errors_keep_their_type() {
        let e = LlmError::mid_stream(
            "Rate limit exceeded".into(),
            true,
            Some("rate_limit_exceeded".into()),
        );
        assert_eq!(e.kind, LlmErrorKind::RateLimited);
        assert!(e.maybe_billed);
        let e = LlmError::mid_stream("boom".into(), true, None);
        assert_eq!(e.kind, LlmErrorKind::Transient);
        assert_eq!(FinishReason::parse("error"), FinishReason::Error);
    }
}
