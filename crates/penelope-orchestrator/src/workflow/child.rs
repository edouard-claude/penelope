//! Un sous-agent dans un process enfant (#395) : `runners.sub_agents_in_child`.
//!
//! Le daemon lance `penelope run --stdio`, lui passe la tâche ([`SubAgentJob`]) et le suit
//! ([`penelope_app::child::supervise`]) : `/stop`, borne de durée, borne de mémoire, cause
//! de la fin. L'enfant ouvre la même base et la même configuration, fait tourner
//! [`run_sub_agent`] avec ses propres providers, et relaie au daemon les appels MCP : le
//! host reste ici. Une panne de l'enfant (signal, mémoire) est relancée une fois, puis rend
//! un échec lisible ; elle ne touche jamais le daemon.

use super::{CallLimits, Context, SubAgentTask, run_sub_agent};
use penelope_app::child::{Cause, ChildCtx, Limits, ToChild, supervise_socket};
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

/// Côté daemon : lance l'enfant sur son socket, note sa fiche, et rend son pid et son
/// socket.
async fn launch(
    cx: &Context,
    id: &str,
    job: &SubAgentJob,
    extra: &Value,
) -> Result<(u32, PathBuf), String> {
    let s = &cx.services;
    let state = s.platform.dirs.state();
    let socket =
        penelope_app::child::registry::socket(&state, id).ok_or("identifiant d'enfant invalide")?;
    std::fs::create_dir_all(penelope_app::child::registry::dir(&state))
        .map_err(|e| e.to_string())?;
    let exe = penelope_app::helpers::running_binary()?;
    let mut spec = penelope_platform::ProcessSpec::new(exe.to_string_lossy().to_string())
        .arg("run")
        .arg("--socket")
        .arg(socket.to_string_lossy().to_string())
        .arg("--max-secs")
        .arg(s.config.config().runners.child_max_secs.max(10).to_string());
    spec.stdin = false;
    // La même maison que le daemon : un `PENELOPE_HOME` d'essai suit l'enfant.
    if let Ok(home) = std::env::var("PENELOPE_HOME") {
        spec = spec.env("PENELOPE_HOME", home);
    }
    use penelope_platform::ProcessHost;
    let child = s
        .platform
        .processes
        .spawn(spec, None)
        .await
        .map_err(|e| e.to_string())?;
    let pid = child.pid;
    // L'enfant vit sans nous : sa sortie n'est pas lue, sa fin se lit sur le socket.
    drop(child);
    penelope_app::child::registry::write(
        &state,
        &penelope_app::child::registry::Entry {
            id: id.into(),
            pid,
            session_id: job.session_id.clone(),
            job_id: penelope_executor::jobs::live::current_job(),
            effect_id: None,
            extra: extra.clone(),
            started_ms: s.clock.now_ms(),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok((pid, socket))
}

fn limits_of(cx: &Context) -> Limits {
    let cfg = cx.services.config.config();
    Limits {
        max_rss_kb: Some(cfg.runners.child_max_rss_mb.max(64) * 1024),
        max_duration: Duration::from_secs(cfg.runners.child_max_secs.max(10)),
        ..Limits::default()
    }
}

async fn journal(
    cx: &Context,
    session: &str,
    sub_agent: &str,
    attempt: u32,
    report: &penelope_app::child::Report,
) {
    let _ = cx
        .services
        .events
        .append(
            EventDraft::new(
                "child.exited",
                json!({"kind": KIND, "sub_agent": sub_agent, "pid": report.pid,
                       "rss_max_kb": report.rss_max_kb, "attempt": attempt,
                       "cause": report.cause, "text": report.describe()}),
            )
            .session(session),
        )
        .await;
}

/// Côté daemon : le sous-agent dans un enfant, relancé une fois s'il meurt.
pub async fn spawn_in_child(
    cx: &Context,
    task: SubAgentTask<'_>,
    cancel: &CancelToken,
) -> Result<String, String> {
    let s = &cx.services;
    let limits = limits_of(cx);
    let job = SubAgentJob::of(&task);
    let payload = serde_json::to_value(&job).map_err(|e| e.to_string())?;
    let extra = json!({"model": job.model_id, "kind": job.kind});
    let mcp = cx.workflows.ports.mcp.get();
    let state = s.platform.dirs.state();
    let mut attempts = 0;
    loop {
        let id = format!("c_{}", penelope_kernel::ids::Ulid::new());
        let (pid, socket) = launch(cx, &id, &job, &extra).await?;
        let init = ToChild::init(&job.session_id, KIND, payload.clone());
        let report = supervise_socket(
            pid,
            &socket,
            init,
            &limits,
            cancel,
            mcp.clone(),
            std::time::Instant::now(),
        )
        .await;
        penelope_app::child::registry::remove(&state, &id);
        journal(cx, &job.session_id, &job.kind, attempts, &report).await;
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

/// Clôt un job d'outil servi par un enfant, comme sa tâche l'aurait fait : ledger, ligne,
/// événement, puis la livraison du daemon s'en charge.
async fn conclude_job(
    cx: &Context,
    job_id: &str,
    outcome: Result<Value, String>,
) -> anyhow::Result<()> {
    use penelope_mcp::tasks::TaskState;
    let s = &cx.services;
    let jobs = penelope_executor::jobs::store(s);
    let Some(job) = jobs.get(job_id).await? else {
        return Ok(());
    };
    if job.state.is_terminal() {
        return Ok(());
    }
    let (state, value, text, error) = match outcome {
        Ok(v) => {
            let text = v["text"].as_str().unwrap_or_default().to_string();
            (TaskState::Completed, v, text, None)
        }
        Err(e) => (TaskState::Failed, json!({"error": e}), e.clone(), Some(e)),
    };
    if let Some(effect) = job
        .effect_id
        .as_deref()
        .map(penelope_kernel::ids::EffectId::from)
    {
        let open = s.effects.get(&effect).await?;
        if open.is_some_and(|e| e.state == penelope_kernel::effects::EffectState::Dispatching) {
            match &error {
                None => s.effects.complete(&effect, value.clone()).await?,
                Some(e) => s.effects.fail(&effect, e.clone()).await?,
            }
        }
    }
    jobs.finish(job_id, state, Some(json!({"value": value, "text": text})))
        .await?;
    s.events
        .append(
            EventDraft::new(
                "tool.job.completed",
                json!({"job": job_id, "state": state.as_str(), "reattached": true}),
            )
            .session(&job.session_id),
        )
        .await?;
    s.jobs.wake();
    Ok(())
}

/// Au démarrage : chaque enfant noté est rattaché (vivant), lu (fini) ou constaté perdu.
/// Un enfant sans job (sous-agent au premier plan d'un tour qui sera rejoué) est arrêté.
pub async fn reattach_all(cx: &Context) {
    let state = cx.services.platform.dirs.state();
    for e in penelope_app::child::registry::list(&state) {
        let Some(socket) = penelope_app::child::registry::socket(&state, &e.id) else {
            continue;
        };
        let Some(job_id) = e.job_id.clone() else {
            if penelope_platform::process::process_exists(e.pid) {
                penelope_platform::process::terminate_group(e.pid, Duration::from_secs(2)).await;
            }
            penelope_app::child::registry::remove(&state, &e.id);
            continue;
        };
        let cx = cx.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let limits = limits_of(&cx);
            let started = std::time::Instant::now()
                .checked_sub(Duration::from_millis(
                    (cx.services.clock.now_ms() - e.started_ms).max(0) as u64,
                ))
                .unwrap_or_else(std::time::Instant::now);
            // `/stop` et `job_cancel` atteignent l'enfant rattaché comme ils atteignaient
            // le job avant le redémarrage.
            let cancel = CancelToken::new();
            cx.services
                .jobs
                .insert(&job_id, &e.session_id, cancel.clone());
            let report = supervise_socket(
                e.pid,
                &socket,
                ToChild::Resume,
                &limits,
                &cancel,
                cx.workflows.ports.mcp.get(),
                started,
            )
            .await;
            cx.services.jobs.forget(&job_id);
            journal(
                &cx,
                &e.session_id,
                e.extra["kind"].as_str().unwrap_or_default(),
                0,
                &report,
            )
            .await;
            let outcome = match &report.cause {
                Cause::Done => {
                    let text = report
                        .result
                        .as_ref()
                        .and_then(|r| r["text"].as_str())
                        .unwrap_or_default();
                    let mut v = e.extra.clone();
                    v["text"] = json!(text);
                    Ok(v)
                }
                Cause::Failed { error } => Err(error.clone()),
                Cause::Interrupted => Err("sous-agent arrêté sur demande".into()),
                _ => Err(format!(
                    "sous-agent {} (après le redémarrage du daemon)",
                    report.describe()
                )),
            };
            if let Err(err) = conclude_job(&cx, &job_id, outcome).await {
                tracing::warn!(job = %job_id, error = %err, "job d'un enfant rattaché non conclu");
            }
            penelope_app::child::registry::remove(&state, &e.id);
        });
    }
}
