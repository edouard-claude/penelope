//! Pilote : les runs actifs avancent étape par étape, dans leur budget.

use super::*;

/// Boucle du pilote : admet les runs en file, pilote les runs actifs.
pub async fn driver_loop(d: Context) {
    while !d.handle.is_shutting_down() {
        if let Err(e) = drive_all(&d).await {
            tracing::warn!(error = %e, "pilote de workflows");
        }
        tokio::select! {
            _ = d.workflows.wake.notified() => {}
            _ = tokio::time::sleep(POLL) => {}
        }
    }
}

/// Un passage : chaque run actif non piloté part dans sa propre tâche.
pub async fn drive_all(d: &Context) -> anyhow::Result<usize> {
    admit_held(d).await?;
    let mut started = 0;
    for run in d.services.runs.list(Some(RunState::Running), 500).await? {
        if d.workflows.is_driving(&run.id) {
            continue;
        }
        started += 1;
        let d2 = d.clone();
        tokio::spawn(async move {
            if let Err(e) = drive(&d2, &run.id).await {
                tracing::warn!(run = %run.id, error = %e, "run interrompu");
            }
        });
    }
    Ok(started)
}

/// Admet les runs mis en file dès qu'une place se libère.
async fn admit_held(d: &Context) -> anyhow::Result<()> {
    let s = &d.services;
    let mut held: Vec<Run> = s
        .runs
        .list(Some(RunState::Paused), 500)
        .await?
        .into_iter()
        .collect();
    held.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    for run in held {
        let key = format!("wf.held.{}", run.id);
        if s.kv_get(&key).await?.is_none() {
            continue;
        }
        let Some(wf) = s.workflows.get(&run.workflow_id) else {
            continue;
        };
        let running = s
            .runs
            .list(Some(RunState::Running), 500)
            .await?
            .iter()
            .filter(|r| r.workflow_id == run.workflow_id)
            .count() as u32;
        if running < wf.settings.concurrency.max_concurrent.max(1) {
            s.store
                .write(move |tx| {
                    tx.execute("DELETE FROM kv WHERE k = ?1", [key])?;
                    Ok(())
                })
                .await?;
            s.runs.set_state(&run.id, RunState::Running, None).await?;
        }
    }
    Ok(())
}

/// Pilote un run jusqu'à ce qu'il attende, s'arrête ou finisse.
pub async fn drive(d: &Context, run_id: &str) -> anyhow::Result<RunState> {
    let Some(cancel) = d.workflows.claim(run_id) else {
        return Ok(RunState::Running);
    };
    // Tout ce que le run journalise porte son identifiant (issue #103).
    let result = {
        use tracing::Instrument;
        drive_claimed(d, run_id, &cancel)
            .instrument(tracing::info_span!("run", run = run_id))
            .await
    };
    d.workflows.release(run_id);
    result
}

async fn drive_claimed(
    d: &Context,
    run_id: &str,
    cancel: &CancelToken,
) -> anyhow::Result<RunState> {
    let s = &d.services;
    loop {
        let Some(run) = s.runs.get(run_id).await? else {
            anyhow::bail!("run {run_id} introuvable");
        };
        if run.state != RunState::Running || cancel.is_cancelled() {
            return Ok(run.state);
        }
        let Some(wf) = s.workflows.get(&run.workflow_id) else {
            return finish(d, &run, RunState::Failed, "workflow retiré du registre").await;
        };
        let run = refresh_spent(s, run).await?;
        let budget = effective_budget(s, &run, &wf.settings.budget).await;
        let limit = check_limits(&run, &budget, s.clock.now_ms());
        if limit != Limit::Ok {
            let reason = limit_reason(&limit, &run, &budget);
            return finish(d, &run, RunState::Blocked, &reason).await;
        }
        let Some(step_id) = run.current_step.clone() else {
            return finish(d, &run, RunState::Done, "").await;
        };
        let Some(step) = wf.step(&step_id).cloned() else {
            return finish(
                d,
                &run,
                RunState::Failed,
                &format!("étape `{step_id}` absente"),
            )
            .await;
        };

        let outcome = execute_with_retry(d, &run, &wf, &step, cancel).await?;
        let (result, output) = match outcome {
            StepOutcome::Waiting(why) => {
                tracing::debug!(run = %run.id, step = %step.id, %why, "étape en attente");
                return Ok(RunState::Running);
            }
            StepOutcome::Done { result, output } => (result, output),
        };
        if cancel.is_cancelled() {
            // Pause ou annulation du **run** pendant l'étape : son résultat n'est pas
            // enregistré. Un délai d'étape, lui, est un résultat comme un autre (#56).
            let state = s.runs.get(run_id).await?.map(|r| r.state);
            return Ok(state.unwrap_or(RunState::Cancelled));
        }

        let metadata = session_metadata(s, &run.session_id).await;
        let next = choose(
            &step.transitions,
            &EvalContext {
                step_result: &result,
                step_output: &output,
                metadata: &metadata,
            },
        );
        // Sortie d'un sous-groupe par sa transition taguée : la boucle s'arrête là.
        if let Some(tag) = penelope_workflow::validate::escape_tag(&wf, &step, &next) {
            let _ = s
                .events
                .append(
                    EventDraft::new(
                        "workflow.subgroup_exited",
                        json!({"run": run.id, "group": step.sub_group, "tag": tag, "from": step.id, "to": next}),
                    )
                    .session(&run.session_id),
                )
                .await;
        }
        // Une boucle qui recommence dit ce qui la retient : les critères non cochés d'une
        // transition `metadata_all_in` écartée (issue #137).
        let unmet: Vec<String> = step
            .transitions
            .iter()
            .take_while(|t| t.goto != next)
            .flat_map(|t| penelope_workflow::conditions::unmet_items(&t.condition, &metadata))
            .collect();
        if !unmet.is_empty() {
            tracing::info!(run = %run.id, step = %step.id, next = %next, ?unmet, "transition retenue");
        }
        let phase = wf.step(&next).map(|n| n.phase.as_str());
        let run = s
            .runs
            .advance(&run.id, &step.id, &result, output.clone(), &next, phase)
            .await?;
        let _ = s
            .events
            .append(
                EventDraft::new(
                    "workflow.step",
                    json!({"run": run.id, "step": step.id, "result": result.as_str(),
                           "next": next, "unmet": unmet}),
                )
                .session(&run.session_id),
            )
            .await;
        progress(d, &run, &wf, Some((&step, &result))).await;
        if next == DONE {
            return finish(d, &run, RunState::Done, "").await;
        }
        if next == BLOCKED {
            let reason = format!(
                "aucune transition de `{}` ne convient au résultat `{}`",
                step.id,
                result.as_str()
            );
            return finish(d, &run, RunState::Blocked, &reason).await;
        }
    }
}

/// Exécute une étape, avec relances (`retry`) sur échec.
async fn execute_with_retry(
    d: &Context,
    run: &Run,
    wf: &Workflow,
    step: &Step,
    cancel: &CancelToken,
) -> anyhow::Result<StepOutcome> {
    let s = &d.services;
    let attempt_key = visit_key("attempt", run, &step.id);
    let mut attempt: u32 = s
        .kv_get(&attempt_key)
        .await?
        .and_then(|a| a.parse().ok())
        .unwrap_or(0);
    loop {
        // Jeton propre à l'étape : son délai n'annule pas le run (issue #56).
        let step_cancel = cancel.child();
        let ctx = StepCtx {
            d,
            run,
            wf,
            step,
            attempt,
            cancel: &step_cancel,
        };
        let outcome = execute_step(&ctx).await?;
        let retry = step.retry.unwrap_or_default();
        match &outcome {
            StepOutcome::Done { result, .. }
                if !result.is_ok()
                    && matches!(result, StepResult::Failure | StepResult::Error)
                    && attempt < retry.max =>
            {
                attempt += 1;
                s.kv_set(&attempt_key, &attempt.to_string()).await?;
                let backoff = retry.backoff_ms.saturating_mul(1 << (attempt - 1).min(6));
                // L'attente écoute la pause et l'annulation : jusqu'à 300 s sans rien
                // regarder, c'était un run qu'on ne pouvait plus arrêter (issue #57).
                let deadline = tokio::time::Instant::now()
                    + Duration::from_millis(backoff.min(300_000) as u64);
                while tokio::time::Instant::now() < deadline {
                    if cancel.is_cancelled() {
                        return Ok(outcome);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            _ => return Ok(outcome),
        }
    }
}

/// Termine un run : état, carte, réveil du parent.
pub(super) async fn finish(
    d: &Context,
    run: &Run,
    state: RunState,
    reason: &str,
) -> anyhow::Result<RunState> {
    let s = &d.services;
    let current = s.runs.get(&run.id).await?.map(|r| r.state);
    if current != Some(state) {
        s.runs
            .set_state(&run.id, state, (!reason.is_empty()).then_some(reason))
            .await?;
    }
    let _ = s
        .events
        .append(
            EventDraft::new(
                "workflow.finished",
                json!({"run": run.id, "state": state.as_str(), "reason": reason}),
            )
            .session(&run.session_id),
        )
        .await;
    if let (Some(run), Some(wf)) = (
        s.runs.get(&run.id).await?,
        s.workflows.get(&run.workflow_id),
    ) {
        progress(d, &run, &wf, None).await;
    }
    if run.parent_run.is_some() {
        d.workflows.wake();
    }
    Ok(state)
}

/// Coût du run d'après le ledger d'usage, pour les bornes de budget. Les tokens sont
/// ceux **facturés** : l'entrée hors cache plus la sortie. Un préfixe servi par le cache
/// (décision 0008, #40) est l'économie voulue, pas une dépense (issue #136).
pub(super) async fn refresh_spent(s: &Services, mut run: Run) -> anyhow::Result<Run> {
    let id = run.id.clone();
    let (usd, tokens): (f64, i64) = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(cost_usd), 0),
                        COALESCE(SUM(MAX(prompt - cached, 0) + completion), 0)
                 FROM usage WHERE run_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await?;
    if (usd - run.spent_usd).abs() > f64::EPSILON || tokens as u64 != run.spent_tokens {
        s.runs.set_spent(&run.id, usd, tokens as u64).await?;
        run.spent_usd = usd;
        run.spent_tokens = tokens as u64;
    }
    Ok(run)
}

fn budget_key(run_id: &str) -> String {
    format!("run.budget.{run_id}")
}

/// Plafonds d'un run : ceux du workflow, relevés au besoin pour ce run seul par
/// `wf control <run> budget` (issue #136).
pub async fn effective_budget(
    s: &Services,
    run: &Run,
    declared: &penelope_workflow::model::Budget,
) -> penelope_workflow::model::Budget {
    let mut b = *declared;
    if let Ok(Some(raw)) = s.kv_get(&budget_key(&run.id)).await
        && let Ok(v) = serde_json::from_str::<Value>(&raw)
    {
        if let Some(usd) = v["max_usd"].as_f64() {
            b.max_usd = usd;
        }
        if let Some(tokens) = v["max_tokens"].as_u64() {
            b.max_tokens = tokens;
        }
    }
    b
}

/// Borne atteinte, avec ses chiffres et la commande qui la relève (issue #136).
pub(super) fn limit_reason(
    limit: &Limit,
    run: &Run,
    b: &penelope_workflow::model::Budget,
) -> String {
    let raise = |what: &str| format!("`penelope wf control {} budget {what}`", run.id);
    match limit {
        Limit::IterationsExhausted => format!(
            "itérations épuisées ({} sur {})",
            run.iterations, run.max_iterations
        ),
        Limit::BudgetUsd => format!(
            "budget de {:.2} $ atteint ({:.2} $ dépensés) : {}",
            b.max_usd,
            run.spent_usd,
            raise("--usd <montant>")
        ),
        Limit::BudgetTokens => format!(
            "budget de tokens atteint ({} tokens facturés sur {}) : {}",
            run.spent_tokens,
            b.max_tokens,
            raise("--tokens <nombre>")
        ),
        Limit::WallClock => format!("durée maximale atteinte ({} min)", b.max_wall_ms / 60_000),
        Limit::Ok => String::new(),
    }
}

/// Relève les plafonds d'un run, pour lui seul et avec trace (issue #136) : l'équivalent
/// de `session budget` pour un run. Un run bloqué par la borne relevée redevient
/// reprenable ; la reprise repart de l'étape courante, sans rejouer les effets faits.
pub async fn raise_budget(
    d: &Context,
    run_id: &str,
    usd: Option<f64>,
    tokens: Option<u64>,
) -> anyhow::Result<Value> {
    let s = &d.services;
    let run = s
        .runs
        .get(run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run {run_id} introuvable"))?;
    if usd.is_none() && tokens.is_none() {
        anyhow::bail!("rien à relever : `--usd <montant>` et/ou `--tokens <nombre>`");
    }
    let wf = s
        .workflows
        .get(&run.workflow_id)
        .ok_or_else(|| anyhow::anyhow!("workflow retiré du registre"))?;
    let mut b = effective_budget(s, &run, &wf.settings.budget).await;
    if let Some(u) = usd {
        b.max_usd = u;
    }
    if let Some(t) = tokens {
        b.max_tokens = t;
    }
    s.kv_set(
        &budget_key(run_id),
        &json!({"max_usd": b.max_usd, "max_tokens": b.max_tokens}).to_string(),
    )
    .await?;
    let _ = s
        .events
        .append(
            EventDraft::new(
                "workflow.budget_raised",
                json!({"run": run_id, "max_usd": b.max_usd, "max_tokens": b.max_tokens}),
            )
            .session(&run.session_id),
        )
        .await;
    let run = refresh_spent(s, run).await?;
    let limit = check_limits(&run, &b, s.clock.now_ms());
    Ok(json!({
        "run": run_id,
        "max_usd": b.max_usd,
        "max_tokens": b.max_tokens,
        "spent_usd": run.spent_usd,
        "spent_tokens": run.spent_tokens,
        "still_blocked": (limit != Limit::Ok).then(|| limit_reason(&limit, &run, &b)),
    }))
}

pub(super) async fn session_metadata(s: &Services, session_id: &str) -> Value {
    s.sessions
        .get(session_id)
        .await
        .ok()
        .flatten()
        .map(|sess| sess.metadata)
        .unwrap_or_else(|| json!({}))
}
