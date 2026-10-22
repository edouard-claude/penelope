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

pub mod registry;
mod relay;
mod serve;
mod socket;
mod supervisor;

pub use relay::{ChildMcp, Relay};
pub use serve::{ChildCtx, ChildRun, Probe, serve_io, serve_stdio};
pub use socket::{read_result, result_path, serve_socket, supervise_socket};
pub use supervisor::{Cause, Limits, Report, supervise};

/// Version du protocole : l'enfant la renvoie dans `ready`, un écart arrête le run.
/// 2 : version du binaire dans `init`, appels MCP relayés au daemon (#395).
pub const PROTOCOL: u32 = 2;

/// La version du binaire : un enfant d'une autre version refuse le run (il migrerait la
/// base sous le daemon, ou parlerait un autre schéma).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Ce que le superviseur envoie à l'enfant, une ligne JSON par message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToChild {
    /// Premier message : le run à faire.
    Init {
        protocol: u32,
        /// Version du binaire du superviseur ; vide : non contrôlée (essais).
        #[serde(default)]
        version: String,
        run_id: String,
        kind: String,
        #[serde(default)]
        payload: Value,
    },
    /// Un superviseur se rattache à un enfant déjà lancé (redémarrage du daemon) : pas de
    /// nouvel `init`, l'enfant renvoie ce qui attend une réponse.
    Resume,
    /// `/stop` : l'enfant s'arrête de lui-même, sinon SIGTERM puis SIGKILL du groupe.
    Interrupt,
    Heartbeat,
    /// Réponse à un [`FromChild::McpCall`].
    McpResult {
        id: u64,
        #[serde(default)]
        ok: Option<Value>,
        #[serde(default)]
        error: Option<String>,
    },
}

/// Ce que l'enfant renvoie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromChild {
    Ready {
        protocol: u32,
        pid: u32,
    },
    Progress {
        text: String,
    },
    Heartbeat,
    Done {
        result: Value,
    },
    Failed {
        error: String,
    },
    /// Un appel au host MCP, qui reste dans le daemon : `call_tool`, `tool_policy` ou
    /// `read_resource`.
    McpCall {
        id: u64,
        op: String,
        qualified: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        origin: Option<Value>,
    },
}

impl ToChild {
    pub fn init(run_id: impl Into<String>, kind: impl Into<String>, payload: Value) -> ToChild {
        ToChild::Init {
            protocol: PROTOCOL,
            version: VERSION.into(),
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
