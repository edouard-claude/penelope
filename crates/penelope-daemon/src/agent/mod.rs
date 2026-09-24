//! Boucle d'agent : un tour, de bout en bout (§3.3, §4.2, §9).
//!
//! Invariants :
//! - tout effet non `readOnly` est **planifié dans le ledger avant** exécution ;
//! - un outil qui exige une approbation suspend **ce tour seulement**, et l'appel reste
//!   dans le transcript : la reprise le retrouve par son identifiant ;
//! - le détecteur de boucles arrête le tour plutôt que de laisser tourner ;
//! - une erreur d'outil est renvoyée au modèle, pas au harnais.
//!
//! Une itération commence **toujours** par résoudre les appels d'outils en attente à la
//! fin du transcript, puis appelle le modèle. Un premier passage et une reprise après
//! approbation suivent donc exactement le même chemin.

use crate::runtime::Services;
use penelope_context::journal::Provenance;
use penelope_hitl::{ApprovalKind, ApprovalState, Decision};
use penelope_kernel::effects::{EffectKind, EffectSpec, Planned};
use penelope_kernel::event::EventDraft;
use penelope_kernel::risk::{PolicyDecision, PolicyWindow, RiskClass};
use penelope_llm::provider::{CancelToken, Provider, collect_stream_observed};
use penelope_llm::types::*;
use penelope_tools::{LoopDetector, LoopVerdict, ToolOutcome};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

mod attempts;
mod conversation;
mod decisions;
mod executor;
mod guards;
mod loop_abort;
mod model;
mod pending;
mod pipeline;
mod rules;
mod spec;
mod turn;
mod turn_log;

use attempts::Attempts;
pub use attempts::{EMPTY_RETRY_PROMPT, MAX_ATTEMPTS_PER_TURN};
pub use conversation::{Compactor, Conversation, MemoryConversation};
pub use decisions::{EFFECT_DONE, EFFECT_IGNORE, EFFECT_RETRY, decide_approval};
pub use executor::{CallInfo, ToolExecutor};
pub use guards::budget_exceeded_text;
use guards::{TurnContext, default_chain, run_guards};
pub use loop_abort::{LOOP_STOP_NOTE, last_result_of, split_choices};
#[cfg(test)]
use model::fit_modalities;
pub use pending::pending_calls;
pub use penelope_app::outcome::{NullSink, RecordingSink, TurnEvent, TurnOutcome, TurnSink};
use pipeline::Pending;
pub(crate) use pipeline::{effect_kind, server_of, without_intention};
pub use rules::{MAX_FAMILIES_PER_CLICK, always_creates_no_rule};
pub(crate) use rules::{arg_pattern, arg_patterns};
pub use spec::{AgentLoop, CALLS_EXHAUSTED, TURN_CALLS, TurnRequest, TurnSpec};
pub use turn_log::{TurnMeta, close_interrupted_turns, close_unopened};

#[cfg(test)]
mod clone_policy_tests;

#[cfg(test)]
mod tests;
