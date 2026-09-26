//! Tests : écrire comme la V0, sans événement. En production, plus rien n'écrit ainsi
//! (T16) ; un historique d'avant le journal n'existe plus que scellé (§4.5), et les
//! tests du scellement, de la vérification et de la lecture ont besoin d'en fabriquer.

use super::*;

impl HistoryStore {
    /// Une ligne de `messages` et son entrée plein texte, sans `conv.*`.
    pub(crate) async fn append_legacy(
        &self,
        session_id: &str,
        message: &ChatMessage,
        tokens: u64,
        episode: i64,
    ) -> penelope_store::Result<i64> {
        let (sid, role, content, text, call_id, name, ts) = (
            session_id.to_string(),
            message.role.as_str().to_string(),
            serialise_content(message)?,
            message.text(),
            message.tool_call_id.clone(),
            message.name.clone(),
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                let seq: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
                    [&sid],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO messages(session_id, seq, role, content, tool_call_id,
                        tool_name, tokens_est, ts, episode)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        sid,
                        seq,
                        role,
                        content,
                        call_id,
                        name,
                        tokens as i64,
                        ts,
                        episode
                    ],
                )?;
                tx.execute(
                    "INSERT INTO messages_fts(content, session_id, msg_id) VALUES(?1,?2,?3)",
                    params![text, sid, tx.last_insert_rowid()],
                )?;
                Ok(seq)
            })
            .await
    }

    /// Un contexte figé, sans `conv.context`.
    pub(crate) async fn freeze_legacy(
        &self,
        session_id: &str,
        seq: i64,
        context: &str,
    ) -> penelope_store::Result<()> {
        let (sid, ctx) = (session_id.to_string(), context.to_string());
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO message_context(session_id, seq, context) VALUES(?1, ?2, ?3)",
                    params![sid, seq, ctx],
                )?;
                Ok(())
            })
            .await
    }
}
