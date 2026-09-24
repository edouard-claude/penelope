//! Rejeu du journal d'une session vers ce que ses caches doivent contenir (épopée #208,
//! T12 et T13 ; `design/v1/source-de-verite.md` §2.4).
//!
//! `penelope history verify` compare ce rejeu aux tables, `reindex` le réécrit : les deux
//! partent de la même dérivation, le journal de la session plié par [`crate::derive`]
//! après son préfixe hérité (la mère d'un fork, par référence et récursivement ; ou
//! l'historique V0 scellé).
//!
//! **Numéros de ligne.** Une ligne de `messages` ne porte pas l'adresse de son nœud : la V0
//! numérote `MAX(seq) + 1` à l'écriture, une coupe libère ses numéros, une ligne scellée
//! garde le sien, une copie de fork garde celui de la mère. Rangés par adresse, les
//! messages non coupés reçoivent donc leur adresse s'ils sont scellés, sinon le numéro qui
//! suit le précédent : la numérotation que la V0 aurait donnée. La comparaison apparie par
//! `event_id` (une ligne scellée par son numéro) et ne vérifie que l'ordre.
//!
//! **Ce que la V0 ne recopie pas.** Une fille de fork ne reçoit pas les contextes figés de
//! sa mère (`copy_messages`), l'archive d'un retour arrière non plus. Le pliage ne les
//! hérite pas non plus (`Sealed::fork`) ; l'archive n'en attend pas.

use crate::derive::{DeriveError, Sealed, Slot, Surface, derive, derive_until};
use crate::journal::{ConvEvent, ImportPayload, KIND_PREFIX, KIND_REWIND, SurfaceOp, is_purged};
use crate::store::seal::{LegacyPrefix, sealed_prefix_in};
use penelope_kernel::event::Event;
use penelope_llm::types::ChatMessage;
use penelope_store::rusqlite::{self, Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::BTreeMap;

/// Une chaîne de forks est finie ; la borne évite de boucler sur un journal corrompu.
const MAX_DEPTH: usize = 64;

/// Pourquoi un rejeu n'aboutit pas.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Store(#[from] penelope_store::StoreError),
    /// Le journal ne se plie pas : c'est une divergence, pas une panne.
    #[error("{0}")]
    Journal(String),
}

impl From<rusqlite::Error> for ReplayError {
    fn from(e: rusqlite::Error) -> Self {
        ReplayError::Store(e.into())
    }
}

impl From<DeriveError> for ReplayError {
    fn from(e: DeriveError) -> Self {
        ReplayError::Journal(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ReplayError>;

/// D'où vient le préfixe d'une session.
pub(crate) enum Origin {
    None,
    Fork,
    Import(Box<(ImportPayload, LegacyPrefix)>),
}

/// L'événement qui a produit une adresse (message ou résumé).
#[derive(Debug, Clone)]
pub(crate) struct Owner {
    pub event_id: i64,
    pub ts: String,
    /// L'événement est dans le journal de la session elle-même.
    pub own: bool,
    pub turn_message_id: Option<String>,
    pub arrived_at: Option<String>,
    /// Pour un `conv.summary` : ce que le nœud LCM porte et que la surface n'a pas.
    pub summary: Option<SummaryMeta>,
}

#[derive(Debug, Clone)]
pub(crate) struct SummaryMeta {
    pub node_id: String,
    pub previous: Option<String>,
    pub anchors: String,
    pub tokens_src: u64,
}

/// Le journal d'une session et ce qu'elle hérite.
pub(crate) struct Lineage {
    pub events: Vec<Event>,
    pub prefix: Sealed,
    pub origin: Origin,
    /// Plus grande adresse héritée (§2.3) ; 0 sans héritage.
    pub offset: i64,
    /// Événement de chaque adresse de la chaîne ; une adresse absente est scellée.
    pub owners: BTreeMap<i64, Owner>,
}

/// Une ligne de `messages` attendue.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub address: i64,
    pub seq: i64,
    pub event_id: Option<i64>,
    pub sealed: bool,
    pub message: ChatMessage,
    pub eager: bool,
    pub artifact_id: Option<String>,
    pub tokens: u64,
    pub episode: i64,
    pub compacted: bool,
    /// Heure de la ligne : l'arrivée d'un message de la file, sinon l'événement.
    pub ts: String,
    pub source_turn_id: Option<String>,
}

/// Un nœud LCM attendu.
#[derive(Debug, Clone)]
pub(crate) struct NodeRow {
    /// `None` pour la copie d'un nœud hérité : la V0 lui donne un identifiant neuf.
    pub node_id: Option<String>,
    pub event_id: Option<i64>,
    /// Nœud nommé par le `conv.import` : la refonte ne le réécrit pas.
    pub sealed: bool,
    pub from_seq: i64,
    pub to_seq: i64,
    pub summary: String,
    pub anchors: String,
    pub tokens_src: u64,
    pub tokens_self: u64,
    pub ts: String,
    /// Indice, dans la même liste, du nœud qui l'a prolongé.
    pub superseded_by: Option<usize>,
}

/// Ce que les caches d'une session doivent contenir.
#[derive(Debug, Clone, Default)]
pub(crate) struct Expected {
    pub rows: Vec<Row>,
    /// Contextes figés, par numéro de ligne.
    pub contexts: BTreeMap<i64, String>,
    pub nodes: Vec<NodeRow>,
    /// Numéro de ligne de chaque adresse de message attendue.
    pub seqs: BTreeMap<i64, i64>,
}

/// Les événements d'une session que le pliage lit (contenu et bornes de tour).
pub(crate) fn session_events(c: &Connection, sid: &str) -> rusqlite::Result<Vec<Event>> {
    session_events_after(c, sid, i64::MIN)
}

/// Comme [`session_events`], après le `seq` donné : ce qu'une lecture n'a pas encore plié.
pub(crate) fn session_events_after(
    c: &Connection,
    sid: &str,
    after: i64,
) -> rusqlite::Result<Vec<Event>> {
    let mut st = c.prepare_cached(
        "SELECT id, session_id, run_id, seq, ts, kind, payload, hash, prev_hash
         FROM events
         WHERE session_id = ?1 AND seq > ?2
           AND (kind LIKE 'conv.%' OR kind IN ('turn.started', 'turn.finished'))
         ORDER BY seq",
    )?;
    let rows = st.query_map(params![sid, after], |r| {
        let payload: String = r.get(6)?;
        Ok(Event {
            id: r.get(0)?,
            session_id: r.get(1)?,
            run_id: r.get(2)?,
            seq: r.get(3)?,
            ts: r.get(4)?,
            kind: r.get(5)?,
            payload: serde_json::from_str(&payload)
                .unwrap_or_else(|e| serde_json::json!({"payload_illisible": e.to_string()})),
            hash: r.get(7)?,
            prev_hash: r.get(8)?,
        })
    })?;
    rows.collect()
}

impl Lineage {
    /// Relit le journal d'une session et, récursivement, ce qu'elle hérite.
    pub fn load(c: &Connection, sid: &str) -> Result<Lineage> {
        Self::load_at(c, sid, 0)
    }

    fn load_at(c: &Connection, sid: &str, depth: usize) -> Result<Lineage> {
        if depth > MAX_DEPTH {
            return Err(ReplayError::Journal(format!(
                "chaîne de forks de plus de {MAX_DEPTH} sessions sous {sid}"
            )));
        }
        let events = session_events(c, sid)?;
        let head = events
            .iter()
            .find(|e| e.kind.starts_with(KIND_PREFIX))
            .filter(|e| !is_purged(&e.payload));
        let mut owners = BTreeMap::new();
        let (prefix, origin, offset) = match head.map(|e| ConvEvent::decode(&e.kind, &e.payload)) {
            Some(Ok(Some(ConvEvent::Fork(p)))) => {
                let parent = Self::load_at(c, &p.parent, depth + 1)?;
                let prefix = Sealed::fork(&p.parent, &parent.prefix, &parent.events, p.up_to)?;
                for (addr, o) in parent.owners.range(..=p.up_to) {
                    owners.insert(
                        *addr,
                        Owner {
                            own: false,
                            ..o.clone()
                        },
                    );
                }
                (prefix, Origin::Fork, p.offset)
            }
            Some(Ok(Some(ConvEvent::Import(_)))) => match sealed_prefix_in(c, sid)? {
                Some((import, legacy)) => {
                    let offset = match import.surface {
                        SurfaceOp::Seal { offset, .. } => offset,
                        _ => legacy.offset(),
                    };
                    let prefix = legacy.sealed();
                    (prefix, Origin::Import(Box::new((import, legacy))), offset)
                }
                None => (Sealed::none(), Origin::None, 0),
            },
            _ => (Sealed::none(), Origin::None, 0),
        };
        for e in &events {
            if let Some(owner) = own_owner(e) {
                owners.insert(offset + e.seq, owner);
            }
        }
        Ok(Lineage {
            events,
            prefix,
            origin,
            offset,
            owners,
        })
    }

    /// La surface de la session.
    pub fn surface(&self) -> Result<Surface> {
        Ok(derive(&self.prefix, &self.events)?)
    }

    /// Le journal a-t-il un `conv.*` à lui ?
    pub fn journaled(&self) -> bool {
        self.events.iter().any(|e| e.kind.starts_with(KIND_PREFIX))
    }

    /// La dernière adresse scellée coupée par un retour arrière, s'il y en a une : la
    /// coupe retire des lignes que l'empreinte du scellement couvre.
    pub fn cuts_sealed_prefix(&self) -> bool {
        let Origin::Import(_) = self.origin else {
            return false;
        };
        self.events.iter().any(|e| {
            e.kind == KIND_REWIND
                && matches!(
                    ConvEvent::decode(&e.kind, &e.payload),
                    Ok(Some(ConvEvent::Rewind(p)))
                        if matches!(p.surface, SurfaceOp::Cut { after } if after < self.offset)
                )
        })
    }

    /// Numéro de ligne de chaque adresse de message de la surface : l'adresse d'une
    /// ligne scellée, sinon le numéro qui suit le précédent (voir l'en-tête).
    pub fn row_seqs(&self, surface: &Surface) -> BTreeMap<i64, i64> {
        row_seqs(&self.owners, surface)
    }

    /// Ce que les caches de la session doivent contenir.
    pub fn expected(&self, c: &Connection, surface: &Surface) -> Result<Expected> {
        let mut out = Expected {
            seqs: self.row_seqs(surface),
            ..Expected::default()
        };
        for (addr, node) in &surface.messages {
            let owner = self.owners.get(addr);
            let seq = out.seqs.get(addr).copied().unwrap_or(*addr);
            let own = owner.is_some_and(|o| o.own);
            out.rows.push(Row {
                address: *addr,
                seq,
                event_id: owner.map(|o| o.event_id),
                sealed: owner.is_none(),
                message: node.message.clone(),
                eager: node.eager,
                artifact_id: node.artifact_id.clone(),
                tokens: node.tokens,
                episode: node.episode,
                compacted: surface.masked.contains(addr),
                ts: owner
                    .and_then(|o| o.arrived_at.clone())
                    .or_else(|| owner.map(|o| o.ts.clone()))
                    .unwrap_or_default(),
                source_turn_id: owner
                    .filter(|_| own)
                    .and_then(|o| o.turn_message_id.clone()),
            });
        }
        for (addr, block) in &surface.contexts {
            if let Some(seq) = out.seqs.get(addr) {
                out.contexts.insert(*seq, block.clone());
            }
        }
        out.nodes = self.nodes(c, surface, &out.seqs)?;
        Ok(out)
    }

    /// Les nœuds LCM : scellés (ceux que l'import nomme), copies des résumés hérités
    /// actifs au fork, et un par `conv.summary` de la session, prolongations chaînées.
    fn nodes(
        &self,
        c: &Connection,
        surface: &Surface,
        seqs: &BTreeMap<i64, i64>,
    ) -> Result<Vec<NodeRow>> {
        let seq_of = |a: i64| seqs.get(&a).copied().unwrap_or(a);
        let sealed_ids: BTreeMap<i64, String> = match &self.origin {
            Origin::Import(b) => {
                b.1.active
                    .iter()
                    .map(|n| (n.to, n.node_id.clone()))
                    .collect()
            }
            _ => BTreeMap::new(),
        };
        let inherited: Vec<i64> = match self.origin {
            Origin::Fork => self
                .prefix
                .surface()
                .nodes
                .iter()
                .filter_map(|s| match s {
                    Slot::Summary(k) => Some(*k),
                    Slot::Message(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut out: Vec<NodeRow> = Vec::new();
        for (key, n) in &surface.summaries {
            let owner = self.owners.get(key);
            let meta = owner.and_then(|o| o.summary.clone());
            let own = owner.is_some_and(|o| o.own) && *key > self.offset;
            let sealed = sealed_ids.get(key).filter(|_| *key <= self.offset);
            let copied = !own && sealed.is_none();
            if copied && !inherited.contains(key) {
                continue;
            }
            let source_id = match (&meta, sealed) {
                (Some(m), _) => Some(m.node_id.clone()),
                (None, Some(id)) => Some(id.clone()),
                (None, None) => None,
            };
            let (mut anchors, mut tokens_src) = meta.as_ref().map_or_else(
                || ("[]".to_string(), 0),
                |m| (m.anchors.clone(), m.tokens_src),
            );
            if copied && let Some(id) = &source_id {
                let stored = c
                    .query_row(
                        "SELECT anchors, tokens_src FROM lcm_nodes WHERE id = ?1",
                        [id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                    )
                    .optional()?;
                if let Some((a, s)) = stored {
                    (anchors, tokens_src) = (a, s as u64);
                }
            }
            let from_seq = seq_of(n.from);
            let at = out.len();
            if own && meta.as_ref().is_some_and(|m| m.previous.is_some()) {
                // La prolongation remplace le dernier nœud vivant qui part du même point.
                if let Some(prev) = out
                    .iter_mut()
                    .rev()
                    .find(|p| p.superseded_by.is_none() && p.from_seq == from_seq)
                {
                    prev.superseded_by = Some(at);
                    tokens_src += prev.tokens_src;
                }
            }
            out.push(NodeRow {
                node_id: if copied { None } else { source_id },
                event_id: owner.filter(|_| own).map(|o| o.event_id),
                sealed: sealed.is_some(),
                from_seq,
                to_seq: seq_of(n.to),
                summary: n.summary.clone(),
                anchors,
                tokens_src,
                tokens_self: n.tokens_self,
                ts: owner.map(|o| o.ts.clone()).unwrap_or_default(),
                superseded_by: None,
            });
        }
        Ok(out)
    }
}

/// Numéro de ligne de chaque adresse de message de la surface, `owners` étant
/// l'événement de chaque adresse de la chaîne (voir l'en-tête).
pub(crate) fn row_seqs(owners: &BTreeMap<i64, Owner>, surface: &Surface) -> BTreeMap<i64, i64> {
    let mut out = BTreeMap::new();
    let mut last = 0;
    for addr in surface.messages.keys() {
        let seq = match owners.get(addr) {
            None => *addr,
            Some(_) => last + 1,
        };
        last = last.max(seq);
        out.insert(*addr, seq);
    }
    out
}

/// L'adresse que produit un événement de la session, et ce qui l'accompagne.
pub(crate) fn own_owner(e: &Event) -> Option<Owner> {
    let conv = ConvEvent::decode(&e.kind, &e.payload).ok()??;
    let text = |k: &str| e.payload.get(k).and_then(Value::as_str).map(String::from);
    let summary = match &conv {
        ConvEvent::User(_) | ConvEvent::Assistant(_) => None,
        ConvEvent::ToolResult(p) if p.surface == SurfaceOp::Append => None,
        ConvEvent::Summary(p) => Some(SummaryMeta {
            node_id: p.node_id.clone(),
            previous: p.previous_node_id.clone(),
            anchors: serde_json::to_string(&p.anchors).unwrap_or_else(|_| "[]".into()),
            tokens_src: p.tokens_src,
        }),
        _ => return None,
    };
    Some(Owner {
        event_id: e.id,
        ts: e.ts.clone(),
        own: true,
        turn_message_id: text("turn_message_id"),
        arrived_at: text("arrived_at"),
        summary,
    })
}

/// L'archive d'un retour arrière : une session sans journal, dont les lignes sont celles
/// qu'un `conv.rewind` de sa mère a coupées (`session_ops::rewind`, T10). Rend la mère,
/// l'adresse du `conv.rewind` et la coupe.
pub(crate) fn archive_of(
    c: &Connection,
    sid: &str,
) -> rusqlite::Result<Option<(String, i64, i64)>> {
    c.query_row(
        "SELECT session_id, seq, json_extract(payload, '$.surface.after') FROM events
         WHERE kind = ?1 AND json_extract(payload, '$.archive_session') = ?2
         ORDER BY id LIMIT 1",
        params![KIND_REWIND, sid],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )
    .optional()
}

/// Ce que l'archive d'un retour arrière doit contenir : les messages de sa mère, juste
/// avant la coupe, qui suivent le nœud `after` ; mêmes numéros que dans la mère. Ni
/// contexte figé ni résumé : la V0 ne les recopie pas.
pub(crate) fn archive_expected(
    c: &Connection,
    mother: &str,
    rewind_seq: i64,
    after: i64,
) -> Result<Expected> {
    let lineage = Lineage::load(c, mother)?;
    let until = lineage.offset + rewind_seq - 1;
    let surface = derive_until(&lineage.prefix, &lineage.events, until)?;
    let full = lineage.expected(c, &surface)?;
    let mut out = Expected::default();
    for mut row in full.rows.into_iter().filter(|r| r.address > after) {
        row.compacted = false;
        row.source_turn_id = None;
        out.seqs.insert(row.address, row.seq);
        out.rows.push(row);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod fixture;
