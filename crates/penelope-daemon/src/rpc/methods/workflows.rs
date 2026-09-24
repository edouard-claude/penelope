//! Méthodes de workflows, de planifications et de skills.

use super::*;

impl Rpc {
    /// Workflows et planifications.
    pub(super) async fn workflows(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::WF_LIST => Ok(json!(
                s.workflows
                    .all()
                    .iter()
                    .map(|e| json!({
                        "id": e.workflow.metadata.id,
                        "name": e.workflow.metadata.name,
                        "scope": e.scope.as_str(),
                        "steps": e.workflow.steps.len(),
                        "runsHere": e.workflow.runs_on(s.platform.os_name()),
                    }))
                    .collect::<Vec<_>>()
            )),
            method::WF_SHOW => {
                let id = required_str(p, "id")?;
                let w = s
                    .workflows
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("workflow `{id}` introuvable"))?;
                Ok(json!({"definition": w, "graph": w.render_graph()}))
            }
            method::WF_VALIDATE => {
                let raw = required_str(p, "json")?;
                let stem = p.get("name").and_then(|n| n.as_str());
                let w = penelope_workflow::Workflow::from_json(&raw)
                    .map_err(|e| anyhow::anyhow!("JSON invalide : {e}"))?;
                let known = crate::runtime::workflow_known_with(
                    &s.config.config(),
                    &s.mcp_tools,
                    &s.workflows,
                )
                .await;
                let report = penelope_workflow::validate(&w, stem, &known);
                Ok(json!({
                    "valid": report.is_valid(),
                    "issues": report.issues.iter().map(|i| json!({
                        "path": i.path, "message": i.message,
                        "severity": format!("{:?}", i.severity).to_lowercase()
                    })).collect::<Vec<_>>(),
                }))
            }
            method::WF_RUNS => Ok(serde_json::to_value(s.runs.list(None, 50).await?)?),
            method::WF_TRACE => {
                let id = required_str(p, "run")?;
                Ok(json!(s.runs.trace(&id).await?))
            }
            method::WF_RUN => {
                let id = required_str(p, "id")?;
                let params = p.get("params").cloned().unwrap_or(json!({}));
                let origin = crate::helpers::owner_origin_of(&self.daemon.services);
                let run = crate::workflow::start_run(&self.daemon, &id, params, &origin, None, 0)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(serde_json::to_value(run)?)
            }
            method::WF_CONTROL => {
                let run = required_str(p, "run")?;
                let op = required_str(p, "op")?;
                if op == "answer" {
                    let choice = required_str(p, "choice")?;
                    let current = s
                        .runs
                        .get(&run)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("run {run} introuvable"))?;
                    let visit = format!(
                        "{}.{}",
                        current.current_step.unwrap_or_default(),
                        current.iterations
                    );
                    crate::workflow::answer(
                        &self.daemon,
                        &run,
                        &visit,
                        &choice,
                        p.get("input").and_then(|i| i.as_str()),
                    )
                    .await?;
                    return Ok(json!({"run": run, "answered": choice}));
                }
                // Plafonds propres au run (issue #136).
                if op == "budget" {
                    return crate::workflow::raise_budget(
                        &self.daemon,
                        &run,
                        p.get("usd").and_then(|v| v.as_f64()),
                        p.get("tokens").and_then(|v| v.as_u64()),
                    )
                    .await;
                }
                let control = penelope_workflow::Control::parse(&op)
                    .ok_or_else(|| anyhow::anyhow!("opération inconnue : {op}"))?;
                let state = crate::workflow::control(&self.daemon, &run, &control).await?;
                Ok(json!({"state": state.as_str()}))
            }
            method::SCHEDULE_LIST => Ok(json!(crate::scheduler::listing(s).await?)),
            // Nouvelle destination, sans recréer la planification (#124) : `private`, ou
            // `chat_id` et `topic_id`.
            method::SCHEDULE_MOVE => {
                let id = required_str(p, "id")?;
                let (chat_id, topic_id) =
                    if p.get("private").and_then(|v| v.as_bool()) == Some(true) {
                        (s.config.config().owner.telegram_user_id, None)
                    } else {
                        let chat = p.get("chat_id").and_then(|v| v.as_i64()).ok_or_else(|| {
                            anyhow::anyhow!("`chat_id` (et `topic_id`) ou `private: true`")
                        })?;
                        (chat, p.get("topic_id").and_then(|v| v.as_i64()))
                    };
                let to = crate::scheduler::retarget(s, &id, chat_id, topic_id)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"id": id, "destination": to}))
            }
            method::SCHEDULE_ADD => {
                let kind = penelope_workflow::TriggerKind::parse(&required_str(p, "kind")?)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "kind inconnu : cron, interval, mcp_poll, watch_file ou event"
                        )
                    })?;
                crate::scheduler::create(
                    s,
                    kind,
                    p.get("spec").cloned().unwrap_or(json!({})),
                    p.get("target").cloned().unwrap_or(json!({})),
                    p.get("dedup").cloned().unwrap_or(json!({})),
                )
                .await
                .map_err(anyhow::Error::msg)
            }
            method::SCHEDULE_RUN_NOW => {
                let id = required_str(p, "id")?;
                crate::scheduler::run_now(&self.daemon, &self.daemon.hooks.scheduler(), &id).await
            }
            method::SCHEDULE_PAUSE => {
                s.schedules
                    .set_state(&required_str(p, "id")?, "paused")
                    .await?;
                Ok(json!({"ok": true}))
            }
            method::SCHEDULE_RESUME => {
                s.schedules
                    .set_state(&required_str(p, "id")?, "active")
                    .await?;
                Ok(json!({"ok": true}))
            }
            method::SCHEDULE_RM => {
                s.schedules
                    .set_state(&required_str(p, "id")?, "deleted")
                    .await?;
                Ok(json!({"ok": true}))
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }

    /// Skills.
    pub(super) async fn skills(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::SKILL_RELOAD => {
                // Skill déposée à l'instant : relue sans attendre la passe d'entretien.
                let generation = crate::runtime::reload_skills(s).await?;
                Ok(json!({"generation": generation, "skills": s.skills.all().len()}))
            }
            method::SKILL_ROLLBACK => {
                let name = required_str(p, "name")?;
                let root = s.platform.dirs.skills();
                let path =
                    penelope_skills::rollback_skill(&root, &name).map_err(anyhow::Error::msg)?;
                crate::runtime::reload_skills(s).await?;
                Ok(json!({"name": name, "restored": path}))
            }
            method::SKILL_LIST => Ok(json!(
                s.skills
                    .all()
                    .iter()
                    .map(|k| json!({
                        "name": k.name, "version": k.version, "scope": k.scope.as_str(),
                        "description": k.description
                    }))
                    .collect::<Vec<_>>()
            )),
            method::SKILL_INSTALL => skill_install(&self.daemon, p).await,
            method::SKILL_SHOW => {
                let name = required_str(p, "name")?;
                Ok(serde_json::to_value(s.skills.get(&name))?)
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }
}

/// Installe des skills tierces et rend ce qui a été posé, avec ce qui manque (#146).
async fn skill_install(d: &Arc<Daemon>, p: &Value) -> anyhow::Result<Value> {
    let source = required_str(p, "source")?;
    let force = p.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let (src, installed, missing) =
        crate::skill_install::install(&d.services, &source, force).await?;
    Ok(json!({
        "source": src.label(),
        "installed": installed,
        "missing": missing,
        "report": crate::skill_install::report(&src, &installed, &missing),
    }))
}
