//! Contexte d'une étape, rendu des gabarits et aiguillage par type d'étape.

use super::*;

pub(super) struct StepCtx<'a> {
    pub(super) d: &'a Context,
    pub(super) run: &'a Run,
    pub(super) wf: &'a Workflow,
    pub(super) step: &'a Step,
    pub(super) attempt: u32,
    pub(super) cancel: &'a CancelToken,
}

impl StepCtx<'_> {
    pub(super) fn s(&self) -> &Services {
        &self.d.services
    }

    pub(super) fn workdir(&self) -> std::path::PathBuf {
        self.run
            .workdir
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                self.s()
                    .platform
                    .dirs
                    .state()
                    .join("runs")
                    .join(&self.run.id)
            })
    }

    /// Substitue les variables `{{…}}` d'un texte (§12.5).
    /// Rend un gabarit sans citation : prompt, `cwd`, champ JSON.
    pub(super) async fn render(&self, template: &str) -> String {
        self.render_quoted(template, penelope_workflow::conditions::Quoting::Raw)
            .await
    }

    async fn render_quoted(
        &self,
        template: &str,
        quoting: penelope_workflow::conditions::Quoting,
    ) -> String {
        let metadata = session_metadata(self.s(), &self.run.session_id).await;
        let workdir = self.workdir().to_string_lossy().to_string();
        let now = self.s().clock.now_rfc3339();
        let last = self
            .run
            .step_outputs
            .get("__last")
            .cloned()
            .unwrap_or(Value::Null);
        let reason = last
            .get("input")
            .or_else(|| last.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let brief = brief_of(self.s(), &self.run.id).await;
        let vars = TemplateVars {
            workdir: &workdir,
            reason: &reason,
            run_id: &self.run.id,
            now: &now,
            os: self.s().platform.os_name(),
            arch: std::env::consts::ARCH,
            params: &self.run.params,
            step_output: &last,
            steps: &self.run.step_outputs,
            metadata: &metadata,
            criteria_key: &self.step.criteria_key,
            brief: &brief,
        };
        let (out, unknown) =
            penelope_workflow::conditions::substitute_with(template, &vars, quoting);
        if !unknown.is_empty() {
            tracing::warn!(run = %self.run.id, step = %self.step.id, ?unknown, "variables inconnues");
        }
        out
    }

    /// Rend un gabarit destiné à un shell : les valeurs substituées y sont citées, sauf si
    /// l'étape le refuse (`quote: false`). Sans cela, un `{{workdir}}` qui contient une
    /// espace — « Application Support » sur tout Mac — casse la commande (issue #154).
    pub(super) async fn render_command(&self, template: &str) -> String {
        let quoting = if self.step.quote {
            penelope_workflow::conditions::Quoting::Shell
        } else {
            penelope_workflow::conditions::Quoting::Raw
        };
        self.render_quoted(template, quoting).await
    }

    /// Substitue récursivement les chaînes d'une valeur JSON.
    pub(super) async fn render_json(&self, v: &Value) -> Value {
        match v {
            Value::String(t) => Value::String(self.render(t).await),
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for i in items {
                    out.push(Box::pin(self.render_json(i)).await);
                }
                Value::Array(out)
            }
            Value::Object(m) => {
                let mut out = serde_json::Map::new();
                for (k, x) in m {
                    out.insert(k.clone(), Box::pin(self.render_json(x)).await);
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }

    pub(super) async fn executor(&self) -> NativeToolExecutor {
        let d = self.d;
        let mut workspaces = vec![self.workdir()];
        workspaces.extend(crate::executor::default_workspaces(self.s()));
        let mut exec = NativeToolExecutor::new(
            d.services.clone(),
            ToolEnv {
                session_id: self.run.session_id.clone(),
                run_id: Some(self.run.id.clone()),
                origin: origin_of(self.s(), &self.run.id).await,
                workspaces,
                in_workflow: true,
                turn_model: None,
            },
        );
        exec.admin = d.admin.clone();
        let ports = &d.workflows.ports;
        exec.messenger = ports.messenger.get();
        exec.mcp = ports.mcp.get();
        exec.orchestrator = ports.orchestrator.get();
        exec
    }

    /// Modèle d'une étape : alias explicite, sinon le rôle de l'agent, sinon `main`.
    pub(super) fn model(&self, role: &str) -> Result<String, String> {
        let cfg = self.s().config.config();
        let alias = if !self.step.model.is_empty() {
            self.step.model.clone()
        } else if cfg.models.roles.contains_key(role) {
            cfg.role_alias(role)
        } else {
            cfg.role_alias("chat_default")
        };
        cfg.alias_model(&alias)
            .map(String::from)
            .ok_or_else(|| format!("alias de modèle inconnu `{alias}`"))
    }
}

pub(super) async fn execute_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let timeout = ctx.step.timeout_ms.map(Duration::from_millis);
    let fut = async {
        match ctx.step.kind.as_str() {
            "agent" => agent_step(ctx).await,
            "sub_agent" => sub_agent_step(ctx).await,
            "shell" => shell_step(ctx).await,
            "tool" => tool_step(ctx).await,
            "user" => user_step(ctx).await,
            "parallel" => parallel_step(ctx).await,
            "workflow" => workflow_step(ctx).await,
            "wait" => wait_step(ctx).await,
            "verify" => verify_step(ctx).await,
            other => Ok(done(
                StepResult::Error,
                json!({"error": format!("type d'étape inconnu `{other}`")}),
            )),
        }
    };
    // Une attente se mesure elle-même ; les autres étapes sont bornées ici.
    match timeout {
        Some(t) if !matches!(ctx.step.kind.as_str(), "wait" | "user" | "workflow") => {
            match tokio::time::timeout(t, fut).await {
                Ok(r) => r,
                Err(_) => {
                    // Le jeton de l'étape seulement : le run continue et sa transition
                    // `step_result = timeout` décide de la suite (issue #56).
                    ctx.cancel.cancel();
                    Ok(done(
                        StepResult::Timeout,
                        json!({"error": format!("étape arrêtée après {} ms", t.as_millis())}),
                    ))
                }
            }
        }
        _ => fut.await,
    }
}
