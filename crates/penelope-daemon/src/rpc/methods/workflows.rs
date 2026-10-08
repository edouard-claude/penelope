//! Méthodes de workflows, de planifications et de skills.

use super::*;
use crate::workflow::context_of;

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
                let known = penelope_app::services::workflow_known_with(
                    &s.config.config(),
                    &s.mcp_tools,
                    &s.workflows,
                    &s.channel,
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
            method::WF_RUNS => penelope_orchestrator::workflow::runs_listing(s, 50).await,
            method::WF_TRACE => {
                let id = required_str(p, "run")?;
                Ok(json!(s.runs.trace(&id).await?))
            }
            method::WF_RUN => {
                let id = required_str(p, "id")?;
                let params = p.get("params").cloned().unwrap_or(json!({}));
                let origin = penelope_app::helpers::owner_origin_of(&self.daemon.services);
                let run = penelope_orchestrator::workflow::start_run(
                    &context_of(&self.daemon),
                    &id,
                    params,
                    &origin,
                    None,
                    0,
                )
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
                    penelope_orchestrator::workflow::answer(
                        &context_of(&self.daemon),
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
                    return penelope_orchestrator::workflow::raise_budget(
                        &context_of(&self.daemon),
                        &run,
                        p,
                    )
                    .await;
                }
                let control = penelope_workflow::Control::parse(&op)
                    .ok_or_else(|| anyhow::anyhow!("opération inconnue : {op}"))?;
                let state = penelope_orchestrator::workflow::control(
                    &context_of(&self.daemon),
                    &run,
                    &control,
                )
                .await?;
                Ok(json!({"state": state.as_str()}))
            }
            method::WF_PLAN_SHOW => {
                let session = required_str(p, "session")?;
                let draft = penelope_workflow::plan::PlanStore::new(s.store.clone())
                    .get(&session)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("plan introuvable pour la session `{session}`")
                    })?;
                let runs = penelope_orchestrator::workflow::plan_runs_of(s, &session).await?;
                Ok(json!({"fingerprint": draft.fingerprint(), "draft": draft, "runs": runs}))
            }
            method::WF_PLAN_GO => {
                let session = required_str(p, "session")?;
                let version = p["version"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("paramètre `version` manquant"))?;
                let launched = penelope_orchestrator::workflow::go_plan(
                    &context_of(&self.daemon),
                    &session,
                    version,
                    &required_str(p, "fingerprint")?,
                    &penelope_app::helpers::owner_origin_of(s),
                )
                .await
                .map_err(anyhow::Error::msg)?;
                Ok(json!({"run": launched.run, "created": launched.created,
                          "version": version, "gate": launched.draft.plan.gate()}))
            }
            method::SCHEDULE_LIST => Ok(json!(penelope_orchestrator::scheduler::listing(s).await?)),
            // Nouvelle destination, sans recréer la planification (#124) : `private`, ou
            // `chat_id` et `topic_id`.
            method::SCHEDULE_MOVE => {
                let id = required_str(p, "id")?;
                let to = if p.get("private").and_then(|v| v.as_bool()) == Some(true) {
                    penelope_app::helpers::owner_origin_of(s)
                } else {
                    let chat = p.get("chat_id").and_then(|v| v.as_i64()).ok_or_else(|| {
                        anyhow::anyhow!("`chat_id` (et `topic_id`) ou `private: true`")
                    })?;
                    penelope_app::bus::Origin::Telegram {
                        chat_id: chat,
                        topic_id: p.get("topic_id").and_then(|v| v.as_i64()),
                        message_id: None,
                    }
                };
                let to = penelope_orchestrator::scheduler::retarget(s, &id, &to)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"id": id, "destination": to}))
            }
            method::SCHEDULE_ADD => {
                let kind = penelope_workflow::TriggerKind::parse_known(&required_str(p, "kind")?)
                    .map_err(anyhow::Error::msg)?;
                penelope_orchestrator::scheduler::create(
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
                let ports = self.daemon.hooks.scheduler();
                penelope_orchestrator::scheduler::run_now(&context_of(&self.daemon), &ports, &id)
                    .await
            }
            method::SCHEDULE_PAUSE | method::SCHEDULE_RESUME | method::SCHEDULE_RM => {
                let id = required_str(p, "id")?;
                let state = match method {
                    method::SCHEDULE_PAUSE => "paused",
                    method::SCHEDULE_RESUME => "active",
                    _ => "deleted",
                };
                // Une faute de frappe ne passe pas pour une pause réussie (#221). Le
                // secret d'un webhook part avec sa planification (#294).
                if !penelope_orchestrator::scheduler::set_state(s, &id, state).await? {
                    anyhow::bail!("planification inconnue : {id}");
                }
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
                let generation = penelope_app::services::reload_skills(s).await?;
                Ok(json!({"generation": generation, "skills": s.skills.all().len()}))
            }
            method::SKILL_ROLLBACK => {
                let name = required_str(p, "name")?;
                let root = s.platform.dirs.skills();
                let path =
                    penelope_skills::rollback_skill(&root, &name).map_err(anyhow::Error::msg)?;
                penelope_app::services::reload_skills(s).await?;
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
async fn skill_install(d: &Core, p: &Value) -> anyhow::Result<Value> {
    let source = required_str(p, "source")?;
    let force = p.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let (src, installed, missing) =
        penelope_ops::skill_install::install(&d.services, &source, force).await?;
    Ok(json!({
        "source": src.label(),
        "installed": installed,
        "missing": missing,
        "report": penelope_ops::skill_install::report(&src, &installed, &missing),
    }))
}
