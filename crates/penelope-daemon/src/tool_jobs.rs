//! Jobs d'outils natifs : un appel long sort du tour (issue #204).
//!
//! Avant : `run_effect` planifiait l'effet puis attendait `execute_cancellable` du début à
//! la fin. Un `shell_exec` à `timeout_ms` d'une heure immobilisait le tour, et le message
//! du propriétaire attendait derrière lui.
//!
//! ```text
//!  appel `background: true`
//!     ledger : effet planifié puis `dispatching` (§4.2, rien ne le contourne)
//!     ligne `tool_jobs` `working`, événement `tool.job.started`
//!     l'outil rend tout de suite {job, state:"working"} ─► le tour continue
//!
//!  ailleurs, hors du tour
//!     l'outil tourne, `/stop` le coupe par son jeton
//!     fin : ledger `completed`/`failed`, job terminal, `tool.job.completed`
//!
//!  livraison
//!     boucle de fond ─► tour `Nudge` dans la session d'origine, avec le résultat
//!     session fermée : rien n'est perdu, la livraison attend sa réouverture
//! ```
//!
//! Le modèle de durée est celui des tâches MCP (`penelope_mcp::tasks`) : mêmes états —
//! `TaskState` est importé, pas redéfini —, mêmes colonnes `request`/`result`, mêmes
//! règles de purge et de rétention. La table est distincte parce qu'un job natif ne se
//! sonde pas chez un serveur : il tourne ici, porte un outil, ses arguments et l'effet du
//! ledger. Sans `poll_at`, donc : rien ne le sonde (décision 0012).

use crate::runtime::Daemon;
use penelope_agent::{JobRequest, ToolExecutor};
use penelope_app::{bus::Origin, services::Services};
use penelope_kernel::event::EventDraft;
use penelope_kernel::ids::EffectId;
use penelope_kernel::turn::TurnKind;
use penelope_llm::CancelToken;
use penelope_mcp::tasks::TaskState;
use penelope_tools::ToolOutcome;
use serde_json::json;
use std::sync::Arc;

use penelope_executor::executor::effective_arguments;
use penelope_executor::jobs::*;
use penelope_executor::tools_on_demand;

// ---------------------------------------------------------------- lancement

/// Vrai quand une session sait recevoir une relance : une conversation, ou une session
/// planifiée, qui est une conversation ouverte par un déclencheur (#39).
async fn delivers_to_a_conversation(s: &Services, session_id: &str) -> bool {
    use penelope_kernel::session::SessionKind;
    matches!(
        s.sessions.get(session_id).await,
        Ok(Some(ref sess)) if matches!(sess.kind, SessionKind::Chat | SessionKind::Scheduled)
    )
}

/// Transforme l'appel en job s'il le demande. `None` : rien à faire, l'appel suit le
/// chemin ordinaire et le ledger reste sur sa trajectoire habituelle.
///
/// Sur refus (plafond atteint), l'effet est clos `failed` : il ne reste pas `planned`
/// dans le ledger pour un appel qui n'aura jamais lieu.
pub async fn maybe_spawn(
    services: &Arc<Services>,
    execute: &(dyn ToolExecutor + Send + Sync),
    req: JobRequest<'_>,
) -> anyhow::Result<Option<ToolOutcome>> {
    // `tool_call` enveloppe l'appel dans `{name, args}` : la demande d'arrière-plan est
    // dans l'enveloppe, pas au premier niveau.
    let asked = effective_arguments(&req.call.name, &req.call.arguments);
    if !wants_background(req.tool, &asked) {
        return Ok(None);
    }
    // Un exécuteur qui ne sait pas se détacher exécute comme avant : mieux vaut un tour
    // occupé qu'un job qui n'aboutit jamais.
    let Some(detached) = execute.detached() else {
        return Ok(None);
    };
    let s = services.clone();
    // Un job n'existe que pour une conversation. La session d'un sous-agent meurt avec sa
    // conclusion et celle d'un run de workflow est pilotée, pas par des tours : la relance
    // n'y trouverait personne, et un run a déjà son attente d'étape (`mcp_tasks`). Là,
    // l'appel suit le chemin ordinaire — c'est le comportement d'avant #204, qui rend son
    // vrai résultat.
    if !delivers_to_a_conversation(&s, req.session_id).await {
        return Ok(None);
    }
    let cfg = s.config.config();
    let jobs = store(&s);
    let (mine, total) = jobs.counts(req.session_id).await?;
    if mine >= cfg.tools.jobs_per_session || total >= cfg.tools.jobs_total {
        let reason = if mine >= cfg.tools.jobs_per_session {
            format!(
                "{mine} job(s) déjà en cours dans cette conversation, plafond \
                 `tools.jobs_per_session` = {}",
                cfg.tools.jobs_per_session
            )
        } else {
            format!(
                "{total} job(s) déjà en cours sur tout le daemon, plafond \
                 `tools.jobs_total` = {}",
                cfg.tools.jobs_total
            )
        };
        s.effects.fail(req.effect, &reason).await?;
        return Ok(Some(ToolOutcome {
            value: json!({"state": "refused", "reason": reason}),
            is_error: true,
            text: format!(
                "Job refusé : {reason}. Attends les précédents — `job_list` les montre, \
                 `job_wait` en attend un — ou lance cet appel sans `background`.",
            ),
            eager: false,
        }));
    }

    s.effects.dispatching(req.effect).await?;
    let job = jobs
        .create(NewJob {
            session_id: req.session_id.to_string(),
            run_id: req.run_id.map(String::from),
            turn_id: req.turn_id.map(String::from),
            call_id: Some(req.call.id.clone()),
            tool: req.tool.to_string(),
            request: penelope_observe::redact_json(&asked),
            effect_id: Some(req.effect.as_str().to_string()),
        })
        .await?;

    // Le résultat renvoie vers `job_status` et consorts : ils doivent être sous la main
    // du tour suivant, pas seulement trouvables par `tool_search` (#104).
    for t in FOLLOW_UP {
        tools_on_demand::touch(&s, req.session_id, t).await;
    }

    let cancel = CancelToken::new();
    s.jobs.insert(&job.id, req.session_id, cancel.clone());
    s.events
        .append(
            EventDraft::new(
                "tool.job.started",
                json!({"job": job.id, "tool": job.tool, "effect": req.effect.as_str()}),
            )
            .session(req.session_id),
        )
        .await?;

    let name = req.call.name.clone();
    let args = req.call.arguments.clone();
    let effect = req.effect.clone();
    let id = job.id.clone();
    let session = req.session_id.to_string();
    tokio::spawn(async move {
        // Une tâche qui panique s'arrête en silence (#84). Ici, le silence laisserait le
        // job `working` et son effet `dispatching` pour toujours : la panique devient un
        // échec ordinaire, que le ledger et la livraison savent traiter.
        let run = std::panic::AssertUnwindSafe(detached.execute_cancellable(&name, &args, &cancel));
        let outcome = match futures::FutureExt::catch_unwind(run).await {
            Ok(o) => o,
            Err(payload) => {
                let message = penelope_app::tasks::panic_text(payload.as_ref());
                tracing::error!(job = %id, tool = %name, %message, "job en panique");
                Err(penelope_tools::ToolError::Other(format!(
                    "l'outil a paniqué : {message}"
                )))
            }
        };
        if let Err(e) = conclude(&s, &id, &session, &effect, outcome, cancel.is_cancelled()).await {
            tracing::error!(job = %id, error = %e, "fin de job non enregistrée");
        }
    });

    Ok(Some(ToolOutcome {
        value: json!({"job": job.id, "state": "working"}),
        is_error: false,
        text: format!(
            "Job `{}` lancé en arrière-plan ({}). Le tour continue : réponds sans \
             l'attendre. `job_status` donne son état, `job_wait` l'attend un moment, \
             `job_cancel` l'arrête. Son résultat reviendra seul dans cette conversation, \
             même si ce tour est clos depuis longtemps.",
            job.id, job.tool
        ),
        eager: false,
    }))
}

/// Clôt un job : ledger d'abord, ligne ensuite, événement enfin.
async fn conclude(
    s: &Arc<Services>,
    job_id: &str,
    session_id: &str,
    effect: &EffectId,
    outcome: Result<ToolOutcome, penelope_tools::ToolError>,
    cancelled: bool,
) -> anyhow::Result<()> {
    s.jobs.forget(job_id);
    let (state, result, error) = match &outcome {
        _ if cancelled => (
            TaskState::Cancelled,
            json!({"cancelled": true}),
            Some("job annulé".to_string()),
        ),
        Ok(o) if !o.is_error => (TaskState::Completed, o.value.clone(), None),
        Ok(o) => (
            TaskState::Failed,
            json!({"error": o.text}),
            Some(o.text.clone()),
        ),
        Err(e) => (
            TaskState::Failed,
            json!({"error": e.to_string()}),
            Some(e.to_string()),
        ),
    };
    // Le ledger d'abord : un job conclu dont l'effet serait resté `dispatching`
    // poserait une question au propriétaire au prochain démarrage pour rien (#83).
    match &error {
        None => s.effects.complete(effect, result.clone()).await?,
        Some(e) => s.effects.fail(effect, e.clone()).await?,
    }
    let text = match &outcome {
        Ok(o) => o.text.clone(),
        Err(e) => e.to_string(),
    };
    let stored = json!({"value": result, "text": text});
    store(s).finish(job_id, state, Some(stored)).await?;
    s.events
        .append(
            EventDraft::new(
                "tool.job.completed",
                json!({"job": job_id, "state": state.as_str(), "effect": effect.as_str()}),
            )
            .session(session_id),
        )
        .await?;
    s.jobs.wake();
    Ok(())
}

// ---------------------------------------------------------------- livraison

/// Ce que le tour de relance lit. Il nomme l'outil et la demande : le job revient dans
/// une conversation qui a pu changer de sujet entre-temps.
pub fn delivery_text(job: &ToolJob, now_ms: i64) -> String {
    let demande = job.request["command"]
        .as_str()
        .or_else(|| job.request["prompt"].as_str())
        .map(|c| c.chars().take(300).collect::<String>())
        .unwrap_or_else(|| job.tool.clone());
    let corps = job
        .result
        .as_ref()
        .map(|r| {
            let text = r["text"].as_str().unwrap_or_default();
            if text.is_empty() {
                r["error"].as_str().unwrap_or(&r.to_string()).to_string()
            } else {
                text.to_string()
            }
        })
        .unwrap_or_else(|| "sans résultat".into());
    format!(
        "Résultat du job `{}` lancé plus tôt dans cette conversation : `{}` — {}, \
         après {} s.\n\nDemande d'origine : {demande}\n\nRésultat :\n{}\n\n\
         Reprends la suite si elle a encore un sens, ou dis-le si le sujet a changé.",
        job.id,
        job.tool,
        job.state.as_str(),
        job.age_s(now_ms),
        corps.chars().take(6_000).collect::<String>()
    )
}

/// Origine de livraison d'une session : le chat Telegram auquel elle est liée, sinon un
/// canal interne. Elle se relit après une purge, contrairement à ce qu'on aurait rangé
/// dans les arguments du job.
async fn origin_of(s: &Services, session_id: &str) -> Origin {
    match s.sessions.get(session_id).await {
        Ok(Some(sess)) => match sess.tg_chat_id {
            Some(chat_id) => Origin::Telegram {
                chat_id,
                topic_id: sess.tg_topic_id,
                message_id: None,
            },
            None => Origin::Internal {
                source: "tool_job".into(),
            },
        },
        _ => Origin::Internal {
            source: "tool_job".into(),
        },
    }
}

/// Livre les résultats prêts. Un job dont la session est fermée attend sa réouverture :
/// `TurnQueue::claim` annule les tours d'une session fermée, une relance enfilée là
/// serait perdue.
pub async fn deliver_due(d: &Arc<Daemon>) -> anyhow::Result<usize> {
    let s = &d.services;
    let now = s.clock.now_ms();
    let mut sent = 0;
    for job in store(s).undelivered(20).await? {
        let open = matches!(
            s.sessions.get(&job.session_id).await,
            Ok(Some(ref sess)) if sess.state == "active"
        );
        if !open {
            continue;
        }
        if !store(s).mark_delivered(&job.id).await? {
            continue;
        }
        let origin = origin_of(s, &job.session_id).await;
        s.turns
            .enqueue(
                &job.session_id,
                TurnKind::Nudge,
                json!({"text": delivery_text(&job, now), "origin": origin.to_value(), "job": job.id}),
                Some(format!("job:{}", job.id)),
                5,
            )
            .await?;
        d.bus.notify_enqueued();
        sent += 1;
    }
    Ok(sent)
}

/// Battement de la boucle de livraison quand rien ne la réveille : une session rouverte
/// ou un job repris au démarrage n'émet pas de réveil, il faut donc repasser.
const DELIVERY_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Boucle de livraison, surveillée comme les autres (issue #84).
pub async fn deliver_loop(d: Arc<Daemon>) {
    while !d.handle.is_shutting_down() {
        if let Err(e) = deliver_due(&d).await {
            tracing::warn!(error = %e, "livraison des jobs d'outils");
        }
        tokio::select! {
            _ = d.services.jobs.woken() => {}
            _ = tokio::time::sleep(DELIVERY_POLL) => {}
        }
    }
}

#[cfg(test)]
mod tests;
