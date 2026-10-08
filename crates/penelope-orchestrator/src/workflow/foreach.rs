//! Étape `foreach` (#338) : une liste ordonnée, un sous-workflow par élément, dans l'ordre.
//!
//! La liste est figée au premier passage (`workflow_items`) ; chaque passage du pilote
//! regarde l'élément courant : son sous-run tourne encore (l'étape attend), a fini
//! (élément fait, compte rendu dans le sujet, point d'arrêt éventuel), ou a échoué
//! (`onError` : arrêt, élément sauté, nouvelle tentative). Une reprise après redémarrage
//! repart de l'élément courant, à l'étape courante de son sous-run.

use super::*;
use penelope_workflow::foreach::{
    self, Item, ItemState, ItemStore, OnError, Source, Tally, default_label,
};

fn items_of(s: &Services) -> ItemStore {
    ItemStore::new(s.store.clone(), s.clock.clone())
}

fn noun(step: &Step) -> &str {
    if step.item_noun.is_empty() {
        "élément"
    } else {
        &step.item_noun
    }
}

/// `foreach` : fige la liste, puis fait avancer l'élément courant.
pub(super) async fn foreach_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let (run, step) = (ctx.run, ctx.step);
    let store = items_of(s);
    let on_error = match OnError::parse(&step.on_error) {
        Ok(o) => o,
        Err(e) => return Ok(done(StepResult::Error, json!({"error": e}))),
    };
    let frozen = visit_key("foreach", run, &step.id);
    if s.kv_get(&frozen).await?.is_none() {
        let items = match resolve(ctx).await? {
            Ok(items) => items,
            Err(StepOutcome::Waiting(why)) => return Ok(StepOutcome::Waiting(why)),
            Err(outcome) => return Ok(outcome),
        };
        let mut labelled = Vec::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            let label = label_of(ctx, &item, i).await;
            labelled.push((item, label));
        }
        let total = labelled.len();
        store
            .freeze(&run.id, &step.id, run.iterations, labelled)
            .await?;
        s.kv_set(&frozen, "1").await?;
        let _ = s
            .events
            .append(
                EventDraft::new(
                    "workflow.foreach_frozen",
                    json!({"run": run.id, "step": step.id, "total": total}),
                )
                .session(&run.session_id),
            )
            .await;
    }
    loop {
        let items = store.list(&run.id, &step.id, run.iterations).await?;
        let Some(cur) = items.iter().find(|i| i.state == ItemState::Running) else {
            if let Some(next) = items.iter().find(|i| i.state == ItemState::Todo) {
                return start_item(ctx, next, items.len()).await;
            }
            let tally = Tally::of(&items);
            let result = if tally.failed + tally.skipped == 0 {
                StepResult::Success
            } else {
                StepResult::Partial
            };
            return Ok(done(result, summary(&items)));
        };
        let child = match &cur.child_run {
            Some(id) => s.runs.get(id).await?,
            None => None,
        };
        let why = match child.as_ref().map(|c| c.state) {
            Some(RunState::Running | RunState::Paused) => {
                return Ok(StepOutcome::Waiting(format!(
                    "{} {}/{}",
                    noun(step),
                    cur.idx + 1,
                    items.len()
                )));
            }
            Some(RunState::Done) => {
                store.finish(cur, ItemState::Done, None).await?;
                report(ctx, cur, items.len(), ItemState::Done, child.as_ref(), None).await;
                if breakpoint(ctx, cur).await? {
                    return Ok(StepOutcome::Waiting("point d'arrêt".into()));
                }
                continue;
            }
            Some(_) => child
                .as_ref()
                .and_then(|c| c.error.clone())
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "sous-run {}",
                        child.as_ref().map_or("?", |c| c.state.as_str())
                    )
                }),
            None => "sous-run disparu".to_string(),
        };
        match on_error {
            OnError::Retry(n) if cur.attempts < n => {
                return start_item(ctx, cur, items.len()).await;
            }
            OnError::Skip => {
                store.finish(cur, ItemState::Skipped, Some(&why)).await?;
                report(
                    ctx,
                    cur,
                    items.len(),
                    ItemState::Skipped,
                    child.as_ref(),
                    Some(&why),
                )
                .await;
            }
            _ => {
                store.finish(cur, ItemState::Failed, Some(&why)).await?;
                report(
                    ctx,
                    cur,
                    items.len(),
                    ItemState::Failed,
                    child.as_ref(),
                    Some(&why),
                )
                .await;
                let items = store.list(&run.id, &step.id, run.iterations).await?;
                return Ok(done(StepResult::Failure, summary(&items)));
            }
        }
    }
}

/// La liste, d'où qu'elle vienne, filtrée et triée. `Err` porte l'issue de l'étape : une
/// attente (approbation de l'outil) ou un échec.
async fn resolve(ctx: &StepCtx<'_>) -> anyhow::Result<Result<Vec<Value>, StepOutcome>> {
    let step = ctx.step;
    let fail = |e: String| Err(done(StepResult::Error, json!({"error": e})));
    let items = match Source::parse(&step.items) {
        Err(e) => return Ok(fail(e)),
        Ok(Source::Inline(items)) => items,
        Ok(Source::File { path }) => {
            let path = ctx.render(&path).await;
            let full = ctx.workdir().join(&path);
            let text = match tokio::fs::read_to_string(&full).await {
                Ok(t) => t,
                Err(e) => return Ok(fail(format!("{} : {e}", full.display()))),
            };
            let at = step.items["path"].as_str().unwrap_or_default();
            match foreach::parse_file(&path, &text, at) {
                Ok(items) => items,
                Err(e) => return Ok(fail(e)),
            }
        }
        Ok(Source::Step { step: from, path }) => {
            let Some(output) = ctx.run.step_outputs.get(&from) else {
                return Ok(fail(format!("l'étape `{from}` n'a pas encore de sortie")));
            };
            match foreach::extract(output, &path) {
                Ok(items) => items,
                Err(e) => return Ok(fail(format!("`{from}` : {e}"))),
            }
        }
        Ok(Source::Tool { tool, args, path }) => {
            // L'outil passe par le chemin d'une étape `tool` : politique, approbation,
            // ledger d'effets. Il est appelé une fois ; la liste est ensuite figée.
            let call = Step {
                id: format!("{}.items", step.id),
                kind: "tool".into(),
                tool,
                args,
                ..Default::default()
            };
            let lister = StepCtx {
                step: &call,
                ..*ctx
            };
            match tool_step(&lister).await? {
                StepOutcome::Done { result, output } if result.is_ok() => {
                    match foreach::extract(&output, &path) {
                        Ok(items) => items,
                        Err(e) => return Ok(fail(format!("`{}` : {e}", call.tool))),
                    }
                }
                StepOutcome::Done { output, .. } => {
                    return Ok(Err(done(StepResult::Error, output)));
                }
                waiting => return Ok(Err(waiting)),
            }
        }
    };
    Ok(Ok(foreach::refine(items, &step.items)))
}

/// Le run tel que le voit un élément : ses paramètres, plus `item`, `item_index` (depuis
/// 1) et `item_total`.
fn with_item(run: &Run, item: &Value, idx: usize, total: usize) -> Run {
    let mut r = run.clone();
    if !r.params.is_object() {
        r.params = json!({});
    }
    r.params["item"] = item.clone();
    r.params["item_index"] = json!(idx + 1);
    r.params["item_total"] = json!(total);
    r
}

async fn label_of(ctx: &StepCtx<'_>, item: &Value, idx: usize) -> String {
    if ctx.step.item_label.is_empty() {
        return default_label(item, idx);
    }
    let run = with_item(ctx.run, item, idx, 0);
    let seen = StepCtx { run: &run, ..*ctx };
    let label = seen.render(&ctx.step.item_label).await;
    label.chars().take(120).collect()
}

/// Lance (ou relance) le sous-workflow d'un élément.
async fn start_item(ctx: &StepCtx<'_>, it: &Item, total: usize) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let (run, step) = (ctx.run, ctx.step);
    let seen_run = with_item(run, &it.item, it.idx, total);
    let seen = StepCtx {
        run: &seen_run,
        ..*ctx
    };
    let mut params = seen.render_json(&step.params).await;
    if !params.is_object() {
        params = json!({});
    }
    // L'élément n'entre que dans les paramètres que le sous-workflow déclare.
    if let Some(child) = s.workflows.get(&step.workflow_id) {
        for p in &child.metadata.parameters {
            if let Some(v) = seen_run.params.get(&p.id)
                && ["item", "item_index", "item_total"].contains(&p.id.as_str())
                && params.get(&p.id).is_none()
            {
                params[&p.id] = v.clone();
            }
        }
    }
    let origin = origin_of(s, &run.id).await;
    match start_run(
        ctx.d,
        &step.workflow_id,
        params,
        &origin,
        Some(&run.id),
        run.depth + 1,
    )
    .await
    {
        Ok(child) => {
            items_of(s).start(it, &child.id).await?;
            if let Some(b) = &step.item_budget {
                set_caps(s, &child.id, b).await?;
            }
            Ok(StepOutcome::Waiting(format!(
                "{} {}/{total}",
                noun(step),
                it.idx + 1
            )))
        }
        Err(e) => {
            items_of(s).finish(it, ItemState::Failed, Some(&e)).await?;
            Ok(done(
                StepResult::Error,
                json!({"error": e, "item": it.idx + 1}),
            ))
        }
    }
}

/// Compte rendu court d'un élément fini, dans le sujet du run.
async fn report(
    ctx: &StepCtx<'_>,
    it: &Item,
    total: usize,
    state: ItemState,
    child: Option<&Run>,
    why: Option<&str>,
) {
    let Some(m) = ctx.d.workflows.ports.messenger.get() else {
        return;
    };
    let head = format!("{} {}/{total} · {}", noun(ctx.step), it.idx + 1, it.label);
    let cost = child
        .map(|c| c.spent_usd)
        .filter(|c| *c > 0.0)
        .map(|c| format!(" ({c:.2} $)"))
        .unwrap_or_default();
    let why = why
        .map(|w| penelope_observe::redact(w.trim()))
        .unwrap_or_default();
    let text = match state {
        ItemState::Done => format!("✅ {head} : fait{cost}"),
        ItemState::Skipped => format!("⏭ {head} : sauté ({why}), je passe au suivant"),
        _ => format!("❌ {head} : en échec ({why}), la liste s'arrête là"),
    };
    let origin = origin_of(ctx.s(), &ctx.run.id).await;
    let _ = m.send_text(&origin, &text).await;
}

/// Point d'arrêt après l'élément `it` (`pauseEvery`, `pauseAfter`) : le run passe en
/// pause, le propriétaire reçoit le bilan. Vrai si le run s'arrête là.
async fn breakpoint(ctx: &StepCtx<'_>, it: &Item) -> anyhow::Result<bool> {
    let s = ctx.s();
    let (run, step) = (ctx.run, ctx.step);
    let items = items_of(s).list(&run.id, &step.id, run.iterations).await?;
    let next = items.iter().find(|i| i.state == ItemState::Todo);
    let Some(next) = next else {
        return Ok(false);
    };
    let tally = Tally::of(&items);
    let every = step
        .pause_every
        .filter(|n| *n > 0)
        .is_some_and(|n| (tally.done + tally.skipped).is_multiple_of(n as usize));
    let after = foreach::pause_after(&step.pause_after, &it.item, Some(&next.item));
    if !every && !after {
        return Ok(false);
    }
    let reason = format!("point d'arrêt après {} : {}", it.label, tally.line());
    s.runs
        .set_state(&run.id, RunState::Paused, Some(&reason))
        .await?;
    let _ = s
        .events
        .append(
            EventDraft::new(
                "workflow.foreach_breakpoint",
                json!({"run": run.id, "step": step.id, "after": it.idx + 1,
                       "done": tally.done, "total": tally.total}),
            )
            .session(&run.session_id),
        )
        .await;
    if let Some(m) = ctx.d.workflows.ports.messenger.get() {
        let text = format!(
            "⏸ Point d'arrêt après {} {}/{} ({}).\n{}\nSuivant : {}. ▶️ Reprendre sur le \
             run (`/runs`) ou `/resume {}`.",
            noun(step),
            it.idx + 1,
            tally.total,
            it.label,
            list_lines(&items),
            next.label,
            run.id
        );
        let origin = origin_of(s, &run.id).await;
        let _ = m.send_text(&origin, &text).await;
    }
    if let Some(current) = s.runs.get(&run.id).await? {
        progress(ctx.d, &current, ctx.wf, None).await;
    }
    Ok(true)
}

/// Bilan d'une liste : décompte, faits (les dix derniers), échecs et sauts avec leur raison.
pub(super) fn list_lines(items: &[Item]) -> String {
    let tally = Tally::of(items);
    let mut out = vec![format!("Bilan : {}.", tally.line())];
    let done: Vec<&str> = items
        .iter()
        .filter(|i| i.state == ItemState::Done)
        .map(|i| i.label.as_str())
        .collect();
    if !done.is_empty() {
        let shown = done.len().saturating_sub(10);
        let more = if shown > 0 { "…, " } else { "" };
        out.push(format!("Faits : {more}{}", done[shown..].join(", ")));
    }
    for i in items
        .iter()
        .filter(|i| matches!(i.state, ItemState::Failed | ItemState::Skipped))
    {
        out.push(format!(
            "{} {} : {}",
            if i.state == ItemState::Failed {
                "En échec"
            } else {
                "Sauté"
            },
            i.label,
            i.error.as_deref().unwrap_or("?")
        ));
    }
    out.join("\n")
}

/// Sortie de l'étape : le décompte et l'état de chaque élément.
fn summary(items: &[Item]) -> Value {
    let tally = Tally::of(items);
    json!({
        "total": tally.total,
        "done": tally.done,
        "failed": tally.failed,
        "skipped": tally.skipped,
        "items": items.iter().map(|i| json!({
            "index": i.idx + 1,
            "label": i.label,
            "state": i.state.as_str(),
            "run": i.child_run,
            "error": i.error,
        })).collect::<Vec<_>>(),
    })
}

/// Où en est la liste d'un run : `story 12/53 · 3.2 Moteur de passation · étape dev`.
/// Pour le sous-run d'un élément, sa place dans la liste du parent.
pub async fn position_of(s: &Services, run: &Run) -> Option<String> {
    let store = items_of(s);
    if let Ok(Some(it)) = store.of_child(&run.id).await {
        let total = store
            .list(&it.run_id, &it.step_id, it.visit)
            .await
            .ok()?
            .len();
        let noun = noun_in(s, &it).await;
        return Some(format!("{noun} {}/{total} · {}", it.idx + 1, it.label));
    }
    let items = store.latest(&run.id).await.ok()?;
    let cur = items
        .iter()
        .find(|i| i.state == ItemState::Running)
        .or_else(|| items.iter().find(|i| i.state == ItemState::Todo))?;
    let noun = noun_in(s, cur).await;
    let mut line = format!("{noun} {}/{} · {}", cur.idx + 1, items.len(), cur.label);
    if let Some(step) = match &cur.child_run {
        Some(id) => s.runs.get(id).await.ok().flatten(),
        None => None,
    }
    .and_then(|c| c.current_step)
    {
        line.push_str(&format!(" · étape {step}"));
    }
    Some(line)
}

/// Le bilan de la dernière liste d'un run, pour l'écran du run ; `None` sans liste.
pub async fn list_of(s: &Services, run_id: &str) -> Option<String> {
    let items = items_of(s).latest(run_id).await.ok()?;
    (!items.is_empty()).then(|| list_lines(&items))
}

async fn noun_in(s: &Services, it: &Item) -> String {
    let Some(parent) = s.runs.get(&it.run_id).await.ok().flatten() else {
        return "élément".into();
    };
    workflow_of(s, &parent)
        .await
        .and_then(|wf| wf.step(&it.step_id).map(|st| noun(st).to_string()))
        .unwrap_or_else(|| "élément".into())
}
