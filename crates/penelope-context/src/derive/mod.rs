//! Le pliage pur du journal en surface (épopée #208, tâche T3).
//!
//! `derive(prefix, events)` rejoue les événements `conv.*` d'une session, dans l'ordre de
//! leur `seq`, et rend la [`Surface`] : le prompt système, les nœuds vus par le modèle
//! dans l'ordre, les contextes figés, la dernière tentative sans réponse. Aucune base,
//! aucune horloge : la même entrée donne toujours la même surface
//! (`design/v1/source-de-verite.md` §2.3).
//!
//! Adresse d'un nœud : `offset + events.seq`, où `offset` vient du `conv.fork` ou du
//! `conv.import` en tête du journal (0 sinon). Les nœuds hérités gardent leur adresse ;
//! les événements d'observation consomment des `seq` sans produire de nœud : la
//! numérotation croît, avec des trous.
//!
//! Un journal incohérent (remplacement d'un nœud absent, coupe dans une plage résumée,
//! héritage ailleurs qu'en tête) est une erreur, jamais une devinette. Seule exception :
//! quand une purge a effacé une partie des événements (ou le préfixe d'un fork), un
//! remplacement qui citait un nœud disparu masque simplement ce qui reste (§1.4, §2.6).

mod fold;
mod sealed;
mod surface;
#[cfg(test)]
mod tests;

pub use crate::journal::DeriveError;
pub(crate) use fold::{assistant_node, tool_node, user_node};
pub use sealed::{Sealed, SealedSummary};
pub use surface::{MERGE_NOTE, summary_message};

use crate::journal::AttemptPayload;
use penelope_kernel::event::Event;
use penelope_llm::types::ChatMessage;
use std::collections::{BTreeMap, BTreeSet};

/// Le prompt système en vigueur : le dernier `conv.system`.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemNode {
    pub seq: i64,
    pub hash: String,
    pub rendered: String,
}

/// Un message de la surface (utilisateur, assistant, résultat d'outil).
#[derive(Debug, Clone, PartialEq)]
pub struct MessageNode {
    pub message: ChatMessage,
    /// Résultat d'outil volatil, candidat au niveau 0.
    pub eager: bool,
    /// Corps externalisé (niveau 1).
    pub artifact_id: Option<String>,
    pub tokens: u64,
    pub episode: i64,
}

/// Un nœud de résumé et les adresses qu'il couvre.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryNode {
    pub node_id: String,
    pub summary: String,
    pub tokens_self: u64,
    pub from: i64,
    pub to: i64,
}

/// Une place de la surface, dans l'ordre vu par le modèle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Un message, par son adresse (clé de [`Surface::messages`]).
    Message(i64),
    /// Un résumé, par sa clé dans [`Surface::summaries`].
    Summary(i64),
}

/// Ce que le pliage rend.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Surface {
    pub system: Option<SystemNode>,
    /// Les nœuds visibles, dans l'ordre de la requête.
    pub nodes: Vec<Slot>,
    /// Tous les messages non coupés, visibles ou masqués par un résumé.
    pub messages: BTreeMap<i64, MessageNode>,
    /// Messages masqués par un résumé (`compacted` en V0).
    pub masked: BTreeSet<i64>,
    /// Résumés, y compris ceux qu'un résumé plus récent a prolongés.
    pub summaries: BTreeMap<i64, SummaryNode>,
    /// Contexte volatil figé, par adresse du message utilisateur.
    pub contexts: BTreeMap<i64, String>,
    /// Dernière tentative sans réponse du tour en cours.
    pub attempts_tail: Option<AttemptPayload>,
    /// Un message utilisateur est arrivé pendant le tour en cours.
    pub merge_note: bool,
    /// Plus grande adresse héritée (fork ou scellement).
    pub offset: i64,
    /// Une purge a effacé des événements de contenu (ici ou dans le préfixe hérité).
    pub purged: bool,
}

/// Plie le journal d'une session en surface.
pub fn derive(prefix: &Sealed, events: &[Event]) -> Result<Surface, DeriveError> {
    fold::Fold::new(prefix).run(events, None)
}

/// Comme [`derive`], en s'arrêtant au dernier événement d'adresse `<= until` : la
/// surface telle qu'elle était à ce point du journal (fork, audit d'un appel passé).
pub fn derive_until(prefix: &Sealed, events: &[Event], until: i64) -> Result<Surface, DeriveError> {
    fold::Fold::new(prefix).run(events, Some(until))
}

impl Slot {
    /// Première et dernière adresses couvertes par la place.
    fn span(self, s: &Surface) -> (i64, i64) {
        match self {
            Slot::Message(a) => (a, a),
            Slot::Summary(k) => s.summaries.get(&k).map_or((k, k), |n| (n.from, n.to)),
        }
    }
}

impl Surface {
    /// Position dans `nodes` de la place qui porte l'adresse `addr` : le message de cette
    /// adresse, ou le résumé de cette clé ou qui la couvre.
    pub fn position(&self, addr: i64) -> Option<usize> {
        self.nodes.iter().position(|slot| match *slot {
            Slot::Message(a) => a == addr,
            Slot::Summary(k) => {
                k == addr
                    || self
                        .summaries
                        .get(&k)
                        .is_some_and(|n| n.from <= addr && addr <= n.to)
            }
        })
    }

    /// Plus grande adresse présente dans la surface (messages, résumés, système).
    pub fn max_address(&self) -> i64 {
        let m = self.messages.keys().next_back().copied().unwrap_or(0);
        let s = self
            .summaries
            .iter()
            .map(|(k, n)| (*k).max(n.to))
            .max()
            .unwrap_or(0);
        let sys = self.system.as_ref().map_or(0, |n| n.seq);
        m.max(s).max(sys)
    }
}
