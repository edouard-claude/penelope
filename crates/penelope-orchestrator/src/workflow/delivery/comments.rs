//! Les commentaires de la PR d'un run pendant la livraison (#396, point 3).
//!
//! Lus à chaque lecture de la CI, si la section `[review]` nomme des auteurs (sinon rien
//! n'est lu) : un commentaire n'est vu qu'une fois (ids en kv), un
//! auteur hors de la section `[review]` est ignoré et journalisé, un seul run lit une PR
//! donnée. Une demande de changements d'un auteur autorisé rend `changes_requested` :
//! la reprise du code, sa revue, puis la même PR ; au-delà de `MAX_REWORKS`, le run
//! s'arrête et le propriétaire décide. Jamais de fusion ni de déploiement.

use super::*;
use penelope_workflow::delivery::CHANGES_REQUESTED;
use penelope_workflow::delivery::review::{self, PrComment};
use penelope_workflow::plan::execution::MAX_REWORKS;

fn seen_key(run_id: &str) -> String {
    format!("wf.delivery.seen.{run_id}")
}

fn reworks_key(run_id: &str) -> String {
    format!("wf.delivery.reworks.{run_id}")
}

/// Ce que les commentaires nouveaux de la PR décident, s'ils décident quelque chose.
pub(super) async fn review_gate(
    ctx: &StepCtx<'_>,
    p: &Project,
    forge: &Forge,
    state: &Value,
) -> anyhow::Result<Option<StepOutcome>> {
    let s = ctx.s();
    let Some(number) = state["pr"]["number"].as_u64() else {
        return Ok(None);
    };
    // Sans section `[review]`, aucun auteur ne compte : rien à lire sur le forgeur.
    if p.cfg.review.roles.is_empty() && p.cfg.review.logins.is_empty() {
        return Ok(None);
    }
    // Un seul run lit une PR : le premier qui la sonde.
    let owner_key = format!(
        "wf.delivery.owner.{}.{}.{number}",
        forge.kind().as_str(),
        forge.target().repo
    );
    match s.kv_get(&owner_key).await? {
        Some(owner) if owner != ctx.run.id => return Ok(None),
        Some(_) => {}
        None => s.kv_set(&owner_key, &ctx.run.id).await?,
    }
    let comments = match forge.comments(number).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(run = %ctx.run.id, error = %e, "commentaires de PR non lus");
            return Ok(None);
        }
    };
    let mut seen: Vec<String> = s
        .kv_get(&seen_key(&ctx.run.id))
        .await?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let fresh: Vec<PrComment> = comments
        .into_iter()
        .filter(|c| !seen.contains(&c.id))
        .collect();
    if fresh.is_empty() {
        return Ok(None);
    }
    seen.extend(fresh.iter().map(|c| c.id.clone()));
    s.kv_set(&seen_key(&ctx.run.id), &serde_json::to_string(&seen)?)
        .await?;
    let (allowed, ignored): (Vec<PrComment>, Vec<PrComment>) =
        fresh.into_iter().partition(|c| c.authorised(&p.cfg.review));
    for c in &ignored {
        tracing::info!(run = %ctx.run.id, pr = number, author = %c.author, role = %c.role,
            id = %c.id, "commentaire de PR ignoré : auteur hors de [review]");
    }
    if allowed.is_empty() {
        return Ok(None);
    }
    let quoted = penelope_observe::injection::wrap_untrusted(
        &format!("PR #{number}"),
        &review::quoted(&allowed),
    );
    if !allowed.iter().any(|c| c.changes_requested) {
        note(
            ctx,
            &format!("comments-{}", seen.len()),
            &format!("💬 Commentaires sur la PR #{number} :\n{quoted}"),
        )
        .await;
        return Ok(None);
    }
    let done: u32 = s
        .kv_get(&reworks_key(&ctx.run.id))
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if done >= MAX_REWORKS {
        return Ok(Some(outcome(
            StepResult::Blocked,
            format!(
                "La revue de la PR #{number} demande encore des changements : limite de \
                 {MAX_REWORKS} reprises atteinte, à toi de décider.\n{quoted}"
            ),
            json!({"review": {"pr": number, "reworks": done}}),
        )));
    }
    s.kv_set(&reworks_key(&ctx.run.id), &(done + 1).to_string())
        .await?;
    Ok(Some(outcome(
        StepResult::Choice(CHANGES_REQUESTED.into()),
        format!(
            "La revue de la PR #{number} demande des changements (reprise {} sur \
             {MAX_REWORKS}) :\n{quoted}",
            done + 1
        ),
        json!({"review": {"pr": number, "reworks": done + 1}}),
    )))
}
