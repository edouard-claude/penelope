//! De l'écriture V0 d'un message à son événement `conv.*` (épopée #208, tâche T5).
//!
//! Phase 1 de la migration (`design/v1/source-de-verite.md` §4.1) : chaque ligne écrite
//! dans `messages` l'est aussi dans le journal, dans l'ordre. Ce module ne fait que
//! traduire ; l'écriture est dans [`crate::store`].

use penelope_llm::types::{ChatMessage, ChatResponse, Role};

use super::*;

/// Ce que l'appelant sait d'un message au-delà de son contenu : le tour, l'étape, la
/// provenance d'un message utilisateur, l'appel au modèle d'une réponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Provenance {
    /// Tour d'origine (`origin_turn`).
    pub turn: Option<String>,
    /// Itération de la boucle.
    pub step: u32,
    /// Origine d'un message utilisateur ; `owner` par défaut.
    pub source: Option<UserSource>,
    /// Clé d'idempotence d'un message venu de la file.
    pub turn_message_id: Option<String>,
    pub arrived_at: Option<String>,
    /// Message absorbé pendant le tour.
    pub mid_turn: bool,
    /// Issue d'un résultat d'outil ; vrai par défaut.
    pub ok: Option<bool>,
    /// L'appel qui a produit une réponse : modèle, usage, empreintes. Le contenu, lui,
    /// vient toujours du message écrit.
    pub call: Option<Box<AssistantPayload>>,
}

impl Provenance {
    /// Un message utilisateur de cette origine.
    pub fn user(source: UserSource) -> Self {
        Provenance {
            source: Some(source),
            ..Default::default()
        }
    }

    /// Un message utilisateur venu de la file, avec sa clé et son heure d'arrivée.
    pub fn queued(source: UserSource, turn_message_id: &str, arrived_at: &str) -> Self {
        Provenance {
            source: Some(source),
            turn_message_id: Some(turn_message_id.to_string()),
            arrived_at: Some(arrived_at.to_string()),
            ..Default::default()
        }
    }
}

impl AssistantPayload {
    /// Ce qu'une réponse du modèle dit de son appel, sans son contenu.
    pub fn of_response(r: &ChatResponse) -> Self {
        AssistantPayload {
            model: Some(r.model.clone()),
            provider: Some(r.provider.clone()),
            upstream: r.upstream.clone(),
            generation_id: (!r.id.is_empty()).then(|| r.id.clone()),
            finish: Some(format!("{:?}", r.finish).to_lowercase()),
            usage: Some(TokenUsage {
                prompt: r.usage.prompt,
                completion: r.usage.completion,
                cached: r.usage.cached,
                cache_write: r.usage.cache_write,
                reasoning: r.usage.reasoning,
            }),
            cost_usd: Some(r.cost_usd),
            estimated: r.cost_estimated,
            ..Default::default()
        }
    }
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

    /// Une réponse reçue puis écartée (réponse vide, #206) : son appel, son usage et son
    /// coût, que `budget.record` a déjà comptés. Rien de son contenu n'entre en surface.
    pub fn of_response(cause: AttemptCause, r: &ChatResponse) -> Self {
        let call = AssistantPayload::of_response(r);
        AttemptPayload {
            model: call.model,
            provider: call.provider,
            upstream: call.upstream,
            usage: call.usage,
            cost_usd: call.cost_usd,
            partial_reasoning: (!r.reasoning.is_empty()).then(|| r.reasoning.clone()),
            ..AttemptPayload::new(cause)
        }
    }
}

/// L'événement d'un message écrit dans `messages`. `None` pour un message système : le
/// préfixe a son propre événement (`conv.system`, T6), un message système dans
/// l'historique n'a pas d'équivalent au §2.2.
pub fn message_event(
    message: &ChatMessage,
    tokens: u64,
    episode: i64,
    eager: bool,
    prov: &Provenance,
) -> Option<ConvEvent> {
    Some(match message.role {
        Role::System => return None,
        Role::User => ConvEvent::User(UserPayload {
            surface: SurfaceOp::Append,
            source: prov.source.unwrap_or(UserSource::Owner),
            turn_message_id: prov.turn_message_id.clone(),
            arrived_at: prov.arrived_at.clone(),
            content: message.content.clone(),
            episode,
            tokens_est: tokens,
            mid_turn: prov.mid_turn,
        }),
        Role::Assistant => {
            let call = prov.call.as_deref().cloned().unwrap_or_default();
            ConvEvent::Assistant(Box::new(AssistantPayload {
                surface: SurfaceOp::Append,
                turn: prov.turn.clone(),
                step: prov.step,
                content: message.content.clone(),
                tool_calls: message.tool_calls.clone(),
                reasoning: message.reasoning.clone(),
                reasoning_details: message.reasoning_details.clone(),
                verbatim: verbatim_of(message),
                eager,
                episode,
                tokens_est: tokens,
                ..call
            }))
        }
        Role::Tool => ConvEvent::ToolResult(ToolResultPayload {
            surface: SurfaceOp::Append,
            turn: prov.turn.clone(),
            step: prov.step,
            call_id: message.tool_call_id.clone().unwrap_or_default(),
            tool: message.name.clone().unwrap_or_default(),
            ok: prov.ok.unwrap_or(true),
            eager,
            content: message.content.clone(),
            episode,
            tokens_est: tokens,
            ..Default::default()
        }),
    })
}
