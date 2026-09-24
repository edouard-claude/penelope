//! Lecture de la conversation d'une session : les entrées que la requête projette.

use crate::engine::ContextEngine;
use crate::transcript::Entry;
use penelope_llm::types::{ChatMessage, Role};

impl ContextEngine {
    /// Entrées à projeter : résumés LCM actifs, puis tout ce qu'ils ne couvrent pas.
    pub async fn projected_from_tables(
        &self,
        session_id: &str,
    ) -> penelope_store::Result<Vec<Entry>> {
        // Les résumés actifs d'abord : ce qu'ils couvrent n'a pas à être relu ni
        // désérialisé pour être aussitôt jeté (issue #55).
        let nodes = self.lcm.active_nodes(session_id).await?;
        let covered_to = nodes.iter().filter_map(|n| n.to_seq).max().unwrap_or(0);
        let from_seq = if nodes.is_empty() { 0 } else { covered_to + 1 };
        let mut entries = self.history.load(session_id, from_seq).await?;
        // Contexte volatil figé avec chaque message utilisateur (issue #17).
        let contexts = self.history.contexts_from(session_id, from_seq).await?;
        for e in entries.iter_mut() {
            if let Some(block) = contexts.get(&e.seq)
                && e.message.role == Role::User
            {
                let m = &mut e.message;
                match m.content.iter_mut().find_map(|c| match c {
                    penelope_llm::types::Content::Text { text } => Some(text),
                    _ => None,
                }) {
                    Some(text) => text.insert_str(0, block),
                    None => m
                        .content
                        .insert(0, penelope_llm::types::Content::text(block.clone())),
                }
            }
        }
        if nodes.is_empty() {
            return Ok(entries);
        }
        let mut out: Vec<Entry> = nodes
            .iter()
            .map(|n| {
                Entry::new(
                    0,
                    ChatMessage::system(format!(
                        "Résumé de la conversation antérieure (nœud {}) :\n{}",
                        n.id, n.summary
                    )),
                    n.tokens_self,
                )
            })
            .collect();
        out.extend(entries.into_iter().filter(|e| !e.compacted));
        Ok(out)
    }
}
