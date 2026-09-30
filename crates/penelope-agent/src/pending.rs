//! Appels d'outils sans résultat en fin de transcript.

use super::*;
use penelope_app::conversation::carries_tool_calls;

/// Appels du dernier message assistant qui n'ont pas encore de résultat.
///
/// Si un message utilisateur est arrivé depuis, les appels sont abandonnés : la
/// conversation a repris ailleurs, et ils ne doivent pas bloquer la nouvelle demande.
pub fn pending_calls(tail: &[ChatMessage]) -> Vec<ToolCall> {
    let Some(idx) = tail.iter().rposition(carries_tool_calls) else {
        return Vec::new();
    };
    let after = &tail[idx + 1..];
    if after.iter().any(|m| m.role != Role::Tool) {
        return Vec::new();
    }
    let answered: BTreeSet<&str> = after
        .iter()
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    tail[idx]
        .tool_calls
        .iter()
        .filter(|c| !answered.contains(c.id.as_str()))
        .cloned()
        .collect()
}

/// L'identifiant d'un appel en attente a-t-il déjà servi plus tôt dans la queue, à un
/// appel émis ou à un résultat (#266) ? Le message qui porte les appels en attente n'est
/// pas relu : ses propres résultats, sur une reprise, ne comptent pas.
pub fn seen_before(tail: &[ChatMessage], call_id: &str) -> bool {
    let Some(idx) = tail.iter().rposition(carries_tool_calls) else {
        return false;
    };
    tail[..idx].iter().any(|m| {
        m.tool_call_id.as_deref() == Some(call_id) || m.tool_calls.iter().any(|c| c.id == call_id)
    })
}
