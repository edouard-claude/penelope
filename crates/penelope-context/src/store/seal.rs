//! Scellement de l'historique V0 (épopée #208, tâche T11 ; `design/v1/source-de-verite.md`
//! §4.5, arbitrage 1 de `design/v1/README.md` §10).
//!
//! Une session écrite avant la double écriture n'a aucun événement `conv.*` pour ses
//! messages, et on ne fabrique pas une chaîne rétroactive : chaque session V0 reçoit **un**
//! `conv.import`, qui porte l'empreinte de son préfixe (lignes `messages`, contextes figés,
//! nœuds LCM actifs), et ses lignes sont marquées `sealed = 1`. Aucun message n'est
//! recopié dans le journal.
//!
//! Forme canonique hachée (sha256, hexadécimal), un tableau JSON sans objet pour que
//! l'ordre des clés ne compte pas :
//!
//! ```text
//! ["penelope.seal.v1",
//!  [[seq, role, content, tool_call_id, tool_name, tokens_est, ts, episode, eager,
//!    artifact_id], ...]                              par seq croissant
//!  [[seq, context], ...]                             par seq croissant
//!  [[node_id, from_seq, to_seq, tokens_self, summary], ...]   par (from, to, id)]
//! ```
//!
//! `content` est le texte stocké, octet pour octet. Le drapeau `compacted` n'y entre pas :
//! c'est un état dérivé, qu'une compaction ultérieure du préfixe (`mark_compacted`)
//! changerait sans rien changer au contenu ; au scellement, il est porté par
//! `lcm_active`, et la relecture le recalcule par couverture, comme la projection V0
//! (`conversation.rs`, qui repart après la fin du dernier résumé).

use super::*;
use crate::derive::{Sealed, SealedSummary};
use crate::journal::{ConvEvent, ImportPayload, KIND_IMPORT, SealedNode, SurfaceOp, is_purged};
use penelope_kernel::event::EventDraft;
use penelope_store::rusqlite::Connection;
use sha2::{Digest, Sha256};

/// Version de la forme canonique ; elle entre dans le texte haché.
pub const SEAL_FORMAT: &str = "penelope.seal.v1";

/// Une ligne `messages` du préfixe, telle qu'en base.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefixRow {
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tokens_est: i64,
    pub ts: String,
    pub episode: i64,
    pub eager: bool,
    pub artifact_id: Option<String>,
    /// Drapeau stocké ; hors empreinte (voir l'en-tête du module).
    pub compacted: bool,
}

/// Le préfixe V0 d'une session : ce que son `conv.import` scelle.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyPrefix {
    pub rows: Vec<PrefixRow>,
    pub contexts: BTreeMap<i64, String>,
    pub active: Vec<SealedSummary>,
}

/// Ce qu'une passe de scellement a fait.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SealReport {
    /// Sessions scellées : identifiant et nombre de messages.
    pub sealed: Vec<(String, i64)>,
    /// Sessions à lignes V0 laissées telles quelles, parce que leur journal a déjà un
    /// `conv.*` (fork, archive d'un retour arrière, base d'alpha en double écriture) :
    /// un `conv.import` ne pourrait plus être en tête (§2.3).
    pub skipped: Vec<String>,
}

impl LegacyPrefix {
    /// Adresse du dernier message scellé : l'`offset` de la session (§2.3).
    pub fn offset(&self) -> i64 {
        self.rows.last().map_or(0, |r| r.seq)
    }

    /// Empreinte canonique du préfixe.
    pub fn digest(&self) -> String {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|r| {
                json!([
                    r.seq,
                    r.role,
                    r.content,
                    r.tool_call_id,
                    r.tool_name,
                    r.tokens_est,
                    r.ts,
                    r.episode,
                    r.eager,
                    r.artifact_id
                ])
            })
            .collect();
        let contexts: Vec<Value> = self.contexts.iter().map(|(s, c)| json!([s, c])).collect();
        let mut active = self.active.clone();
        active.sort_by(|a, b| (a.from, a.to, &a.node_id).cmp(&(b.from, b.to, &b.node_id)));
        let nodes: Vec<Value> = active
            .iter()
            .map(|n| json!([n.node_id, n.from, n.to, n.tokens_self, n.summary]))
            .collect();
        let canonical = json!([SEAL_FORMAT, rows, contexts, nodes]).to_string();
        let hash = Sha256::digest(canonical.as_bytes());
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Le `conv.import` qui scelle ce préfixe.
    pub fn payload(&self) -> ImportPayload {
        let messages = self.rows.len() as i64;
        ImportPayload {
            surface: SurfaceOp::Seal {
                messages,
                offset: self.offset(),
            },
            messages,
            contexts: self.contexts.len() as i64,
            lcm_active: self
                .active
                .iter()
                .map(|n| SealedNode {
                    node: n.node_id.clone(),
                    from: n.from,
                    to: n.to,
                    superseded_by: None,
                })
                .collect(),
            digest: self.digest(),
        }
    }

    /// Le préfixe à plier avant les événements de la session. Un message est masqué
    /// quand un nœud actif le couvre, quel que soit son drapeau stocké.
    pub fn sealed(&self) -> Sealed {
        let covered = |seq: i64| self.active.iter().any(|n| n.from <= seq && seq <= n.to);
        let entries = self
            .rows
            .iter()
            .map(|r| Entry {
                seq: r.seq,
                message: deserialise_content(
                    Role::parse(&r.role).unwrap_or(Role::User),
                    &r.content,
                    r.tool_call_id.clone(),
                    r.tool_name.clone(),
                ),
                tokens: r.tokens_est as u64,
                episode: r.episode,
                eager: r.eager,
                artifact_id: r.artifact_id.clone(),
                compacted: covered(r.seq),
            })
            .collect();
        Sealed::import(entries, self.contexts.clone(), self.active.clone())
    }

    /// Les lignes d'une session : V0 à sceller (`sealed = false`) ou déjà scellées.
    fn read_rows(
        c: &Connection,
        sid: &str,
        sealed: bool,
    ) -> penelope_store::Result<Vec<PrefixRow>> {
        let sql = if sealed {
            "SELECT seq, role, content, tool_call_id, tool_name, tokens_est, ts, episode,
                    eager, artifact_id, compacted
             FROM messages WHERE session_id = ?1 AND sealed = 1 ORDER BY seq"
        } else {
            "SELECT seq, role, content, tool_call_id, tool_name, tokens_est, ts, episode,
                    eager, artifact_id, compacted
             FROM messages WHERE session_id = ?1 AND event_id IS NULL AND sealed = 0
             ORDER BY seq"
        };
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([sid], |r| {
            Ok(PrefixRow {
                seq: r.get(0)?,
                role: r.get(1)?,
                content: r.get(2)?,
                tool_call_id: r.get(3)?,
                tool_name: r.get(4)?,
                tokens_est: r.get(5)?,
                ts: r.get(6)?,
                episode: r.get(7)?,
                eager: r.get::<_, i64>(8)? != 0,
                artifact_id: r.get(9)?,
                compacted: r.get::<_, i64>(10)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn read_contexts(
        c: &Connection,
        sid: &str,
        up_to: i64,
    ) -> penelope_store::Result<BTreeMap<i64, String>> {
        let mut st = c.prepare(
            "SELECT seq, context FROM message_context WHERE session_id = ?1 AND seq <= ?2",
        )?;
        let rows = st.query_map(params![sid, up_to], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Le préfixe V0 à sceller : lignes sans événement, contextes, nœuds actifs (même
    /// règle que `Lcm::active_nodes`).
    fn read_legacy(c: &Connection, sid: &str) -> penelope_store::Result<LegacyPrefix> {
        let rows = Self::read_rows(c, sid, false)?;
        let up_to = rows.last().map_or(0, |r| r.seq);
        let contexts = Self::read_contexts(c, sid, up_to)?;
        let mut st = c.prepare(
            "SELECT n.id, n.summary, n.tokens_self, n.from_seq, n.to_seq
             FROM lcm_nodes n
             WHERE n.session_id = ?1
               AND n.superseded_by IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM lcm_edges e
                   JOIN lcm_nodes p ON p.id = e.parent_id
                   WHERE e.child_id = n.id AND p.superseded_by IS NULL
               )
             ORDER BY COALESCE(n.from_seq, 0), n.level",
        )?;
        let active = st
            .query_map([sid], |r| {
                Ok(SealedSummary {
                    node_id: r.get(0)?,
                    summary: r.get(1)?,
                    tokens_self: r.get::<_, i64>(2)? as u64,
                    from: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    to: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(LegacyPrefix {
            rows,
            contexts,
            active,
        })
    }
}

/// Le journal de la session a-t-il déjà un événement `conv.*` ?
fn has_conv_event(c: &Connection, sid: &str) -> penelope_store::Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE session_id = ?1 AND kind LIKE 'conv.%')",
        [sid],
        |r| r.get(0),
    )?)
}

impl HistoryStore {
    /// Scelle chaque session V0 (§4.5) : un `conv.import` par session qui a au moins un
    /// message sans événement ni scellement, et un journal sans `conv.*`. Idempotent : une
    /// session scellée a son `conv.import` et ses lignes `sealed`, elle n'est plus
    /// candidate.
    ///
    /// Une transaction par session : l'événement (`EventLog::append_in`) et le marquage
    /// sont commités ensemble, sans fenêtre de crash entre les deux. L'événement n'est pas
    /// diffusé en direct : l'étape tourne au démarrage, avant tout abonné.
    pub async fn seal_legacy(&self) -> penelope_store::Result<SealReport> {
        let events = self
            .events
            .clone()
            .ok_or_else(|| penelope_store::StoreError::other("scellement sans journal attaché"))?;
        let candidates: Vec<String> = self
            .store
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT DISTINCT session_id FROM messages INDEXED BY messages_unsealed
                     WHERE event_id IS NULL AND sealed = 0 ORDER BY session_id",
                )?;
                let rows = st.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        let mut report = SealReport::default();
        for sid in candidates {
            let events = events.clone();
            let session = sid.clone();
            let sealed = self
                .store
                .write(move |tx| {
                    if has_conv_event(tx, &session)? {
                        return Ok(None);
                    }
                    let prefix = LegacyPrefix::read_legacy(tx, &session)?;
                    let payload = prefix.payload();
                    let draft = EventDraft::new(KIND_IMPORT, ConvEvent::Import(payload).payload())
                        .session(&session);
                    events.append_in(tx, draft)?;
                    tx.execute(
                        "UPDATE messages SET sealed = 1
                         WHERE session_id = ?1 AND event_id IS NULL AND sealed = 0 AND seq <= ?2",
                        params![session, prefix.offset()],
                    )?;
                    Ok(Some(prefix.rows.len() as i64))
                })
                .await?;
            match sealed {
                Some(n) => report.sealed.push((sid, n)),
                None => report.skipped.push(sid),
            }
        }
        Ok(report)
    }

    /// Le préfixe scellé d'une session et son `conv.import`, relus en base ; `None` pour
    /// une session jamais scellée (ou dont l'import a été purgé). Les nœuds LCM sont ceux
    /// que l'import nomme, avec ses bornes : un `superseded_by` posé depuis ne les retire
    /// pas du préfixe.
    pub async fn sealed_prefix(
        &self,
        session_id: &str,
    ) -> penelope_store::Result<Option<(ImportPayload, LegacyPrefix)>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT payload FROM events WHERE session_id = ?1 AND kind = ?2 ORDER BY seq",
                )?;
                let payloads: Vec<String> = st
                    .query_map(params![sid, KIND_IMPORT], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut import = None;
                for text in payloads {
                    let value: Value = serde_json::from_str(&text)?;
                    if is_purged(&value) {
                        continue;
                    }
                    if let Some(ConvEvent::Import(p)) = ConvEvent::decode(KIND_IMPORT, &value)
                        .map_err(|e| penelope_store::StoreError::other(e.to_string()))?
                    {
                        import = Some(p);
                        break;
                    }
                }
                let Some(import) = import else {
                    return Ok(None);
                };
                let rows = LegacyPrefix::read_rows(c, &sid, true)?;
                let up_to = match import.surface {
                    SurfaceOp::Seal { offset, .. } => offset,
                    _ => rows.last().map_or(0, |r| r.seq),
                };
                let contexts = LegacyPrefix::read_contexts(c, &sid, up_to)?;
                let mut active = Vec::new();
                for n in &import.lcm_active {
                    let found = c
                        .query_row(
                            "SELECT summary, tokens_self FROM lcm_nodes WHERE id = ?1",
                            [&n.node],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                        )
                        .optional()?;
                    if let Some((summary, tokens_self)) = found {
                        active.push(SealedSummary {
                            node_id: n.node.clone(),
                            summary,
                            tokens_self: tokens_self as u64,
                            from: n.from,
                            to: n.to,
                        });
                    }
                }
                Ok(Some((
                    import,
                    LegacyPrefix {
                        rows,
                        contexts,
                        active,
                    },
                )))
            })
            .await
    }
}
