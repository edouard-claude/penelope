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
use crate::journal::{ConvEvent, KIND_USER, Provenance, message_event};
use penelope_kernel::event::{EventDraft, EventLog};
use penelope_store::rusqlite::Transaction;
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

    /// L'événement puis la ligne, ou la ligne seule sans journal ou sans événement.
    async fn write_row(&self, row: Row, event: Option<ConvEvent>) -> penelope_store::Result<i64> {
        let (Some(log), Some(event)) = (&self.events, event) else {
            return self.store.write(move |tx| insert_row(tx, &row, None)).await;
        };
        let seq = Arc::new(Mutex::new(None));
        let out = seq.clone();
        let draft = EventDraft::new(event.kind(), event.payload()).session(&row.sid);
        log.append_with(draft, move |tx, ev| {
            let s = insert_row(tx, &row, Some(ev.id))?;
            *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(s);
            Ok(())
        })
        .await
        .map_err(|e| penelope_store::StoreError::other(e.to_string()))?;
        let seq = *seq.lock().unwrap_or_else(|p| p.into_inner());
        seq.ok_or_else(|| penelope_store::StoreError::other("ligne de message non écrite"))
    }
}

#[cfg(test)]
mod tests;
