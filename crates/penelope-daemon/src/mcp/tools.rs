//! Appels d'outils, saisies demandées par un serveur (MRTR) et tâches.

use super::*;

impl McpSupervisor {
    // -------------------------------------------------------------- appels

    /// Appelle un outil par son nom qualifié.
    pub async fn call(
        &self,
        qualified: &str,
        args: &Value,
        from: crate::elicitation::Destination,
    ) -> Result<Value, String> {
        let tool = self
            .services
            .mcp_tools
            .get(qualified)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("outil MCP inconnu : `{qualified}`"))?;
        let slot = self
            .slot(&tool.server)
            .await
            .ok_or_else(|| format!("le serveur `{}` n'est plus déclaré dans mcp.d", tool.server))?;
        let client = self.ensure_live(&slot).await?;
        // Le serveur peut demander une confirmation pendant l'appel : elle doit revenir
        // dans la conversation qui l'a provoqué (issue #143). Le garde tombe avec
        // l'appel, réussi ou non.
        let _scope = self.services.elicitations.scope(&tool.server, from);
        let timeout = slot.config().timeout_for(&tool.name);
        let started = std::time::Instant::now();
        let call = |input: Option<Value>, state: Option<String>| {
            client.retry_tool(
                &tool.name,
                args.clone(),
                tool.output_schema.as_ref(),
                Some(timeout),
                input,
                state,
            )
        };
        let mut outcome = call(None, None).await;
        // MRTR (2026-07-28) : le serveur demande une saisie, l'appel est relancé avec les
        // réponses et son état opaque.
        let mut rounds = 0;
        while let Ok(r) = &outcome
            && r.needs_input()
            && rounds < MAX_INPUT_ROUNDS
        {
            rounds += 1;
            let url_ok = client.version().has_url_elicitation();
            let input = self
                .input_responses(&tool.server, r.input_requests.as_ref(), url_ok)
                .await;
            let state = r.request_state.clone();
            outcome = call(input, state).await;
        }
        // 2025-11-25 : l'appel exige un lien mené à bien, puis un nouvel essai.
        if let Err(McpError::Rpc { code, data, .. }) = &outcome
            && *code == penelope_mcp::protocol::URL_ELICITATION_REQUIRED
            && self.links_completed(&tool.server, data.clone()).await
        {
            outcome = call(None, None).await;
        }
        let notes = self
            .services
            .elicitations
            .notes_since(&tool.server, started);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let now = self.now_ms();
        match outcome {
            Ok(mut result) => {
                if result.needs_input() {
                    result.is_error = true;
                    result.content.push(ContentBlock::Text {
                        text: format!(
                            "[Pénélope : `{}` demande encore une saisie après {rounds} \
                             échanges, appel abandonné.]",
                            tool.server
                        ),
                    });
                }
                for note in &notes {
                    result
                        .content
                        .push(ContentBlock::Text { text: note.clone() });
                }
                if result.is_error {
                    let said: Vec<&str> = result
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if let Some(hint) =
                        keychain_hint(&self.services, &slot.config(), &said.join("\n"))
                    {
                        result.content.push(ContentBlock::Text { text: hint });
                    }
                }
                let now_s = self.now();
                slot.info(|i| {
                    i.metrics.record(ms, !result.is_error);
                    i.last_used_ms = now;
                    i.last_ok = Some(now_s);
                    if i.state == ServerState::Degraded {
                        i.state = ServerState::Ready;
                    }
                });
                self.persist(&slot).await;
                Ok(result_json(&result))
            }
            Err(e) => {
                slot.info(|i| {
                    i.metrics.record(ms, false);
                    i.last_used_ms = now;
                });
                match &e {
                    McpError::Transport(_) => self.connection_lost(&slot, &e).await,
                    // Le serveur répond mais refuse nos requêtes : `mcp list` et `doctor`
                    // doivent le dire, un état « prêt » mentirait (issue #126).
                    McpError::Rpc { code, .. }
                        if *code == penelope_mcp::protocol::HEADER_MISMATCH =>
                    {
                        slot.info(|i| {
                            i.state = ServerState::Degraded;
                            i.last_error = Some(e.to_string());
                        });
                        self.persist(&slot).await;
                    }
                    McpError::Timeout { .. } => {
                        slot.info(|i| {
                            i.state = ServerState::Degraded;
                            i.last_error = Some(e.to_string());
                        });
                        self.persist(&slot).await;
                    }
                    _ => self.persist(&slot).await,
                }
                let mut message = format!("`{qualified}` : {}", call_error(&e));
                if let Some(hint) = keychain_hint(&self.services, &slot.config(), &message) {
                    message.push('\n');
                    message.push_str(&hint);
                }
                for note in &notes {
                    message.push('\n');
                    message.push_str(note);
                }
                Err(message)
            }
        }
    }

    /// Demande `elicitation/create` d'un serveur : présentée au propriétaire, résultat
    /// `ElicitResult`. Le mode URL n'est accepté que s'il a été annoncé.
    pub async fn elicit(
        &self,
        server: &str,
        params: &Value,
        url_allowed: bool,
    ) -> penelope_mcp::Result<Value> {
        let invalid = |message: String| McpError::Rpc {
            code: penelope_mcp::protocol::INVALID_PARAMS,
            message,
            data: None,
        };
        if params.get("mode").and_then(|m| m.as_str()) == Some("url") && !url_allowed {
            return Err(invalid(
                "mode `url` non annoncé pour cette version du protocole".into(),
            ));
        }
        let timeout = match self.slot(server).await {
            Some(slot) => slot.config().elicitation_duration(),
            None => Duration::from_secs(600),
        };
        self.services
            .elicitations
            .ask(server, params, timeout)
            .await
            .map(|o| o.result())
            .map_err(invalid)
    }

    /// Réponses aux `inputRequests` d'un résultat MRTR : élicitations et racines. Le
    /// sampling, jamais annoncé, reste sans réponse.
    async fn input_responses(
        &self,
        server: &str,
        requests: Option<&Value>,
        url_allowed: bool,
    ) -> Option<Value> {
        let requests = requests?.as_object()?;
        let mut out = serde_json::Map::new();
        for (key, request) in requests {
            let params = request.get("params").cloned().unwrap_or(json!({}));
            match request.get("method").and_then(|m| m.as_str()) {
                Some("elicitation/create") => {
                    if let Ok(v) = self.elicit(server, &params, url_allowed).await {
                        out.insert(key.clone(), v);
                    }
                }
                Some("roots/list") => {
                    let roots = match self.slot(server).await {
                        Some(slot) => self.roots_json(&slot.config().roots),
                        None => Vec::new(),
                    };
                    out.insert(key.clone(), json!({ "roots": roots }));
                }
                other => {
                    tracing::debug!(server, method = ?other, "demande MRTR sans réponse");
                }
            }
        }
        Some(Value::Object(out))
    }

    /// Erreur −32042 : chaque lien exigé est présenté au propriétaire ; vrai quand tous ont
    /// été acceptés et que le serveur en a signalé la fin.
    async fn links_completed(&self, server: &str, data: Option<Value>) -> bool {
        let Some(links) = data
            .as_ref()
            .and_then(|d| d.get("elicitations"))
            .and_then(|e| e.as_array())
            .filter(|l| !l.is_empty())
        else {
            return false;
        };
        let timeout = match self.slot(server).await {
            Some(slot) => slot.config().elicitation_duration(),
            None => Duration::from_secs(600),
        };
        let broker = &self.services.elicitations;
        for link in links {
            let Some(id) = link.get("elicitationId").and_then(|i| i.as_str()) else {
                return false;
            };
            match broker.ask(server, link, timeout).await {
                Ok(o) if o.accepted() => {
                    if !broker.wait_completion(server, id, timeout).await {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }

    /// Racines déclarées, telles que `roots/list` les rend.
    pub(super) fn roots_json(&self, roots: &[String]) -> Vec<Value> {
        roots
            .iter()
            .map(|r| {
                let path = self.services.platform.dirs.expand(r);
                json!({
                    "uri": format!("file://{}", path.display()),
                    "name": path.file_name().map(|n| n.to_string_lossy().to_string()),
                })
            })
            .collect()
    }

    /// État d'une tâche MCP (`tasks/get`) et, une fois terminée, son résultat
    /// (`tasks/result`, sinon celui que porte la tâche). `{status, task, result}`.
    pub async fn task_status(&self, server: &str, task_ref: &str) -> Result<Value, String> {
        let slot = self
            .slot(server)
            .await
            .ok_or_else(|| format!("serveur MCP inconnu : `{server}`"))?;
        let client = self.ensure_live(&slot).await?;
        let v = client
            .task_get(task_ref)
            .await
            .map_err(|e| format!("tasks/get `{task_ref}` : {e}"))?;
        let task = v.get("task").cloned().unwrap_or(v);
        let status = task["status"]
            .as_str()
            .or_else(|| task["state"].as_str())
            .unwrap_or("working")
            .to_string();
        let terminal =
            penelope_mcp::tasks::TaskState::parse(&status).is_some_and(|s| s.is_terminal());
        let result = if terminal && status != "cancelled" && status != "canceled" {
            match client.task_result(task_ref).await {
                Ok(r) => r,
                Err(McpError::Rpc { code, .. })
                    if code == penelope_mcp::protocol::METHOD_NOT_FOUND =>
                {
                    task.get("result").cloned().unwrap_or(Value::Null)
                }
                Err(e) => return Err(format!("tasks/result `{task_ref}` : {e}")),
            }
        } else {
            Value::Null
        };
        Ok(json!({"status": status, "task": task, "result": result}))
    }
}
