//! Double écriture de l'historique (épopée #208, tâche T5 ; phase 1 de
//! `design/v1/source-de-verite.md` §4.1).
//!
//! Une ligne de `messages` s'écrit avec son événement `conv.*` : l'événement d'abord,
//! commité seul (c'est la vérité), puis la ligne dans une seconde transaction du même
//! thread écrivain, qui reçoit `event_id` (`EventLog::append_with`). Les tables restent
//! la source de lecture : rien de ce que le modèle lit ne change.
//!
//! Sans journal attaché (`with_events` jamais appelé : outils, tests unitaires), la ligne
//! s'écrit seule, comme avant.

use super::*;
use crate::journal::{
    ContextPayload, ConvEvent, KIND_FORK, KIND_IMPORT, KIND_SUMMARY, KIND_SYSTEM, KIND_USER,
    Provenance, SurfaceOp, SystemPayload, SystemReason, message_event,
};
use crate::tiers::{Tiers, TileMap};
use penelope_kernel::event::{EventDraft, EventLog};
use penelope_store::rusqlite::{Connection, Transaction};
use std::sync::{Arc, Mutex};

/// Une ligne de `messages` prête à écrire.
pub(super) struct Row {
    pub sid: String,
    pub role: String,
    pub content: String,
    pub searchable: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tokens: u64,
    pub ts: String,
    pub episode: i64,
    pub eager: bool,
    pub artifact_id: Option<String>,
    /// Clé d'idempotence d'un message venu de la file (#161).
    pub source_turn_id: Option<String>,
}

impl Row {
    fn of(
        session_id: &str,
        message: &ChatMessage,
        tokens: u64,
        episode: i64,
        eager: bool,
        ts: String,
    ) -> penelope_store::Result<Row> {
        Ok(Row {
            sid: session_id.to_string(),
            role: message.role.as_str().to_string(),
            content: serialise_content(message)?,
            searchable: message.text(),
            tool_call_id: message.tool_call_id.clone(),
            tool_name: message.name.clone(),
            tokens,
            ts,
            episode,
            eager,
            artifact_id: None,
            source_turn_id: None,
        })
    }
}

/// Insère la ligne et son entrée FTS ; rend son `seq`. Un message de la file déjà écrit
/// rend le `seq` existant sans rien écrire.
fn insert_row(
    tx: &Transaction<'_>,
    row: &Row,
    event_id: Option<i64>,
) -> penelope_store::Result<i64> {
    if let Some(source) = &row.source_turn_id
        && let Some(seq) = tx
            .query_row(
                "SELECT seq FROM messages WHERE source_turn_id=?1",
                [source],
                |r| r.get(0),
            )
            .optional()?
    {
        return Ok(seq);
    }
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
        [&row.sid],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO messages(session_id, seq, role, content, tool_call_id, tool_name,
            tokens_est, ts, episode, eager, artifact_id, source_turn_id, event_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            row.sid,
            seq,
            row.role,
            row.content,
            row.tool_call_id,
            row.tool_name,
            row.tokens as i64,
            row.ts,
            row.episode,
            row.eager as i64,
            row.artifact_id,
            row.source_turn_id,
            event_id
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO messages_fts(content, session_id, msg_id) VALUES(?1,?2,?3)",
        params![row.searchable, row.sid, id],
    )?;
    Ok(seq)
}

impl HistoryStore {
    /// Attache le journal : chaque message écrit ensuite l'est aussi en `conv.*`.
    pub fn with_events(mut self, events: EventLog) -> Self {
        self.events = Some(events);
        self
    }

    /// Ajoute un message avec ce que l'appelant sait de sa provenance (tour, étape,
    /// appel au modèle) : l'événement `conv.*` le porte, la ligne n'en garde rien.
    #[allow(clippy::too_many_arguments)] // la signature de `append`, plus la provenance
    pub async fn append_as(
        &self,
        session_id: &str,
        message: &ChatMessage,
        tokens: u64,
        episode: i64,
        eager: bool,
        artifact_id: Option<String>,
        prov: &Provenance,
    ) -> penelope_store::Result<i64> {
        let mut row = Row::of(
            session_id,
            message,
            tokens,
            episode,
            eager,
            self.clock.now_rfc3339(),
        )?;
        row.artifact_id = artifact_id;
        let event = message_event(message, tokens, episode, eager, prov);
        self.write_row(row, event).await
    }

    /// Ajoute **une seule fois** un message utilisateur venu de la file, sous sa clé
    /// d'idempotence (`prov.turn_message_id`, #161) et avec son heure d'arrivée.
    ///
    /// Rejoué après un crash, il retrouve la ligne ; ou, si le crash est tombé entre
    /// l'événement et la ligne, l'événement, et n'écrit que la ligne (§2.7).
    pub async fn append_queued(
        &self,
        session_id: &str,
        message: &ChatMessage,
        tokens: u64,
        episode: i64,
        prov: &Provenance,
    ) -> penelope_store::Result<i64> {
        let key = prov
            .turn_message_id
            .clone()
            .ok_or_else(|| penelope_store::StoreError::other("message de file sans clé"))?;
        let ts = prov
            .arrived_at
            .clone()
            .unwrap_or_else(|| self.clock.now_rfc3339());
        let mut row = Row::of(session_id, message, tokens, episode, false, ts)?;
        row.source_turn_id = Some(key.clone());
        let (sid, source) = (session_id.to_string(), key);
        let (seq, journaled) = self
            .store
            .read(move |c| {
                let seq = c
                    .query_row(
                        "SELECT seq FROM messages WHERE source_turn_id=?1",
                        [&source],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?;
                let event = c
                    .query_row(
                        "SELECT id FROM events WHERE session_id=?1 AND kind=?2
                           AND json_extract(payload, '$.turn_message_id')=?3",
                        params![sid, KIND_USER, source],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?;
                Ok((seq, event))
            })
            .await?;
        if let Some(seq) = seq {
            return Ok(seq);
        }
        match (journaled, &self.events) {
            (Some(event_id), Some(_)) => {
                self.store
                    .write(move |tx| insert_row(tx, &row, Some(event_id)))
                    .await
            }
            _ => {
                let event = message_event(message, tokens, episode, false, prov);
                self.write_row(row, event).await
            }
        }
    }

    /// Fige le contexte volatil (T4) d'un message utilisateur : il l'accompagnera dans
    /// toutes les requêtes suivantes, pour que le préfixe ne change plus (issue #17).
    /// Sans effet s'il est déjà figé. Journalisé en `conv.context` (T6), qui vise
    /// l'adresse du `conv.user` ([`HistoryStore::address`], §2.3).
    pub async fn freeze_context(
        &self,
        session_id: &str,
        seq: i64,
        context: &str,
    ) -> penelope_store::Result<bool> {
        let (sid, ctx) = (session_id.to_string(), context.to_string());
        let insert = move |tx: &Transaction<'_>| -> penelope_store::Result<bool> {
            Ok(tx.execute(
                "INSERT OR IGNORE INTO message_context(session_id, seq, context)
                 VALUES(?1, ?2, ?3)",
                params![sid, seq, ctx],
            )? > 0)
        };
        let Some(log) = &self.events else {
            return self.store.write(insert).await;
        };
        let sid = session_id.to_string();
        let (frozen, target) = self
            .store
            .read(move |c| {
                let frozen = c
                    .query_row(
                        "SELECT 1 FROM message_context WHERE session_id=?1 AND seq=?2",
                        params![sid, seq],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                Ok((frozen, address_in(c, &sid, seq)?))
            })
            .await?;
        if frozen {
            return Ok(false);
        }
        let event = ConvEvent::Context(ContextPayload {
            target,
            block: context.to_string(),
        });
        let done = Arc::new(Mutex::new(false));
        let out = done.clone();
        let draft = EventDraft::new(event.kind(), event.payload()).session(session_id);
        log.append_with(draft, move |tx, _| {
            *out.lock().unwrap_or_else(|p| p.into_inner()) = insert(tx)?;
            Ok(())
        })
        .await
        .map_err(|e| penelope_store::StoreError::other(e.to_string()))?;
        Ok(*done.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Journalise le préfixe système retenu pour la prochaine requête, en entier
    /// (`conv.system`, T6, arbitrage du propriétaire README §10.2), s'il diffère du
    /// dernier journalisé. Rend la raison écrite, ou `None` si le préfixe n'a pas bougé
    /// ou qu'aucun journal n'est attaché.
    ///
    /// La raison : `first` pour le premier de la session, `compaction` si un résumé a été
    /// publié depuis le précédent (`context.compacted`), `cold` sinon (le préfixe en
    /// attente sort à un cache froid, `stable_prefix`). Le premier préfixe d'une session
    /// fille remplace celui qu'elle hérite de sa mère (T10), à son adresse.
    pub async fn journal_system(
        &self,
        session_id: &str,
        tiers: &Tiers,
    ) -> penelope_store::Result<Option<SystemReason>> {
        let Some(log) = &self.events else {
            return Ok(None);
        };
        let rendered = tiers.prefix();
        let hash = penelope_kernel::canonical::sha256_hex(rendered.as_bytes());
        let sid = session_id.to_string();
        let last = self
            .store
            .read(move |c| {
                let Some(at) = system_in(c, &sid)? else {
                    return Ok(None);
                };
                let compacted = match at.own_seq {
                    Some(seq) => c
                        .query_row(
                            "SELECT 1 FROM events WHERE session_id=?1 AND seq>?2
                               AND kind='context.compacted' LIMIT 1",
                            params![sid, seq],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some(),
                    None => false,
                };
                Ok(Some((at, compacted)))
            })
            .await?;
        let (surface, reason) = match last {
            None => (SurfaceOp::Append, SystemReason::First),
            Some((at, _)) if at.own_seq.is_some() && at.hash.as_deref() == Some(&hash) => {
                return Ok(None);
            }
            // Le premier préfixe d'une session fille remplace celui qu'elle hérite.
            Some((at, _)) if at.own_seq.is_none() => (
                SurfaceOp::Replace {
                    from: at.address,
                    to: at.address,
                },
                SystemReason::First,
            ),
            Some((at, compacted)) => (
                SurfaceOp::Replace {
                    from: at.address,
                    to: at.address,
                },
                if compacted {
                    SystemReason::Compaction
                } else {
                    SystemReason::Cold
                },
            ),
        };
        let event = ConvEvent::System(SystemPayload {
            surface,
            hash,
            rendered,
            tiles: TileMap::of(tiers),
            reason,
        });
        log.append(EventDraft::new(event.kind(), event.payload()).session(session_id))
            .await
            .map_err(|e| penelope_store::StoreError::other(e.to_string()))?;
        Ok(Some(reason))
    }

    /// L'événement puis la ligne, ou la ligne seule sans journal ou sans événement.
    async fn write_row(&self, row: Row, event: Option<ConvEvent>) -> penelope_store::Result<i64> {
        let sid = row.sid.clone();
        self.journaled(&sid, event, move |tx, event_id| {
            insert_row(tx, &row, event_id)
        })
        .await
    }

    /// Écrit `event` au journal, puis `write` dans la seconde transaction du même thread
    /// écrivain, avec l'identifiant de l'événement (`EventLog::append_with`). Sans
    /// journal attaché ou sans événement, `write` seul, sans identifiant.
    pub(crate) async fn journaled<T, F>(
        &self,
        session_id: &str,
        event: Option<ConvEvent>,
        write: F,
    ) -> penelope_store::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>, Option<i64>) -> penelope_store::Result<T> + Send + 'static,
    {
        let (Some(log), Some(event)) = (&self.events, event) else {
            return self.store.write(move |tx| write(tx, None)).await;
        };
        let out = Arc::new(Mutex::new(None));
        let slot = out.clone();
        let draft = EventDraft::new(event.kind(), event.payload()).session(session_id);
        log.append_with(draft, move |tx, ev| {
            let v = write(tx, Some(ev.id))?;
            *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(v);
            Ok(())
        })
        .await
        .map_err(|e| penelope_store::StoreError::other(e.to_string()))?;
        let v = out.lock().unwrap_or_else(|p| p.into_inner()).take();
        v.ok_or_else(|| penelope_store::StoreError::other("écriture journalisée sans résultat"))
    }

    /// Le `conv.summary` déjà écrit pour un travail de résumé (sa clé d'idempotence) :
    /// son identifiant et le nœud qu'il annonce.
    pub(crate) async fn summary_event(
        &self,
        session_id: &str,
        key: &str,
    ) -> penelope_store::Result<Option<(i64, String)>> {
        let (sid, key) = (session_id.to_string(), key.to_string());
        self.store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT id, json_extract(payload, '$.node_id') FROM events
                     WHERE session_id=?1 AND kind=?2
                       AND json_extract(payload, '$.idempotency_key')=?3
                     ORDER BY seq LIMIT 1",
                    params![sid, KIND_SUMMARY, key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
            })
            .await
    }

    /// Vrai si un journal est attaché.
    pub(crate) fn journals(&self) -> bool {
        self.events.is_some()
    }

    /// Adresse de surface (§2.3) du message `seq` d'une session : `offset + events.seq`
    /// de son événement, l'offset étant celui de la session qui porte l'événement (la
    /// mère, pour une ligne héritée d'un fork) ; le `seq` V0 d'une ligne sans événement.
    pub async fn address(&self, session_id: &str, seq: i64) -> penelope_store::Result<i64> {
        let sid = session_id.to_string();
        self.store.read(move |c| address_in(c, &sid, seq)).await
    }
}

/// [`HistoryStore::address`] dans une connexion ou une transaction de l'appelant.
pub(crate) fn address_in(
    c: &Connection,
    session_id: &str,
    seq: i64,
) -> penelope_store::Result<i64> {
    let found = c
        .query_row(
            "SELECT e.session_id, e.seq FROM messages m JOIN events e ON e.id = m.event_id
             WHERE m.session_id = ?1 AND m.seq = ?2",
            params![session_id, seq],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    match found {
        Some((owner, event_seq)) => Ok(origin_in(c, &owner)?.offset + event_seq),
        None => Ok(seq),
    }
}

/// Ce qu'une session hérite, lu sur le `conv.fork` ou le `conv.import` en tête de son
/// journal (§2.3).
pub(crate) struct Origin {
    /// Plus grande adresse héritée ; 0 sans héritage.
    pub offset: i64,
    /// La mère et l'adresse jusqu'où la fille en hérite, pour un fork.
    pub parent: Option<(String, i64)>,
}

pub(crate) fn origin_in(c: &Connection, session_id: &str) -> penelope_store::Result<Origin> {
    let head = c
        .query_row(
            "SELECT json_extract(payload, '$.offset'), json_extract(payload, '$.parent'),
                    json_extract(payload, '$.up_to')
             FROM events WHERE session_id = ?1 AND kind IN (?2, ?3) ORDER BY seq LIMIT 1",
            params![session_id, KIND_FORK, KIND_IMPORT],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((offset, parent, up_to)) = head else {
        return Ok(Origin {
            offset: 0,
            parent: None,
        });
    };
    Ok(Origin {
        offset: offset.unwrap_or(0),
        parent: parent.zip(up_to),
    })
}

/// Le prompt système en vigueur pour une session : son dernier `conv.system`, ou celui
/// qu'elle hérite de sa mère (jusqu'à l'adresse du fork, récursivement).
pub(crate) struct SystemAt {
    pub address: i64,
    /// `seq` de l'événement quand il est dans le journal de la session elle-même.
    pub own_seq: Option<i64>,
    /// `None` pour un événement purgé.
    pub hash: Option<String>,
}

pub(crate) fn system_in(
    c: &Connection,
    session_id: &str,
) -> penelope_store::Result<Option<SystemAt>> {
    let (mut session, mut limit, mut own) = (session_id.to_string(), i64::MAX, true);
    // Une chaîne de forks est finie ; la borne ne sert qu'à ne jamais boucler.
    for _ in 0..256 {
        let origin = origin_in(c, &session)?;
        let last = c
            .query_row(
                "SELECT seq, json_extract(payload, '$.hash') FROM events
                 WHERE session_id = ?1 AND kind = ?2 AND seq <= ?3 - ?4
                 ORDER BY seq DESC LIMIT 1",
                params![session, KIND_SYSTEM, limit, origin.offset],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        if let Some((seq, hash)) = last {
            return Ok(Some(SystemAt {
                address: origin.offset + seq,
                own_seq: own.then_some(seq),
                hash,
            }));
        }
        let Some((parent, up_to)) = origin.parent else {
            return Ok(None);
        };
        (session, limit, own) = (parent, up_to, false);
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
