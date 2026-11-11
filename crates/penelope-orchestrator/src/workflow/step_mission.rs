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
//!
//! La mission suit aussi le run qui cesse d'avancer (#456) : en pause ou bloqué par ses
//! bornes, il lui demande d'attendre (`mission::hold`), elle se range à la fin de son tour
//! et repart quand il avance de nouveau. Sa dépense compte dans le budget du run
//! (`budget::refresh_spent`).

use super::*;
use penelope_app::mission::{self, Held, State};
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
    if m.live() {
        // Rangée, ou priée d'attendre : elle repart dès que le run qui l'a demandé avance.
        if let Some(asked) = &m.hold
            && !stalled(s, &asked.by).await
        {
            resume(ctx, &session).await?;
        }
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

/// Le run et ses sous-runs (une story de `backlog`), lui en tête : leurs missions suivent
/// son sort.
async fn family(s: &Services, run_id: &str) -> Vec<String> {
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
    ids
}

/// Le run s'arrête : sa mission en cours aussi, et celles de ses sous-runs, avec leurs
/// tours en file. Une mission rangée en attendant la reprise s'arrête de même.
pub(super) async fn stop_current(d: &Context, run_id: &str) {
    let s = &d.services;
    for id in family(s, run_id).await {
        stop_one(s, &id).await;
    }
}

/// Arrête la mission du run seul : son étape est passée, plus rien ne l'attendrait.
pub(super) async fn stop_own(s: &Services, run_id: &str) {
    stop_one(s, run_id).await;
}

async fn stop_one(s: &Services, run_id: &str) {
    let Ok(Some(session)) = s.kv_get(&current_key(run_id)).await else {
        return;
    };
    if mission::load(s, &session).await.is_some_and(|m| m.live()) {
        let _ = mission::stop(s, &session).await;
        let _ = s
            .turns
            .cancel_pending(&session, "mission arrêtée avec son run")
            .await;
    }
    let _ = s.kv_delete(&current_key(run_id)).await;
}

/// Demande à la mission en cours de `run_id` d'attendre le run `by` ; rend sa session et
/// ce qu'elle devient. Rien si le run n'a pas de mission.
async fn hold_one(s: &Services, run_id: &str, by: &str, why: &str) -> Option<(String, Held)> {
    let session = s.kv_get(&current_key(run_id)).await.ok().flatten()?;
    match mission::hold(s, &session, by, why).await {
        Ok(held) => Some((session, held)),
        Err(e) => {
            tracing::warn!(run = run_id, error = %e, "mission non mise en attente");
            None
        }
    }
}

/// Les missions des sous-runs attendent avec `run_id`. À appeler une fois son état écrit :
/// une mission ne repart que si le run qui l'a demandé avance.
async fn hold_below(s: &Services, run_id: &str, why: &str) {
    for id in family(s, run_id).await.iter().skip(1) {
        hold_one(s, id, run_id, why).await;
    }
}

/// Le run est en pause (#456) : sa mission et celles de ses sous-runs finissent leur tour
/// et attendent. Aucune n'est arrêtée ; la reprise du run les relance.
pub(super) async fn hold_current(d: &Context, run_id: &str) {
    let (s, why) = (&d.services, "son run est en pause");
    hold_one(s, run_id, run_id, why).await;
    hold_below(s, run_id, why).await;
}

/// Le run a atteint une borne (#456). Rend l'état du run, ou rien s'il doit d'abord
/// laisser son étape `mission` rendre le résultat d'une mission déjà finie.
///
/// Le tour en cours de la mission finit avant que le run ne se bloque : elle se range, et
/// le motif la nomme ; ou elle finit, et son étape conclut au lieu de laisser un run
/// bloqué sur une tâche terminée.
pub(super) async fn on_limit(
    d: &Context,
    run: &Run,
    wf: &Workflow,
    mut reason: String,
) -> anyhow::Result<Option<RunState>> {
    let (s, why) = (&d.services, "son run a atteint une borne");
    match hold_one(s, &run.id, &run.id, why).await {
        Some((_, Held::Turning)) => return Ok(Some(RunState::Running)),
        Some((session, Held::Settled)) if on_its_step(s, run, wf, &session).await => {
            return Ok(None);
        }
        Some((session, Held::Parked)) => reason.push_str(&format!(
            " ; sa mission (session {session}) attend et repart à la reprise"
        )),
        _ => {}
    }
    let state = finish(d, run, RunState::Blocked, &reason).await?;
    hold_below(s, &run.id, why).await;
    Ok(Some(state))
}

/// Vrai si l'étape courante du run est l'étape `mission` qui a lancé `session`.
async fn on_its_step(s: &Services, run: &Run, wf: &Workflow, session: &str) -> bool {
    let Some(step) = run.current_step.as_deref().and_then(|id| wf.step(id)) else {
        return false;
    };
    step.kind == "mission"
        && s.kv_get(&visit_key("mission", run, &step.id))
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some(session)
}

/// Vrai tant que le run qui a demandé l'attente n'avance pas.
async fn stalled(s: &Services, by: &str) -> bool {
    matches!(
        s.runs.get(by).await,
        Ok(Some(r)) if matches!(r.state, RunState::Paused | RunState::Blocked)
    )
}

/// Relance la mission rangée : son tour suivant repart, avec la consigne gardée.
async fn resume(ctx: &StepCtx<'_>, session: &str) -> anyhow::Result<()> {
    let s = ctx.s();
    if let Some(next) = mission::release(s, session).await? {
        s.turns
            .enqueue(
                session,
                TurnKind::Message,
                json!({"text": next.text, "origin": next.origin}),
                Some(next.dedup),
                0,
            )
            .await?;
        ctx.d.bus.notify_enqueued();
    }
    Ok(())
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
