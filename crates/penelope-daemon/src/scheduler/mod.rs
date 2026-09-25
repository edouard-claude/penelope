//! Ordonnanceur : parti dans `penelope-orchestrator` (épopée #208, T27), où il reçoit le
//! contexte de l'orchestrateur au lieu du daemon. Ce module en est la façade : il garde
//! jusqu'à T30 les chemins `crate::scheduler::*` et leurs entrées en `&Arc<Daemon>`,
//! appelées par le coureur, le RPC et la passerelle.

pub use penelope_orchestrator::scheduler::*;

use penelope_orchestrator::scheduler as inner;

use crate::runtime::Daemon;
use crate::workflow::context_of;
use serde_json::Value;
use std::sync::Arc;

pub async fn scheduler_loop(d: Arc<Daemon>, ports: Ports) {
    inner::scheduler_loop(context_of(&d), ports).await
}

pub async fn tick(d: &Arc<Daemon>, ports: &Ports) -> anyhow::Result<TickReport> {
    inner::tick(&context_of(d), ports).await
}

pub async fn run_now(d: &Arc<Daemon>, ports: &Ports, id: &str) -> anyhow::Result<Value> {
    inner::run_now(&context_of(d), ports, id).await
}

pub async fn trigger_outcome_of(
    d: &Arc<Daemon>,
    ports: &Ports,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
    turn: &penelope_kernel::turn::Turn,
) {
    inner::trigger_outcome_of(&context_of(d), ports, schedule_id, outcome, turn).await
}

pub async fn trigger_outcome(
    d: &Arc<Daemon>,
    ports: &Ports,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
) {
    inner::trigger_outcome(&context_of(d), ports, schedule_id, outcome).await
}

pub async fn final_already_sent(d: &Daemon, session_id: &str, final_text: &str) -> bool {
    inner::final_already_sent(&d.services, session_id, final_text).await
}
