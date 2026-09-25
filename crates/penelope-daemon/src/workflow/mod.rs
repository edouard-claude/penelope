//! Moteur de workflows : parti dans `penelope-orchestrator` (épopée #208, T27), où il
//! reçoit un [`Context`] au lieu du daemon. Ce module en est la façade : il garde jusqu'à
//! T30 les chemins `crate::workflow::*` et leurs entrées en `&Arc<Daemon>`, appelées par
//! la passerelle, le RPC et les tests.

pub use penelope_orchestrator::workflow::*;

use penelope_orchestrator::workflow as inner;

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

/// L'orchestrateur offert aux outils et à l'ordonnanceur, sur le contexte du daemon.
pub fn orchestrator_of(d: &Arc<Daemon>) -> WorkflowOrchestrator {
    WorkflowOrchestrator {
        context: context_of(d),
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
    inner::start_run(&context_of(d), workflow_id, params, origin, parent, depth).await
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
    inner::start_run_briefed(&cx, workflow_id, params, origin, parent, depth, brief).await
}

pub async fn control(d: &Arc<Daemon>, run_id: &str, op: &Control) -> anyhow::Result<RunState> {
    inner::control(&context_of(d), run_id, op).await
}

pub async fn answer(
    d: &Arc<Daemon>,
    run_id: &str,
    visit: &str,
    choice: &str,
    input: Option<&str>,
) -> anyhow::Result<()> {
    inner::answer(&context_of(d), run_id, visit, choice, input).await
}

pub async fn form_of(d: &Daemon, run_id: &str, visit: &str) -> Option<Value> {
    inner::form_of(&d.services, run_id, visit).await
}

pub async fn origin_of(d: &Daemon, run_id: &str) -> Origin {
    inner::origin_of(&d.services, run_id).await
}

pub async fn raise_budget(
    d: &Arc<Daemon>,
    run_id: &str,
    usd: Option<f64>,
    tokens: Option<u64>,
) -> anyhow::Result<Value> {
    inner::raise_budget(&context_of(d), run_id, usd, tokens).await
}

pub async fn drive(d: &Arc<Daemon>, run_id: &str) -> anyhow::Result<RunState> {
    inner::drive(&context_of(d), run_id).await
}

pub async fn drive_all(d: &Arc<Daemon>) -> anyhow::Result<usize> {
    inner::drive_all(&context_of(d)).await
}

pub async fn driver_loop(d: Arc<Daemon>) {
    inner::driver_loop(context_of(&d)).await
}
