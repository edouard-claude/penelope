//! Le préfixe hérité d'une session : rien, la surface de sa mère (fork par référence,
//! README §10.3) ou l'historique V0 scellé (`source-de-verite.md` §4.5).

use super::{MessageNode, Slot, SummaryNode, Surface, derive_until};
use crate::journal::DeriveError;
use crate::transcript::Entry;
use penelope_kernel::event::Event;
use std::collections::BTreeMap;

/// D'où vient le préfixe ; le `conv.fork` ou le `conv.import` en tête du journal doit
/// annoncer la même chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Origin {
    None,
    Fork { parent: String, up_to: i64 },
    Import { messages: i64 },
}

/// Préfixe à plier avant les événements de la session.
#[derive(Debug, Clone)]
pub struct Sealed {
    pub(super) origin: Origin,
    pub(super) surface: Surface,
    /// Le préfixe a perdu des événements à la purge de la mère : ses remplacements
    /// sont relus avec indulgence (§2.6).
    pub(super) lost: bool,
}

/// Un nœud LCM actif au moment du scellement.
#[derive(Debug, Clone, PartialEq)]
pub struct SealedSummary {
    pub node_id: String,
    pub summary: String,
    pub tokens_self: u64,
    pub from: i64,
    pub to: i64,
}

impl Sealed {
    /// Session sans héritage.
    pub fn none() -> Self {
        Sealed {
            origin: Origin::None,
            surface: Surface::default(),
            lost: false,
        }
    }

    /// Préfixe d'une session fille : la dérivation de la mère jusqu'à l'adresse `up_to`.
    /// Récursif : `parent_prefix` est lui-même le préfixe de la mère.
    pub fn fork(
        parent: &str,
        parent_prefix: &Sealed,
        parent_events: &[Event],
        up_to: i64,
    ) -> Result<Self, DeriveError> {
        let surface = derive_until(parent_prefix, parent_events, up_to)?;
        let lost = surface.purged;
        Ok(Sealed {
            origin: Origin::Fork {
                parent: parent.to_string(),
                up_to,
            },
            surface,
            lost,
        })
    }

    /// Préfixe scellé d'une session V0 : ses lignes `messages` (drapeau `compacted`
    /// compris), ses contextes figés et ses nœuds LCM actifs.
    ///
    /// La surface reprend la projection V0 : les résumés actifs d'abord, puis les
    /// messages qu'ils ne couvrent pas. Un résumé est rangé sous l'adresse de son dernier
    /// message couvert.
    pub fn import(
        entries: Vec<Entry>,
        contexts: BTreeMap<i64, String>,
        active: Vec<SealedSummary>,
    ) -> Self {
        let mut surface = Surface::default();
        let mut active = active;
        active.sort_by_key(|n| (n.from, n.to));
        for n in active {
            surface.nodes.push(Slot::Summary(n.to));
            surface.summaries.insert(
                n.to,
                SummaryNode {
                    node_id: n.node_id,
                    summary: n.summary,
                    tokens_self: n.tokens_self,
                    from: n.from,
                    to: n.to,
                },
            );
        }
        let messages = entries.len() as i64;
        for e in entries {
            if e.compacted {
                surface.masked.insert(e.seq);
            } else {
                surface.nodes.push(Slot::Message(e.seq));
            }
            surface.messages.insert(
                e.seq,
                MessageNode {
                    message: e.message,
                    eager: e.eager,
                    artifact_id: e.artifact_id,
                    tokens: e.tokens,
                    episode: e.episode,
                },
            );
        }
        surface.contexts = contexts;
        Sealed {
            origin: Origin::Import { messages },
            surface,
            lost: false,
        }
    }

    /// La surface héritée, telle que les événements de la session la trouveront.
    pub fn surface(&self) -> &Surface {
        &self.surface
    }
}
