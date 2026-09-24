//! Progression des runs et orchestrateur offert aux outils.

use super::*;

/// Carte de progression du run : un message, mis à jour à chaque transition (§12.7).
pub(super) async fn progress(
    d: &Arc<Daemon>,
    run: &Run,
    wf: &Workflow,
    last: Option<(&Step, &StepResult)>,
) {
    let Some(m) = d.workflows.ports.messenger.get() else {
        return;
    };
    let origin = origin_of(d, &run.id).await;
    let (icon, state) = match run.state {
        RunState::Running => ("🔧", "en cours"),
        RunState::Paused => ("⏸", "en pause"),
        RunState::Blocked => ("⛔", "bloqué"),
        RunState::Done => ("✅", "terminé"),
        RunState::Failed => ("❌", "en échec"),
        RunState::Cancelled => ("🛑", "annulé"),
    };
    let mut text = format!("{icon} **{}** · `{}` · {state}", wf.metadata.name, run.id);
    if let Some(step) = run.current_step.as_ref().and_then(|id| wf.step(id)) {
        text.push_str(&format!(
            "\nÉtape : {} ({})",
            if step.name.is_empty() {
                &step.id
            } else {
                &step.name
            },
            step.phase.as_str()
        ));
    }
    text.push_str(&format!(
        "\nItérations : {}/{} · Coût : {:.2} $",
        run.iterations, run.max_iterations, run.spent_usd
    ));
    let brief = brief_of(&d.services, &run.id).await;
    if !brief.is_empty() {
        let short: String = brief.chars().take(BRIEF_CARD_CHARS).collect();
        let more = if brief.chars().count() > BRIEF_CARD_CHARS {
            "…"
        } else {
            ""
        };
        text.push_str(&format!("\nBrief : {}{more}", short.replace('\n', " ")));
    }
    if let Some((step, result)) = last {
        text.push_str(&format!(
            "\nDernière étape : `{}` → {}",
            step.id,
            result.as_str()
        ));
    }
    if let Some(e) = run.error.as_ref().filter(|e| !e.is_empty()) {
        text.push_str(&format!("\nRaison : {e}"));
    }
    if run.state == RunState::Blocked {
        text.push_str(&format!("\n\n`/resume {}` pour reprendre.", run.id));
    }
    if let Err(e) = m
        .upsert_card(&origin, &format!("run.{}", run.id), &text)
        .await
    {
        tracing::debug!(run = %run.id, error = %e, "carte de progression non envoyée");
    }
}

// ------------------------------------------------------------------ orchestrateur

/// Workflows, sous-agents et images, offerts aux outils (`workflow_start`, …).
pub struct WorkflowOrchestrator {
    pub daemon: Arc<Daemon>,
}

#[async_trait::async_trait]
impl crate::executor::Orchestrator for WorkflowOrchestrator {
    async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        crate::embeddings::query_vector(&self.daemon.embedder(), text).await
    }

    async fn start_workflow(
        &self,
        id: &str,
        params: Value,
        brief: Option<&str>,
        origin: &Origin,
    ) -> Result<Value, String> {
        let run = start_run_briefed(&self.daemon, id, params, origin, None, 0, brief).await?;
        // Le tour ne doit pas annoncer un état qu'il n'a pas vérifié (issue #154). Le
        // 21/09, « 🚀 Lancé — en cours » est parti dans le sujet pendant que le run mourait
        // à `git clone` quinze secondes plus tôt : l'outil avait rendu `running` avant que
        // la première étape ne tourne. On attend son verdict, au plus cinq secondes.
        let settled = first_verdict(&self.daemon, &run.id, Duration::from_secs(5)).await;
        let state = settled.as_ref().map_or(run.state, |r| r.state);
        let mut out = json!({"run_id": run.id, "state": state.as_str(), "workflow": id});
        match state {
            RunState::Blocked | RunState::Failed => {
                let step = settled
                    .as_ref()
                    .and_then(|r| r.step_outputs.get("__last"))
                    .and_then(|v| v.get("error").or_else(|| v.get("stderr")))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                out["remarque"] = json!(format!(
                    "Le run est déjà `{}` : ne l'annonce pas « en cours ». Dis ce qui a \
                     échoué{} et renvoie à la carte du run pour réessayer ou passer \
                     l'étape.",
                    state.as_str(),
                    if step.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", penelope_observe::redact(step.trim()))
                    }
                ));
            }
            RunState::Running if settled.is_some() => {
                out["remarque"] = json!(
                    "Première étape passée, le run continue. Relaie cet état tel quel : \
                     n'invente pas la liste des étapes à venir ni leur avancement."
                );
            }
            _ => {}
        }
        Ok(out)
    }

    async fn spawn_sub_agent(
        &self,
        session_id: &str,
        prompt: &str,
        model: Option<&str>,
        tools: Vec<String>,
        origin: &Origin,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        let cfg = self.daemon.services.config.config();
        let alias = model
            .map(String::from)
            .unwrap_or_else(|| cfg.role_alias("chat_default"));
        let model_id = cfg
            .alias_model(&alias)
            .map(String::from)
            .or_else(|| alias.contains(':').then(|| alias.clone()))
            .ok_or_else(|| format!("alias de modèle inconnu `{alias}`"))?;
        // Le sous-agent hérite du périmètre de son tour : l'abonnement ChatGPT sert ceux
        // du propriétaire, pas une planification qui passerait par là (#142).
        let model_id =
            crate::codex_scope::for_origin(&self.daemon.services, &model_id, origin).await;
        let text = run_sub_agent(
            &self.daemon,
            SubAgentTask {
                session_id,
                run_id: None,
                kind: "general",
                prompt,
                model_id: &model_id,
                tools: &tools,
                workspaces: crate::executor::default_workspaces(&self.daemon.services),
            },
            // Jeton enfant : `/stop` sur le tour parent arrête le sous-agent, et un
            // sous-agent qui s'arrête ne touche pas au parent (issue #57).
            &cancel.child(),
        )
        .await?;
        Ok(json!({"text": text, "model": model_id}))
    }

    async fn generate_image(&self, prompt: &str, size: Option<&str>) -> Result<Value, String> {
        let d = &self.daemon;
        crate::images::generate(&d.services, d.providers.as_ref(), prompt, size).await
    }

    async fn inspect_image(
        &self,
        session_id: &str,
        path: &std::path::Path,
        task: crate::vision::Task,
        question: &str,
    ) -> Result<Value, String> {
        let d = &self.daemon;
        crate::vision::inspect(
            &d.services,
            d.providers.as_ref(),
            session_id,
            path,
            task,
            question,
        )
        .await
    }

    async fn control_run(&self, run_id: &str, op: &str) -> Result<Value, String> {
        let parsed = penelope_workflow::Control::parse(op)
            .ok_or_else(|| format!("opération inconnue : {op}"))?;
        // Passer une étape ou sauter ailleurs reste une décision du propriétaire (§12.7).
        if parsed.requires_approval() {
            return Err(format!(
                "`{op}` demande l'accord du propriétaire : `penelope wf control {run_id} {op}`"
            ));
        }
        let state = control(&self.daemon, run_id, &parsed)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({"run": run_id, "state": state.as_str()}))
    }
}
