//! Crédits épuisés pendant un run (#339) : pause au prochain point sûr, message au
//! propriétaire dans le sujet du run, reprise au retour du quota.
//!
//! Le point sûr est la fin de l'appel d'outil en cours : la boucle d'agent n'abandonne
//! qu'un appel **au modèle**, les résultats d'outils déjà rendus sont au journal. Le run
//! passe `paused` sans enregistrer de résultat d'étape ; la reprise rejoue la visite de
//! l'étape là où elle en était.

use super::*;
use penelope_app::credits::{CREDITS_EXHAUSTED, CreditStop, DAILY_BUDGET};

fn pause_key(run_id: &str) -> String {
    format!("wf.credits.{run_id}")
}

/// L'arrêt faute de crédits d'un tour d'étape qui a échoué sur `error` : celui que la
/// boucle a rangé pour la session, sinon déduit du message.
pub(super) async fn stop_of(s: &Services, session_id: &str, error: &str) -> Option<CreditStop> {
    if !error.starts_with(CREDITS_EXHAUSTED) {
        return None;
    }
    let stored = penelope_app::credits::take(&s.store, session_id).await;
    Some(stored.unwrap_or_else(|| {
        CreditStop {
            provider: String::new(),
            model: String::new(),
            reason: error
                .trim_start_matches(CREDITS_EXHAUSTED)
                .trim()
                .to_string(),
            at_ms: s.clock.now_ms(),
            until_ms: None,
        }
    }))
}

/// Le budget journalier (`budget.daily_usd`) atteint : retour au minuit du propriétaire.
pub(super) fn daily_stop(s: &Services) -> CreditStop {
    let now = s.clock.now_ms();
    let tz = s.config.config().owner.timezone.clone();
    CreditStop {
        provider: DAILY_BUDGET.into(),
        model: String::new(),
        reason: "budget.daily_usd".into(),
        at_ms: now,
        until_ms: Some(penelope_app::credits::next_midnight_ms(now, &tz)),
    }
}

/// Met le run en pause faute de crédits, dit une fois où il s'est arrêté et quand il
/// reprend. L'étape n'a pas de résultat : sa visite reprendra telle quelle.
pub(super) async fn pause(ctx: &StepCtx<'_>, stop: CreditStop) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let run = ctx.run;
    s.runs
        .set_state(
            &run.id,
            RunState::Paused,
            Some(&format!("en pause : {}", stop.what())),
        )
        .await?;
    s.kv_set(&pause_key(&run.id), &serde_json::to_string(&stop)?)
        .await?;
    let _ = s
        .events
        .append(
            EventDraft::new(
                "workflow.credits_paused",
                json!({"run": run.id, "step": ctx.step.id, "provider": stop.provider,
                       "until_ms": stop.until_ms}),
            )
            .session(&run.session_id),
        )
        .await;
    // Une fois par arrêt, même si la visite est rejouée avant la reprise.
    let said = format!("wf.credits_said.{}.{}", run.id, stop.at_ms);
    if s.kv_get(&said).await?.is_none()
        && let Some(m) = ctx.d.workflows.ports.messenger.get()
    {
        let text = pause_text(ctx, &stop).await;
        let origin = origin_of(s, &run.id).await;
        if m.send_text(&origin, &text).await.is_ok() {
            s.kv_set(&said, "1").await?;
        }
    }
    if let Some(current) = s.runs.get(&run.id).await? {
        progress(ctx.d, &current, ctx.wf, None).await;
    }
    Ok(StepOutcome::Waiting(stop.what()))
}

/// « ⏸ Je me suis arrêtée là, crédits Codex épuisés. Dernier point : … Reprise prévue à
/// 17 h 40, au retour du quota. »
async fn pause_text(ctx: &StepCtx<'_>, stop: &CreditStop) -> String {
    let s = ctx.s();
    let cfg = s.config.config();
    let run = ctx.run;
    let mut text = format!(
        "⏸ Je me suis arrêtée là, {}. Dernier point : {}.",
        stop.what(),
        last_point(ctx).await
    );
    match (cfg.workflows.resume_on_quota, stop.until_ms) {
        (true, Some(at)) => text.push_str(&format!(
            " Reprise prévue à {}, {}.",
            penelope_app::credits::clock_at(at, &cfg.owner.timezone),
            stop.back_when()
        )),
        (true, None) => text.push_str(&format!(
            " Reprise {} : ▶️ Reprendre sur le run (`/runs`) ou `/resume {}`.",
            stop.back_when(),
            run.id
        )),
        (false, _) => text.push_str(&format!(
            " Reprise à la main : ▶️ Reprendre sur le run (`/runs`) ou `/resume {}`.",
            run.id
        )),
    }
    text
}

/// Où en est le run : position dans la liste s'il en déroule une, étape, dernier commit.
async fn last_point(ctx: &StepCtx<'_>) -> String {
    let mut parts = Vec::new();
    let step = if ctx.step.name.is_empty() {
        &ctx.step.id
    } else {
        &ctx.step.name
    };
    parts.push(format!("étape {step}"));
    let metadata = session_metadata(ctx.s(), &ctx.run.session_id).await;
    let dir = metadata["project"]["dir"]
        .as_str()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ctx.workdir());
    if let Some(commit) = last_commit(&dir).await {
        parts.push(commit);
    }
    parts.join(", ")
}

/// `commit `abc1234` poussé`, ou `non poussé`, si le répertoire est un dépôt git.
async fn last_commit(dir: &std::path::Path) -> Option<String> {
    let git = |args: &[&str]| {
        let mut c = tokio::process::Command::new("git");
        c.arg("-C").arg(dir).args(args);
        c
    };
    let out = git(&["log", "-1", "--format=%h"]).output().await.ok()?;
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if !out.status.success() || sha.is_empty() {
        return None;
    }
    let ahead = git(&["rev-list", "--count", "@{u}..HEAD"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|n| n.trim() == "0");
    Some(match ahead {
        Some(true) => format!("commit `{sha}` poussé"),
        Some(false) => format!("commit `{sha}` non poussé"),
        None => format!("commit `{sha}`"),
    })
}

/// Reprend les runs en pause faute de crédits dont le retour est passé, si
/// `workflows.resume_on_quota` le permet ; un arrêt sans heure de retour (402) attend
/// « Reprendre ». Rend le nombre de runs repris.
pub(super) async fn resume_due(d: &Context) -> anyhow::Result<usize> {
    let s = &d.services;
    if !s.config.config().workflows.resume_on_quota {
        return Ok(0);
    }
    let now = s.clock.now_ms();
    let mut resumed = 0;
    for run in s.runs.list(Some(RunState::Paused), 500).await? {
        let Some(raw) = s.kv_get(&pause_key(&run.id)).await? else {
            continue;
        };
        let Ok(stop) = serde_json::from_str::<CreditStop>(&raw) else {
            continue;
        };
        if stop.until_ms.is_none_or(|at| at > now) {
            continue;
        }
        forget(s, &run.id).await?;
        s.runs
            .set_state(&run.id, RunState::Running, Some(""))
            .await?;
        let _ = s
            .events
            .append(
                EventDraft::new(
                    "workflow.credits_resumed",
                    json!({"run": run.id, "provider": stop.provider}),
                )
                .session(&run.session_id),
            )
            .await;
        if let Some(m) = d.workflows.ports.messenger.get() {
            let origin = origin_of(s, &run.id).await;
            let text = format!(
                "▶️ Je reprends `{}` : {}, retour constaté.",
                run.id,
                stop.what()
            );
            let _ = m.send_text(&origin, &text).await;
        }
        resumed += 1;
    }
    Ok(resumed)
}

/// Oublie la pause faute de crédits d'un run : reprise automatique ou à la main.
pub(super) async fn forget(s: &Services, run_id: &str) -> anyhow::Result<()> {
    s.kv_delete(&pause_key(run_id)).await
}
