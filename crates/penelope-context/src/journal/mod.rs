//! Vocabulaire des événements de contenu `conv.*` (épopée #208, tâche T1).
//!
//! Chaque contenu vu par le modèle devient un événement du journal `events`, avec un
//! numéro de format `v` et une opération de surface (`design/v1/source-de-verite.md`
//! §2.2 et §2.5). Ce module nomme les kinds, type les payloads et les relit ; le pliage
//! qui en tire l'historique est dans [`crate::derive`].
//!
//! Relire est strict : un `v` que ce code ne connaît pas, un kind `conv.*` inconnu non
//! marqué `"ignorable": true`, une opération de surface qui ne va pas avec son kind sont
//! des erreurs. Le journal est haché et ne se réécrit pas : deviner serait mentir sur
//! ce que le modèle a lu.

mod build;
mod payload;
#[cfg(test)]
mod tests;
mod verbatim;

pub use build::*;
pub use payload::*;
pub use penelope_kernel::journal::{
    AttemptCause, AttemptPayload, CallRecord, KIND_TURN_FINISHED, KIND_TURN_STARTED, Provenance,
    TokenUsage, TurnCall, TurnEnd, TurnIdentity, TurnReason, UserSource, finished_payload,
    interrupted_payload, is_purged, started_payload,
};
pub use verbatim::{restore_verbatim, verbatim_of};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Version du format des payloads `conv.*` écrite par ce code.
pub const FORMAT_VERSION: u64 = 1;

/// Préfixe commun des kinds de contenu.
pub const KIND_PREFIX: &str = "conv.";
pub const KIND_SYSTEM: &str = "conv.system";
pub const KIND_USER: &str = "conv.user";
pub const KIND_CONTEXT: &str = "conv.context";
pub const KIND_ASSISTANT: &str = "conv.assistant";
pub const KIND_TOOL_RESULT: &str = "conv.tool_result";
pub const KIND_ATTEMPT: &str = "conv.attempt";
pub const KIND_SUMMARY: &str = "conv.summary";
pub const KIND_REWIND: &str = "conv.rewind";
pub const KIND_FORK: &str = "conv.fork";
pub const KIND_IMPORT: &str = "conv.import";

/// Opération d'un événement sur la surface (la liste des nœuds vus par le modèle).
///
/// Les adresses sont des `seq` dérivés (`offset + events.seq`, §2.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SurfaceOp {
    /// Ajoute un nœud en fin de surface.
    #[default]
    Append,
    /// Remplace la plage `[from, to]` de la surface (résumé, niveau 1, nouveau système).
    Replace { from: i64, to: i64 },
    /// Coupe la surface après le nœud `after` (retour arrière).
    Cut { after: i64 },
    /// Hérite de la surface de `parent` jusqu'à `up_to` (fork par référence).
    Inherit {
        parent: String,
        up_to: i64,
        offset: i64,
    },
    /// Scelle l'historique V0 de `messages` lignes (§4.5).
    Seal { messages: i64, offset: i64 },
}

/// Un événement de contenu relu et vérifié.
#[derive(Debug, Clone, PartialEq)]
pub enum ConvEvent {
    System(SystemPayload),
    User(UserPayload),
    Context(ContextPayload),
    /// En boîte : le plus gros payload, de loin.
    Assistant(Box<AssistantPayload>),
    ToolResult(ToolResultPayload),
    Attempt(AttemptPayload),
    Summary(SummaryPayload),
    Rewind(RewindPayload),
    Fork(ForkPayload),
    Import(ImportPayload),
}

/// Ce que le pliage refuse. Jamais de silence : un journal incohérent se dit.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DeriveError {
    #[error("{kind} : format v{found} inconnu (ce code lit jusqu'à v{known})")]
    Format {
        kind: String,
        found: u64,
        known: u64,
    },
    #[error("{kind} : kind de contenu inconnu, non marqué ignorable")]
    UnknownKind { kind: String },
    #[error("{kind} : payload illisible : {reason}")]
    Payload { kind: String, reason: String },
    #[error("événement {seq} ({kind}) : {reason}")]
    Surface {
        seq: i64,
        kind: String,
        reason: String,
    },
}

/// Monte un payload de la version `v` à [`FORMAT_VERSION`], **à la lecture** : le
/// journal n'est jamais réécrit (§2.5). Les montées se chaînent v1→v2→v3 ; il n'y en a
/// pas encore, la v1 passe telle quelle.
pub fn upgrade_payload(kind: &str, v: u64, payload: Value) -> Result<Value, DeriveError> {
    match v {
        FORMAT_VERSION => Ok(payload),
        found => Err(DeriveError::Format {
            kind: kind.to_string(),
            found,
            known: FORMAT_VERSION,
        }),
    }
}

impl ConvEvent {
    /// Kind de l'événement.
    pub fn kind(&self) -> &'static str {
        match self {
            ConvEvent::System(_) => KIND_SYSTEM,
            ConvEvent::User(_) => KIND_USER,
            ConvEvent::Context(_) => KIND_CONTEXT,
            ConvEvent::Assistant(_) => KIND_ASSISTANT,
            ConvEvent::ToolResult(_) => KIND_TOOL_RESULT,
            ConvEvent::Attempt(_) => KIND_ATTEMPT,
            ConvEvent::Summary(_) => KIND_SUMMARY,
            ConvEvent::Rewind(_) => KIND_REWIND,
            ConvEvent::Fork(_) => KIND_FORK,
            ConvEvent::Import(_) => KIND_IMPORT,
        }
    }

    /// Payload à écrire dans le journal, avec `"v"`.
    pub fn payload(&self) -> Value {
        let body = match self {
            ConvEvent::System(p) => serde_json::to_value(p),
            ConvEvent::User(p) => serde_json::to_value(p),
            ConvEvent::Context(p) => serde_json::to_value(p),
            ConvEvent::Assistant(p) => serde_json::to_value(p),
            ConvEvent::ToolResult(p) => serde_json::to_value(p),
            ConvEvent::Attempt(p) => serde_json::to_value(p),
            ConvEvent::Summary(p) => serde_json::to_value(p),
            ConvEvent::Rewind(p) => serde_json::to_value(p),
            ConvEvent::Fork(p) => serde_json::to_value(p),
            ConvEvent::Import(p) => serde_json::to_value(p),
        };
        // Des structures à clés textuelles : la sérialisation ne peut pas échouer (un
        // flottant non fini devient `null`).
        let mut v = body.unwrap_or_else(|_| Value::Object(Default::default()));
        if let Value::Object(map) = &mut v {
            map.insert("v".into(), Value::from(FORMAT_VERSION));
        }
        v
    }

    /// Relit un événement du journal.
    ///
    /// `Ok(None)` : l'événement n'entre pas dans le pliage (kind d'observation, payload
    /// purgé, kind `conv.*` inconnu marqué `"ignorable": true`).
    pub fn decode(kind: &str, payload: &Value) -> Result<Option<ConvEvent>, DeriveError> {
        if !kind.starts_with(KIND_PREFIX) || is_purged(payload) {
            return Ok(None);
        }
        let known = [
            KIND_SYSTEM,
            KIND_USER,
            KIND_CONTEXT,
            KIND_ASSISTANT,
            KIND_TOOL_RESULT,
            KIND_ATTEMPT,
            KIND_SUMMARY,
            KIND_REWIND,
            KIND_FORK,
            KIND_IMPORT,
        ];
        if !known.contains(&kind) {
            if payload.get("ignorable").and_then(Value::as_bool) == Some(true) {
                return Ok(None);
            }
            return Err(DeriveError::UnknownKind {
                kind: kind.to_string(),
            });
        }
        let v = payload.get("v").and_then(Value::as_u64).unwrap_or(0);
        let mut body = upgrade_payload(kind, v, payload.clone())?;
        if let Value::Object(map) = &mut body {
            map.remove("v");
        }
        let bad = |e: serde_json::Error| DeriveError::Payload {
            kind: kind.to_string(),
            reason: e.to_string(),
        };
        let event = match kind {
            KIND_SYSTEM => ConvEvent::System(serde_json::from_value(body).map_err(bad)?),
            KIND_USER => ConvEvent::User(serde_json::from_value(body).map_err(bad)?),
            KIND_CONTEXT => ConvEvent::Context(serde_json::from_value(body).map_err(bad)?),
            KIND_ASSISTANT => ConvEvent::Assistant(serde_json::from_value(body).map_err(bad)?),
            KIND_TOOL_RESULT => ConvEvent::ToolResult(serde_json::from_value(body).map_err(bad)?),
            KIND_ATTEMPT => ConvEvent::Attempt(serde_json::from_value(body).map_err(bad)?),
            KIND_SUMMARY => ConvEvent::Summary(serde_json::from_value(body).map_err(bad)?),
            KIND_REWIND => ConvEvent::Rewind(serde_json::from_value(body).map_err(bad)?),
            KIND_FORK => ConvEvent::Fork(serde_json::from_value(body).map_err(bad)?),
            _ => ConvEvent::Import(serde_json::from_value(body).map_err(bad)?),
        };
        event.check_surface()?;
        Ok(Some(event))
    }

    /// L'opération de surface, si le kind en porte une.
    pub fn surface(&self) -> Option<&SurfaceOp> {
        match self {
            ConvEvent::System(p) => Some(&p.surface),
            ConvEvent::User(p) => Some(&p.surface),
            ConvEvent::Assistant(p) => Some(&p.surface),
            ConvEvent::ToolResult(p) => Some(&p.surface),
            ConvEvent::Summary(p) => Some(&p.surface),
            ConvEvent::Rewind(p) => Some(&p.surface),
            ConvEvent::Fork(p) => Some(&p.surface),
            ConvEvent::Import(p) => Some(&p.surface),
            ConvEvent::Context(_) | ConvEvent::Attempt(_) => None,
        }
    }

    /// Chaque kind n'admet que ses opérations (§2.2, colonne « Surface »).
    fn check_surface(&self) -> Result<(), DeriveError> {
        let fits = match (self, self.surface()) {
            (_, None) => true,
            (ConvEvent::System(_), Some(op)) => {
                matches!(op, SurfaceOp::Append | SurfaceOp::Replace { .. })
            }
            (ConvEvent::User(_) | ConvEvent::Assistant(_), Some(op)) => *op == SurfaceOp::Append,
            (ConvEvent::ToolResult(_), Some(op)) => match op {
                SurfaceOp::Append => true,
                SurfaceOp::Replace { from, to } => from == to,
                _ => false,
            },
            (ConvEvent::Summary(_), Some(op)) => {
                matches!(op, SurfaceOp::Replace { from, to } if from <= to)
            }
            (ConvEvent::Rewind(_), Some(op)) => matches!(op, SurfaceOp::Cut { .. }),
            (ConvEvent::Fork(p), Some(op)) => {
                *op == SurfaceOp::Inherit {
                    parent: p.parent.clone(),
                    up_to: p.up_to,
                    offset: p.offset,
                }
            }
            (ConvEvent::Import(p), Some(op)) => {
                matches!(op, SurfaceOp::Seal { messages, .. } if *messages == p.messages)
            }
            (ConvEvent::Context(_) | ConvEvent::Attempt(_), Some(_)) => false,
        };
        if fits {
            Ok(())
        } else {
            Err(DeriveError::Payload {
                kind: self.kind().to_string(),
                reason: format!(
                    "opération de surface refusée pour ce kind : {:?}",
                    self.surface()
                ),
            })
        }
    }
}
