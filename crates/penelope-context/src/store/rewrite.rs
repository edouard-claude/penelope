//! Réécritures de l'historique canonique : copie et coupe (fork, retour arrière),
//! marquage d'une plage résumée, corps externalisé (niveau 1).

use super::*;
use crate::journal::{ConvEvent, SurfaceOp, ToolResultPayload};
use dual::address_in;
use penelope_store::rusqlite::Transaction;

impl HistoryStore {
    /// Copie l'historique d'une session vers une autre, numéros et état de compaction
    /// compris, index plein texte avec (fork, archive d'un rewind).
    pub async fn copy_messages(
        &self,
        from: &str,
        to: &str,
        from_seq: i64,
        to_seq: Option<i64>,
    ) -> penelope_store::Result<usize> {
        let (from, to) = (from.to_string(), to.to_string());
        self.store
            .write(move |tx| {
                let n = tx.execute(
                    "INSERT INTO messages(session_id, seq, role, content, tool_call_id, tool_name,
                        tokens_est, ts, episode, eager, artifact_id, compacted)
                     SELECT ?2, seq, role, content, tool_call_id, tool_name, tokens_est, ts,
                        episode, eager, artifact_id, compacted
                     FROM messages
                     WHERE session_id = ?1 AND seq >= ?3 AND (?4 IS NULL OR seq <= ?4)
                     ORDER BY seq",
                    params![from, to, from_seq, to_seq],
                )?;
                tx.execute(
                    "INSERT INTO messages_fts(content, session_id, msg_id)
                     SELECT f.content, ?2, dst.id
                     FROM messages dst
                     JOIN messages src ON src.session_id = ?1 AND src.seq = dst.seq
                     JOIN messages_fts f ON f.msg_id = src.id
                     WHERE dst.session_id = ?2 AND dst.seq >= ?3 AND (?4 IS NULL OR dst.seq <= ?4)",
                    params![from, to, from_seq, to_seq],
                )?;
                Ok(n)
            })
            .await
    }

    /// Retire les messages d'une session à partir d'une séquence (incluse).
    pub async fn truncate_from(
        &self,
        session_id: &str,
        from_seq: i64,
    ) -> penelope_store::Result<usize> {
        let sid = session_id.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM messages_fts WHERE msg_id IN
                        (SELECT id FROM messages WHERE session_id = ?1 AND seq >= ?2)",
                    params![sid, from_seq],
                )?;
                Ok(tx.execute(
                    "DELETE FROM messages WHERE session_id = ?1 AND seq >= ?2",
                    params![sid, from_seq],
                )?)
            })
            .await
    }

    /// Marque une plage de séquences comme couverte par un nœud de résumé.
    pub async fn mark_compacted(
        &self,
        session_id: &str,
        from_seq: i64,
        to_seq: i64,
    ) -> penelope_store::Result<usize> {
        let sid = session_id.to_string();
        self.store
            .write(move |tx| mark_compacted_in(tx, &sid, from_seq, to_seq))
            .await
    }

    /// Remplace le corps d'un message par sa version externalisée (niveau 1, canonique).
    pub async fn externalise(
        &self,
        session_id: &str,
        seq: i64,
        new_body: &str,
        artifact_id: &str,
        new_tokens: u64,
    ) -> penelope_store::Result<()> {
        let (sid, body, art) = (
            session_id.to_string(),
            new_body.to_string(),
            artifact_id.to_string(),
        );
        self.store
            .write(move |tx| externalise_in(tx, &sid, seq, &body, &art, new_tokens))
            .await
    }

    /// Externalise le corps d'un résultat d'outil (niveau 1) et, journal attaché, le
    /// journalise comme un `conv.tool_result` qui remplace ce seul nœud (T8) : l'artefact
    /// est déjà écrit, l'événement le cite (`artifact_id`, `artifact_sha256`) avec le
    /// `call_id` du nœud, puis la ligne est réécrite dans la seconde transaction. Le nœud
    /// garde son adresse (§2.3) : les paires appel/résultat ne bougent pas.
    pub async fn externalise_as(
        &self,
        session_id: &str,
        seq: i64,
        new_body: &str,
        artifact: &Artifact,
        new_tokens: u64,
        original_tokens: u64,
    ) -> penelope_store::Result<()> {
        let event = match self.journals() {
            true => self
                .replacement(session_id, seq, new_body, artifact, new_tokens)
                .await?
                .map(|mut p| {
                    p.original_tokens = Some(original_tokens);
                    ConvEvent::ToolResult(p)
                }),
            false => None,
        };
        let (sid, body, art) = (
            session_id.to_string(),
            new_body.to_string(),
            artifact.id.clone(),
        );
        self.journaled(session_id, event, move |tx, _| {
            externalise_in(tx, &sid, seq, &body, &art, new_tokens)
        })
        .await
    }

    /// Le remplacement d'un résultat d'outil par son corps externalisé ; `None` si la
    /// ligne n'est pas un résultat d'outil (rien à citer par `call_id`).
    async fn replacement(
        &self,
        session_id: &str,
        seq: i64,
        new_body: &str,
        artifact: &Artifact,
        new_tokens: u64,
    ) -> penelope_store::Result<Option<ToolResultPayload>> {
        let sid = session_id.to_string();
        let row = self
            .store
            .read(move |c| {
                let row = c
                    .query_row(
                        "SELECT m.tool_call_id, m.tool_name, m.episode, m.eager,
                                json_extract(e.payload, '$.ok'), json_extract(e.payload, '$.turn'),
                                json_extract(e.payload, '$.step')
                         FROM messages m LEFT JOIN events e ON e.id = m.event_id
                         WHERE m.session_id=?1 AND m.seq=?2",
                        params![sid, seq],
                        |r| {
                            Ok((
                                r.get::<_, Option<String>>(0)?,
                                r.get::<_, Option<String>>(1)?,
                                r.get::<_, i64>(2)?,
                                r.get::<_, bool>(3)?,
                                r.get::<_, Option<bool>>(4)?,
                                r.get::<_, Option<String>>(5)?,
                                r.get::<_, Option<u32>>(6)?,
                            ))
                        },
                    )
                    .optional()?;
                let address = address_in(c, &sid, seq)?;
                Ok(row.map(|r| (r, address)))
            })
            .await?;
        let Some(((Some(call_id), tool, episode, eager, ok, turn, step), address)) = row else {
            return Ok(None);
        };
        let tool = tool.unwrap_or_default();
        Ok(Some(ToolResultPayload {
            surface: SurfaceOp::Replace {
                from: address,
                to: address,
            },
            turn,
            step: step.unwrap_or(0),
            content: ChatMessage::tool_result(call_id.clone(), tool.clone(), new_body).content,
            call_id,
            tool,
            ok: ok.unwrap_or(true),
            eager,
            episode,
            tokens_est: new_tokens,
            artifact_id: Some(artifact.id.clone()),
            artifact_sha256: Some(artifact.sha256.clone()),
            original_tokens: None,
        }))
    }
}

/// [`HistoryStore::mark_compacted`] dans la transaction de l'appelant.
pub(crate) fn mark_compacted_in(
    tx: &Transaction<'_>,
    session_id: &str,
    from_seq: i64,
    to_seq: i64,
) -> penelope_store::Result<usize> {
    Ok(tx.execute(
        "UPDATE messages SET compacted = 1
         WHERE session_id = ?1 AND seq >= ?2 AND seq <= ?3",
        params![session_id, from_seq, to_seq],
    )?)
}

/// [`HistoryStore::externalise`] dans la transaction de l'appelant.
fn externalise_in(
    tx: &Transaction<'_>,
    session_id: &str,
    seq: i64,
    body: &str,
    artifact_id: &str,
    new_tokens: u64,
) -> penelope_store::Result<()> {
    let content: String = tx.query_row(
        "SELECT content FROM messages WHERE session_id=?1 AND seq=?2",
        params![session_id, seq],
        |r| r.get(0),
    )?;
    let mut v: Value = serde_json::from_str(&content).unwrap_or_else(|_| json!({"blocks": []}));
    v["blocks"] = json!([{"type":"text","text": body}]);
    tx.execute(
        "UPDATE messages SET content=?3, artifact_id=?4, tokens_est=?5
         WHERE session_id=?1 AND seq=?2",
        params![
            session_id,
            seq,
            v.to_string(),
            artifact_id,
            new_tokens as i64
        ],
    )?;
    Ok(())
}
