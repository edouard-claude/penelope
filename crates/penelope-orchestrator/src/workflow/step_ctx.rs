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
        self.executor_in(&self.run.session_id).await
    }

    /// Exécuteur des outils d'une étape qui parle dans `session` (la session neuve d'une
    /// étape `context: fresh`, sinon celle du run).
    pub(super) async fn executor_in(&self, session: &str) -> NativeToolExecutor {
        let d = self.d;
        let mut workspaces = vec![self.workdir()];
        workspaces.extend(penelope_executor::executor::default_workspaces(self.s()));
        let mut exec = NativeToolExecutor::new(
            d.services.clone(),
            ToolEnv {
                session_id: session.to_string(),
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

    /// Modèle d'une étape : celui qu'elle nomme, sinon son rôle par la résolution unique
    /// (#332), le rôle `workflow` pour une étape sans rôle que le profil connaisse.
    pub(super) fn model(&self, role: &str) -> Result<String, String> {
        let cfg = self.s().config.config();
        let alias = if !self.step.model.is_empty() {
            self.step.model.clone()
        } else {
            cfg.role_alias(step_role(&cfg, role))
        };
        cfg.alias_model(&alias)
            .map(String::from)
            .ok_or_else(|| format!("alias de modèle inconnu `{alias}`"))
    }
}

impl StepCtx<'_> {
    /// Garde Codex d'une étape (#333) : un run lancé par le propriétaire (gate « vas-y »,
    /// `/run`, CLI, outil de son tour) est son tour, il suit le principal ; seul un run
    /// planifié passe par la garde, annoncée dans la conversation du run.
    pub(super) async fn guard(&self, model_id: &str) -> String {
        let s = self.s();
        if scheduled_by(s, &self.run.id).await.is_none() {
            return model_id.to_string();
        }
        let origin = origin_of(s, &self.run.id).await;
        penelope_app::codex_scope::guarded(
            s,
            model_id,
            "ce workflow",
            "tâche planifiée",
            Some(&origin),
        )
        .await
    }
}

/// Replis d'un modèle d'étape, par la chaîne du profil actif (#335) : une étape
/// n'avait aucun repli, une panne du principal la faisait échouer.
pub(super) fn fallbacks_of(cfg: &penelope_kernel::config::Config, model_id: &str) -> Vec<String> {
    cfg.fallback_labels(model_id)
        .iter()
        .filter_map(|l| cfg.alias_model(l).map(String::from))
        .filter(|m| m != model_id)
        .collect()
}

/// Le rôle d'une étape : le sien s'il est connu (`code`, une surcharge du profil),
/// `workflow` sinon.
pub(super) fn step_role<'a>(cfg: &penelope_kernel::config::Config, role: &'a str) -> &'a str {
    let known = penelope_kernel::config::role_spec(role).is_some()
        || cfg.models.active().overrides.contains_key(role);
    if known && role != "chat_default" {
        role
    } else {
        "workflow"
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
            "delivery" => delivery_step(ctx).await,
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
