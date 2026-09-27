//! De la surface aux entrées et à la requête (`source-de-verite.md` §2.3, « De la
//! surface à la requête »).
//!
//! Chaque texte ajouté ici reproduit octet pour octet celui de la projection V0 du
//! daemon (`conversation.rs`, `agent.rs`) : la phase 3 compare les deux, et un octet de
//! différence casse le cache de prompt (§7, risque 1).

use super::{Slot, Surface};
use crate::transcript::Entry;
use penelope_llm::types::{ChatMessage, Content, Role};

/// Note insérée après les messages système quand un message utilisateur est arrivé
/// pendant le tour (`SessionConversation::with_merge_note`).
pub const MERGE_NOTE: &str = "Un nouveau message utilisateur est arrivé pendant le tour ; ce qui précède est déjà exécuté. Tiens compte du nouveau message dans la réponse en cours.";

/// Message qui tient la place d'un résumé (`SessionConversation::projected_entries`).
pub fn summary_message(node_id: &str, summary: &str) -> ChatMessage {
    ChatMessage::system(format!(
        "Résumé de la conversation antérieure (nœud {node_id}) :\n{summary}"
    ))
}

impl Surface {
    /// Tous les messages non coupés, par adresse, comme les lignes de `messages` :
    /// `compacted` vaut « masqué par un résumé ». Les résumés n'y sont pas (ce sont les
    /// lignes de `lcm_nodes`).
    pub fn entries(&self) -> Vec<Entry> {
        self.messages
            .iter()
            .map(|(seq, n)| Entry {
                seq: *seq,
                message: n.message.clone(),
                eager: n.eager,
                artifact_id: n.artifact_id.clone(),
                tokens: n.tokens,
                episode: n.episode,
                compacted: self.masked.contains(seq),
            })
            .collect()
    }

    /// Les nœuds visibles, dans l'ordre de la requête, prêts pour les niveaux 0, 2 et 4 :
    /// un résumé devient un message système (adresse 0, comme en V0), chaque message
    /// utilisateur reçoit en tête son contexte figé.
    pub fn projected_entries(&self) -> Vec<Entry> {
        self.nodes
            .iter()
            .filter_map(|slot| match *slot {
                Slot::Summary(k) => self
                    .summaries
                    .get(&k)
                    .map(|n| Entry::new(0, summary_message(&n.node_id, &n.summary), n.tokens_self)),
                Slot::Message(a) => self.messages.get(&a).map(|n| {
                    let mut message = n.message.clone();
                    if let Some(block) = self.contexts.get(&a)
                        && message.role == Role::User
                    {
                        with_context(&mut message, block);
                    }
                    Entry {
                        seq: a,
                        message,
                        eager: n.eager,
                        artifact_id: n.artifact_id.clone(),
                        tokens: n.tokens,
                        episode: n.episode,
                        compacted: false,
                    }
                }),
            })
            .collect()
    }

    /// La requête avant les niveaux 0, 2 et 4 : le système (le dernier `conv.system`,
    /// sinon `fallback_system`, le préfixe construit par les tuiles), la note de fusion,
    /// les nœuds visibles, et la consigne de relance d'une tentative restée sans réponse.
    pub fn request_messages(&self, fallback_system: &str) -> Vec<ChatMessage> {
        let system = self
            .system
            .as_ref()
            .map_or(fallback_system, |s| s.rendered.as_str());
        let mut out = vec![ChatMessage::system(system)];
        out.extend(self.projected_entries().into_iter().map(|e| e.message));
        if self.merge_note {
            // Après tous les messages système de tête, résumés compris, comme en V0.
            let at = out.iter().take_while(|m| m.role == Role::System).count();
            out.insert(at, ChatMessage::system(MERGE_NOTE));
        }
        if let Some(prompt) = self
            .attempts_tail
            .as_ref()
            .and_then(|a| a.retry_prompt.as_deref())
        {
            out.push(ChatMessage::user(prompt));
        }
        out
    }
}

/// Insère le bloc volatil devant le premier texte du message (`conversation.rs`).
fn with_context(m: &mut ChatMessage, block: &str) {
    match m.content.iter_mut().find_map(|c| match c {
        Content::Text { text } => Some(text),
        _ => None,
    }) {
        Some(text) => text.insert_str(0, block),
        None => m.content.insert(0, Content::text(block)),
    }
}
