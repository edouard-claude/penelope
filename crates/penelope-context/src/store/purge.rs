//! Purge et rétention des caches de la conversation (épopée #208, T16).
//!
//! `penelope-ops` purge une session table par table dans une seule transaction ; les
//! tables que le projecteur tient (`messages`, `messages_fts`, `message_context`,
//! `lcm_*`, `prompt_snapshots`) ne s'écrivent qu'ici, dans cette crate
//! (`penelope-archtest`, `cache_writes`). Le journal, lui, est purgé par
//! `EventLog::purge_session` : ses payloads partent, ses hash restent.

use super::HistoryStore;
use penelope_store::rusqlite::{Transaction, params};

/// Ce que la purge d'une session a retiré de ses caches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CachesPurged {
    pub messages: usize,
    pub nodes: usize,
    pub prompts: usize,
}

impl HistoryStore {
    /// Efface les caches d'une session dans la transaction de l'appelant : messages et
    /// leur plein texte, contextes figés, nœuds LCM, et les prompts système que cette
    /// session seule citait (#205 : ils contiennent le profil, la mémoire rappelée et les
    /// notes de session ; ce qu'une autre lit encore attend sa purge à elle).
    pub fn purge_session_in(
        tx: &Transaction<'_>,
        session_id: &str,
    ) -> penelope_store::Result<CachesPurged> {
        let messages = tx.execute(
            "DELETE FROM messages_fts WHERE msg_id IN
               (SELECT id FROM messages WHERE session_id = ?1)",
            [session_id],
        )?;
        tx.execute("DELETE FROM messages WHERE session_id = ?1", [session_id])?;
        tx.execute(
            "DELETE FROM message_context WHERE session_id = ?1",
            [session_id],
        )?;
        tx.execute(
            "DELETE FROM lcm_edges WHERE parent_id IN
               (SELECT id FROM lcm_nodes WHERE session_id = ?1)
             OR child_id IN (SELECT id FROM lcm_nodes WHERE session_id = ?1)",
            [session_id],
        )?;
        let nodes = tx.execute("DELETE FROM lcm_nodes WHERE session_id = ?1", [session_id])?;
        let prompts = tx.execute(
            "DELETE FROM prompt_snapshots WHERE hash IN (
                SELECT system_hash FROM usage
                 WHERE session_id = ?1 AND system_hash IS NOT NULL
                UNION
                SELECT system_hash FROM llm_requests
                 WHERE session_id = ?1 AND system_hash IS NOT NULL
                UNION
                SELECT json_extract(payload, '$.hash') FROM events
                 WHERE session_id = ?1 AND kind = 'conv.system')
             AND hash NOT IN (
                SELECT system_hash FROM usage
                 WHERE COALESCE(session_id, '') <> ?1 AND system_hash IS NOT NULL)
             AND hash NOT IN (
                SELECT system_hash FROM llm_requests
                 WHERE COALESCE(session_id, '') <> ?1 AND system_hash IS NOT NULL)
             AND hash NOT IN (
                SELECT json_extract(payload, '$.hash') FROM events
                 WHERE kind = 'conv.system' AND COALESCE(session_id, '') <> ?1
                   AND json_extract(payload, '$.hash') IS NOT NULL)",
            [session_id],
        )?;
        Ok(CachesPurged {
            messages,
            nodes,
            prompts,
        })
    }

    /// Rétention (#205) : un instantané de prompt vu pour la dernière fois avant `cutoff`
    /// part, une fois que plus aucune ligne d'`usage` ni de `llm_requests` ne le cite.
    pub fn retire_prompts_in(tx: &Transaction<'_>, cutoff: &str) -> penelope_store::Result<usize> {
        Ok(tx.execute(
            "DELETE FROM prompt_snapshots
             WHERE last_seen_at < ?1
               AND hash NOT IN (SELECT system_hash FROM usage
                                 WHERE system_hash IS NOT NULL)
               AND hash NOT IN (SELECT system_hash FROM llm_requests
                                 WHERE system_hash IS NOT NULL)",
            params![cutoff],
        )?)
    }
}
