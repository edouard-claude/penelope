//! Le pas du pliage : un événement, une transition de la surface (§2.3).

use super::sealed::Origin;
use super::{MessageNode, Sealed, Slot, SummaryNode, Surface, SystemNode};
use crate::journal::*;
use penelope_kernel::event::Event;
use penelope_llm::types::{ChatMessage, Role};

pub(super) struct Fold<'a> {
    prefix: &'a Sealed,
    surface: Surface,
    /// Un événement `conv.*` a déjà été plié : l'héritage n'est plus permis.
    seen_conv: bool,
    /// Le préfixe fourni a été consommé par son `conv.fork` ou son `conv.import`.
    inherited: bool,
    last_seq: Option<i64>,
}

/// Erreur de surface sur l'événement `seq`.
fn refuse(seq: i64, kind: &str, reason: impl Into<String>) -> DeriveError {
    DeriveError::Surface {
        seq,
        kind: kind.to_string(),
        reason: reason.into(),
    }
}

impl<'a> Fold<'a> {
    pub(super) fn new(prefix: &'a Sealed) -> Self {
        Fold {
            prefix,
            surface: Surface::default(),
            seen_conv: false,
            inherited: false,
            last_seq: None,
        }
    }

    pub(super) fn run(
        mut self,
        events: &[Event],
        until: Option<i64>,
    ) -> Result<Surface, DeriveError> {
        for ev in events {
            if self.last_seq.is_some_and(|l| ev.seq <= l) {
                return Err(refuse(
                    ev.seq,
                    &ev.kind,
                    "événements hors de l'ordre des seq",
                ));
            }
            self.last_seq = Some(ev.seq);
            if until.is_some_and(|u| self.address(ev) > u) {
                break;
            }
            self.step(ev)?;
        }
        if !self.inherited && !matches!(self.prefix.origin, Origin::None) && !self.surface.purged {
            return Err(refuse(
                0,
                "",
                "préfixe hérité fourni, mais aucun conv.fork ni conv.import en tête du journal",
            ));
        }
        Ok(self.surface)
    }

    fn address(&self, ev: &Event) -> i64 {
        self.surface.offset + ev.seq
    }

    fn lenient(&self) -> bool {
        self.surface.purged
    }

    fn step(&mut self, ev: &Event) -> Result<(), DeriveError> {
        let Some(conv) = ConvEvent::decode(&ev.kind, &ev.payload)? else {
            match ev.kind.as_str() {
                KIND_TURN_STARTED => self.surface.merge_note = false,
                KIND_TURN_FINISHED => {
                    self.surface.merge_note = false;
                    self.surface.attempts_tail = None;
                }
                k if k.starts_with(KIND_PREFIX) && is_purged(&ev.payload) => {
                    self.surface.purged = true;
                }
                _ => {}
            }
            return Ok(());
        };
        let first = !self.seen_conv;
        self.seen_conv = true;
        match conv {
            ConvEvent::Fork(p) => {
                let origin = Origin::Fork {
                    parent: p.parent.clone(),
                    up_to: p.up_to,
                };
                self.inherit(ev, first, origin, p.offset)
            }
            ConvEvent::Import(p) => {
                let origin = Origin::Import {
                    messages: p.messages,
                };
                self.inherit(ev, first, origin, surface_offset(&p.surface))
            }
            ConvEvent::System(p) => self.system(ev, p),
            ConvEvent::User(p) => {
                let addr = self.address(ev);
                self.surface.merge_note |= p.mid_turn;
                self.push(
                    addr,
                    MessageNode {
                        message: ChatMessage {
                            content: p.content,
                            ..ChatMessage::user("")
                        },
                        eager: false,
                        artifact_id: None,
                        tokens: p.tokens_est,
                        episode: p.episode,
                    },
                );
                Ok(())
            }
            ConvEvent::Assistant(p) => {
                let addr = self.address(ev);
                let p = *p;
                self.surface.attempts_tail = None;
                let message = ChatMessage {
                    content: p.content,
                    tool_calls: p.tool_calls,
                    reasoning: p.reasoning,
                    reasoning_details: p.reasoning_details,
                    ..ChatMessage::assistant("")
                };
                self.push(
                    addr,
                    MessageNode {
                        message,
                        eager: false,
                        artifact_id: None,
                        tokens: p.tokens_est,
                        episode: p.episode,
                    },
                );
                Ok(())
            }
            ConvEvent::ToolResult(p) => self.tool_result(ev, p),
            ConvEvent::Context(p) => {
                // Le premier bloc figé fait foi : il a été envoyé tel quel (§3.1).
                self.surface.contexts.entry(p.target).or_insert(p.block);
                Ok(())
            }
            ConvEvent::Attempt(p) => {
                self.surface.attempts_tail = Some(p);
                Ok(())
            }
            ConvEvent::Summary(p) => self.summary(ev, p),
            ConvEvent::Rewind(p) => self.rewind(ev, &p.surface),
        }
    }

    fn push(&mut self, addr: i64, node: MessageNode) {
        self.surface.nodes.push(Slot::Message(addr));
        self.surface.messages.insert(addr, node);
    }

    /// `conv.fork` ou `conv.import` : seulement en tête, avec le préfixe annoncé.
    fn inherit(
        &mut self,
        ev: &Event,
        first: bool,
        origin: Origin,
        offset: i64,
    ) -> Result<(), DeriveError> {
        if !first {
            return Err(refuse(
                ev.seq,
                &ev.kind,
                "héritage ailleurs qu'en tête du journal",
            ));
        }
        if self.prefix.origin != origin {
            return Err(refuse(
                ev.seq,
                &ev.kind,
                format!(
                    "préfixe fourni {:?}, le journal annonce {:?}",
                    self.prefix.origin, origin
                ),
            ));
        }
        let mut surface = self.prefix.surface.clone();
        if surface.max_address() > offset {
            return Err(refuse(
                ev.seq,
                &ev.kind,
                format!(
                    "offset {offset} inférieur à l'adresse héritée {}",
                    surface.max_address()
                ),
            ));
        }
        surface.offset = offset;
        surface.purged |= self.prefix.lost;
        surface.attempts_tail = None;
        surface.merge_note = false;
        self.surface = surface;
        self.inherited = true;
        Ok(())
    }

    fn system(&mut self, ev: &Event, p: SystemPayload) -> Result<(), DeriveError> {
        let current = self.surface.system.as_ref().map(|s| s.seq);
        let fits = match (&p.surface, current) {
            (SurfaceOp::Append, None) => true,
            (SurfaceOp::Replace { from, to }, Some(s)) => *from == s && *to == s,
            _ => false,
        };
        if !fits && !self.lenient() {
            return Err(refuse(
                ev.seq,
                &ev.kind,
                format!(
                    "{:?} ne couvre pas le système en vigueur {current:?}",
                    p.surface
                ),
            ));
        }
        self.surface.system = Some(SystemNode {
            seq: self.address(ev),
            hash: p.hash,
            rendered: p.rendered,
        });
        Ok(())
    }

    /// Niveau 1 : le corps d'un résultat change, son adresse et sa place restent.
    fn tool_result(&mut self, ev: &Event, p: ToolResultPayload) -> Result<(), DeriveError> {
        let target = match p.surface {
            SurfaceOp::Replace { from, .. } => from,
            _ => {
                let addr = self.address(ev);
                let message = ChatMessage {
                    content: p.content,
                    ..ChatMessage::tool_result(p.call_id, p.tool, "")
                };
                self.push(
                    addr,
                    MessageNode {
                        message,
                        eager: p.eager,
                        artifact_id: p.artifact_id,
                        tokens: p.tokens_est,
                        episode: p.episode,
                    },
                );
                return Ok(());
            }
        };
        let visible = self.surface.nodes.contains(&Slot::Message(target));
        let node = self.surface.messages.get_mut(&target).filter(|_| visible);
        let Some(node) = node else {
            if self.lenient() {
                return Ok(());
            }
            return Err(refuse(ev.seq, &ev.kind, format!("nœud {target} absent")));
        };
        let same_call = node.message.role == Role::Tool
            && node.message.tool_call_id.as_deref() == Some(p.call_id.as_str());
        if !same_call {
            return Err(refuse(
                ev.seq,
                &ev.kind,
                format!(
                    "le nœud {target} n'est pas le résultat de l'appel {}",
                    p.call_id
                ),
            ));
        }
        node.message.content = p.content;
        node.artifact_id = p.artifact_id;
        node.tokens = p.tokens_est;
        Ok(())
    }

    fn summary(&mut self, ev: &Event, p: SummaryPayload) -> Result<(), DeriveError> {
        let SurfaceOp::Replace { from, to } = p.surface else {
            unreachable!("vérifié par ConvEvent::decode");
        };
        let s = &self.surface;
        let range = match (s.position(from), s.position(to)) {
            (Some(i), Some(j)) if i <= j => Some((i, j)),
            _ if self.lenient() => {
                // Ce qui reste de la plage, par adresses.
                let hit: Vec<usize> = (0..s.nodes.len())
                    .filter(|&i| {
                        let (a, b) = s.nodes[i].span(s);
                        a <= to && b >= from
                    })
                    .collect();
                hit.first().zip(hit.last()).map(|(i, j)| (*i, *j))
            }
            (i, j) => {
                return Err(refuse(
                    ev.seq,
                    &ev.kind,
                    format!("remplacement {from}..{to} invalide (positions {i:?}, {j:?})"),
                ));
            }
        };
        let key = self.address(ev);
        let (at, cover) = match range {
            Some((i, j)) => {
                let cover = (s.nodes[i].span(s).0, s.nodes[j].span(s).1);
                for slot in self.surface.nodes.drain(i..=j) {
                    if let Slot::Message(a) = slot {
                        self.surface.masked.insert(a);
                    }
                }
                (i, cover)
            }
            // Plage entièrement purgée : le résumé prend sa place par adresse.
            None => {
                let at = (0..s.nodes.len())
                    .find(|&i| s.nodes[i].span(s).0 > to)
                    .unwrap_or(s.nodes.len());
                (at, (from, to))
            }
        };
        self.surface.nodes.insert(at, Slot::Summary(key));
        self.surface.summaries.insert(
            key,
            SummaryNode {
                node_id: p.node_id,
                summary: p.summary,
                tokens_self: p.tokens_self,
                from: cover.0,
                to: cover.1,
            },
        );
        Ok(())
    }

    /// Retour arrière : les nœuds après `after` quittent la surface. Jamais à travers un
    /// résumé (`session_ops.rs`, « pas de retour avant le dernier résumé »).
    fn rewind(&mut self, ev: &Event, op: &SurfaceOp) -> Result<(), DeriveError> {
        let SurfaceOp::Cut { after } = *op else {
            unreachable!("vérifié par ConvEvent::decode");
        };
        let s = &self.surface;
        let keep = match s.position(after) {
            Some(i) => i + 1,
            None if after == 0 => 0,
            None if self.lenient() => (0..s.nodes.len())
                .find(|&i| s.nodes[i].span(s).0 > after)
                .unwrap_or(s.nodes.len()),
            None => return Err(refuse(ev.seq, &ev.kind, format!("nœud {after} absent"))),
        };
        if s.nodes[keep..]
            .iter()
            .any(|slot| matches!(slot, Slot::Summary(_)))
        {
            return Err(refuse(
                ev.seq,
                &ev.kind,
                format!("coupe après {after} à travers un résumé"),
            ));
        }
        for slot in self.surface.nodes.split_off(keep) {
            if let Slot::Message(a) = slot {
                self.surface.messages.remove(&a);
                self.surface.contexts.remove(&a);
            }
        }
        self.surface.attempts_tail = None;
        Ok(())
    }
}

fn surface_offset(op: &SurfaceOp) -> i64 {
    match op {
        SurfaceOp::Seal { offset, .. } | SurfaceOp::Inherit { offset, .. } => *offset,
        _ => 0,
    }
}
