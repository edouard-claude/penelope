//! De l'écriture V0 d'un message à son événement `conv.*` (épopée #208, tâche T5).
//!
//! Phase 1 de la migration (`design/v1/source-de-verite.md` §4.1) : chaque ligne écrite
//! dans `messages` l'est aussi dans le journal, dans l'ordre. Ce module ne fait que
//! traduire ; l'écriture est dans [`crate::store`].

use penelope_llm::types::{ChatMessage, Role};

use super::*;

impl AssistantPayload {
    /// Ce qu'un appel dit de lui-même, sans contenu : le reste vient du message écrit.
    pub fn of_call(c: &CallRecord) -> Self {
        AssistantPayload {
            model: c.model.clone(),
            provider: c.provider.clone(),
            upstream: c.upstream.clone(),
            generation_id: c.generation_id.clone(),
            finish: c.finish.clone(),
            usage: c.usage.clone(),
            cost_usd: c.cost_usd,
            estimated: c.estimated,
            system_hash: c.system_hash.clone(),
            tools_hash: c.tools_hash.clone(),
            request_hash: c.request_hash.clone(),
            interrupted: c.interrupted,
            ..Default::default()
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
            let call = prov
                .call
                .as_deref()
                .map(AssistantPayload::of_call)
                .unwrap_or_default();
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
