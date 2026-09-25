//! Ordonnanceur : il part dans `penelope-orchestrator` (épopée #208, T27) ; `engine/`
//! en est le code, qui reçoit le contexte de l'orchestrateur au lieu du daemon, et ce
//! module la façade qui garde jusqu'à T30 les chemins `crate::scheduler::*` et leurs
//! entrées en `&Arc<Daemon>`, appelées par le coureur, le RPC et la passerelle.

mod engine;
pub use engine::*;

use crate::runtime::Daemon;
use crate::workflow::context_of;
use serde_json::Value;
use std::sync::Arc;

pub async fn scheduler_loop(d: Arc<Daemon>, ports: Ports) {
    engine::scheduler_loop(context_of(&d), ports).await
}

pub async fn tick(d: &Arc<Daemon>, ports: &Ports) -> anyhow::Result<TickReport> {
    engine::tick(&context_of(d), ports).await
}

pub async fn run_now(d: &Arc<Daemon>, ports: &Ports, id: &str) -> anyhow::Result<Value> {
    engine::run_now(&context_of(d), ports, id).await
}

pub async fn trigger_outcome_of(
    d: &Arc<Daemon>,
    ports: &Ports,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
    turn: &penelope_kernel::turn::Turn,
) {
    engine::trigger_outcome_of(&context_of(d), ports, schedule_id, outcome, turn).await
}

pub async fn trigger_outcome(
    d: &Arc<Daemon>,
    ports: &Ports,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
) {
    engine::trigger_outcome(&context_of(d), ports, schedule_id, outcome).await
}

pub async fn final_already_sent(d: &Daemon, session_id: &str, final_text: &str) -> bool {
    engine::final_already_sent(&d.services, session_id, final_text).await
}
