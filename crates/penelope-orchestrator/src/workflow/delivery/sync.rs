//! `sync_with_base` (#397) : la branche de base (`branches.dev` de
//! `.penelope/delivery.toml`) récupérée puis fusionnée dans la branche du run. Une fusion
//! propre rend `passed` ; des conflits rendent `failed` avec la liste des fichiers, la
//! fusion laissée en cours : c'est une donnée pour l'étape suivante, qui les résout et
//! commite. Rien n'est poussé ici.

use super::*;

/// Fichiers en conflit cités au plus dans la sortie.
const MAX_CONFLICTS: usize = 50;

fn conflicted(base: &str, files: Vec<String>) -> StepOutcome {
    let shown: Vec<String> = files.iter().take(MAX_CONFLICTS).cloned().collect();
    outcome(
        StepResult::Failed,
        format!(
            "La fusion de `{base}` laisse {} fichier(s) en conflit : {}. Résous-les, puis \
             commite la fusion (`git add` puis `git commit --no-edit`).",
            files.len(),
            shown.join(", ")
        ),
        json!({"base": base, "conflicts": shown}),
    )
}

pub(super) async fn sync_with_base(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let purpose = "synchroniser la branche avec sa base";
    let p = match project(ctx, purpose).await {
        Ok(p) => p,
        Err(o) => return Ok(o),
    };
    let Some(base) = config::non_empty_branch(&p.cfg.branches.dev) else {
        return Ok(asked(
            purpose,
            &p.dir,
            &[config::Missing::new(
                "branches.dev",
                "la branche de base à fusionner dans celle du run",
            )],
        ));
    };
    // Une fusion déjà en cours : ses conflits d'abord, ou son commit.
    if git::merging(&p.dir).await {
        let files = git::conflicts(&p.dir).await;
        if !files.is_empty() {
            return Ok(conflicted(&base, files));
        }
        return Ok(outcome(
            StepResult::Failed,
            "Les conflits de la fusion sont résolus mais la fusion n'est pas commitée : \
             `git commit --no-edit`."
                .into(),
            json!({"base": base, "conflicts": []}),
        ));
    }
    let remote = config::remote_name(&p.cfg);
    if let Err(e) = git::fetch(&p.dir, &remote, &base).await {
        return Ok(blocked(format!("Pour {purpose} : {e}.")));
    }
    let before = git::head_sha(&p.dir).await.unwrap_or_default();
    let rev = format!("{remote}/{base}");
    if let Err(e) = git::merge(&p.dir, &rev).await {
        let files = git::conflicts(&p.dir).await;
        if !files.is_empty() {
            return Ok(conflicted(&base, files));
        }
        git::merge_abort(&p.dir).await;
        return Ok(blocked(format!("Pour {purpose} : {e}.")));
    }
    let after = git::head_sha(&p.dir).await.unwrap_or_default();
    let content = if before == after {
        format!("La branche contient déjà `{rev}`.")
    } else {
        format!(
            "`{rev}` fusionnée : {} → {}.",
            short(&before),
            short(&after)
        )
    };
    Ok(outcome(
        StepResult::Passed,
        content,
        json!({"base": base, "merged": before != after, "head": after}),
    ))
}
