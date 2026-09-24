//! Le projecteur : les caches de la conversation maintenus depuis le journal (épopée #208,
//! T13 ; `design/v1/source-de-verite.md` §2.4).
//!
//! Tant que les tables restent la source de lecture (jusqu'à T14), le chemin direct de la
//! V0 écrit les lignes dans la seconde transaction de chaque événement
//! (`EventLog::append_with`). Le projecteur ne double pas ce chemin : il **rattrape** ce
//! qu'une seconde transaction n'a pas écrit (arrêt du processus, écriture en erreur) et
//! **refond** les caches d'une session.
//!
//! - [`apply_in`] : un événement `conv.*`, une écriture idempotente ; ce qui est déjà en
//!   base n'est pas réécrit. Le chemin direct l'est aussi (`dual.rs`, `publish.rs`) : l'un
//!   ou l'autre peut passer le premier.
//! - Filigrane : `projections_session` (`last_event`, et `state` = `fold_version`,
//!   `offset`, `dirty`). [`HistoryStore::catch_up`], appelé à l'ouverture d'une session
//!   (`runner.rs`), rejoue les événements postérieurs ; une erreur ne fait pas échouer le
//!   tour, elle pose `dirty` que `doctor` signale.
//! - [`FOLD_VERSION`] : quand la sémantique du pliage change, le numéro monte et chaque
//!   session est refondue à son prochain rattrapage, jamais réutilisée telle quelle.
//! - [`HistoryStore::reindex`] : efface les lignes de cache non scellées et les réécrit
//!   depuis le rejeu ([`crate::replay`]), dans une transaction par session. Idempotent.
//! - `prompt_snapshots` : chaque `conv.system` y laisse son texte sous son empreinte
//!   ([`ensure_snapshot`]) ; ni la refonte ni la purge d'une session ne l'effacent ici.

use crate::derive::MessageNode;
use crate::derive::{assistant_node, tool_node, user_node};
use crate::journal::{ConvEvent, SurfaceOp, SystemPayload};
use crate::lcm::{NodeWrite, insert_leaf_in, replace_in};
use crate::replay::{
    Expected, Lineage, Origin, ReplayError, archive_expected, archive_of, session_events,
};
use crate::store::{HistoryStore, mark_compacted_in, origin_in, serialise_content};
use penelope_kernel::event::Event;
use penelope_kernel::ids::NodeId;
use penelope_store::rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Version de la sémantique du pliage des caches. La monter refond toutes les sessions.
pub const FOLD_VERSION: u64 = 1;

/// Ce qu'une refonte a écrit.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Reindexed {
    pub session: String,
    pub rows: usize,
    pub contexts: usize,
    pub nodes: usize,
}

/// Le rapport de `history reindex`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReindexReport {
    pub ok: bool,
    pub sessions: Vec<Reindexed>,
    /// Sessions laissées telles quelles, avec la raison (journal incohérent, lignes V0
    /// sans journal).
    pub refused: Vec<(String, String)>,
}

/// Ce qu'un rattrapage a fait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUp {
    /// Aucun journal attaché, ou rien après le filigrane.
    Current,
    /// Des événements rejoués un à un.
    Applied(usize),
    /// Session refondue (nouvelle `FOLD_VERSION`, ou fork à recopier).
    Rebuilt,
}

/// Issue d'un [`apply_in`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Applied {
    Done,
    /// L'événement demande de refondre la session (fork dont la copie manque, bornes
    /// introuvables).
    Rebuild,
}

/// Le filigrane d'une session : dernier événement projeté et version du pliage.
fn watermark(c: &Connection, sid: &str) -> penelope_store::Result<Option<(i64, Value)>> {
    Ok(c.query_row(
        "SELECT last_event, state FROM projections_session WHERE session_id = ?1",
        [sid],
        |r| {
            let state: String = r.get(1)?;
            Ok((
                r.get(0)?,
                serde_json::from_str(&state).unwrap_or(Value::Null),
            ))
        },
    )
    .optional()?)
}

fn set_watermark(
    tx: &Transaction<'_>,
    sid: &str,
    last_event: i64,
    state: Value,
    ts: &str,
) -> penelope_store::Result<()> {
    tx.execute(
        "INSERT INTO projections_session(session_id, state, updated_at, last_event)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(session_id) DO UPDATE SET
           state = excluded.state, updated_at = excluded.updated_at,
           last_event = excluded.last_event",
        params![sid, state.to_string(), ts, last_event],
    )?;
    Ok(())
}

/// Adresse (§2.3) de chaque ligne de la session, et son numéro.
fn addresses(c: &Connection, sid: &str) -> penelope_store::Result<BTreeMap<i64, i64>> {
    let mut st = c.prepare(
        "SELECT m.seq, e.session_id, e.seq FROM messages m
         LEFT JOIN events e ON e.id = m.event_id WHERE m.session_id = ?1",
    )?;
    let rows: Vec<(i64, Option<String>, Option<i64>)> = st
        .query_map([sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut offsets: BTreeMap<String, i64> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for (seq, owner, event_seq) in rows {
        let address = match (owner, event_seq) {
            (Some(owner), Some(es)) => {
                let offset = match offsets.get(&owner) {
                    Some(o) => *o,
                    None => {
                        let o = origin_in(c, &owner)?.offset;
                        offsets.insert(owner, o);
                        o
                    }
                };
                offset + es
            }
            _ => seq,
        };
        out.insert(address, seq);
    }
    Ok(out)
}

/// Le texte que le plein texte indexe pour une ligne : celui du message, sauf pour un
/// corps externalisé (niveau 1), dont l'index garde le texte d'origine, celui de
/// l'événement d'ajout (`externalise` ne touche pas `messages_fts`).
fn searchable(
    tx: &Transaction<'_>,
    node: &MessageNode,
    event_id: Option<i64>,
) -> penelope_store::Result<String> {
    let original = match (&node.artifact_id, event_id) {
        (Some(_), Some(id)) => tx
            .query_row(
                "SELECT kind, payload FROM events WHERE id = ?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .and_then(|(kind, payload)| {
                let value: Value = serde_json::from_str(&payload).ok()?;
                match ConvEvent::decode(&kind, &value).ok()?? {
                    ConvEvent::ToolResult(p) => Some(tool_node(p).message.text()),
                    _ => None,
                }
            }),
        _ => None,
    };
    Ok(original.unwrap_or_else(|| node.message.text()))
}

/// Écrit une ligne `messages` et son entrée plein texte.
#[allow(clippy::too_many_arguments)] // une colonne par argument, comme `insert_row`
fn insert_message(
    tx: &Transaction<'_>,
    sid: &str,
    seq: i64,
    node: &MessageNode,
    ts: &str,
    compacted: bool,
    source_turn_id: Option<&str>,
    event_id: Option<i64>,
) -> penelope_store::Result<()> {
    let taken = match source_turn_id {
        Some(key) => tx
            .query_row(
                "SELECT 1 FROM messages WHERE source_turn_id = ?1",
                [key],
                |_| Ok(()),
            )
            .optional()?
            .is_some(),
        None => false,
    };
    let m = &node.message;
    tx.execute(
        "INSERT INTO messages(session_id, seq, role, content, tool_call_id, tool_name,
            tokens_est, ts, episode, eager, artifact_id, compacted, source_turn_id, event_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            sid,
            seq,
            m.role.as_str(),
            serialise_content(m)?,
            m.tool_call_id,
            m.name,
            node.tokens as i64,
            ts,
            node.episode,
            node.eager as i64,
            node.artifact_id,
            compacted as i64,
            source_turn_id.filter(|_| !taken),
            event_id
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO messages_fts(content, session_id, msg_id) VALUES(?1, ?2, ?3)",
        params![searchable(tx, node, event_id)?, sid, id],
    )?;
    Ok(())
}

/// Un message d'ajout : sa ligne, si aucune ne cite déjà l'événement.
fn ensure_row(
    tx: &Transaction<'_>,
    sid: &str,
    ev: &Event,
    node: MessageNode,
    source_turn_id: Option<&str>,
    ts: Option<&str>,
) -> penelope_store::Result<Applied> {
    let present = tx
        .query_row(
            "SELECT 1 FROM messages WHERE session_id = ?1 AND event_id = ?2",
            params![sid, ev.id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !present {
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
            [sid],
            |r| r.get(0),
        )?;
        let ts = ts.unwrap_or(ev.ts.as_str());
        insert_message(tx, sid, seq, &node, ts, false, source_turn_id, Some(ev.id))?;
    }
    Ok(Applied::Done)
}

/// Applique un événement de la session `sid` aux caches, sans rien réécrire de ce qui y
/// est déjà. `System`, `Attempt` et `Import` n'ont rien à y écrire : le préfixe est lu dans
/// le journal, la tentative est hors surface, le scellement a marqué ses lignes dans sa
/// propre transaction.
pub(crate) fn apply_in(
    tx: &Transaction<'_>,
    sid: &str,
    ev: &Event,
) -> Result<Applied, ReplayError> {
    let Some(conv) = ConvEvent::decode(&ev.kind, &ev.payload)? else {
        return Ok(Applied::Done);
    };
    let seq_at = |address: i64| -> penelope_store::Result<Option<i64>> {
        Ok(addresses(tx, sid)?.get(&address).copied())
    };
    Ok(match conv {
        ConvEvent::User(p) => {
            let (key, at) = (p.turn_message_id.clone(), p.arrived_at.clone());
            ensure_row(tx, sid, ev, user_node(p), key.as_deref(), at.as_deref())?
        }
        ConvEvent::Assistant(p) => ensure_row(tx, sid, ev, assistant_node(*p), None, None)?,
        ConvEvent::ToolResult(p) => match p.surface {
            SurfaceOp::Replace { from, .. } => match seq_at(from)? {
                Some(seq) => {
                    let node = tool_node(p);
                    tx.execute(
                        "UPDATE messages SET content = ?3, artifact_id = ?4, tokens_est = ?5
                         WHERE session_id = ?1 AND seq = ?2
                           AND (artifact_id IS NOT ?4 OR tokens_est != ?5)",
                        params![
                            sid,
                            seq,
                            serialise_content(&node.message)?,
                            node.artifact_id,
                            node.tokens as i64
                        ],
                    )?;
                    Applied::Done
                }
                None => Applied::Rebuild,
            },
            _ => ensure_row(tx, sid, ev, tool_node(p), None, None)?,
        },
        ConvEvent::Context(p) => {
            if let Some(seq) = seq_at(p.target)? {
                tx.execute(
                    "INSERT OR IGNORE INTO message_context(session_id, seq, context)
                     VALUES(?1, ?2, ?3)",
                    params![sid, seq, p.block],
                )?;
            }
            Applied::Done
        }
        ConvEvent::Summary(p) => {
            let SurfaceOp::Replace { from, to } = p.surface else {
                return Ok(Applied::Rebuild);
            };
            let (Some(from_seq), Some(to_seq)) = (seq_at(from)?, seq_at(to)?) else {
                return Ok(Applied::Rebuild);
            };
            let exists = tx
                .query_row(
                    "SELECT 1 FROM lcm_nodes WHERE id = ?1",
                    [&p.node_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                let node = NodeWrite {
                    id: p.node_id.clone(),
                    session_id: sid.to_string(),
                    summary: p.summary.clone(),
                    anchors: serde_json::to_string(&p.anchors).unwrap_or_else(|_| "[]".into()),
                    tokens_src: p.tokens_src,
                    tokens_self: p.tokens_self,
                    ts: ev.ts.clone(),
                    event_id: Some(ev.id),
                };
                match &p.previous_node_id {
                    Some(prev) => replace_in(tx, prev, Some((to_seq, p.tokens_src)), &node)?,
                    None => insert_leaf_in(tx, &node, from_seq, to_seq)?,
                }
            }
            mark_compacted_in(tx, sid, from_seq, to_seq)?;
            Applied::Done
        }
        ConvEvent::Rewind(p) => {
            let SurfaceOp::Cut { after } = p.surface else {
                return Ok(Applied::Rebuild);
            };
            let doomed: Vec<i64> = {
                let mut st = tx.prepare(
                    "SELECT seq FROM messages
                     WHERE session_id = ?1 AND (event_id IS NULL OR event_id < ?2)",
                )?;
                let before: BTreeSet<i64> = st
                    .query_map(params![sid, ev.id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                addresses(tx, sid)?
                    .into_iter()
                    .filter(|(a, s)| *a > after && before.contains(s))
                    .map(|(_, s)| s)
                    .collect()
            };
            for seq in doomed {
                delete_row(tx, sid, seq)?;
            }
            Applied::Done
        }
        ConvEvent::Fork(_) => {
            let copied: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages m JOIN events e ON e.id = m.event_id
                               WHERE m.session_id = ?1 AND e.session_id != ?1)
                     OR EXISTS(SELECT 1 FROM messages WHERE session_id = ?1 AND sealed = 1)",
                [sid],
                |r| r.get(0),
            )?;
            if copied {
                Applied::Done
            } else {
                Applied::Rebuild
            }
        }
        ConvEvent::System(p) => {
            ensure_snapshot(tx, &p, &ev.ts)?;
            Applied::Done
        }
        ConvEvent::Attempt(_) | ConvEvent::Import(_) => Applied::Done,
    })
}

/// L'instantané du prompt système d'un `conv.system` (`prompt_snapshots`, #205), s'il
/// manque. La table est partagée entre sessions par empreinte : rien n'est réécrit ni
/// effacé ; `uses` part de zéro, le daemon le compte à chaque appel qui l'envoie.
pub(crate) fn ensure_snapshot(
    tx: &Transaction<'_>,
    p: &SystemPayload,
    ts: &str,
) -> penelope_store::Result<()> {
    let tiles = serde_json::to_string(&p.tiles).ok();
    tx.execute(
        "INSERT INTO prompt_snapshots(hash, rendered, tiers, first_seen_at, last_seen_at, uses)
         VALUES(?1, ?2, ?3, ?4, ?4, 0) ON CONFLICT(hash) DO NOTHING",
        params![p.hash, p.rendered, tiles, ts],
    )?;
    Ok(())
}

/// Les instantanés de tous les `conv.system` de la session (refonte).
fn ensure_snapshots_of(tx: &Transaction<'_>, sid: &str) -> penelope_store::Result<()> {
    for ev in session_events(tx, sid)? {
        if let Ok(Some(ConvEvent::System(p))) = ConvEvent::decode(&ev.kind, &ev.payload) {
            ensure_snapshot(tx, &p, &ev.ts)?;
        }
    }
    Ok(())
}

/// Retire une ligne, son entrée plein texte et son contexte figé.
fn delete_row(tx: &Transaction<'_>, sid: &str, seq: i64) -> penelope_store::Result<()> {
    tx.execute(
        "DELETE FROM messages_fts WHERE msg_id IN
            (SELECT id FROM messages WHERE session_id = ?1 AND seq = ?2)",
        params![sid, seq],
    )?;
    tx.execute(
        "DELETE FROM messages WHERE session_id = ?1 AND seq = ?2",
        params![sid, seq],
    )?;
    tx.execute(
        "DELETE FROM message_context WHERE session_id = ?1 AND seq = ?2",
        params![sid, seq],
    )?;
    Ok(())
}

/// Ce que la session doit contenir, pour une refonte ; `None` pour une session sans rien
/// à rejouer.
fn rebuild_plan(tx: &Transaction<'_>, sid: &str) -> Result<Option<(Expected, i64)>, ReplayError> {
    let lineage = Lineage::load(tx, sid)?;
    if !lineage.journaled() {
        if let Some((mother, seq, after)) = archive_of(tx, sid)? {
            return Ok(Some((archive_expected(tx, &mother, seq, after)?, 0)));
        }
        let rows: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            [sid],
            |r| r.get(0),
        )?;
        if rows > 0 {
            return Err(ReplayError::Journal(
                "lignes V0 sans journal ni scellement : rien à rejouer".into(),
            ));
        }
        return Ok(None);
    }
    let unjournaled: i64 = tx.query_row(
        "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND event_id IS NULL AND sealed = 0",
        [sid],
        |r| r.get(0),
    )?;
    if unjournaled > 0 {
        return Err(ReplayError::Journal(format!(
            "{unjournaled} lignes sans événement ni scellement : elles seraient perdues"
        )));
    }
    let surface = lineage.surface()?;
    let sealed_offset = match lineage.origin {
        Origin::Import(_) => lineage.offset,
        _ => 0,
    };
    Ok(Some((lineage.expected(tx, &surface)?, sealed_offset)))
}

/// Refond les caches d'une session dans la transaction de l'appelant : lignes non
/// scellées, contextes au-delà du préfixe scellé, nœuds LCM hors ceux que l'import nomme.
pub(crate) fn reindex_in(
    tx: &Transaction<'_>,
    sid: &str,
    ts: &str,
) -> Result<Reindexed, ReplayError> {
    let plan = rebuild_plan(tx, sid)?;
    Ok(write_plan(tx, sid, plan, ts)?)
}

/// Écrit ce que [`rebuild_plan`] a calculé. Une erreur ici est une erreur de base :
/// l'appelant annule la transaction.
fn write_plan(
    tx: &Transaction<'_>,
    sid: &str,
    plan: Option<(Expected, i64)>,
    ts: &str,
) -> penelope_store::Result<Reindexed> {
    let last_event: i64 = tx.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM events WHERE session_id = ?1",
        [sid],
        |r| r.get(0),
    )?;
    let mut done = Reindexed {
        session: sid.to_string(),
        ..Reindexed::default()
    };
    let Some((expected, sealed_offset)) = plan else {
        set_watermark(tx, sid, last_event, state(0, false, None), ts)?;
        return Ok(done);
    };
    tx.execute(
        "DELETE FROM messages_fts WHERE msg_id IN
            (SELECT id FROM messages WHERE session_id = ?1 AND sealed = 0)",
        [sid],
    )?;
    tx.execute(
        "DELETE FROM messages WHERE session_id = ?1 AND sealed = 0",
        [sid],
    )?;
    tx.execute(
        "DELETE FROM message_context WHERE session_id = ?1 AND seq > ?2",
        params![sid, sealed_offset],
    )?;
    for row in &expected.rows {
        if row.sealed {
            tx.execute(
                "UPDATE messages SET compacted = ?3 WHERE session_id = ?1 AND seq = ?2",
                params![sid, row.seq, row.compacted as i64],
            )?;
            continue;
        }
        let node = MessageNode {
            message: row.message.clone(),
            eager: row.eager,
            artifact_id: row.artifact_id.clone(),
            tokens: row.tokens,
            episode: row.episode,
        };
        let ts = if row.ts.is_empty() { ts } else { &row.ts };
        insert_message(
            tx,
            sid,
            row.seq,
            &node,
            ts,
            row.compacted,
            row.source_turn_id.as_deref(),
            row.event_id,
        )?;
        done.rows += 1;
    }
    for (seq, block) in expected.contexts.range(sealed_offset + 1..) {
        tx.execute(
            "INSERT OR REPLACE INTO message_context(session_id, seq, context) VALUES(?1, ?2, ?3)",
            params![sid, seq, block],
        )?;
        done.contexts += 1;
    }
    done.nodes = rewrite_nodes(tx, sid, &expected, ts)?;
    ensure_snapshots_of(tx, sid)?;
    set_watermark(tx, sid, last_event, state(sealed_offset, false, None), ts)?;
    Ok(done)
}

/// Les nœuds LCM de la session, hors scellés, effacés puis réécrits ; les scellés
/// reçoivent leur `superseded_by`.
fn rewrite_nodes(
    tx: &Transaction<'_>,
    sid: &str,
    expected: &Expected,
    ts: &str,
) -> penelope_store::Result<usize> {
    let keep: BTreeSet<&str> = expected
        .nodes
        .iter()
        .filter(|n| n.sealed)
        .filter_map(|n| n.node_id.as_deref())
        .collect();
    let existing: Vec<String> = {
        let mut st = tx.prepare("SELECT id FROM lcm_nodes WHERE session_id = ?1")?;
        st.query_map([sid], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    for id in existing.iter().filter(|id| !keep.contains(id.as_str())) {
        tx.execute(
            "DELETE FROM lcm_edges WHERE parent_id = ?1 OR child_id = ?1",
            [id],
        )?;
        tx.execute("DELETE FROM lcm_nodes WHERE id = ?1", [id])?;
    }
    let ids: Vec<String> = expected
        .nodes
        .iter()
        .map(|n| n.node_id.clone().unwrap_or_else(|| NodeId::new().0))
        .collect();
    let mut written = 0;
    for (n, id) in expected.nodes.iter().zip(&ids) {
        let superseded = n.superseded_by.map(|i| ids[i].clone());
        if n.sealed {
            tx.execute(
                "UPDATE lcm_nodes SET superseded_by = ?2 WHERE id = ?1",
                params![id, superseded],
            )?;
            continue;
        }
        tx.execute(
            "INSERT INTO lcm_nodes(id, session_id, kind, level, from_seq, to_seq, summary,
                anchors, tokens_src, tokens_self, tokens_subtree, created_at, superseded_by,
                event_id)
             VALUES(?1, ?2, 'leaf', 0, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11)",
            params![
                id,
                sid,
                n.from_seq,
                n.to_seq,
                n.summary,
                n.anchors,
                n.tokens_src as i64,
                n.tokens_self as i64,
                if n.ts.is_empty() { ts } else { &n.ts },
                superseded,
                n.event_id
            ],
        )?;
        written += 1;
    }
    Ok(written)
}

fn state(offset: i64, dirty: bool, error: Option<&str>) -> Value {
    let mut v = json!({"fold_version": FOLD_VERSION, "offset": offset, "dirty": dirty});
    if let Some(e) = error {
        v["error"] = json!(e);
    }
    v
}

/// Rattrape une session depuis son filigrane, dans la transaction de l'appelant.
fn catch_up_in(tx: &Transaction<'_>, sid: &str, ts: &str) -> Result<CatchUp, ReplayError> {
    let (last, state_now) = watermark(tx, sid)?.unwrap_or((0, Value::Null));
    let version = state_now.get("fold_version").and_then(Value::as_u64);
    if version.is_some_and(|v| v != FOLD_VERSION) {
        reindex_in(tx, sid, ts)?;
        return Ok(CatchUp::Rebuilt);
    }
    let events: Vec<Event> = session_events(tx, sid)?
        .into_iter()
        .filter(|e| e.id > last)
        .collect();
    let Some(newest) = events.iter().map(|e| e.id).max() else {
        return Ok(CatchUp::Current);
    };
    for ev in &events {
        if apply_in(tx, sid, ev)? == Applied::Rebuild {
            reindex_in(tx, sid, ts)?;
            return Ok(CatchUp::Rebuilt);
        }
    }
    let offset = origin_in(tx, sid)?.offset;
    set_watermark(tx, sid, newest, state(offset, false, None), ts)?;
    Ok(CatchUp::Applied(events.len()))
}

impl HistoryStore {
    /// Rattrape les caches d'une session depuis son filigrane (§2.4) : à l'ouverture
    /// d'une session, avant de la lire. Sans journal attaché, rien. En erreur, le
    /// filigrane porte `dirty` et l'erreur remonte ; l'appelant la journalise sans
    /// échouer.
    pub async fn catch_up(&self, session_id: &str) -> penelope_store::Result<CatchUp> {
        if self.events.is_none() {
            return Ok(CatchUp::Current);
        }
        let sid = session_id.to_string();
        let ts = self.clock.now_rfc3339();
        // Une erreur annule toute la transaction : le rattrapage ne laisse rien à moitié.
        let outcome = self
            .store()
            .write(move |tx| {
                catch_up_in(tx, &sid, &ts)
                    .map_err(|e| penelope_store::StoreError::other(e.to_string()))
            })
            .await;
        match outcome {
            Ok(done) => Ok(done),
            Err(e) => {
                let (sid, ts, msg) = (
                    session_id.to_string(),
                    self.clock.now_rfc3339(),
                    e.to_string(),
                );
                self.store()
                    .write(move |tx| {
                        let last = watermark(tx, &sid)?.map_or(0, |w| w.0);
                        set_watermark(tx, &sid, last, state(0, true, Some(&msg)), &ts)
                    })
                    .await?;
                Err(penelope_store::StoreError::other(format!(
                    "rattrapage de {session_id} : {e}"
                )))
            }
        }
    }

    /// Refond les caches de `session`, ou de toutes les sessions (`None`) : lignes non
    /// scellées effacées puis réécrites depuis le journal, plein texte compris, filigrane
    /// posé. Une session dont le journal ne se plie pas est laissée intacte et nommée.
    pub async fn reindex(&self, session: Option<&str>) -> penelope_store::Result<ReindexReport> {
        let ids: Vec<String> = match session {
            Some(s) => vec![s.to_string()],
            None => {
                self.store()
                    .read(|c| Ok(crate::verify::all_sessions(c)?))
                    .await?
            }
        };
        let mut report = ReindexReport::default();
        for sid in ids {
            let ts = self.clock.now_rfc3339();
            let session = sid.clone();
            let done = self
                .store()
                .write(move |tx| match rebuild_plan(tx, &session) {
                    Ok(plan) => Ok(Ok(write_plan(tx, &session, plan, &ts)?)),
                    Err(ReplayError::Journal(e)) => Ok(Err(e)),
                    Err(ReplayError::Store(e)) => Err(e),
                })
                .await?;
            match done {
                Ok(r) => report.sessions.push(r),
                Err(e) => report.refused.push((sid, e)),
            }
        }
        report.ok = report.refused.is_empty();
        Ok(report)
    }

    /// Sessions dont le dernier rattrapage a échoué (`dirty`), avec l'erreur.
    pub async fn dirty_projections(&self) -> penelope_store::Result<Vec<(String, String)>> {
        self.store()
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT session_id, COALESCE(json_extract(state, '$.error'), '')
                     FROM projections_session WHERE json_extract(state, '$.dirty') = 1
                     ORDER BY session_id",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }
}

#[cfg(test)]
mod tests;
