//! Le préfixe système retenu et les instantanés envoyés (issue #17, #205 ; épopée #208,
//! T16 : la clé `kv prompt.prefix.*` est retirée, le journal la remplace).

use super::*;
use crate::journal::{KIND_ASSISTANT, KIND_ATTEMPT, KIND_SYSTEM, SystemPayload};
use crate::tiers::Tiers;
use dual::system_in;
use penelope_kernel::journal::TileMap;

/// Un projet fixé à la main pour la session (`penelope-vault`, `session_project::set`).
pub const KIND_SESSION_PROJECT: &str = "session.project";

/// Les événements qui libèrent le préfixe retenu : une compaction (le cache est cassé de
/// toute façon), un projet fixé à la main (l'instantané de l'épisode est refigé au tour
/// suivant).
pub const PREFIX_RELEASES: &[&str] = &["context.compacted", KIND_SESSION_PROJECT];

impl HistoryStore {
    /// Le préfixe (T0, T1, T2) du dernier `conv.system` de la session, relu dans le
    /// journal, tant qu'il tient : `None` sans préfixe journalisé par la session
    /// elle-même, pour un payload purgé, ou si un [`PREFIX_RELEASES`] est arrivé depuis le
    /// dernier appel au modèle (ou le dernier préfixe). Le volatil (T4) est vide.
    pub async fn retained_prefix(&self, session_id: &str) -> penelope_store::Result<Option<Tiers>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let Some(seq) = system_in(c, &sid)?.and_then(|at| at.own_seq) else {
                    return Ok(None);
                };
                let pinned: i64 = c.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM events
                     WHERE session_id = ?1 AND kind IN (?2, ?3, ?4)",
                    params![sid, KIND_SYSTEM, KIND_ASSISTANT, KIND_ATTEMPT],
                    |r| r.get(0),
                )?;
                let released = c
                    .query_row(
                        "SELECT 1 FROM events WHERE session_id = ?1 AND seq > ?2
                           AND kind IN (?3, ?4) LIMIT 1",
                        params![sid, pinned, PREFIX_RELEASES[0], PREFIX_RELEASES[1]],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if released {
                    return Ok(None);
                }
                let payload: String = c.query_row(
                    "SELECT payload FROM events WHERE session_id = ?1 AND seq = ?2",
                    params![sid, seq],
                    |r| r.get(0),
                )?;
                let Ok(p) = serde_json::from_str::<SystemPayload>(&payload) else {
                    return Ok(None);
                };
                let tile = |name| p.tiles.slice(&p.rendered, name).map(String::from);
                Ok(match (tile("T0"), tile("T1"), tile("T2")) {
                    (Some(identity), Some(index), Some(context)) => Some(Tiers {
                        identity,
                        index,
                        context,
                        volatile: String::new(),
                    }),
                    _ => None,
                })
            })
            .await
    }

    /// Compte un envoi du prompt système `hash` dans son instantané (`uses`,
    /// `last_seen_at`) ; l'écrit s'il manque (préfixe hérité d'une mère, sans
    /// `conv.system` à la session).
    pub async fn prompt_sent(
        &self,
        hash: &str,
        rendered: &str,
        tiles: Option<&TileMap>,
    ) -> penelope_store::Result<()> {
        let (hash, rendered, ts) = (
            hash.to_string(),
            rendered.to_string(),
            self.clock.now_rfc3339(),
        );
        let tiles = tiles.and_then(|t| serde_json::to_string(t).ok());
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO prompt_snapshots(hash, rendered, tiers, first_seen_at,
                        last_seen_at, uses)
                     VALUES(?1,?2,?3,?4,?4,1)
                     ON CONFLICT(hash) DO UPDATE SET last_seen_at = ?4, uses = uses + 1",
                    params![hash, rendered, tiles, ts],
                )?;
                Ok(())
            })
            .await
    }
}
