//! Jobs d'outils, notes de session, question au propriétaire, images.

use super::*;

impl NativeToolExecutor {
    /// Jobs d'outils (#204), session, question, images.
    pub(super) async fn misc_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;

        let v = match name {
            "job_status" | "job_wait" | "job_cancel" | "job_list" => {
                crate::jobs::tool(s, &self.env.session_id, name, args).await?
            }
            "session_notes" => penelope_vault::session_notes::tool(s, &self.env.session_id, args)
                .await
                .map_err(ToolError::Invalid)?,
            "session_metadata" => {
                let op = penelope_kernel::session::MetadataOp::parse(&str_arg(args, "op")?)
                    .ok_or_else(|| ToolError::Invalid("op inconnue".into()))?;
                let key = str_arg(args, "key")?;
                let mut entry = args.get("entry").cloned().unwrap_or(Value::Null);
                if key == "criteria" {
                    let current = s
                        .sessions
                        .get(&self.env.session_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|x| x.metadata["criteria"].clone())
                        .unwrap_or(Value::Null);
                    entry = criteria_entry(op, entry, &current).map_err(ToolError::Invalid)?;
                }
                s.sessions
                    .metadata(&self.env.session_id, op, &key, entry)
                    .await
                    .map_err(|e| ToolError::Other(e.to_string()))?
            }
            "ask_user" => {
                let q = str_arg(args, "question")?;
                let choices: Vec<String> = args
                    .get("choices")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut text = format!("❓ {q}");
                if !choices.is_empty() {
                    text.push_str("\n\n");
                    for c in &choices {
                        text.push_str(&format!("- {c}\n"));
                    }
                }
                if let Some(m) = &self.messenger {
                    m.send_session_text(&self.env.session_id, &self.env.origin, &text)
                        .await
                        .map_err(ToolError::Network)?;
                }
                json!({"asked": true, "remarque": "la réponse arrivera comme un nouveau message"})
            }
            "step_done" | "return_value" if self.env.in_workflow => {
                let run = self
                    .env
                    .run_id
                    .clone()
                    .ok_or_else(|| ToolError::Denied("aucun run en cours".into()))?;
                let key = penelope_app::helpers::step_done_key(&run);
                let stored = s.kv_get(&key).await;
                let mut state: Value = stored
                    .map_err(|e| ToolError::Other(e.to_string()))?
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_else(|| json!({}));
                if name == "return_value" {
                    state["result"] = args.get("result").cloned().unwrap_or(Value::Null);
                    state["content"] = args.get("content").cloned().unwrap_or(Value::Null);
                } else {
                    state["done"] = json!(true);
                }
                s.kv_set(&key, &state.to_string())
                    .await
                    .map_err(|e| ToolError::Other(e.to_string()))?;
                if name == "step_done" {
                    json!({"ok": true, "remarque": "étape terminée : termine ta réponse"})
                } else {
                    json!({"ok": true})
                }
            }
            "step_done" | "return_value" => {
                return Err(ToolError::Denied(format!(
                    "`{name}` n'a de sens que dans une étape de workflow"
                )));
            }
            "image_inspect" => {
                let task =
                    crate::vision::Task::parse(&str_arg(args, "mode")?).ok_or_else(|| {
                        ToolError::Invalid("`mode` : describe, read ou locate".into())
                    })?;
                // Une photo reçue vit dans `{data}/media/photos`, hors des workspaces.
                let mut roots = self.workspaces();
                roots.push(penelope_platform::sandbox::normalise(
                    &s.platform.dirs.data().join("media").join("photos"),
                ));
                let path = penelope_tools::fs::resolve(&str_arg(args, "path")?, &roots)?;
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("modèle de vision indisponible".into()))?;
                let v = o
                    .inspect_image(
                        &self.env.session_id,
                        &path,
                        task,
                        args.get("question")
                            .and_then(|q| q.as_str())
                            .unwrap_or_default(),
                    )
                    .await
                    .map_err(ToolError::Other)?;
                // Ce que le modèle de vision a lu dans l'image est une donnée (§13.3).
                return Ok(untrusted_listing("image", v));
            }
            "image_generate" => {
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("génération d'image indisponible".into()))?;
                let v = o
                    .generate_image(
                        &str_arg(args, "prompt")?,
                        args.get("size").and_then(|v| v.as_str()),
                    )
                    .await
                    .map_err(ToolError::Other)?;
                // Les images partent aussitôt vers le propriétaire (§10.4).
                if let Some(m) = &self.messenger {
                    for f in v["files"].as_array().cloned().unwrap_or_default() {
                        if let Some(path) = f.as_str() {
                            let _ = m
                                .send_session_file(
                                    &self.env.session_id,
                                    &self.env.origin,
                                    Path::new(path),
                                    None,
                                )
                                .await;
                        }
                    }
                }
                v
            }
            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }
}
