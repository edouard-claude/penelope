//! Payloads des événements `conv.*` (`design/v1/source-de-verite.md` §2.2).
//!
//! Un champ par colonne du tableau du §2.2. Les champs facultatifs sont omis à
//! l'écriture quand ils sont vides : le journal est haché et vérifié en entier, il n'a
//! pas à porter des `null` qui ne disent rien.

use crate::anchors::Anchor;
use crate::compaction::AppliedStep;
use crate::tiers::TileMap;
use penelope_llm::types::{Content, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::SurfaceOp;

fn is_false(b: &bool) -> bool {
    !*b
}

/// Pourquoi le préfixe système change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemReason {
    /// Premier préfixe de la session.
    First,
    /// Préfixe en attente sorti à un cache froid (`stable_prefix`).
    Cold,
    /// Préfixe sorti avec un résumé publié.
    Compaction,
}

/// `conv.system` : le texte entier du préfixe (arbitrage du propriétaire, README §10.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPayload {
    pub surface: SurfaceOp,
    /// Empreinte du préfixe rendu (`system_hash`).
    pub hash: String,
    pub rendered: String,
    pub tiles: TileMap,
    pub reason: SystemReason,
}

/// Origine d'un message utilisateur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserSource {
    Owner,
    Merged,
    Trigger,
    Nudge,
    Photo,
    Import,
}

/// `conv.user`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPayload {
    pub surface: SurfaceOp,
    pub source: UserSource,
    /// Clé d'idempotence : identifiant du tour de la file (`turn_queue.id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrived_at: Option<String>,
    pub content: Vec<Content>,
    #[serde(default)]
    pub episode: i64,
    #[serde(default)]
    pub tokens_est: u64,
    /// Message absorbé pendant le tour : la requête porte la note de fusion.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mid_turn: bool,
}

/// `conv.context` : bloc volatil figé avec un message utilisateur (hors surface).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPayload {
    /// Adresse du `conv.user` visé.
    pub target: i64,
    pub block: String,
}

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

/// Ce que la projection a fait à la requête (niveaux 0, 2, 4), sans être journalisé.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectionTrace {
    #[serde(default)]
    pub levels: Vec<u8>,
    #[serde(default)]
    pub steps: Vec<AppliedStep>,
}

/// `conv.assistant`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssistantPayload {
    #[serde(default)]
    pub surface: SurfaceOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    #[serde(default)]
    pub step: u32,
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub estimated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<ProjectionTrace>,
    /// Réponse partielle gardée après `/stop`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupted: bool,
    #[serde(default)]
    pub episode: i64,
    #[serde(default)]
    pub tokens_est: u64,
}

/// `conv.tool_result` : un résultat (`append`) ou son corps externalisé (`replace` d'un
/// seul nœud, niveau 1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolResultPayload {
    #[serde(default)]
    pub surface: SurfaceOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    #[serde(default)]
    pub step: u32,
    pub call_id: String,
    pub tool: String,
    #[serde(default = "yes")]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub eager: bool,
    pub content: Vec<Content>,
    #[serde(default)]
    pub episode: i64,
    #[serde(default)]
    pub tokens_est: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_tokens: Option<u64>,
}

fn yes() -> bool {
    true
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

/// `conv.summary` : un nœud de résumé qui remplace une plage de la surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryPayload {
    pub surface: SurfaceOp,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_node_id: Option<String>,
    /// Texte rendu du résumé (`render_summary`).
    pub summary: String,
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub verbatim_users: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub tokens_src: u64,
    #[serde(default)]
    pub tokens_self: u64,
    #[serde(default)]
    pub batches_left: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

/// `conv.rewind` : coupe la surface après un nœud.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewindPayload {
    pub surface: SurfaceOp,
    #[serde(default)]
    pub turns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_session: Option<String>,
}

/// `conv.fork` : premier événement d'une session fille, qui hérite du préfixe de sa
/// mère par référence (README §10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForkPayload {
    pub surface: SurfaceOp,
    pub parent: String,
    pub up_to: i64,
    pub offset: i64,
}

/// Nœud LCM actif au scellement, bornes en adresses V0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealedNode {
    pub node: String,
    pub from: i64,
    pub to: i64,
    #[serde(default)]
    pub superseded_by: Option<String>,
}

/// `conv.import` : scelle l'historique V0 d'une session (§4.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportPayload {
    pub surface: SurfaceOp,
    pub messages: i64,
    #[serde(default)]
    pub contexts: i64,
    #[serde(default)]
    pub lcm_active: Vec<SealedNode>,
    pub digest: String,
}
