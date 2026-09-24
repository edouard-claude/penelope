//! Validation d'un appel, explication des erreurs et implémentation de `ToolExecutor`.

use super::*;

impl NativeToolExecutor {
    /// Exécute, et rend une erreur d'arguments ou de nom avec de quoi se corriger.
    async fn run(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> Result<ToolOutcome, ToolError> {
        let started = std::time::Instant::now();
        let result = match self.dispatch(name, args, cancel).await {
            Err(e) => Err(self.explain(name, args, e).await),
            Ok(o) => {
                // Un secret lu (fichier, sortie de commande, page) devient une valeur
                // connue : recopié plus tard dans une commande ou un message, il est
                // masqué partout où la rédaction passe (issue #134). Rien n'est modifié
                // de ce qui s'exécute.
                penelope_observe::redact::learn_secrets(&o.text);
                Ok(o)
            }
        };
        let (tool, effective_args) = if name == "tool_call" {
            (
                args.get("name").and_then(Value::as_str).unwrap_or(name),
                effective_arguments(name, args),
            )
        } else {
            (name, args.clone())
        };
        let (ok, output) = match &result {
            Ok(outcome) => (!outcome.is_error, outcome.value.clone()),
            Err(error) => (false, json!({"error": error.to_string()})),
        };
        let payload = json!({
            "tool": tool,
            "args": crate::runtime_events::bounded_redacted(&crate::agent::without_intention(&effective_args)),
            "result": crate::runtime_events::bounded_redacted(&output),
            "ok": ok,
            "duration_ms": started.elapsed().as_millis() as u64,
            "cost_usd_estimated": if tool.starts_with("mcp__") { Value::Null } else { json!(0.0) },
        });
        let mut event = penelope_kernel::event::EventDraft::new("runtime.tool", payload)
            .session(&self.env.session_id);
        if let Some(run_id) = &self.env.run_id {
            event = event.run(run_id);
        }
        if let Err(error) = self.services.events.append(event).await {
            tracing::warn!(%error, tool, "événement d'outil non enregistré");
        }
        result
    }

    /// Arguments d'un appel, validés sans rien exécuter (issue #117) : balisage laissé
    /// par le modèle, outil connu, schéma natif ou MCP.
    async fn validate_call(&self, name: &str, args: &Value) -> ToolResult<()> {
        if let Some((path, marker)) = penelope_tools::call_markup(args) {
            let field = if path.is_empty() {
                "arguments".to_string()
            } else {
                format!("`{path}`")
            };
            return Err(ToolError::Invalid(format!(
                "la valeur de {field} contient le balisage d'appel d'outil du modèle \
                 (`{marker}`) : l'appel est mal formé, ce n'est pas une valeur ; renvoie les \
                 arguments en JSON structuré, selon leur type"
            )));
        }
        let (target, inner) = if name == "tool_call" {
            (
                args.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                call_arguments(args)?,
            )
        } else {
            (name.to_string(), args.clone())
        };
        match target.as_str() {
            "tool_search" | "tool_describe" | "tool_call" => Ok(()),
            "git_clone" => {
                penelope_tools::validate_args(&target, &inner)?;
                penelope_tools::git::normalize_clone_url(&str_arg(&inner, "url")?)?;
                Ok(())
            }
            t if penelope_tools::tool_spec(t).is_some() => penelope_tools::validate_args(t, &inner),
            t if t.starts_with("mcp__") || name == "tool_call" => self
                .services
                .mcp_tools
                .validate_args(t, &crate::agent::without_intention(&inner))
                .await
                .map_err(|e| match e {
                    penelope_mcp::McpError::UnknownTool(q) => ToolError::Unknown(q),
                    penelope_mcp::McpError::InvalidArguments { reason, .. } => {
                        ToolError::Invalid(reason)
                    }
                    other => ToolError::Invalid(other.to_string()),
                }),
            t => Err(ToolError::Unknown(t.to_string())),
        }
    }

    /// Schéma d'arguments d'un outil natif ou MCP.
    async fn schema_of(&self, tool: &str) -> Option<Value> {
        if let Some(t) = penelope_tools::tool_spec(tool) {
            return Some(t.schema);
        }
        self.services
            .mcp_tools
            .get(tool)
            .await
            .ok()
            .flatten()
            .map(|t| t.input_schema)
    }

    /// Issue #110 : une erreur d'arguments porte les paramètres attendus (outil natif ou
    /// MCP, borné) ; un `tool_call` arrivé sans arguments le dit comme tel ; un nom inconnu
    /// rend les noms proches. Le filet reste la garde de boucle, qui compare les appels.
    async fn explain(&self, name: &str, args: &Value, e: ToolError) -> ToolError {
        let via_call = name == "tool_call";
        let target = if via_call {
            args.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        } else {
            name.to_string()
        };
        // `args` vide et pas de repli en chaîne : les arguments ont pu se perdre en route.
        let lost = via_call
            && call_arguments(args)
                .ok()
                .and_then(|v| v.as_object().map(|o| o.is_empty()))
                .unwrap_or(false);
        let lost_note = |reason: String, schema: &Value| -> String {
            if !lost {
                return reason;
            }
            let required: Vec<String> = schema["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(|r| format!("`{r}`"))
                        .collect()
                })
                .unwrap_or_default();
            if required.is_empty() {
                return reason;
            }
            format!(
                "`tool_call` est arrivé avec `args` vide alors que {} est requis. Si tu \
                 l'avais rempli, les arguments ont été perdus en route : renvoie-les dans \
                 `args_json`, en chaîne JSON (par exemple \"{{\\\"{}\\\": …}}\") ; sinon, \
                 ajoute-les ({reason})",
                required.join(", "),
                required[0].trim_matches('`')
            )
        };
        match e {
            ToolError::Invalid(reason) if !target.is_empty() => match self.schema_of(&target).await
            {
                Some(schema) => ToolError::BadArguments {
                    reason: lost_note(reason, &schema),
                    expected: penelope_tools::expected_args(
                        &schema,
                        penelope_tools::EXPECTED_ARGS_MAX_CHARS,
                    ),
                    tool: target,
                },
                None => ToolError::Invalid(reason),
            },
            ToolError::BadArguments {
                tool,
                reason,
                expected,
            } => {
                let reason = match self.schema_of(&tool).await {
                    Some(schema) => lost_note(reason, &schema),
                    None => reason,
                };
                ToolError::BadArguments {
                    tool,
                    reason,
                    expected,
                }
            }
            ToolError::Unknown(n) | ToolError::NoSuchTool { name: n, .. } => {
                let mut names: Vec<String> = penelope_tools::all_tools()
                    .iter()
                    .map(|t| t.name.to_string())
                    .collect();
                names.extend(self.services.mcp_tools.names().await.unwrap_or_default());
                ToolError::NoSuchTool {
                    close: penelope_tools::close_names(&n, names.iter().map(String::as_str), 5),
                    name: n,
                }
            }
            other => other,
        }
    }
}

/// `cd <répertoire> && <commande>` rendu en `{command: <commande>, cwd: <répertoire>}`
/// quand le répertoire est dans un workspace et que l'appel ne donne pas déjà son `cwd`
/// (issue #123) : la commande se classe, s'approuve et se règle pour ce qu'elle est. Hors
/// des workspaces, la ligne reste composée, donc demandée.
pub(crate) fn lift_cd(args: &Value, workspaces: &[PathBuf]) -> Option<Value> {
    let obj = args.as_object()?;
    if obj
        .get("cwd")
        .and_then(|v| v.as_str())
        .is_some_and(|c| !c.trim().is_empty())
    {
        return None;
    }
    let (dir, rest) = penelope_tools::shell::split_cd_prefix(obj.get("command")?.as_str()?)?;
    let cwd = penelope_tools::fs::resolve(&dir, workspaces).ok()?;
    let mut out = obj.clone();
    out.insert("command".into(), json!(rest));
    out.insert("cwd".into(), json!(cwd.to_string_lossy()));
    Some(Value::Object(out))
}

#[async_trait::async_trait]
impl ToolExecutor for NativeToolExecutor {
    fn policy_workspace(&self) -> Option<PathBuf> {
        self.workspaces().into_iter().next()
    }

    /// Même session, même origine, mêmes branchements : ce que le job exécutera hors du
    /// tour est l'exécuteur du tour, sans son emprunt (issue #204).
    fn detached(&self) -> Option<Arc<dyn ToolExecutor + Send + Sync>> {
        let mut copy = NativeToolExecutor::new(self.services.clone(), self.env.clone());
        copy.locks = self.locks.clone();
        copy.messenger = self.messenger.clone();
        copy.mcp = self.mcp.clone();
        copy.orchestrator = self.orchestrator.clone();
        copy.admin = self.admin.clone();
        Some(Arc::new(copy))
    }

    fn normalise_call(&self, name: &str, args: &Value) -> Option<Value> {
        let workspaces = self.workspaces();
        match name {
            "shell_exec" => lift_cd(args, &workspaces),
            // Par `tool_call`, l'appel interne ; `args_json` cède la place à l'objet.
            "tool_call" if args.get("name").and_then(|v| v.as_str()) == Some("shell_exec") => {
                let inner = lift_cd(&call_arguments(args).ok()?, &workspaces)?;
                let mut out = args.as_object()?.clone();
                out.remove("args_json");
                out.insert("args".into(), inner);
                Some(Value::Object(out))
            }
            _ => None,
        }
    }

    async fn precheck(&self, name: &str, args: &Value) -> Result<(), ToolError> {
        match self.validate_call(name, args).await {
            Ok(()) => Ok(()),
            Err(e) => Err(self.explain(name, args, e).await),
        }
    }

    async fn execute(&self, name: &str, args: &Value) -> Result<ToolOutcome, ToolError> {
        self.run(name, args, &penelope_llm::CancelToken::new())
            .await
    }

    async fn execute_cancellable(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> Result<ToolOutcome, ToolError> {
        self.run(name, args, cancel).await
    }

    async fn describe_call(&self, name: &str, args: &Value) -> CallInfo {
        let shell_network = self.services.config.config().sandbox.shell_network;
        let s = &self.services;
        let mcp_info = |q: String| async move {
            let risk = match s.mcp_tools.get(&q).await {
                Ok(Some(t)) => t.risk,
                _ => RiskClass::Unknown,
            };
            let policy = match &self.mcp {
                Some(gw) => gw
                    .tool_policy(&q)
                    .await
                    .and_then(|p| penelope_kernel::risk::PolicyDecision::parse(&p)),
                None => None,
            };
            CallInfo {
                idempotent: risk == RiskClass::Read,
                effective_name: q,
                risk,
                policy,
            }
        };
        match name {
            "tool_search" | "tool_describe" => CallInfo {
                effective_name: name.to_string(),
                risk: RiskClass::Read,
                idempotent: true,
                policy: None,
            },
            "tool_call" => {
                let q = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool_call")
                    .to_string();
                // Un natif par `tool_call` garde sa classe de risque et sa politique : la
                // même carte qu'un appel direct (#104).
                if penelope_tools::tool_spec(&q).is_some() {
                    let inner = effective_arguments("tool_call", args);
                    return native_info(&q, &inner, shell_network);
                }
                mcp_info(q).await
            }
            n if n.starts_with("mcp__") => mcp_info(n.to_string()).await,
            _ => native_info(name, args, shell_network),
        }
    }
}
