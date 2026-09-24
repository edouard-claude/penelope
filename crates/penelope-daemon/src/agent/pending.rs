//! Appels d'outils sans résultat en fin de transcript.

use super::*;

/// Appels du dernier message assistant qui n'ont pas encore de résultat.
///
/// Si un message utilisateur est arrivé depuis, les appels sont abandonnés : la
/// conversation a repris ailleurs, et ils ne doivent pas bloquer la nouvelle demande.
pub fn pending_calls(tail: &[ChatMessage]) -> Vec<ToolCall> {
    let Some(idx) = tail
        .iter()
        .rposition(|m| m.role == Role::Assistant && !m.tool_calls.is_empty())
    else {
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
