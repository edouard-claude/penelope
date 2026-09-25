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
//!
//! La crate ne dépend que du noyau, des crates métier de la boucle et des ports de
//! `penelope-app` ; jamais de `penelope-context`, `penelope-memory`, `penelope-telegram`
//! (`design/v1/README.md` §3.2), ni directement ni par les types des ports : le
//! vocabulaire du journal qu'elle écrit est dans `penelope_kernel::journal`. Le daemon la
//! compose par sa façade `agent.rs`.

#![forbid(unsafe_code)]

use penelope_hitl::{ApprovalKind, ApprovalState, Decision};
use penelope_kernel::effects::{EffectKind, EffectSpec, Planned};
use penelope_kernel::journal::Provenance;
use penelope_kernel::risk::{PolicyDecision, PolicyWindow, RiskClass};
use penelope_llm::provider::{CancelToken, Provider, collect_stream_observed};
use penelope_llm::types::*;
use penelope_tools::{LoopDetector, LoopVerdict, ToolOutcome};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

mod attempts;
mod decisions;
mod events;
mod guards;
mod loop_abort;
mod model;
mod pending;
mod pipeline;
mod ports;
mod rules;
mod spec;
mod steering;
mod turn;
mod turn_log;

use attempts::Attempts;
pub use attempts::{EMPTY_RETRY_PROMPT, MAX_ATTEMPTS_PER_TURN};
pub use decisions::{EFFECT_DONE, EFFECT_IGNORE, EFFECT_RETRY, decide_approval};
pub use events::TurnEventKind;
pub use guards::budget_exceeded_text;
use guards::{TurnContext, default_chain, run_guards};
pub use loop_abort::{LOOP_STOP_NOTE, last_result_of, split_choices};
#[cfg(test)]
use model::fit_modalities;
pub use pending::pending_calls;
pub use penelope_app::conversation::{Compactor, Conversation, MemoryConversation};
pub use penelope_app::outcome::{NullSink, RecordingSink, TurnEvent, TurnOutcome, TurnSink};
pub use penelope_app::tool_executor::{CallInfo, ToolExecutor};
pub use penelope_llm::cache::{
    CACHE_TTL_MS, Fingerprint, Observed, PreviousCall, STICKY_MS, miss_cause, sticky_upstream,
};
pub use penelope_tools::args::{
    call_arguments, effective_arguments, wants_network, without_intention,
};
use pipeline::Pending;
pub use pipeline::effect_kind;
pub use pipeline::server_of;
pub use pipeline::{ApprovalMode, declared_allow, local_draft_allow};
pub use pipeline::{CallContext, CallId};
pub use ports::{
    AgentServices, CacheAudit, JobRequest, JobRunner, MemoryModes, NoAudit, NoJobs,
    PromptSnapshots, SessionInfo, SessionModes,
};
pub use rules::{MAX_FAMILIES_PER_CLICK, always_creates_no_rule};
pub use rules::{arg_pattern, arg_patterns};
pub use spec::{AgentLoop, CALLS_EXHAUSTED, TURN_CALLS, TurnSpec};
use steering::Steering;
pub use steering::{
    Checkpoint, INTERRUPTED_NOTE, Inbox, Injection, MERGE_NOTE, NOT_RUN_NEW_MESSAGE,
    NOT_RUN_STOPPED, Steer, interruption_note,
};
pub use turn_log::{TurnMeta, close_interrupted_turns, close_unopened};

#[cfg(test)]
mod clone_policy_tests;

#[cfg(test)]
mod tests;
