//! Moteur de workflows : il part dans `penelope-orchestrator` (épopée #208, T27) ;
//! `engine/` en est le code, qui reçoit un [`Context`] au lieu du daemon, et ce module la
//! façade qui garde jusqu'à T30 les chemins `crate::workflow::*` et leurs entrées en
//! `&Arc<Daemon>`, appelées par la passerelle, le RPC et les tests.

mod engine;
pub use engine::*;

use crate::bus::Origin;
use crate::runtime::Daemon;
use penelope_workflow::{Control, Run, RunState};
use serde_json::Value;
use std::sync::Arc;

/// Le contexte de l'orchestrateur sur le daemon : ses services, ses providers, l'état de
/// ses runs, les ports de la boucle sur ses modules (`agent::services_of`), et le daemon
/// lui-même comme `Admin` de l'exécuteur des étapes.
pub fn context_of(d: &Arc<Daemon>) -> Context {
    Context {
        services: d.services.clone(),
        providers: d.providers.clone(),
        handle: d.handle.clone(),
        bus: d.bus.clone(),
        workflows: d.workflows.clone(),
        embeddings: d.embeddings.clone(),
        agent: crate::agent::services_of(&d.services),
        admin: Some(d.clone()),
    }
}

pub async fn start_run(
    d: &Arc<Daemon>,
    workflow_id: &str,
    params: Value,
    origin: &Origin,
    parent: Option<&str>,
    depth: u32,
) -> Result<Run, String> {
    engine::start_run(&context_of(d), workflow_id, params, origin, parent, depth).await
}

pub async fn start_run_briefed(
    d: &Arc<Daemon>,
    workflow_id: &str,
    params: Value,
    origin: &Origin,
    parent: Option<&str>,
    depth: u32,
    brief: Option<&str>,
) -> Result<Run, String> {
    let cx = context_of(d);
    engine::start_run_briefed(&cx, workflow_id, params, origin, parent, depth, brief).await
}

pub async fn control(d: &Arc<Daemon>, run_id: &str, op: &Control) -> anyhow::Result<RunState> {
    engine::control(&context_of(d), run_id, op).await
}

pub async fn answer(
    d: &Arc<Daemon>,
    run_id: &str,
    visit: &str,
    choice: &str,
    input: Option<&str>,
) -> anyhow::Result<()> {
    engine::answer(&context_of(d), run_id, visit, choice, input).await
}

pub async fn form_of(d: &Daemon, run_id: &str, visit: &str) -> Option<Value> {
    engine::form_of(&d.services, run_id, visit).await
}

pub async fn origin_of(d: &Daemon, run_id: &str) -> Origin {
    engine::origin_of(&d.services, run_id).await
}

pub async fn raise_budget(
    d: &Arc<Daemon>,
    run_id: &str,
    usd: Option<f64>,
    tokens: Option<u64>,
) -> anyhow::Result<Value> {
    engine::raise_budget(&context_of(d), run_id, usd, tokens).await
}

pub async fn drive(d: &Arc<Daemon>, run_id: &str) -> anyhow::Result<RunState> {
    engine::drive(&context_of(d), run_id).await
}

pub async fn drive_all(d: &Arc<Daemon>) -> anyhow::Result<usize> {
    engine::drive_all(&context_of(d)).await
}

pub async fn driver_loop(d: Arc<Daemon>) {
    engine::driver_loop(context_of(&d)).await
}

mod compat;
pub use compat::WorkflowOrchestrator;
