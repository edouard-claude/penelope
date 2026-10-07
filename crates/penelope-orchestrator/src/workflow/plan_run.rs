//! Runs de plans approuvés (T3 de #185, issue #191).
//!
//! « Vas-y » approuve la révision exacte que le bouton montre (version et empreinte), puis
//! lance **un** run durable : son identifiant dérive de la session et de l'empreinte, sa
//! définition compilée (`penelope_workflow::plan::execution`) est écrite avant la ligne du
//! run et relue à chaque passage du pilote. Un double clic, un clic rejoué après un
//! redémarrage ou la même demande par la RPC retrouvent ce run ; un bouton d'une autre
//! révision est refusé.

use super::*;
use penelope_workflow::plan::execution::{self, Limits};
use penelope_workflow::plan::{PlanDraft, PlanStore, plan_run_id};

fn def_key(run_id: &str) -> String {
    format!("wf.plan.def.{run_id}")
}

fn link_key(run_id: &str) -> String {
    format!("wf.plan.link.{run_id}")
}

/// Événement écrit dans la **session d'origine** quand « vas-y » lance le run (#302) :
/// la conversation qui a préparé le plan sait qu'il est parti.
pub const KIND_PLAN_LAUNCHED: &str = "workflow.plan.launched";

/// La note du prochain tour de la conversation d'origine (#302) : le 03/10, la session
/// ignorait que le clic avait lancé le run et a rappelé `workflow_start` quatre fois.
fn launched_note(draft: &PlanDraft, run: &Run) -> String {
    let step = run
        .current_step
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| format!(", étape `{s}`"))
        .unwrap_or_default();
    format!(
        "Le propriétaire a cliqué « vas-y » sur la carte du plan v{} (« {} ») : le run `{}` \
         est lancé{step}. Ne rappelle ni `workflow_start` ni `workflow_plan` pour ce plan ; \
         `workflow_status` (run_id `{}`) donne son état, `workflow_control` le met en pause \
         ou le reprend.",
        draft.plan.version(),
        draft.plan.goal(),
        run.id,
        run.id
    )
}

/// Le lien d'un run de plan vers la révision qu'il exécute : `session`, `version`,
/// `fingerprint`, `workflow`. Un run lancé hors plan n'en a pas.
pub(super) async fn plan_link(s: &Services, run_id: &str) -> Option<Value> {
    let raw = s.kv_get(&link_key(run_id)).await.ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Le workflow d'un run : la définition compilée d'un plan approuvé, sinon le registre.
pub async fn workflow_of(s: &Services, run: &Run) -> Option<Workflow> {
    if run.id.starts_with("r_plan_")
        && let Ok(Some(raw)) = s.kv_get(&def_key(&run.id)).await
    {
        return Workflow::from_json(&raw)
            .inspect_err(
                |e| tracing::warn!(run = %run.id, error = %e, "définition de plan illisible"),
            )
            .ok();
    }
    s.workflows.get(&run.workflow_id)
}

/// Ce que « vas-y » a produit.
#[derive(Debug, Clone)]
pub struct Launched {
    pub run: Run,
    /// `false` : le run existait déjà (clic rejoué, double clic).
    pub created: bool,
    pub draft: PlanDraft,
}

/// Approuve la révision `version` d'empreinte `fingerprint` du plan de `session`, puis
/// lance son run, une seule fois.
pub async fn go(
    d: &Context,
    session: &str,
    version: u64,
    fingerprint: &str,
    origin: &Origin,
) -> Result<Launched, String> {
    let s = &d.services;
    let plans = PlanStore::new(s.store.clone());
    let err = |e: penelope_store::StoreError| e.to_string();
    let active = plans
        .get(session)
        .await
        .map_err(err)?
        .ok_or("aucun plan dans cette conversation")?;
    let draft = if active.plan.version() == version && active.fingerprint() == fingerprint {
        if active.plan.can_execute() {
            active
        } else {
            let mut approved = active.clone();
            approved.approve(version).map_err(|e| e.to_string())?;
            plans
                .replace(session, &active, &approved)
                .await
                .map_err(|_| "plan modifié entre-temps : relis la dernière carte".to_string())?;
            approved
        }
    } else {
        // Un plan approuvé plus tôt dans la conversation garde son bouton : le relancer
        // retrouve son run. Toute autre révision est un clic périmé.
        plans
            .approved(session)
            .await
            .map_err(err)?
            .into_iter()
            .find(|p| p.plan.version() == version && p.fingerprint() == fingerprint)
            .ok_or_else(|| stale(version, &active))?
    };
    let (run, created) = launch(d, session, &draft, origin).await?;
    Ok(Launched {
        run,
        created,
        draft,
    })
}

/// Le refus d'un bouton qui ne montre pas la révision courante.
fn stale(version: u64, active: &PlanDraft) -> String {
    let current = active.plan.version();
    if version == current {
        format!(
            "clic périmé : ce bouton montre une autre révision que le plan v{current} en cours \
             (empreinte {}) ; valide la dernière carte",
            &active.fingerprint()[..12]
        )
    } else {
        format!(
            "clic périmé : ce bouton lance le plan v{version}, la conversation en est au plan \
             v{current} ; valide la dernière carte"
        )
    }
}

/// Crée le run d'un plan approuvé, ou rend celui qui existe.
async fn launch(
    d: &Context,
    session: &str,
    draft: &PlanDraft,
    origin: &Origin,
) -> Result<(Run, bool), String> {
    let s = &d.services;
    let fingerprint = draft.fingerprint();
    let id = plan_run_id(session, &fingerprint);
    if let Some(run) = s.runs.get(&id).await.map_err(|e| e.to_string())? {
        return Ok((run, false));
    }
    let limits = s
        .workflows
        .get(&draft.workflow_id)
        .map(|w| Limits::of(&w.settings))
        .unwrap_or_default();
    let mut wf = execution::compile(draft, &limits).map_err(|e| e.to_string())?;
    resolve_models(&mut wf, &s.config.config());
    // Tout ce que le run relira est écrit avant sa ligne : un arrêt entre les deux laisse
    // un clic rejoué refaire la même chose, sous le même identifiant.
    let link = json!({
        "session": session,
        "version": draft.plan.version(),
        "fingerprint": fingerprint,
        "workflow": draft.workflow_id,
    });
    for (key, value) in [
        (def_key(&id), wf.to_json()),
        (link_key(&id), link.to_string()),
        (origin_key(&id), origin.to_value().to_string()),
    ] {
        s.kv_set(&key, &value).await.map_err(|e| e.to_string())?;
    }
    if let Some(brief) = draft
        .brief
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        let brief: String = brief.chars().take(BRIEF_CHARS).collect();
        let _ = s.kv_set(&brief_key(&id), &brief).await;
    }
    let run_session = s
        .sessions
        .create(SessionKind::WorkflowRun, Some(wf.metadata.name.clone()))
        .await
        .map_err(|e| e.to_string())?;
    let (run, created) = s
        .runs
        .create_as(&id, &wf, run_session.id.as_str(), draft.params.clone())
        .await
        .map_err(|e| e.to_string())?;
    if !created {
        return Ok((run, false));
    }
    let workdir = workdir_for(s, &wf, &run.id, None);
    std::fs::create_dir_all(&workdir).map_err(|e| format!("{}: {e}", workdir.display()))?;
    s.runs
        .set_workdir(&run.id, &workdir.to_string_lossy())
        .await
        .map_err(|e| e.to_string())?;
    let _ = s
        .events
        .append(
            EventDraft::new(
                "workflow.started",
                json!({"run": run.id, "workflow": wf.metadata.id, "parent": null,
                       "held": false, "plan": link}),
            )
            .session(run.session_id.as_str()),
        )
        .await;
    let run = s
        .runs
        .get(&run.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("run introuvable")?;
    progress(d, &run, &wf, None).await;
    d.workflows.wake();
    // La conversation d'origine apprend le lancement (#302) : l'événement dans sa
    // session, et une note pour son prochain tour, puisque rien n'entre dans un
    // transcript hors tour. Le cœur écrit les deux ; le canal n'y est pour rien.
    let _ = s
        .events
        .append(
            EventDraft::new(
                KIND_PLAN_LAUNCHED,
                json!({"run": run.id, "version": draft.plan.version(), "fingerprint": fingerprint,
                       "workflow": draft.workflow_id, "step": run.current_step}),
            )
            .session(session),
        )
        .await;
    if let Err(e) = penelope_app::notices::push(s, session, &launched_note(draft, &run)).await {
        tracing::warn!(run = %run.id, error = %e, "lancement du plan non noté pour la conversation");
    }
    Ok((run, true))
}

/// Le modèle d'une phase est un alias (`reasoning`, `main`), un identifiant ou un rôle
/// (`code`) : il est résolu ici par le profil actif (#332), et figé dans la définition du
/// run. Un nom que la configuration ne connaît pas prend le rôle `workflow`.
fn resolve_models(wf: &mut Workflow, cfg: &penelope_kernel::config::Config) {
    for step in wf.steps.iter_mut().filter(|s| !s.model.is_empty()) {
        if cfg.alias_model(&step.model).is_some() {
            continue;
        }
        step.model = cfg.role_alias(super::step_ctx::step_role(cfg, &step.model));
    }
}

/// Les runs des plans approuvés d'une conversation : ce que `/stop` y arrête.
pub async fn plan_runs_of(s: &Services, session: &str) -> anyhow::Result<Vec<Run>> {
    let mut out = Vec::new();
    for draft in PlanStore::new(s.store.clone()).approved(session).await? {
        if let Some(run) = s
            .runs
            .get(&plan_run_id(session, &draft.fingerprint()))
            .await?
        {
            out.push(run);
        }
    }
    Ok(out)
}

/// Session neuve d'une visite d'étape `context: fresh`, créée une fois par visite : une
/// reprise après redémarrage retrouve la sienne, avec ce qu'elle avait déjà fait.
pub(super) async fn fresh_session(ctx: &StepCtx<'_>) -> anyhow::Result<String> {
    let s = ctx.s();
    let key = visit_key("session", ctx.run, &ctx.step.id);
    if let Some(id) = s.kv_get(&key).await? {
        return Ok(id);
    }
    let title = format!(
        "{} · {}",
        ctx.wf.metadata.name,
        if ctx.step.name.is_empty() {
            &ctx.step.id
        } else {
            &ctx.step.name
        }
    );
    let session = s
        .sessions
        .create_with(
            SessionKind::WorkflowRun,
            Some(title),
            Some(ctx.run.session_id.clone()),
            None,
        )
        .await?;
    s.kv_set(&key, session.id.as_str()).await?;
    Ok(session.id.to_string())
}

/// Sorties rendues par les étapes précédentes, les plus récentes en dernier : tout ce
/// qu'une étape à contexte neuf sait du run, en plus de sa consigne.
const PRIOR_OUTPUTS: usize = 6;
const PRIOR_CHARS: usize = 2_000;

pub(super) fn prior_outputs(ctx: &StepCtx<'_>) -> String {
    let Some(outputs) = ctx.run.step_outputs.as_object() else {
        return String::new();
    };
    let rendered: Vec<String> = outputs
        .iter()
        .filter(|(id, _)| id.as_str() != "__last")
        .filter_map(|(id, out)| {
            let content = out.get("content").and_then(Value::as_str)?.trim();
            (!content.is_empty()).then(|| {
                let name = ctx
                    .wf
                    .step(id)
                    .map(|s| s.name.as_str())
                    .filter(|n| !n.is_empty())
                    .unwrap_or(id);
                let result = out.get("result").and_then(Value::as_str).unwrap_or("rendu");
                let short: String = content.chars().take(PRIOR_CHARS).collect();
                format!("### {name} ({result})\n{short}")
            })
        })
        .collect();
    if rendered.is_empty() {
        return String::new();
    }
    let skip = rendered.len().saturating_sub(PRIOR_OUTPUTS);
    format!(
        "\n\nSorties rendues avant cette étape (les plus récentes en dernier) :\n\n{}",
        rendered[skip..].join("\n\n")
    )
}
