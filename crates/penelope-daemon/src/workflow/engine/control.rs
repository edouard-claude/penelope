//! Contrôle d'un run (pause, reprise, arrêt) et réponses du propriétaire.

use super::*;

/// Applique une opération de contrôle puis réveille le pilote.
pub async fn control(
    d: &Context,
    run_id: &str,
    op: &penelope_workflow::Control,
) -> anyhow::Result<RunState> {
    use penelope_workflow::Control;
    let s = &d.services;
    let run = s
        .runs
        .get(run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run {run_id} introuvable"))?;
    if run.state.is_terminal() {
        anyhow::bail!("run {run_id} déjà {}", run.state.as_str());
    }
    // Reprendre un run toujours au-dessus de sa borne le re-bloquerait dans la seconde :
    // on le dit au lieu de le faire (issue #136).
    if *op == Control::Resume
        && run.state == RunState::Blocked
        && let Some(wf) = s.workflows.get(&run.workflow_id)
    {
        let current = refresh_spent(s, run.clone()).await?;
        let budget = effective_budget(s, &current, &wf.settings.budget).await;
        let limit = check_limits(&current, &budget, s.clock.now_ms());
        if matches!(limit, Limit::BudgetUsd | Limit::BudgetTokens) {
            anyhow::bail!(
                "toujours bloqué : {}",
                limit_reason(&limit, &current, &budget)
            );
        }
    }
    let state = match op {
        Control::Pause | Control::Cancel => {
            let st = s.runs.control(run_id, op).await?;
            d.workflows.interrupt(run_id);
            if *op == Control::Cancel {
                finish(d, &run, RunState::Cancelled, "annulé par le propriétaire").await?;
            }
            st
        }
        Control::RetryStep => {
            // La visite repart de zéro : nouveaux effets (tentative suivante), nouvelle question.
            if let Some(step) = &run.current_step {
                let attempt_key = visit_key("attempt", &run, step);
                let n: u32 = s
                    .kv_get(&attempt_key)
                    .await?
                    .and_then(|a| a.parse().ok())
                    .unwrap_or(0);
                for what in ["agent", "answer", "asked", "wait", "child", "approval"] {
                    kv_delete_prefix(s, &visit_key(what, &run, step)).await?;
                }
                s.kv_set(&attempt_key, &(n + 1).to_string()).await?;
            }
            s.runs.control(run_id, op).await?
        }
        Control::SkipStep => {
            let Some(step_id) = run.current_step.clone() else {
                anyhow::bail!("aucune étape à passer");
            };
            let wf = s
                .workflows
                .get(&run.workflow_id)
                .ok_or_else(|| anyhow::anyhow!("workflow retiré du registre"))?;
            let step = wf
                .step(&step_id)
                .ok_or_else(|| anyhow::anyhow!("étape `{step_id}` absente"))?;
            let result = StepResult::Choice("skipped".into());
            let output = json!({"skipped": true});
            let metadata = session_metadata(s, &run.session_id).await;
            let next = choose(
                &step.transitions,
                &EvalContext {
                    step_result: &result,
                    step_output: &output,
                    metadata: &metadata,
                },
            );
            s.runs.set_state(run_id, RunState::Running, None).await?;
            let phase = wf.step(&next).map(|n| n.phase.as_str());
            let advanced = s
                .runs
                .advance(run_id, &step_id, &result, output, &next, phase)
                .await?;
            advanced.state
        }
        other => s.runs.control(run_id, other).await?,
    };
    let _ = s
        .events
        .append(EventDraft::new(
            "workflow.control",
            json!({"run": run_id, "op": format!("{op:?}"), "state": state.as_str()}),
        ))
        .await;
    d.workflows.wake();
    Ok(state)
}

/// Enregistre la réponse du propriétaire à une étape `user` et réveille le run.
pub async fn answer(
    d: &Context,
    run_id: &str,
    visit: &str,
    choice: &str,
    input: Option<&str>,
) -> anyhow::Result<()> {
    let s = &d.services;
    let run = s
        .runs
        .get(run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run {run_id} introuvable"))?;
    let step = run
        .current_step
        .clone()
        .ok_or_else(|| anyhow::anyhow!("le run n'attend plus de réponse"))?;
    let expected = format!("{step}.{}", run.iterations);
    if visit != expected {
        anyhow::bail!("cette question n'est plus d'actualité");
    }
    let wf = s
        .workflows
        .get(&run.workflow_id)
        .ok_or_else(|| anyhow::anyhow!("workflow retiré du registre"))?;
    let def = wf
        .step(&step)
        .ok_or_else(|| anyhow::anyhow!("étape `{step}` absente"))?;
    if def.kind != "user" {
        anyhow::bail!("l'étape `{step}` n'attend pas de réponse");
    }
    if !def.choices.is_empty() && !def.choices.iter().any(|c| c == choice) {
        anyhow::bail!(
            "choix inconnu `{choice}` (choix : {})",
            def.choices.join(", ")
        );
    }
    // Un formulaire arrive en objet JSON, validé contre son schéma : l'étape suivante lit
    // `stepOutput.input.<champ>`.
    let input: Value = match def.input.strip_prefix("form:") {
        Some(id) => {
            let schema = wf
                .settings
                .forms
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("formulaire `{id}` absent du workflow"))?;
            let raw = input.ok_or_else(|| anyhow::anyhow!("formulaire `{id}` non rempli"))?;
            let v: Value = serde_json::from_str(raw)
                .map_err(|e| anyhow::anyhow!("formulaire `{id}` : JSON illisible ({e})"))?;
            penelope_kernel::schema::validate_ok(schema, &v)
                .map_err(|e| anyhow::anyhow!("formulaire `{id}` : {e}"))?;
            v
        }
        None => input.map(|t| json!(t)).unwrap_or(Value::Null),
    };
    s.kv_set(
        &visit_key("answer", &run, &step),
        &json!({"choice": choice, "input": input}).to_string(),
    )
    .await?;
    d.workflows.wake();
    Ok(())
}

/// Schéma du formulaire qu'attend la question `visit` d'un run, s'il y en a un.
pub async fn form_of(s: &Services, run_id: &str, visit: &str) -> Option<Value> {
    let run = s.runs.get(run_id).await.ok()??;
    let step = run.current_step.clone()?;
    if visit != format!("{step}.{}", run.iterations) {
        return None;
    }
    let wf = s.workflows.get(&run.workflow_id)?;
    let id = wf.step(&step)?.input.strip_prefix("form:")?.to_string();
    wf.settings.forms.get(&id).cloned()
}
