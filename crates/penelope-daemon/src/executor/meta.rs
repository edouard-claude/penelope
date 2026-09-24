//! Méta-outils (`tool_search`, `tool_describe`, `tool_call`, MCP) et réponses web.

use super::*;

impl NativeToolExecutor {
    /// Réponse de `http_fetch` telle que le modèle la lit : une page HTML devient du texte
    /// lisible et le brut reste relisible en artefact (issue #8) ; le tout encadré comme
    /// donnée non fiable (§13.3).
    pub(super) async fn fetched_page(&self, mut v: Value, url: &str) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let content_type = v["contentType"].as_str().unwrap_or_default().to_string();
        let body = v["body"].as_str().unwrap_or_default().to_string();
        if penelope_tools::html::looks_like_html(&content_type, &body) {
            let base = v["url"].as_str().and_then(|u| url::Url::parse(u).ok());
            let text = penelope_tools::html::to_text(&body, base.as_ref());
            let raw = s
                .context
                .history
                .put_artifact(
                    Some(&self.env.session_id),
                    self.env.run_id.as_deref(),
                    "html",
                    None,
                    &body,
                )
                .await?;
            v["body"] = json!(text);
            v["format"] = json!("texte extrait du HTML");
            v["raw_artifact"] = json!(raw.id);
        }
        // Une forge dont le client est connecté : le dire au lieu de laisser le modèle
        // réapprendre à chaque sujet (issue #156). La lecture a déjà eu lieu, rien n'est
        // bloqué — c'est une remarque, comme celle du réseau coupé (#106).
        let hint = match crate::machine::cached(s).await {
            Some(inv) => crate::machine::forge_hint(&inv, url),
            None => None,
        };
        if let Some(h) = &hint {
            v["remarque"] = json!(h);
        }
        let mut shown = penelope_tools::render(&v);
        if let Some(id) = v["raw_artifact"].as_str() {
            shown.push_str(&format!("\n\n[HTML d'origine : artifact_read(\"{id}\")]"));
        }
        let mut o = ToolOutcome::ok(v);
        o.text = penelope_observe::injection::wrap_untrusted(&format!("http_fetch {url}"), &shown);
        Ok(o)
    }

    /// Vrai si `citation` figure mot pour mot (espaces et casse près) dans un message du
    /// propriétaire du tour en cours : messages utilisateur depuis la dernière réponse
    /// finale, hors déclencheurs, relances et contenus transférés.
    pub(super) async fn cited_by_owner(&self, citation: &str) -> ToolResult<bool> {
        let norm = |t: &str| {
            t.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let needle = norm(citation);
        if needle.chars().count() < 8 || matches!(self.env.origin, Origin::Internal { .. }) {
            return Ok(false);
        }
        let s = &self.services;
        let sid = self.env.session_id.clone();
        let last: i64 = s
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1",
                    [sid],
                    |r| r.get(0),
                )?)
            })
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let entries = s
            .context
            .history
            .load(&self.env.session_id, (last - 60).max(0))
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        for e in entries.iter().rev() {
            let m = &e.message;
            match m.role {
                penelope_llm::types::Role::Assistant if m.tool_calls.is_empty() => break,
                penelope_llm::types::Role::User => {
                    let text = m.text();
                    if text.starts_with("[déclencheur planifié]")
                        || text.starts_with("[relance]")
                        || text.contains("<<<DONNÉES NON FIABLES")
                    {
                        continue;
                    }
                    if norm(&text).contains(&needle) {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        Ok(false)
    }

    pub(super) async fn tool_search(&self, args: &Value) -> ToolResult<ToolOutcome> {
        let q = str_arg(args, "query")?;
        let vector = match &self.orchestrator {
            Some(o) => o.embed_query(&q).await,
            None => None,
        };
        let limit = u_arg(args, "limit").unwrap_or(10);
        let server = args.get("server").and_then(|v| v.as_str());
        // Outils natifs à la demande d'abord (#104) : leur description est la nôtre.
        let natives: Vec<Value> = if server.is_none() || server == Some("natif") {
            penelope_tools::search_on_demand(&q, limit)
                .into_iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "server": "natif",
                        "description": t.description.chars().take(200).collect::<String>(),
                        "risk": t.risk.as_str(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        let hits = self
            .services
            .mcp_tools
            .search_hybrid(&q, vector.as_deref(), server, limit)
            .await?;
        if hits.is_empty() && natives.is_empty() {
            return Ok(ToolOutcome::ok(json!({
                "résultats": [],
                "remarque": "aucun outil ne correspond ; les serveurs MCP : /mcp",
            })));
        }
        if hits.is_empty() {
            return Ok(ToolOutcome::ok(json!(natives)));
        }
        // Descriptions écrites par les serveurs : encadrées comme tout contenu observé,
        // avec l'alerte du détecteur local s'il y voit une consigne (#92).
        let mut all = natives;
        all.extend(hits.iter().map(|h| h.tool.short()));
        Ok(untrusted_listing("mcp tool_search", json!(all)))
    }

    pub(super) async fn tool_describe(&self, args: &Value) -> ToolResult<ToolOutcome> {
        let names: Vec<String> = args
            .get("names")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() {
            return Err(ToolError::Invalid("`names` est vide".into()));
        }
        // Natifs : schéma du catalogue, et l'outil décrit rejoint la liste de la session.
        let (natives, mcp): (Vec<String>, Vec<String>) = names
            .into_iter()
            .partition(|n| penelope_tools::tool_spec(n).is_some());
        let mut described: Vec<Value> = Vec::new();
        for n in &natives {
            if let Some(t) = penelope_tools::tool_spec(n) {
                crate::tools_on_demand::touch(&self.services, &self.env.session_id, n).await;
                described.push(json!({
                    "name": t.name,
                    "server": "natif",
                    "description": t.description,
                    "inputSchema": t.schema,
                    "risk": t.risk.as_str(),
                }));
            }
        }
        if mcp.is_empty() {
            return Ok(ToolOutcome::ok(json!(described)));
        }
        let v = self.services.mcp_tools.describe(&mcp).await?;
        described.extend(v);
        Ok(untrusted_listing("mcp tool_describe", json!(described)))
    }

    /// `tool_call` : un outil natif (à la demande ou non) part par le même chemin qu'un
    /// appel direct ; sinon, un outil MCP.
    pub(super) async fn tool_call(
        &self,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let name = str_arg(args, "name")?;
        let inner = call_arguments(args)?;
        if penelope_tools::tool_spec(&name).is_some() {
            return self.dispatch(&name, &inner, cancel).await;
        }
        self.mcp_call(&name, &inner).await
    }

    pub(super) async fn mcp_call(&self, qualified: &str, args: &Value) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        // L'intention est pour la carte, pas pour le serveur (#116).
        let args = &crate::agent::without_intention(args);
        // Refus local, sans aller au serveur : `explain` y joint son schéma (#110).
        s.mcp_tools
            .validate_args(qualified, args)
            .await
            .map_err(|e| match e {
                penelope_mcp::McpError::UnknownTool(q) => ToolError::Unknown(q),
                penelope_mcp::McpError::InvalidArguments { reason, .. } => {
                    ToolError::Invalid(reason)
                }
                other => ToolError::Invalid(other.to_string()),
            })?;
        s.mcp_tools.mark_for_promotion(&[qualified.to_string()]);
        let gw = self
            .mcp
            .as_ref()
            .ok_or_else(|| ToolError::Other("aucun serveur MCP n'est démarré".into()))?;
        let v = gw
            .call_tool(qualified, args, self.elicitation_destination())
            .await
            .map_err(ToolError::Other)?;
        let is_error = v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false);
        let text = penelope_observe::injection::wrap_untrusted(qualified, &render_mcp_result(&v));
        Ok(ToolOutcome {
            value: v,
            is_error,
            text,
            eager: true,
        })
    }
}
