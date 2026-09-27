//! Moteur de workflows (§12.7) : chaque run avance étape par étape, durablement.
//!
//! Un pilote par run actif exécute l'étape courante, choisit la transition, puis
//! `RunStore::advance` enregistre résultat et étape suivante dans une même transaction :
//! un arrêt brutal reprend à l'étape courante. Les effets (shell, outils) passent par le
//! ledger : rejoués, jamais ré-exécutés. Une étape qui attend (question, approbation,
//! délai, sous-workflow) rend la main ; le pilote repasse quand l'attente peut finir.
//!
//! Les marqueurs d'une visite d'étape sont rangés en `kv` sous
//! `wf.<quoi>.<run>.<étape>.<itération>` : une boucle qui revient sur une étape repart
//! d'une page blanche, une reprise après crash retrouve la sienne.

use penelope_agent::{AgentLoop, MemoryConversation, NullSink, TurnOutcome, TurnSpec};
use penelope_app::bus::Origin;
use penelope_app::helpers::step_done_key;
use penelope_app::ports::Slot;
use penelope_app::services::Services;
use penelope_conversation::SessionConversation;
use penelope_executor::executor::{NativeToolExecutor, ToolEnv};
use penelope_hitl::{ApprovalKind, ApprovalState};
use penelope_kernel::effects::{EffectSpec, Planned};
use penelope_kernel::event::EventDraft;
use penelope_kernel::risk::PolicyDecision;
use penelope_kernel::session::{MetadataOp, SessionKind};
use penelope_llm::provider::CancelToken;
use penelope_llm::types::ChatMessage;
use penelope_workflow::conditions::{EvalContext, TemplateVars, choose};
use penelope_workflow::model::{BLOCKED, DONE, Step, StepResult, Workflow};
use penelope_workflow::runs::{Admission, Limit, check_limits};
use penelope_workflow::{Run, RunState};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Délai entre deux passages sur les runs qui attendent.
pub const POLL: Duration = Duration::from_secs(5);
/// Relances d'une étape `agent` qui s'arrête sans `step_done()`.
const MAX_NUDGES: u32 = 3;
/// Profondeur maximale des sous-workflows (§12.3).
const MAX_DEPTH: u32 = 3;
/// Tentatives d'un sous-agent dont la sortie doit suivre un schéma (§12.3).
const SCHEMA_ATTEMPTS: u32 = 2;
/// Délai par défaut d'une étape `shell`.
const SHELL_TIMEOUT: Duration = Duration::from_secs(600);

/// État partagé : runs pilotés par ce processus, jetons d'arrêt, réveil.
#[derive(Default)]
pub struct State {
    driving: Mutex<HashMap<String, CancelToken>>,
    wake: tokio::sync::Notify,
    /// Branchements reçus de la composition, lus au moment de s'en servir.
    pub ports: Ports,
}

/// Ce que le moteur de workflows reçoit au lieu des branchements du daemon : canal du
/// propriétaire, passerelle et superviseur MCP, orchestrateur des sous-agents.
#[derive(Clone, Default)]
pub struct Ports {
    pub messenger: Slot<dyn penelope_executor::executor::Messenger>,
    pub mcp: Slot<dyn penelope_executor::executor::McpGateway>,
    pub orchestrator: Slot<dyn penelope_executor::executor::Orchestrator>,
    pub mcp_supervisor: Slot<dyn penelope_app::ports::McpAdmin>,
}

impl State {
    pub fn with_ports(ports: Ports) -> State {
        State {
            ports,
            ..State::default()
        }
    }

    /// Réveille le pilote : un run vient de démarrer ou une attente peut finir.
    pub fn wake(&self) {
        self.wake.notify_waiters();
    }

    fn claim(&self, run_id: &str) -> Option<CancelToken> {
        let mut g = self.driving.lock().ok()?;
        if g.contains_key(run_id) {
            return None;
        }
        let token = CancelToken::new();
        g.insert(run_id.to_string(), token.clone());
        Some(token)
    }

    fn release(&self, run_id: &str) {
        if let Ok(mut g) = self.driving.lock() {
            g.remove(run_id);
        }
    }

    /// Interrompt le tour d'agent en cours d'un run (pause, annulation).
    pub fn interrupt(&self, run_id: &str) {
        if let Ok(g) = self.driving.lock()
            && let Some(t) = g.get(run_id)
        {
            t.cancel();
        }
    }

    pub fn is_driving(&self, run_id: &str) -> bool {
        self.driving
            .lock()
            .map(|g| g.contains_key(run_id))
            .unwrap_or(false)
    }
}

/// Issue de l'exécution d'une étape.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    Done {
        result: StepResult,
        output: Value,
    },
    /// L'étape attend : réponse, approbation, délai, sous-workflow.
    Waiting(String),
}

fn done(result: StepResult, output: Value) -> StepOutcome {
    StepOutcome::Done { result, output }
}

// ------------------------------------------------------------------ kv

async fn kv_delete_prefix(s: &Services, prefix: &str) -> anyhow::Result<()> {
    let p = format!("{prefix}%");
    s.store
        .write(move |tx| {
            tx.execute("DELETE FROM kv WHERE k LIKE ?1", [p])?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Clé d'un marqueur de la visite courante d'une étape.
fn visit_key(what: &str, run: &Run, step_id: &str) -> String {
    format!("wf.{what}.{}.{step_id}.{}", run.id, run.iterations)
}

fn origin_key(run_id: &str) -> String {
    format!("wf.origin.{run_id}")
}

/// Canal du run : là où partent questions, approbations et carte de progression.
pub async fn origin_of(s: &Services, run_id: &str) -> Origin {
    match s.kv_get(&origin_key(run_id)).await.ok().flatten() {
        Some(raw) => {
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            Origin::from_payload(&json!({ "origin": v }))
        }
        None => penelope_app::helpers::owner_origin_of(s),
    }
}

/// Longueur maximale d'un brief, en caractères.
pub const BRIEF_CHARS: usize = 4_000;
/// Part du brief affichée sur la carte de progression.
const BRIEF_CARD_CHARS: usize = 280;

fn brief_key(run_id: &str) -> String {
    format!("wf.brief.{run_id}")
}

/// Résumé de la conversation qui a lancé le run (issue #35) ; vide sinon.
pub async fn brief_of(s: &Services, run_id: &str) -> String {
    s.kv_get(&brief_key(run_id))
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Le brief précède la consigne de la première étape `agent` ou `sub_agent` visitée, sauf
/// si elle l'emploie déjà (`{{brief}}`). Rend la consigne à envoyer.
async fn with_brief(ctx: &StepCtx<'_>, prompt: String) -> String {
    let s = ctx.s();
    let brief = brief_of(s, &ctx.run.id).await;
    if brief.is_empty() {
        return prompt;
    }
    let key = format!("wf.brief_step.{}", ctx.run.id);
    let first = match s.kv_get(&key).await.ok().flatten() {
        Some(step) => step == ctx.step.id,
        None => {
            let _ = s.kv_set(&key, &ctx.step.id).await;
            true
        }
    };
    if !first || ctx.step.prompt.contains("{{brief}}") {
        return prompt;
    }
    format!("Brief de la conversation qui a lancé ce run :\n{brief}\n\n{prompt}")
}

mod context;
mod control;
mod driver;
mod orchestrator;
mod start;
mod step_agent;
mod step_compose;
mod step_ctx;
mod step_shell_tool;
mod step_user;
mod step_verify;

pub use context::Context;
pub use control::{answer, control, form_of};
pub use driver::{drive, drive_all, driver_loop, effective_budget, raise_budget};
use driver::{finish, limit_reason, refresh_spent, session_metadata};
pub use orchestrator::WorkflowOrchestrator;
use orchestrator::progress;
use start::first_verdict;
pub use start::{resolve_params, start_run, start_run_briefed};
pub use step_agent::{SubAgentTask, run_sub_agent};
use step_agent::{agent_step, extract_json, send_approval_once, sub_agent_step};
use step_compose::{parallel_step, wait_step, workflow_step};
use step_ctx::{StepCtx, execute_step};
use step_shell_tool::{shell_step, tool_step};
use step_user::user_step;
use step_verify::verify_step;
#[cfg(test)]
use step_verify::{
    classify_project_test, evidence_matches_head, project_test_spec, repository_rules,
    requires_current_sha, verifier_prompt,
};

#[cfg(test)]
pub(crate) mod harness;
#[cfg(test)]
mod tests;
