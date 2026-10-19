//! Un run dans un process enfant (#395, lot 1) : le protocole JSONL sur stdio, le
//! superviseur côté daemon et la boucle côté enfant (`penelope run --stdio`).
//!
//! Le daemon reste superviseur : il lance l'enfant dans son propre groupe de process, lui
//! envoie `init`, lit ses lignes, et l'arrête sur `/stop` (jeton annulé), sur sa borne de
//! durée ou sur sa borne mémoire. Un enfant qui meurt rend une cause (`oom`, `signal`,
//! `exit`) au lieu de faire tomber le daemon. Ce lot pose le protocole et la supervision ;
//! le branchement des sous-agents, des workflows et des missions vient ensuite.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod serve;
mod supervisor;

pub use serve::{ChildRun, Probe, serve_stdio};
pub use supervisor::{Cause, Limits, Report, supervise};

/// Version du protocole : l'enfant la renvoie dans `ready`, un écart arrête le run.
pub const PROTOCOL: u32 = 1;

/// Ce que le superviseur envoie à l'enfant, une ligne JSON par message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToChild {
    /// Premier message : le run à faire.
    Init {
        protocol: u32,
        run_id: String,
        kind: String,
        #[serde(default)]
        payload: Value,
    },
    /// `/stop` : l'enfant s'arrête de lui-même, sinon SIGTERM puis SIGKILL du groupe.
    Interrupt,
    Heartbeat,
}

/// Ce que l'enfant renvoie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromChild {
    Ready { protocol: u32, pid: u32 },
    Progress { text: String },
    Heartbeat,
    Done { result: Value },
    Failed { error: String },
}

impl ToChild {
    pub fn init(run_id: impl Into<String>, kind: impl Into<String>, payload: Value) -> ToChild {
        ToChild::Init {
            protocol: PROTOCOL,
            run_id: run_id.into(),
            kind: kind.into(),
            payload,
        }
    }
}

/// Une ligne du protocole : le message sérialisé, sans saut de ligne interne.
pub fn line<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests;
