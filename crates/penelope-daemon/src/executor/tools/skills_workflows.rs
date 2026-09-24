//! Skills, workflows et sous-agents.

use super::*;

impl NativeToolExecutor {
    /// Skills.
    pub(super) async fn skill_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;

        let v = match name {
            "skill_search" => {
                let hits = s
                    .skills
                    .search(&str_arg(args, "query")?, u_arg(args, "limit").unwrap_or(5));
                json!(
                    hits.iter()
                        .map(|(k, score)| {
                            // Une skill dont un binaire requis manque est annoncée telle
                            // quelle (issue #156) : elle reste listée — c'est au
                            // propriétaire d'installer, jamais à Pénélope (#146) — mais
                            // le modèle sait avant de l'appliquer qu'elle échouera.
                            let mut o = json!({
                                "name": k.name, "description": k.description, "score": score
                            });
                            let missing = crate::machine::missing_binaries(&k.requires);
                            if !missing.is_empty() {
                                o["binaires_manquants"] = json!(missing);
                            }
                            o
                        })
                        .collect::<Vec<_>>()
                )
            }
            "skill_load" => {
                let name = str_arg(args, "name")?;
                let sk = s
                    .skills
                    .get(&name)
                    .ok_or_else(|| ToolError::Invalid(format!("skill `{name}` introuvable")))?;
                // Un corps venu d'ailleurs parle en outils Claude Code et en chemins
                // relatifs : la table de correspondance et le dossier absolu sont posés
                // devant, sans toucher au fichier (issue #146).
                let content = match penelope_skills::install::portage_note(&sk) {
                    Some(note) => format!("{note}{}", sk.body),
                    None => sk.body.clone(),
                };
                let mut out = json!({
                    "name": sk.name, "allowed_tools": sk.allowed_tools,
                    "requires": sk.requires, "content": content
                });
                // Dit avant l'application, pas au premier échec de commande (issue #156).
                let missing = crate::machine::missing_binaries(&sk.requires);
                if !missing.is_empty() {
                    out["binaires_manquants"] = json!(missing);
                    out["remarque"] = json!(format!(
                        "Binaire(s) absent(s) de cette machine : {}. Les étapes qui les \
                         appellent échoueront ; dis-le au propriétaire plutôt que de \
                         contourner.",
                        missing.join(", ")
                    ));
                }
                out
            }
            "skill_propose" | "skill_patch" => {
                let skill_name = str_arg(args, "name")?;
                let existing = s.skills.get(&skill_name);
                let proposal = penelope_skills::SkillProposal {
                    name: skill_name.clone(),
                    description: args
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| existing.as_ref().map(|k| k.description.clone()))
                        .unwrap_or_default(),
                    body: str_arg(args, "body")?,
                    allowed_tools: args
                        .get("allowed_tools")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .or_else(|| existing.as_ref().map(|k| k.allowed_tools.clone()))
                        .unwrap_or_default(),
                    kind: if name == "skill_patch" {
                        "patch"
                    } else {
                        "create"
                    }
                    .into(),
                    diff: String::new(),
                    rationale: String::new(),
                };
                proposal.validate().map_err(ToolError::Invalid)?;
                let root = s.platform.dirs.skills();
                let path = penelope_skills::write_skill(&root, &proposal).map_err(ToolError::Io)?;
                crate::runtime::reload_skills(s)
                    .await
                    .map_err(|e| ToolError::Io(e.to_string()))?;
                json!({"written": path, "name": proposal.name})
            }

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// Workflows et sous-agents.
    pub(super) async fn workflow_tools(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        let v = match name {
            "workflow_list" => {
                json!(
                s.workflows
                    .all()
                    .iter()
                    .map(|e| json!({"id": e.workflow.metadata.id, "name": e.workflow.metadata.name,
                                    "description": e.workflow.metadata.description}))
                    .collect::<Vec<_>>()
            )
            }
            "workflow_describe" => {
                let id = str_arg(args, "id")?;
                let w = s
                    .workflows
                    .get(&id)
                    .ok_or_else(|| ToolError::Invalid(format!("workflow `{id}` introuvable")))?;
                json!({"definition": w, "graphe": w.render_graph()})
            }
            "workflow_plan" => {
                use penelope_workflow::plan::{PlanStep, PlanStore};
                let id = str_arg(args, "id")?;
                s.workflows
                    .get(&id)
                    .ok_or_else(|| ToolError::Invalid(format!("workflow `{id}` introuvable")))?;
                let plans = PlanStore::new(s.store.clone());
                let existing = plans.get(&self.env.session_id).await?;
                let restore = args.get("restore_version").and_then(Value::as_u64);
                let goal = args.get("goal").and_then(Value::as_str);
                let steps = args.get("steps");
                let draft = if let Some(mut old) = existing {
                    if old.plan.can_execute()
                        && args.get("expected_version").is_none()
                        && goal.is_some()
                        && steps.is_some()
                    {
                        let new = new_workflow_plan(&id, args)?;
                        plans.start_next(&self.env.session_id, &old, &new).await?;
                        new
                    } else {
                        if old.workflow_id != id {
                            return Err(ToolError::Invalid(format!(
                                "la session prépare déjà le workflow `{}`",
                                old.workflow_id
                            )));
                        }
                        if restore.is_some() || goal.is_some() || steps.is_some() {
                            let previous = old.clone();
                            let expected = args
                                .get("expected_version")
                                .and_then(Value::as_u64)
                                .ok_or_else(|| {
                                    ToolError::Invalid(
                                        "expected_version requis pour réviser".into(),
                                    )
                                })?;
                            if let Some(version) = restore {
                                old.restore(expected, version)
                                    .map_err(|e| ToolError::Invalid(e.to_string()))?;
                            } else {
                                let goal =
                                    goal.ok_or_else(|| ToolError::Invalid("goal requis".into()))?;
                                let steps: Vec<PlanStep> =
                                    serde_json::from_value(steps.cloned().ok_or_else(|| {
                                        ToolError::Invalid("steps requis".into())
                                    })?)
                                    .map_err(|e| ToolError::Invalid(e.to_string()))?;
                                old.revise(expected, goal, steps)
                                    .map_err(|e| ToolError::Invalid(e.to_string()))?;
                            }
                            old.params = args.get("params").cloned().unwrap_or(old.params);
                            old.brief = args
                                .get("brief")
                                .and_then(Value::as_str)
                                .map(String::from)
                                .or(old.brief);
                            plans.replace(&self.env.session_id, &previous, &old).await?;
                        }
                        old
                    }
                } else {
                    let new = new_workflow_plan(&id, args)?;
                    plans.create(&self.env.session_id, &new).await?;
                    new
                };
                if let Some(messenger) = &self.messenger {
                    messenger
                        .send_plan_card(&self.env.origin, &self.env.session_id, &draft)
                        .await
                        .map_err(ToolError::Other)?;
                }
                serde_json::to_value(&draft).unwrap_or_default()
            }
            "workflow_start" => {
                if matches!(self.env.origin, Origin::Telegram { .. }) && !self.env.in_workflow {
                    return Err(ToolError::Invalid(
                        "propose d'abord un plan avec workflow_plan ; seul le propriétaire peut valider « vas-y »".into(),
                    ));
                }
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("moteur de workflows indisponible".into()))?;
                o.start_workflow(
                    &str_arg(args, "id")?,
                    args.get("params").cloned().unwrap_or(json!({})),
                    args.get("brief").and_then(|v| v.as_str()),
                    &self.env.origin,
                )
                .await
                .map_err(ToolError::Other)?
            }
            "workflow_status" => serde_json::to_value(s.runs.get(&str_arg(args, "run_id")?).await?)
                .unwrap_or_default(),
            "workflow_control" => {
                let op = str_arg(args, "op")?;
                let run_id = str_arg(args, "run_id")?;
                match &self.orchestrator {
                    Some(o) => o
                        .control_run(&run_id, &op)
                        .await
                        .map_err(ToolError::Other)?,
                    None => {
                        let control = penelope_workflow::Control::parse(&op).ok_or_else(|| {
                            ToolError::Invalid(format!("opération inconnue : {op}"))
                        })?;
                        let st = s.runs.control(&run_id, &control).await?;
                        json!({"state": st.as_str()})
                    }
                }
            }
            "workflow_author" => {
                let draft = args.get("draft").cloned().unwrap_or(Value::Null);
                let raw = match &draft {
                    Value::String(t) => t.clone(),
                    other => other.to_string(),
                };
                // Un brouillon refusé renvoie à la section de la documentation (issue #34).
                let with_doc = |e: String| {
                    let (heading, link) = crate::selfdocs::workflow_doc_for(&e);
                    ToolError::Invalid(format!(
                        "{e}\nDocumentation : « {heading} », {link} (lire avec `self_docs` \
                         action `read`, file `docs/workflows.md`, section « {heading} »)."
                    ))
                };
                let w = penelope_workflow::Workflow::from_json(&raw)
                    .map_err(|e| with_doc(format!("JSON invalide : {e}")))?;
                let known =
                    crate::runtime::workflow_known_with(&cfg, &s.mcp_tools, &s.workflows).await;
                let dir = s.platform.dirs.workflows();
                let path = s.workflows.write(&dir, &w, &known).map_err(with_doc)?;
                s.workflows
                    .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);
                json!({"written": path, "id": w.metadata.id})
            }
            "sub_agent_spawn" => {
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("sous-agents indisponibles".into()))?;
                o.spawn_sub_agent(
                    &self.env.session_id,
                    &str_arg(args, "prompt")?,
                    args.get("model").and_then(|v| v.as_str()),
                    args.get("tools")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    &self.env.origin,
                    cancel,
                )
                .await
                .map_err(ToolError::Other)?
            }
            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }
}
