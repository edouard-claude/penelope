//! Gate de production (T5 de #185, issue #193) : le bilan vérifié, puis la PR dev → prod
//! que seul le clic du propriétaire déclenche.
//!
//! Le bilan fige ce que le propriétaire approuve : le plan et le run exacts, la PR dev et
//! son commit, la CI et l'E2E de ce commit, l'heure. Après le clic, rien n'est cru sur
//! parole : le plan actif, le commit de la PR dev et la CI sont relus, et un bilan périmé
//! renvoie vérifier au lieu d'ouvrir quoi que ce soit.
//!
//! La PR de production passe par le ledger sous une clé qui ne dépend que du run : planifiée
//! avant l'appel, cherchée sur le forgeur (par la marque du run dans sa description) avant
//! d'être ouverte. Un redémarrage, un double passage ou une nouvelle approbation dans le même
//! run la retrouvent, jamais ne la rouvrent. Pénélope ne la fusionne pas et ne déploie rien.

use super::*;
use penelope_workflow::delivery::config::ForgeKind;
use penelope_workflow::delivery::gate::{
    self, APPROVE, Bilan, Freshness, GATE_STEP, PlanRef, ProdTarget, REPORT_STEP, STALE,
    SUPERSEDED, Seen,
};
use penelope_workflow::delivery::{CI_STEP, E2E_STEP, PASSED};
use penelope_workflow::plan::PlanStore;

/// Le plan exact qu'exécute le run, et le plan actif de sa conversation ; `None` pour un
/// run qui ne vient pas d'un plan.
async fn plans(s: &Services, run_id: &str) -> Option<(PlanRef, Option<PlanRef>)> {
    let link = plan_link(s, run_id).await?;
    let executed = PlanRef {
        version: link["version"].as_u64()?,
        fingerprint: link["fingerprint"].as_str()?.to_string(),
    };
    let session = link["session"].as_str()?;
    let active = PlanStore::new(s.store.clone())
        .get(session)
        .await
        .ok()
        .flatten()
        .map(|draft| PlanRef {
            version: draft.plan.version(),
            fingerprint: draft.fingerprint(),
        });
    Some((executed, active))
}

fn superseded(why: &str) -> StepOutcome {
    outcome(
        StepResult::Choice(SUPERSEDED.into()),
        format!("Aucune PR prod : {why}. Le run s'arrête ; le plan actif a son propre run."),
        json!({"superseded": why}),
    )
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

// ------------------------------------------------------------------ bilan

/// `prod_report` : fige le bilan de ce qui vient d'être vérifié. La carte d'approbation qui
/// suit le montre (sortie de l'étape précédente).
pub(super) async fn report(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    const PURPOSE: &str = "présenter le bilan avant la production";
    let s = ctx.s();
    let p = match project(ctx, PURPOSE).await {
        Ok(p) => p,
        Err(o) => return Ok(o),
    };
    let target = match gate::resolve_prod(&p.cfg) {
        Ok(t) => t,
        Err(m) => return Ok(asked(PURPOSE, &p.dir, &m)),
    };
    let Some(state) = state_of(s, &ctx.run.id).await else {
        return Ok(blocked(
            "Aucune PR dev enregistrée pour ce run : pas de bilan sans elle.".into(),
        ));
    };
    let sha = text(&state["sha"]);
    let verified_sha = verified_sha(&state);
    let (ci, e2e) = (
        &ctx.run.step_outputs[CI_STEP],
        &ctx.run.step_outputs[E2E_STEP],
    );
    // Le bilan ne dit que ce qui a été vérifié sur ce commit-là.
    let verified =
        |out: &Value, sha_of: &Value| out["result"] == PASSED && sha_of == &json!(verified_sha);
    if !verified(ci, &ci["ci"]["sha"]) || !verified(e2e, &e2e["e2e"]["sha"]) {
        return Ok(blocked(format!(
            "Pour {PURPOSE} : la CI et l'E2E du commit `{}` ne sont pas tous deux verts. \
             « Réessayer » relit le bilan.",
            short(&verified_sha)
        )));
    }
    let plan = match plans(s, &ctx.run.id).await {
        Some((executed, active)) => {
            if let Some(why) = gate::supersession(&executed, active.as_ref()) {
                return Ok(superseded(&why));
            }
            Some(executed)
        }
        None => None,
    };
    let bilan = Bilan {
        run: ctx.run.id.clone(),
        plan,
        forge: text(&state["forge"]),
        repo: text(&state["repo"]),
        head: text(&state["head"]),
        sha,
        verified_sha,
        dev_branch: target.dev_branch.clone(),
        prod_branch: target.prod_branch.clone(),
        dev_pr_number: state["pr"]["number"].as_u64().unwrap_or_default(),
        dev_pr_url: text(&state["pr"]["url"]),
        ci: if ci["ci"]["provider"] == "none" {
            "none".into()
        } else {
            "green".into()
        },
        ci_detail: text(&ci["ci"]["detail"]),
        e2e_url: text(&e2e["e2e"]["url"]),
        e2e_checks: e2e["e2e"]["checks"].as_array().map_or(0, Vec::len),
        evidence: text(&e2e["evidence"]),
        verified_at_ms: s.clock.now_ms().max(0) as u64,
    };
    Ok(outcome(
        StepResult::Passed,
        bilan.render(target.max_age_ms),
        json!({"bilan": bilan, "fingerprint": bilan.fingerprint()}),
    ))
}

// ------------------------------------------------------------------ PR prod

/// `prod_pull_request` : la PR dev → prod, une fois, après l'approbation d'un bilan frais.
pub(super) async fn pull_request(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    const PURPOSE: &str = "proposer la PR prod";
    let s = ctx.s();
    let outputs = &ctx.run.step_outputs;
    if outputs[GATE_STEP]["choice"] != APPROVE {
        return Ok(blocked(format!(
            "Pour {PURPOSE} : aucune approbation du propriétaire sur ce run. Rien n'est proposé."
        )));
    }
    let Ok(bilan) = serde_json::from_value::<Bilan>(outputs[REPORT_STEP]["bilan"].clone()) else {
        return Ok(blocked(format!(
            "Pour {PURPOSE} : aucun bilan vérifié sur ce run. Rien n'est proposé."
        )));
    };

    // Une seule PR prod par run : la clé ne dépend ni du bilan, ni de la configuration, ni
    // de la visite. Elle est posée avant tout appel au forgeur.
    let spec = EffectSpec::new(
        EffectKind::Http,
        "delivery.prod_pull_request",
        json!({"run": ctx.run.id, "pr": "prod"}),
    )
    .run(&ctx.run.id)
    .session(&ctx.run.session_id)
    .step(&ctx.step.id)
    .idempotent(true);
    let (id, judge) = match s.effects.plan(spec).await? {
        Planned::Replayed(pr) => return Ok(proposed(ctx, &bilan, pr).await),
        Planned::InFlight(_) => {
            return Ok(StepOutcome::Waiting("PR prod en cours d'ouverture".into()));
        }
        Planned::NeedsDecision(id) => {
            s.effects
                .resolve_unknown(&id, UnknownDecision::Retry)
                .await?;
            (id, false)
        }
        // Un envoi commencé puis interrompu (redémarrage : l'effet idempotent revient en
        // `planned`, sans erreur) : une vie antérieure a agi sur une approbation valide. On
        // termine ce qu'elle a commencé, la PR cherchée avant d'être ouverte, sans rejuger
        // le bilan.
        Planned::Fresh(id) => {
            let interrupted = s
                .effects
                .get(&id)
                .await?
                .is_some_and(|e| e.attempts > 0 && e.error.is_none());
            (id, !interrupted)
        }
    };
    let p = match project(ctx, PURPOSE).await {
        Ok(p) => p,
        Err(o) => return Ok(o),
    };
    let target = match gate::resolve_prod(&p.cfg) {
        Ok(t) => t,
        Err(m) => return Ok(asked(PURPOSE, &p.dir, &m)),
    };
    let forge = match connect(s, &p, PURPOSE) {
        Ok(f) => f,
        Err(o) => return Ok(o),
    };
    if judge && let Err(o) = still_valid(ctx, &forge, &bilan, &target).await {
        return Ok(o);
    }
    s.effects.dispatching(&id).await?;
    match open_once(ctx, &forge, &bilan, &target).await {
        Ok(pr) => {
            s.effects.complete(&id, pr.clone()).await?;
            Ok(proposed(ctx, &bilan, pr).await)
        }
        Err(e) => {
            s.effects.fail(&id, e.to_string()).await?;
            Ok(blocked(format!("Pour {PURPOSE} : {e}.")))
        }
    }
}

/// Relit ce que le bilan affirmait : le plan actif, le commit de la PR dev, la CI. Rend la
/// sortie de l'étape quand le bilan ne tient plus ou que la PR dev n'est pas fusionnée.
async fn still_valid(
    ctx: &StepCtx<'_>,
    forge: &forge::Forge,
    bilan: &Bilan,
    target: &ProdTarget,
) -> Result<(), StepOutcome> {
    const PURPOSE: &str = "proposer la PR prod";
    let s = ctx.s();
    let active = match plans(s, &ctx.run.id).await {
        Some((_, active)) => active,
        None => None,
    };
    let dev_pr = match forge.list_prs(&bilan.head, &bilan.dev_branch).await {
        Ok(prs) => prs.into_iter().find(|p| p.number == bilan.dev_pr_number),
        Err(e) => {
            return Err(blocked(format!(
                "Pour {PURPOSE}, la PR dev ne se relit pas : {e}. « Réessayer » la relit ; \
                 rien n'est proposé sans elle."
            )));
        }
    };
    let ci = if bilan.ci == "none" {
        None
    } else {
        forge.ci(&bilan.verified_sha).await.ok()
    };
    let seen = Seen {
        now_ms: s.clock.now_ms().max(0) as u64,
        active_plan: active,
        dev_pr_head: dev_pr.as_ref().and_then(|p| p.head_sha.clone()),
        dev_pr_merge: dev_pr
            .as_ref()
            .filter(|p| p.merged)
            .and_then(|p| p.merge_sha.clone()),
        ci,
    };
    match gate::freshness(bilan, &seen, target.max_age_ms) {
        Freshness::Fresh => {}
        Freshness::Superseded(why) => return Err(superseded(&why)),
        Freshness::Stale(why) => {
            note(
                ctx,
                "stale",
                &format!(
                    "⚠️ Bilan périmé : {why}. Aucune PR prod : je re-vérifie la PR dev, la CI \
                     et l'E2E, puis une nouvelle carte suivra."
                ),
            )
            .await;
            return Err(outcome(
                StepResult::Choice(STALE.into()),
                format!("Bilan périmé, aucune PR prod : {why}. Re-vérification."),
                json!({"stale": why}),
            ));
        }
    }
    match dev_pr {
        Some(pr) if pr.merged => Ok(()),
        _ => Err(blocked(format!(
            "Pour {PURPOSE} : la PR dev {} n'est pas fusionnée dans `{}` ; la PR `{}` → `{}` ne \
             porterait pas ce travail. Fusionne-la, puis « Réessayer » : l'approbation tient tant \
             que le bilan est frais.",
            bilan.dev_pr_url, bilan.dev_branch, target.dev_branch, target.prod_branch
        ))),
    }
}

/// Marque du run dans la description de sa PR de production.
fn marker(run_id: &str) -> String {
    format!("<!-- penelope-prod-run: {run_id} -->")
}

/// Cherche la PR de production de ce run (sa marque), puis une PR dev → prod déjà ouverte,
/// et n'ouvre la sienne qu'à défaut.
async fn open_once(
    ctx: &StepCtx<'_>,
    forge: &forge::Forge,
    bilan: &Bilan,
    target: &ProdTarget,
) -> Result<Value, forge::ForgeError> {
    let (head, base) = (&target.dev_branch, &target.prod_branch);
    let mark = marker(&ctx.run.id);
    let prs = forge.list_prs(head, base).await?;
    let existing = prs
        .iter()
        .find(|p| p.body.contains(&mark))
        .or_else(|| prs.iter().find(|p| p.is_open()));
    if let Some(pr) = existing {
        let mut v = pr.to_json();
        v["found"] = json!(true);
        return Ok(v);
    }
    let goal = ctx
        .wf
        .metadata
        .name
        .split_once(" · ")
        .map_or(ctx.wf.metadata.name.as_str(), |(_, goal)| goal);
    let body = format!(
        "Proposée par Pénélope après approbation explicite du propriétaire ({}).\n\n{}\n\n\
         Pénélope ne fusionne pas cette PR et ne déploie rien : le déploiement suit la \
         politique du projet.\n\n{mark}",
        ctx.s().clock.now_rfc3339(),
        bilan.summary()
    );
    let pr = forge
        .open_pr(head, base, &format!("Production · {goal}"), &body)
        .await?;
    let mut v = pr.to_json();
    v["found"] = json!(false);
    Ok(v)
}

/// La PR de production est proposée (ou retrouvée) : le lien part dans le sujet du run.
async fn proposed(ctx: &StepCtx<'_>, bilan: &Bilan, pr: Value) -> StepOutcome {
    let s = ctx.s();
    let noun = ForgeKind::parse(&bilan.forge).map_or("PR", ForgeKind::request_noun);
    let url = text(&pr["url"]);
    let verb = if pr["found"].as_bool().unwrap_or(false) {
        "retrouvée"
    } else {
        "proposée"
    };
    let branches = format!("`{}` → `{}`", bilan.dev_branch, bilan.prod_branch);
    if let Some(mut state) = state_of(s, &ctx.run.id).await {
        state["prod_pr"] = pr.clone();
        let _ = s.kv_set(&state_key(&ctx.run.id), &state.to_string()).await;
    }
    note(
        ctx,
        "prod",
        &format!(
            "🚀 {noun} prod {verb} : {url} ({branches}). Rien n'est fusionné ni déployé : le \
             déploiement suit la politique du projet."
        ),
    )
    .await;
    outcome(
        StepResult::Passed,
        format!(
            "{noun} prod {verb} : {url} ({branches}), bilan approuvé. Ni fusionnée ni \
             déployée par Pénélope : le déploiement suit la politique du projet."
        ),
        json!({"prod_pr": pr, "bilan": bilan.fingerprint()}),
    )
}
