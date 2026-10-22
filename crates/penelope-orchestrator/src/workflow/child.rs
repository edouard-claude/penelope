//! Un sous-agent dans un process enfant (#395) : `runners.sub_agents_in_child`.
//!
//! Le daemon lance `penelope run --stdio`, lui passe la tâche ([`SubAgentJob`]) et le suit
//! ([`penelope_app::child::supervise`]) : `/stop`, borne de durée, borne de mémoire, cause
//! de la fin. L'enfant ouvre la même base et la même configuration, fait tourner
//! [`run_sub_agent`] avec ses propres providers, et relaie au daemon les appels MCP : le
//! host reste ici. Une panne de l'enfant (signal, mémoire) est relancée une fois, puis rend
//! un échec lisible ; elle ne touche jamais le daemon.

use super::{CallLimits, Context, SubAgentTask, run_sub_agent};
use penelope_app::child::{Cause, ChildCtx, Limits, ToChild, supervise};
use penelope_kernel::event::EventDraft;
use penelope_llm::CancelToken;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Le genre de run d'un sous-agent dans le protocole.
pub const KIND: &str = "sub_agent";

/// La tâche d'un sous-agent, telle qu'elle traverse le protocole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubAgentJob {
    pub session_id: String,
    pub run_id: Option<String>,
    pub kind: String,
    pub prompt: String,
    pub model_id: String,
    pub tools: Vec<String>,
    pub workspaces: Vec<PathBuf>,
    /// `(appels, tours)` d'une étape bornée.
    pub limits: Option<(u32, u32)>,
}

impl SubAgentJob {
    pub fn of(task: &SubAgentTask<'_>) -> SubAgentJob {
        SubAgentJob {
            session_id: task.session_id.into(),
            run_id: task.run_id.map(String::from),
            kind: task.kind.into(),
            prompt: task.prompt.into(),
            model_id: task.model_id.into(),
            tools: task.tools.to_vec(),
            workspaces: task.workspaces.clone(),
            limits: task.limits.as_ref().map(|l| (l.calls, l.turns)),
        }
    }
}

/// Côté enfant : le sous-agent sur le contexte que l'enfant a monté.
pub async fn run_job(
    cx: &Context,
    job: SubAgentJob,
    cancel: &CancelToken,
) -> Result<Value, String> {
    let text = run_sub_agent(
        cx,
        SubAgentTask {
            session_id: &job.session_id,
            run_id: job.run_id.as_deref(),
            kind: &job.kind,
            prompt: &job.prompt,
            model_id: &job.model_id,
            tools: &job.tools,
            workspaces: job.workspaces.clone(),
            limits: job.limits.map(|(calls, turns)| CallLimits { calls, turns }),
        },
        cancel,
    )
    .await?;
    Ok(json!({ "text": text }))
}

/// Côté enfant : la boucle sert un sous-agent, son contexte monté par `build` sur le
/// relais MCP de ce run.
pub struct SubAgentChild<F>(pub F)
where
    F: Fn(Arc<dyn penelope_executor::executor::McpGateway>) -> Context + Send + Sync;

#[async_trait::async_trait]
impl<F> penelope_app::child::ChildRun for SubAgentChild<F>
where
    F: Fn(Arc<dyn penelope_executor::executor::McpGateway>) -> Context + Send + Sync,
{
    async fn run(&self, kind: &str, payload: Value, ctx: ChildCtx) -> Result<Value, String> {
        if kind != KIND {
            return Err(format!("genre de run inconnu : {kind}"));
        }
        let job: SubAgentJob =
            serde_json::from_value(payload).map_err(|e| format!("tâche illisible : {e}"))?;
        let cx = (self.0)(Arc::new(penelope_app::child::ChildMcp(ctx.relay.clone())));
        run_job(&cx, job, &ctx.cancel).await
    }
}

/// Côté daemon : le sous-agent dans un enfant, relancé une fois s'il meurt.
pub async fn spawn_in_child(
    cx: &Context,
    task: SubAgentTask<'_>,
    cancel: &CancelToken,
) -> Result<String, String> {
    let s = &cx.services;
    let cfg = s.config.config();
    let exe = penelope_app::helpers::running_binary()?;
    let mut spec = penelope_platform::ProcessSpec::new(exe.to_string_lossy().to_string())
        .arg("run")
        .arg("--stdio");
    // La même maison que le daemon : un `PENELOPE_HOME` d'essai suit l'enfant, et la clé
    // du magasin de secrets chiffré (Linux, ou macOS sans Trousseau) aussi.
    for k in [
        "PENELOPE_HOME",
        "PENELOPE_MASTER_KEY_FILE",
        "PENELOPE_PASSPHRASE",
    ] {
        if let Ok(v) = std::env::var(k) {
            spec = spec.env(k, v);
        }
    }
    let limits = Limits {
        max_rss_kb: Some(cfg.runners.child_max_rss_mb.max(64) * 1024),
        max_duration: Duration::from_secs(cfg.runners.child_max_secs.max(10)),
        ..Limits::default()
    };
    let job = SubAgentJob::of(&task);
    let payload = serde_json::to_value(&job).map_err(|e| e.to_string())?;
    let mcp = cx.workflows.ports.mcp.get();
    let mut attempts = 0;
    loop {
        let init = ToChild::init(&job.session_id, KIND, payload.clone());
        let report = supervise(
            &s.platform.processes,
            spec.clone(),
            init,
            &limits,
            cancel,
            mcp.clone(),
        )
        .await;
        let _ = s
            .events
            .append(
                EventDraft::new(
                    "child.exited",
                    json!({"kind": KIND, "sub_agent": job.kind, "pid": report.pid,
                           "rss_max_kb": report.rss_max_kb, "attempt": attempts,
                           "cause": report.cause, "text": report.describe()}),
                )
                .session(&job.session_id),
            )
            .await;
        match &report.cause {
            Cause::Done => {
                return Ok(report
                    .result
                    .as_ref()
                    .and_then(|r| r["text"].as_str())
                    .unwrap_or_default()
                    .to_string());
            }
            Cause::Failed { error } => return Err(error.clone()),
            _ if report.retry_allowed(attempts) => {
                tracing::warn!(session = %job.session_id, cause = %report.describe(), "sous-agent enfant mort : relancé une fois");
                attempts += 1;
            }
            _ => return Err(format!("sous-agent {}", report.describe())),
        }
    }
}
