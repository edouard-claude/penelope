//! Vocabulaire pur du journal que la boucle d'agent écrit (épopée #208, lot K).
//!
//! Des charges sérialisables sans type de `penelope-llm` ni de `penelope-context` : la
//! boucle les écrit par le noyau, `penelope-context` les relit et les plie, et les
//! réexporte à l'identique sous `penelope_context::journal`.

mod attempt;
mod call;
mod turn;

pub use attempt::*;
pub use call::*;
pub use turn::*;

use serde_json::Value;

/// Événements d'exécution qui bornent un tour ; le pliage les lit sans les versionner.
pub const KIND_TURN_STARTED: &str = "turn.started";
pub const KIND_TURN_FINISHED: &str = "turn.finished";

/// Vrai pour un payload effacé par la purge (`{"purged":true}`, `event.rs`).
pub fn is_purged(payload: &Value) -> bool {
    payload.get("purged").and_then(Value::as_bool) == Some(true)
}
