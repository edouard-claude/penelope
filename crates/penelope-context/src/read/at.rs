//! Ce que le modèle avait sous les yeux à un appel passé (épopée #208, T15).
//!
//! `penelope audit show` relisait les lignes d'aujourd'hui, tronquées au nombre de
//! messages de l'appel : après une compaction, ce n'était plus la requête envoyée. Le
//! journal, lui, se replie jusqu'à l'appel : la réponse du modèle (`conv.assistant`) porte
//! l'empreinte de la requête qui l'a produite (`request_hash`, la même que la ligne
//! d'`usage`), et la surface d'avant cette réponse est celle de la requête (résumés de
//! l'époque, contextes figés, messages que la compaction a masqués depuis).

use crate::derive::{Slot, derive_until, summary_message};
use crate::journal::KIND_ASSISTANT;
use crate::replay::Lineage;
use crate::store::HistoryStore;
use penelope_llm::types::ChatMessage;
use serde_json::Value;

/// Un nœud de la requête d'un appel passé.
#[derive(Debug, Clone, PartialEq)]
pub struct CallNode {
    /// Numéro de ligne du message ; 0 pour un résumé.
    pub seq: i64,
    /// Le message, sans son contexte figé.
    pub message: ChatMessage,
    /// Le contexte volatil figé avec un message utilisateur.
    pub context: Option<String>,
}

/// La requête d'un appel passé, repliée depuis le journal.
#[derive(Debug, Clone, PartialEq)]
pub struct CallView {
    pub nodes: Vec<CallNode>,
    /// Nombre de messages de la requête : système, note de fusion, nœuds, consigne de
    /// relance (à comparer au `msg_count` de la ligne d'`usage`).
    pub messages: usize,
}

impl HistoryStore {
    /// La requête de l'appel dont la réponse porte `request_hash`, repliée jusqu'à
    /// lui. `Ok(None)` : aucune réponse journalisée ne porte cette empreinte (appel
    /// antérieur au journal, en échec, ou purgé).
    pub async fn call_view(
        &self,
        session_id: &str,
        request_hash: &str,
    ) -> penelope_store::Result<Result<Option<CallView>, String>> {
        let (sid, hash) = (session_id.to_string(), request_hash.to_string());
        self.store()
            .read(move |c| {
                Ok((|| {
                    let lineage = Lineage::load(c, &sid).map_err(|e| e.to_string())?;
                    let Some(answer) = lineage.events.iter().find(|e| {
                        e.kind == KIND_ASSISTANT
                            && e.payload.get("request_hash").and_then(Value::as_str)
                                == Some(hash.as_str())
                    }) else {
                        return Ok(None);
                    };
                    let until = lineage.offset + answer.seq - 1;
                    let surface = derive_until(&lineage.prefix, &lineage.events, until)
                        .map_err(|e| e.to_string())?;
                    let seqs = lineage.row_seqs(&surface);
                    let nodes = surface
                        .nodes
                        .iter()
                        .filter_map(|slot| match *slot {
                            Slot::Summary(k) => surface.summaries.get(&k).map(|n| CallNode {
                                seq: 0,
                                message: summary_message(&n.node_id, &n.summary),
                                context: None,
                            }),
                            Slot::Message(a) => surface.messages.get(&a).map(|n| CallNode {
                                seq: seqs.get(&a).copied().unwrap_or(a),
                                message: n.message.clone(),
                                context: surface.contexts.get(&a).cloned(),
                            }),
                        })
                        .collect();
                    let messages = surface.request_messages("").len();
                    Ok(Some(CallView { nodes, messages }))
                })())
            })
            .await
    }
}
