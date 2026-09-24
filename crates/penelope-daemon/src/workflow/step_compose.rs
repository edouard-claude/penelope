//! Étapes composées : `parallel`, `workflow`, `wait`.

use super::*;

/// `parallel` : enfants `shell`, `tool` ou `sub_agent` en concurrence bornée.
async fn run_child(ctx: &StepCtx<'_>, child: &Step) -> (String, anyhow::Result<StepOutcome>) {
    // Un enfant qui expire n'annule pas ses frères : chacun a son jeton (issue #56).
    let child_cancel = ctx.cancel.child();
    let child_ctx = StepCtx {
        d: ctx.d,
        run: ctx.run,
        wf: ctx.wf,
        step: child,
        attempt: ctx.attempt,
        cancel: &child_cancel,
    };
    (child.id.clone(), Box::pin(execute_step(&child_ctx)).await)
}

pub(super) async fn parallel_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let max = ctx.step.max_concurrency.unwrap_or(4).max(1) as usize;
    let children: Vec<Step> = ctx.step.children.clone();
    let mut results: Vec<(String, anyhow::Result<StepOutcome>)> = Vec::new();
    for chunk in children.chunks(max) {
        let batch = chunk.iter().map(|child| run_child(ctx, child));
        results.extend(futures::future::join_all(batch).await);
    }

    let mut output = serde_json::Map::new();
    let (mut ok, mut total) = (0usize, 0usize);
    let mut waiting = None;
    for (id, r) in results {
        total += 1;
        match r? {
            StepOutcome::Waiting(why) => waiting = Some(format!("{id} : {why}")),
            StepOutcome::Done {
                result,
                output: out,
            } => {
                if result.is_ok() {
                    ok += 1;
                }
                let mut entry = out;
                if let Some(o) = entry.as_object_mut() {
                    o.insert("result".into(), json!(result.as_str()));
                }
                output.insert(id, entry);
            }
        }
    }
    if let Some(why) = waiting {
        return Ok(StepOutcome::Waiting(why));
    }
    let result = if ok == total {
        StepResult::Success
    } else if ok > 0 {
        StepResult::Partial
    } else {
        StepResult::Failure
    };
    Ok(done(result, Value::Object(output)))
}

/// `workflow` : sous-workflow ; son issue devient le résultat.
pub(super) async fn workflow_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let (run, step) = (ctx.run, ctx.step);
    let key = visit_key("child", run, &step.id);
    if let Some(child_id) = s.kv_get(&key).await? {
        let Some(child) = s.runs.get(&child_id).await? else {
            return Ok(done(
                StepResult::Error,
                json!({"error": "sous-run disparu"}),
            ));
        };
        let last = child
            .step_outputs
            .get("__last")
            .cloned()
            .unwrap_or(Value::Null);
        let output = json!({"run": child.id, "state": child.state.as_str(), "output": last, "error": child.error});
        return Ok(match child.state {
            RunState::Done => done(StepResult::Success, output),
            RunState::Blocked => done(StepResult::Blocked, output),
            RunState::Failed | RunState::Cancelled => done(StepResult::Failure, output),
            RunState::Running | RunState::Paused => {
                StepOutcome::Waiting(format!("sous-run {}", child.id))
            }
        });
    }
    let params = ctx.render_json(&step.params).await;
    let origin = origin_of(ctx.d, &run.id).await;
    match start_run(
        ctx.d,
        &step.workflow_id,
        params,
        &origin,
        Some(&run.id),
        run.depth + 1,
    )
    .await
    {
        Ok(child) => {
            s.kv_set(&key, &child.id).await?;
            Ok(StepOutcome::Waiting(format!("sous-run {}", child.id)))
        }
        Err(e) => Ok(done(StepResult::Error, json!({"error": e}))),
    }
}

/// `wait` : délai, événement ou échéance cron.
pub(super) async fn wait_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let (run, step) = (ctx.run, ctx.step);
    let key = visit_key("wait", run, &step.id);
    let now = s.clock.now_ms();
    let state: Value = match s.kv_get(&key).await? {
        Some(raw) => serde_json::from_str(&raw).unwrap_or(json!({})),
        None => {
            let cursor = last_event_id(s).await?;
            let v = json!({"since_ms": now, "cursor": cursor});
            s.kv_set(&key, &v.to_string()).await?;
            v
        }
    };
    let since = state["since_ms"].as_i64().unwrap_or(now);
    let elapsed = now - since;
    if let Some(t) = step.timeout_ms
        && elapsed >= t as i64
    {
        return Ok(done(StepResult::Timeout, json!({"waited_ms": elapsed})));
    }
    let on = &step.on;
    if let Some(ms) = on.get("duration_ms").and_then(|v| v.as_i64()) {
        if elapsed >= ms {
            return Ok(done(StepResult::Fired, json!({"waited_ms": elapsed})));
        }
    } else if let Some(name) = on.get("event").and_then(|v| v.as_str()) {
        let cursor = state["cursor"].as_i64().unwrap_or(0);
        for e in s.events.range(cursor, 500).await? {
            if e.kind == name {
                return Ok(done(
                    StepResult::Fired,
                    json!({"event": e.kind, "payload": e.payload, "waited_ms": elapsed}),
                ));
            }
        }
    } else if let Some(expr) = on.get("cron").and_then(|v| v.as_str()) {
        let tz = s.config.config().owner.timezone.clone();
        match penelope_kernel::cron::Cron::parse(expr) {
            Ok(c) => {
                if let Some(at) = c.next_after_ms(since, &tz)
                    && now >= at
                {
                    return Ok(done(StepResult::Fired, json!({"at_ms": at})));
                }
            }
            Err(e) => {
                return Ok(done(
                    StepResult::Error,
                    json!({"error": format!("cron invalide : {e}")}),
                ));
            }
        }
    } else if let Some(spec) = on.get("mcp_task") {
        return mcp_task_wait(ctx, spec, &state, elapsed).await;
    } else if step.timeout_ms.is_none() {
        return Ok(done(
            StepResult::Timeout,
            json!({"error": "attente sans condition connue ni délai"}),
        ));
    }
    Ok(StepOutcome::Waiting("attente".into()))
}

/// `wait` sur une tâche MCP longue : `{"mcp_task": "serveur:tâche"}` ou
/// `{"mcp_task": {"server": "…", "task": "{{steps.lancer.data.task.taskId}}"}}`. La tâche est
/// suivie dans `mcp_tasks` (elle survit à un redémarrage) et sondée à intervalle croissant ;
/// l'étape se déclenche quand elle se termine, quel que soit son sort (`status` en sortie).
async fn mcp_task_wait(
    ctx: &StepCtx<'_>,
    spec: &Value,
    state: &Value,
    elapsed: i64,
) -> anyhow::Result<StepOutcome> {
    use penelope_mcp::tasks::{TaskState, TaskStore};
    let s = ctx.s();
    let (run, step) = (ctx.run, ctx.step);
    let (server, reference) = match spec {
        Value::String(raw) => {
            let raw = ctx.render(raw).await;
            match raw.split_once(':') {
                Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
                None => (String::new(), raw),
            }
        }
        Value::Object(_) => (
            ctx.render(spec["server"].as_str().unwrap_or_default())
                .await,
            ctx.render(
                spec["task"]
                    .as_str()
                    .or_else(|| spec["taskId"].as_str())
                    .unwrap_or_default(),
            )
            .await,
        ),
        _ => (String::new(), String::new()),
    };
    if server.is_empty() || reference.is_empty() {
        return Ok(done(
            StepResult::Error,
            json!({"error": "mcp_task : serveur ou tâche absent (\"serveur:tâche\")"}),
        ));
    }
    let tasks = TaskStore::new(s.store.clone(), s.clock.clone());
    let key = visit_key("mcp_task", run, &step.id);
    let task_id = match s.kv_get(&key).await? {
        Some(id) => id,
        None => {
            let t = tasks
                .create(
                    &server,
                    &reference,
                    Some(&run.session_id),
                    Some(&run.id),
                    &json!({"step": step.id}),
                )
                .await?;
            s.kv_set(&key, &t.id).await?;
            t.id
        }
    };
    let Some(task) = tasks.get(&task_id).await? else {
        return Ok(done(
            StepResult::Error,
            json!({"error": format!("tâche {task_id} perdue")}),
        ));
    };
    let fired = |state: TaskState, result: Option<Value>| {
        done(
            StepResult::Fired,
            json!({
                "server": server,
                "task": reference,
                "status": state.as_str(),
                "result": result.unwrap_or(Value::Null),
                "waited_ms": elapsed,
            }),
        )
    };
    if task.state.is_terminal() {
        return Ok(fired(task.state, task.result));
    }
    let now = s.clock.now_ms();
    let due = task
        .poll_at
        .as_deref()
        .and_then(|p| chrono::DateTime::parse_from_rfc3339(p).ok())
        .is_none_or(|at| at.timestamp_millis() <= now);
    if !due {
        return Ok(StepOutcome::Waiting("tâche MCP en cours".into()));
    }
    let Some(sup) = ctx.d.hooks.mcp_supervisor() else {
        return Ok(StepOutcome::Waiting("superviseur MCP non démarré".into()));
    };
    let attempts = state["mcp_polls"].as_u64().unwrap_or(0) as u32;
    let mut next = state.clone();
    next["mcp_polls"] = json!(attempts + 1);
    s.kv_set(&visit_key("wait", run, &step.id), &next.to_string())
        .await?;
    match sup.task_status(&server, &reference).await {
        Ok(v) => {
            let status = TaskState::parse(v["status"].as_str().unwrap_or_default())
                .unwrap_or(TaskState::Working);
            if status.is_terminal() {
                let result = Some(v["result"].clone()).filter(|r| !r.is_null());
                tasks.update(&task_id, status, result.clone(), None).await?;
                let _ = s
                    .events
                    .append(
                        EventDraft::new(
                            "mcp.task.completed",
                            json!({"server": server, "task": reference, "status": status.as_str(), "run": run.id}),
                        )
                        .session(&run.session_id),
                    )
                    .await;
                return Ok(fired(status, result));
            }
            tasks
                .update(
                    &task_id,
                    status,
                    None,
                    Some(TaskStore::poll_interval_ms(attempts)),
                )
                .await?;
        }
        Err(e) => {
            tracing::warn!(run = %run.id, step = %step.id, error = %e, "sondage de tâche MCP");
            tasks
                .update(
                    &task_id,
                    TaskState::Working,
                    None,
                    Some(TaskStore::poll_interval_ms(attempts)),
                )
                .await?;
        }
    }
    Ok(StepOutcome::Waiting("tâche MCP en cours".into()))
}

async fn last_event_id(s: &Services) -> anyhow::Result<i64> {
    Ok(s.store
        .read(|c| Ok(c.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))?))
        .await?)
}
