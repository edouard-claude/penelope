//! Étape `user` : question au propriétaire.

use super::*;

/// `user` : question au propriétaire ; le choix devient le résultat.
pub(super) async fn user_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let (run, step) = (ctx.run, ctx.step);
    if let Some(raw) = s.kv_get(&visit_key("answer", run, &step.id)).await? {
        let v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
        let choice = v["choice"].as_str().unwrap_or("répondu").to_string();
        return Ok(done(
            StepResult::Choice(choice.clone()),
            json!({"choice": choice, "input": v["input"]}),
        ));
    }
    let asked_key = visit_key("asked", run, &step.id);
    if s.kv_get(&asked_key).await?.is_none() {
        let text = question_text(ctx).await;
        let visit = format!("{}.{}", step.id, run.iterations);
        let wants_input = step.input != "none" && !step.input.is_empty();
        let form = step
            .input
            .strip_prefix("form:")
            .and_then(|id| ctx.wf.settings.forms.get(id));
        let origin = origin_of(ctx.s(), &run.id).await;
        if let Some(m) = ctx.d.workflows.ports.messenger.get() {
            m.send_question(
                &origin,
                &text,
                &run.id,
                &visit,
                &step.choices,
                wants_input,
                form,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        }
        s.kv_set(&asked_key, "1").await?;
        let _ = s
            .events
            .append(
                EventDraft::new(
                    "workflow.question",
                    json!({"run": run.id, "step": step.id, "choices": step.choices}),
                )
                .session(&run.session_id),
            )
            .await;
    }
    Ok(StepOutcome::Waiting("réponse du propriétaire".into()))
}

/// Texte d'une question : le gabarit de l'étape rempli avec le contexte du run.
async fn question_text(ctx: &StepCtx<'_>) -> String {
    let s = ctx.s();
    let step = ctx.step;
    let last = ctx
        .run
        .step_outputs
        .get("__last")
        .cloned()
        .unwrap_or(Value::Null);
    let mut body = match s.channel.template(if step.template.is_empty() {
        "question"
    } else {
        &step.template
    }) {
        Some(tpl) => {
            // Variables du gabarit : sortie précédente, puis paramètres, puis contexte.
            let mut t = tpl.body.clone();
            for var in &tpl.variables {
                let value = last
                    .get(var)
                    .or_else(|| last.get("content").and_then(|c| c.get(var)))
                    .or_else(|| ctx.run.params.get(var))
                    .map(|v| match v {
                        Value::String(x) => x.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| match var.as_str() {
                        "question" => {
                            if step.name.is_empty() {
                                step.id.clone()
                            } else {
                                step.name.clone()
                            }
                        }
                        _ => "—".into(),
                    });
                t = t.replace(&format!("{{{{{var}}}}}"), &value);
            }
            t
        }
        None => format!(
            "❓ {}",
            if step.name.is_empty() {
                &step.id
            } else {
                &step.name
            }
        ),
    };
    body = ctx.render(&body).await;
    if let Some(content) = last.get("content").and_then(|c| c.as_str())
        && !content.trim().is_empty()
        && !body.contains(content.trim())
    {
        body.push_str(&format!("\n\n{}", content.trim()));
    }
    format!(
        "🔧 **{}** · `{}`\n\n{body}",
        ctx.wf.metadata.name, ctx.run.id
    )
}
