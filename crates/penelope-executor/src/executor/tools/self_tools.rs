//! Outils sur Pénélope elle-même.

use super::*;

impl NativeToolExecutor {
    /// Soi-même : état, documentation, configuration, heure.
    pub(super) async fn self_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        let v = match name {
            "self_status" => {
                let section = args
                    .get("section")
                    .and_then(|v| v.as_str())
                    .unwrap_or("all")
                    .to_string();
                crate::selfknow::status(
                    s,
                    &self.env.session_id,
                    self.env.turn_model.as_ref(),
                    self.admin.as_deref(),
                    &section,
                )
                .await
                .map_err(|e| ToolError::Io(e.to_string()))?
            }
            "self_docs" => crate::selfdocs::tool(args).map_err(ToolError::Invalid)?,
            "config_set" => {
                let path = str_arg(args, "path")?;
                if let Some(why) = crate::selfknow::forbidden_path(&path) {
                    return Err(ToolError::Denied(why));
                }
                let raw = str_arg(args, "value")?;
                let value = crate::selfknow::parse_scalar(&raw);
                let admin = self.admin.as_ref().ok_or_else(|| {
                    ToolError::Denied("configuration non modifiable depuis ce contexte".into())
                })?;
                let generation = admin
                    .set_config(&path, value.clone())
                    .await
                    .map_err(ToolError::Invalid)?;
                let mut warnings: Vec<String> =
                    penelope_kernel::coherence::contradictions(&s.config.config())
                        .into_iter()
                        .filter(|c| c.concerns(&path))
                        .map(|c| c.message)
                        .collect();
                let applied_value = if path == "sandbox.workspaces" {
                    let stored = s.config.config().sandbox.workspaces.clone();
                    let requested: Vec<&str> = match &value {
                        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
                        Value::String(item) => vec![item],
                        _ => Vec::new(),
                    };
                    for (asked, actual) in requested.iter().zip(&stored) {
                        if asked != actual {
                            warnings.push(format!(
                                "workspace `{asked}` enregistré sous sa forme réelle `{actual}`"
                            ));
                        }
                    }
                    for root in &stored {
                        if !s.platform.dirs.expand(root).exists() {
                            warnings.push(format!("workspace `{root}` n'existe pas encore"));
                        }
                    }
                    json!(stored)
                } else {
                    value
                };
                let (applied, note) = config_application_time(&path);
                json!({
                    "path": path,
                    "value": applied_value,
                    "generation": generation,
                    "applied": applied,
                    "remarque": note,
                    "avertissements": warnings,
                })
            }
            "time_now" => {
                let tz = args
                    .get("timezone")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&cfg.owner.timezone)
                    .to_string();
                let utc =
                    chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default();
                match tz.parse::<chrono_tz::Tz>() {
                    Ok(t) => {
                        let local = utc.with_timezone(&t);
                        json!({"iso": local.to_rfc3339(), "timezone": tz,
                               "lisible": local.format("%A %d %B %Y, %H:%M").to_string()})
                    }
                    Err(_) => {
                        return Err(ToolError::Invalid(format!("fuseau inconnu : {tz}")));
                    }
                }
            }

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }
}
