//! Réécritures de l'historique canonique : copie et coupe (fork, retour arrière),
//! marquage d'une plage résumée, corps externalisé (niveau 1).

use super::*;
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
            .write(move |tx| {
                let content: String = tx.query_row(
                    "SELECT content FROM messages WHERE session_id=?1 AND seq=?2",
                    params![sid, seq],
                    |r| r.get(0),
                )?;
                let mut v: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| json!({"blocks": []}));
                v["blocks"] = json!([{"type":"text","text": body}]);
                tx.execute(
                    "UPDATE messages SET content=?3, artifact_id=?4, tokens_est=?5
                     WHERE session_id=?1 AND seq=?2",
                    params![sid, seq, v.to_string(), art, new_tokens as i64],
                )?;
                Ok(())
            })
            .await
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
