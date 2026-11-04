//! Étape `mission` (#425, décision 0025) : un workflow délègue une tâche ouverte, du code
//! à écrire dans un dépôt, à une mission, au lieu de la découper en étapes `agent` qui
//! bouclent. Le workflow garde l'extérieur (la liste, l'ordre, les arrêts) ; la mission
//! tient la boucle jusqu'à son critère de fin.
//!
//! Champs : `prompt` (l'objectif, en gabarit), `cwd` (le dépôt, dans un workspace
//! autorisé), `command` (la commande qui prouve la fin, `--check`), `budget.maxUsd` (le
//! budget de la mission). La mission tourne dans une session neuve, sur le canal du run ;
//! elle est autonome comme toute mission. L'étape attend sa fin : `success` si elle est
//! terminée, `failure` sinon (bloquée, budget, arrêtée), avec son état et son résumé.
//! Annuler le run arrête sa mission.

use super::*;
use penelope_app::mission::{self, State};
use penelope_kernel::turn::TurnKind;

/// La session de la mission lancée par cette visite d'étape.
fn session_key(ctx: &StepCtx<'_>) -> String {
    visit_key("mission", ctx.run, &ctx.step.id)
}

/// La mission en cours d'un run : `control` l'arrête avec lui.
pub(super) fn current_key(run_id: &str) -> String {
    format!("wf.mission_current.{run_id}")
}

/// Le dépôt de la mission : `cwd` rendu ; un chemin relatif se lit dans les workspaces
/// configurés, puis dans l'espace du run. Vide : l'espace du run.
async fn cwd_of(ctx: &StepCtx<'_>) -> std::path::PathBuf {
    let workdir = ctx.workdir();
    if ctx.step.cwd.trim().is_empty() {
        return workdir;
    }
    let given = mission::expand(&ctx.render(&ctx.step.cwd).await);
    if given.is_absolute() {
        return given;
    }
    penelope_executor::executor::default_workspaces(ctx.s())
        .into_iter()
        .map(|w| w.join(&given))
        .find(|c| c.exists())
        .unwrap_or_else(|| workdir.join(&given))
}

async fn start(ctx: &StepCtx<'_>) -> anyhow::Result<Result<String, StepOutcome>> {
    let s = ctx.s();
    let goal = ctx.render(&ctx.step.prompt).await;
    let check = ctx
        .step
        .command
        .as_str()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(String::from);
    let check = match check {
        Some(c) => Some(ctx.render(&c).await),
        None => None,
    };
    let budget_usd = ctx
        .step
        .budget
        .as_ref()
        .map(|b| b.max_usd)
        .filter(|b| *b > 0.0);
    let cwd = cwd_of(ctx).await.to_string_lossy().to_string();
    let title = format!(
        "Mission · {}",
        if ctx.step.name.is_empty() {
            &ctx.step.id
        } else {
            &ctx.step.name
        }
    );
    let session = s
        .sessions
        .create(SessionKind::Chat, Some(title))
        .await?
        .id
        .to_string();
    let origin = origin_of(s, &ctx.run.id).await;
    let req = mission::Request {
        goal,
        check,
        cwd: Some(cwd),
        budget_usd,
        ..Default::default()
    };
    let (_, text) = match mission::start(s, &session, req, &origin).await {
        Ok(started) => started,
        Err(e) => {
            return Ok(Err(done(
                StepResult::Error,
                json!({"error": format!("mission non lancée : {e}")}),
            )));
        }
    };
    s.turns
        .enqueue(
            &session,
            TurnKind::Message,
            json!({"text": text, "origin": origin.to_value()}),
            Some(format!("mission:{session}:0")),
            0,
        )
        .await?;
    ctx.d.bus.notify_enqueued();
    s.kv_set(&session_key(ctx), &session).await?;
    s.kv_set(&current_key(&ctx.run.id), &session).await?;
    Ok(Ok(session))
}

pub(super) async fn mission_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let session = match s.kv_get(&session_key(ctx)).await? {
        Some(session) => session,
        None => match start(ctx).await? {
            Ok(session) => session,
            Err(outcome) => return Ok(outcome),
        },
    };
    let Some(m) = mission::load(s, &session).await else {
        return Ok(done(
            StepResult::Error,
            json!({"error": "mission introuvable", "session": session}),
        ));
    };
    if m.state == State::Running {
        return Ok(StepOutcome::Waiting(format!("mission {session} en cours")));
    }
    let _ = s.kv_delete(&current_key(&ctx.run.id)).await;
    let spent = s.budget.spent_session(&session).await.unwrap_or(0.0);
    let output = json!({
        "session": session,
        "state": m.state.as_str(),
        "content": m.summary(spent, s.clock.now_ms()),
        "spent_usd": spent,
    });
    Ok(done(
        if m.state == State::Done {
            StepResult::Success
        } else {
            StepResult::Failure
        },
        output,
    ))
}

/// Le run s'arrête : sa mission en cours aussi, et celles de ses sous-runs (une story de
/// `backlog`), avec leurs tours en file.
pub(super) async fn stop_current(d: &Context, run_id: &str) {
    let s = &d.services;
    let all = s.runs.list(None, 500).await.unwrap_or_default();
    let mut ids = vec![run_id.to_string()];
    let mut i = 0;
    while i < ids.len() && ids.len() < 64 {
        let parent = ids[i].clone();
        ids.extend(
            all.iter()
                .filter(|r| r.parent_run.as_deref() == Some(parent.as_str()))
                .map(|r| r.id.clone()),
        );
        i += 1;
    }
    for id in ids {
        stop_one(s, &id).await;
    }
}

async fn stop_one(s: &Services, run_id: &str) {
    let Ok(Some(session)) = s.kv_get(&current_key(run_id)).await else {
        return;
    };
    if mission::is_running(s, &session).await {
        let _ = mission::stop(s, &session).await;
        let _ = s
            .turns
            .cancel_pending(&session, "mission arrêtée avec son run")
            .await;
    }
    let _ = s.kv_delete(&current_key(run_id)).await;
}

/// Au démarrage (#425) : ce qui nomme encore un workflow retiré par la décision 0025, un
/// fichier d'instance de ce nom (chargé quand même) ou une planification qui le lance
/// (gardée). Rien n'est supprimé ; chaque ligne part au journal.
pub(super) async fn retired_report(s: &Services) -> Vec<String> {
    let retired = penelope_workflow::bundled::RETIRED;
    let mut out: Vec<String> = s
        .workflows
        .all()
        .into_iter()
        .filter(|e| retired.contains(&e.workflow.metadata.id.as_str()))
        .map(|e| {
            format!(
                "workflow d'instance `{}` chargé ({}) : il n'est plus livré, le code passe \
                 par une mission (décision 0025)",
                e.workflow.metadata.id,
                e.path.map(|p| p.display().to_string()).unwrap_or_default()
            )
        })
        .collect();
    for sched in s.schedules.list().await.unwrap_or_default() {
        if let Some(id) = sched.target["workflowId"].as_str()
            && retired.contains(&id)
        {
            out.push(format!(
                "planification `{}` : elle lance `{id}`, qui n'est plus livré",
                sched.id
            ));
        }
    }
    for line in &out {
        tracing::warn!("{line}");
    }
    out
}
