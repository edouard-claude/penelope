//! Planification et messages.

use super::*;

impl NativeToolExecutor {
    /// Planification.
    pub(super) async fn schedule_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;

        let v = match name {
            "schedule_create" => {
                let kind = penelope_workflow::TriggerKind::parse(&str_arg(args, "kind")?)
                    .ok_or_else(|| ToolError::Invalid("kind inconnu".into()))?;
                let mut target = args.get("target").cloned().unwrap_or(Value::Null);
                // Le déclencheur répond par ce canal ; la session d'origine n'est qu'une
                // référence, chaque exécution ouvre la sienne (issue #39).
                if let Some(o) = target.as_object_mut() {
                    if let Some(sid) = o.remove("session_id") {
                        o.entry("origin_session").or_insert(sid);
                    }
                    o.entry("origin_session")
                        .or_insert(json!(self.env.session_id));
                    o.entry("origin").or_insert(self.env.origin.to_value());
                }
                let spec = args.get("spec").cloned().unwrap_or(Value::Null);
                self.scheduler()?
                    .schedule_create(
                        kind,
                        spec,
                        target,
                        args.get("dedup").cloned().unwrap_or(Value::Null),
                    )
                    .await
                    .map_err(ToolError::Invalid)?
            }
            "schedule_list" => json!(
                self.scheduler()?
                    .schedule_list()
                    .await
                    .map_err(ToolError::Other)?
            ),
            "schedule_move" => {
                let id = str_arg(args, "id")?;
                let to = match str_arg(args, "to")?.as_str() {
                    "private" => penelope_app::helpers::owner_origin_of(s),
                    "here" if self.env.origin.is_channel() => self.env.origin.clone(),
                    "here" => {
                        return Err(ToolError::Invalid(
                            "`here` : cette conversation n'est pas celle d'un canal, `private` \
                             ou `penelope schedule move`"
                                .into(),
                        ));
                    }
                    other => {
                        return Err(ToolError::Invalid(format!(
                            "`to` : `here` ou `private`, pas `{other}`"
                        )));
                    }
                };
                let to = self
                    .scheduler()?
                    .schedule_move(&id, &to)
                    .await
                    .map_err(ToolError::Invalid)?;
                json!({"id": id, "destination": to})
            }
            "schedule_delete" => {
                self.scheduler()?
                    .schedule_delete(&str_arg(args, "id")?)
                    .await
                    .map_err(ToolError::Other)?;
                json!({"deleted": true})
            }

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// L'ordonnanceur, par le port `Orchestrator` : la planification vit au-dessus de
    /// l'exécuteur (T24).
    fn scheduler(&self) -> ToolResult<&Arc<dyn Orchestrator>> {
        self.orchestrator
            .as_ref()
            .ok_or_else(|| ToolError::Other("planificateur indisponible ici".into()))
    }

    /// Messages.
    pub(super) async fn message_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let v =
            match name {
                "send_message" => {
                    let m = self.messenger.as_ref().ok_or_else(|| {
                        ToolError::Other("aucun canal de message disponible".into())
                    })?;
                    m.send_session_text(
                        &self.env.session_id,
                        &self.env.origin,
                        &str_arg(args, "text")?,
                    )
                    .await
                    .map_err(ToolError::Network)?;
                    json!({"sent": true})
                }
                "send_voice" => {
                    let admin = self.admin.as_ref().ok_or_else(|| {
                        ToolError::Other("vocal indisponible hors du daemon".into())
                    })?;
                    admin
                        .send_voice(&self.env.session_id, &self.env.origin, args)
                        .await
                        .map_err(ToolError::Invalid)?
                }
                "send_file" => {
                    let p = self.path_arg(args, "path")?;
                    let m = self.messenger.as_ref().ok_or_else(|| {
                        ToolError::Other("aucun canal de message disponible".into())
                    })?;
                    m.send_session_file(
                        &self.env.session_id,
                        &self.env.origin,
                        &p,
                        args.get("caption").and_then(|v| v.as_str()),
                    )
                    .await
                    .map_err(ToolError::Network)?;
                    json!({"sent": true, "path": p})
                }

                other => return Err(ToolError::Unknown(other.to_string())),
            };
        Ok(ToolOutcome::ok(v))
    }
}
