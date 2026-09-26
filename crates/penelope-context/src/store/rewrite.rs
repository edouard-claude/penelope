//! Réécritures de l'historique : fork et retour arrière, marquage d'une plage résumée,
//! corps externalisé (niveau 1). Chacune est un événement `conv.*` d'abord ; ses lignes
//! de cache suivent dans la seconde transaction (T16).

use super::*;
use crate::journal::{ConvEvent, ForkPayload, RewindPayload, SurfaceOp, ToolResultPayload};
use dual::address_in;
use penelope_store::rusqlite::Transaction;
use seal::sealed_offset_in;

/// Ce qu'un retour arrière a fait.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rewound {
    /// Lignes retirées de la session.
    pub removed: usize,
    /// Lignes projetées dans la session d'archive.
    pub archived: usize,
}

impl HistoryStore {
    /// Fork par référence (T10) : `conv.fork` en tête du journal de la fille, qui hérite
    /// de toute la surface de sa mère (`up_to` = sa dernière adresse, qui sert aussi
    /// d'`offset`). À écrire avant tout autre `conv.*` de la fille. La seconde transaction
    /// projette les caches de la fille depuis cet héritage : lignes (scellées comprises,
    /// avec leur drapeau), résumés actifs recopiés ; plus de copie hors du projecteur
    /// (T16). Rend le nombre de messages hérités.
    pub async fn fork(&self, child: &str, parent: &str) -> penelope_store::Result<usize> {
        let p = parent.to_string();
        let up_to = self
            .store
            .read(move |c| {
                let last: i64 = c.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
                    [&p],
                    |r| r.get(0),
                )?;
                Ok(dual::origin_in(c, &p)?.offset + last)
            })
            .await?;
        let event = ConvEvent::Fork(ForkPayload {
            surface: SurfaceOp::Inherit {
                parent: parent.to_string(),
                up_to,
                offset: up_to,
            },
            parent: parent.to_string(),
            up_to,
            offset: up_to,
        });
        let (sid, ts) = (child.to_string(), self.clock.now_rfc3339());
        self.journaled(child, event, move |tx, _| {
            Ok(crate::projector::project_in(tx, &sid, &ts)?.rows)
        })
        .await
    }

    /// Retour arrière journalisé (T10) : `conv.rewind` coupe la surface après le nœud qui
    /// précède `from_seq` (0 s'il n'y en a pas). La seconde transaction projette d'abord
    /// l'archive (`archive`, session déjà créée : ce que la coupe retire, relu dans le
    /// journal de la mère), puis retire les lignes coupées ; celles du préfixe scellé
    /// restent, masquées (`seal.rs`).
    pub async fn rewind_from(
        &self,
        session_id: &str,
        from_seq: i64,
        turns: u64,
        archive: Option<&str>,
    ) -> penelope_store::Result<Rewound> {
        let sid = session_id.to_string();
        let after = self
            .store
            .read(move |c| {
                let before: Option<i64> = c.query_row(
                    "SELECT MAX(seq) FROM messages
                     WHERE session_id = ?1 AND seq < ?2 AND sealed != 2",
                    params![sid, from_seq],
                    |r| r.get(0),
                )?;
                match before {
                    Some(seq) => address_in(c, &sid, seq),
                    None => Ok(0),
                }
            })
            .await?;
        let event = ConvEvent::Rewind(RewindPayload {
            surface: SurfaceOp::Cut { after },
            turns,
            archive_session: archive.map(String::from),
        });
        let (sid, archive, ts) = (
            session_id.to_string(),
            archive.map(String::from),
            self.clock.now_rfc3339(),
        );
        self.journaled(session_id, event, move |tx, _| {
            let archived = match &archive {
                Some(a) => crate::projector::project_in(tx, a, &ts)?.rows,
                None => 0,
            };
            Ok(Rewound {
                removed: truncate_in(tx, &sid, from_seq)?,
                archived,
            })
        })
        .await
    }

    /// Marque une plage de séquences comme couverte par un nœud de résumé : un résumé
    /// déjà publié sur exactement cette plage (`publish_summary`).
    pub(crate) async fn mark_compacted(
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

    /// Externalise le corps d'un résultat d'outil (niveau 1) : un `conv.tool_result` qui
    /// remplace ce seul nœud (T8), l'artefact déjà écrit et cité (`artifact_id`,
    /// `artifact_sha256`) avec le `call_id` du nœud, puis la ligne réécrite dans la
    /// seconde transaction. Le nœud garde son adresse (§2.3) : les paires appel/résultat
    /// ne bougent pas. Une ligne qui n'est pas un résultat d'outil n'a rien à citer : erreur.
    pub async fn externalise_as(
        &self,
        session_id: &str,
        seq: i64,
        new_body: &str,
        artifact: &Artifact,
        new_tokens: u64,
        original_tokens: u64,
    ) -> penelope_store::Result<()> {
        let mut payload = self
            .replacement(session_id, seq, new_body, artifact, new_tokens)
            .await?
            .ok_or_else(|| {
                penelope_store::StoreError::other(format!(
                    "{session_id} #{seq} n'est pas un résultat d'outil : rien à externaliser"
                ))
            })?;
        payload.original_tokens = Some(original_tokens);
        let event = ConvEvent::ToolResult(payload);
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

/// [`HistoryStore::externalise_as`] dans la transaction de l'appelant.
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

/// La coupe d'un retour arrière dans la transaction de l'appelant. Le contexte figé des
/// messages retirés part avec eux : laissé, il collait au message suivant écrit sous le
/// même numéro, et `freeze_context` le trouvait déjà figé (relevé par
/// `history verify`, T12). Les lignes du préfixe scellé de la session sont masquées, leur
/// contexte gardé : le `conv.import` les compte (`seal.rs`). Rend le nombre de lignes
/// retirées de la conversation, masquées comprises.
fn truncate_in(
    tx: &Transaction<'_>,
    session_id: &str,
    from_seq: i64,
) -> penelope_store::Result<usize> {
    let sealed = sealed_offset_in(tx, session_id)?;
    let masked = tx.execute(
        "UPDATE messages SET sealed = 2
         WHERE session_id = ?1 AND seq >= ?2 AND seq <= ?3 AND sealed = 1",
        params![session_id, from_seq, sealed],
    )?;
    tx.execute(
        "DELETE FROM messages_fts WHERE msg_id IN
            (SELECT id FROM messages WHERE session_id = ?1 AND seq >= ?2 AND sealed != 2)",
        params![session_id, from_seq],
    )?;
    tx.execute(
        "DELETE FROM message_context WHERE session_id = ?1 AND seq >= ?2 AND seq > ?3",
        params![session_id, from_seq, sealed],
    )?;
    let removed = tx.execute(
        "DELETE FROM messages WHERE session_id = ?1 AND seq >= ?2 AND sealed != 2",
        params![session_id, from_seq],
    )?;
    Ok(masked + removed)
}

#[cfg(test)]
mod tests;
