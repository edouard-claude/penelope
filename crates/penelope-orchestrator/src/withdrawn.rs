//! Une demande d'accord dont le demandeur a disparu (#464).
//!
//! Une carte attend le propriétaire au nom de quelqu'un : le sous-agent d'un job
//! d'arrière-plan (#422), un run, le tour d'une session. Quand celui-là n'existe plus, la
//! décision n'irait nulle part. Avant, la demande restait `pending` jusqu'à son échéance :
//! le propriétaire tapait la carte, lisait « approuvé », et rien ne se passait ; et tant
//! qu'elle attendait, elle retenait le réveil du parent (`jobs::wake_gate`), qui
//! n'apprenait la mort de son sous-agent qu'un jour plus tard.
//!
//! ```text
//!  demande en attente ── son demandeur ?
//!     ├─ un job (clé `approval.child.<id>`) : terminé ou introuvable ─┐
//!     ├─ un run : terminé (fini, en échec, annulé) ───────────────────┼─▶ `expired`, avec
//!     └─ une session : close ─────────────────────────────────────────┘   la raison ; la
//!                                                 clé du job est retirée, le canal désarme la carte
//! ```
//!
//! Un job seulement lent à reprendre après un démarrage est encore `working` ou
//! `input_required` : il n'est pas touché. Un run ou une session introuvable non plus :
//! seule une fin constatée retire une demande.

use penelope_app::ports::Messenger;
use penelope_app::services::Services;
use penelope_executor::jobs;
use penelope_hitl::{ApprovalKind, ApprovalRequest, ApprovalState};
use penelope_kernel::event::EventDraft;
use serde_json::json;

/// Pourquoi `a` n'a plus de demandeur. `None` : il attend encore sa décision.
pub async fn gone(s: &Services, a: &ApprovalRequest) -> anyhow::Result<Option<String>> {
    // Le sous-agent d'un job : seul l'état du job compte, pas celui du run ou de la
    // session qui l'ont lancé, il leur survit.
    if let Some(job) = s.kv_get(&jobs::child_approval_key(a.id.as_str())).await? {
        return Ok(match jobs::store(s).get(&job).await? {
            Some(j) if !j.state.is_terminal() => None,
            Some(j) => Some(format!(
                "le sous-agent qui la posait s'est arrêté (job {}, {})",
                j.id,
                j.state.as_str()
            )),
            None => Some("le sous-agent qui la posait n'existe plus".into()),
        });
    }
    // Les autres demandeurs n'attendent qu'un appel d'outil ou un gate. Un effet
    // incertain, une proposition de mémoire ou de skill se tranchent sans eux.
    if !matches!(a.kind, ApprovalKind::ToolCall | ApprovalKind::WorkflowGate) {
        return Ok(None);
    }
    if let Some(run) = a.run_id.as_deref()
        && let Some(run) = s.runs.get(run).await?
        && run.state.is_terminal()
    {
        return Ok(Some(format!(
            "le run qui la posait est terminé ({}, {})",
            run.id,
            run.state.as_str()
        )));
    }
    if let Some(sid) = a.session_id.as_deref()
        && let Some(session) = s.sessions.get(sid).await?
        && session.state == "closed"
    {
        return Ok(Some("la session qui la posait est close".into()));
    }
    Ok(None)
}

/// Retire `a` : `expired` avec sa raison, la clé de son job oubliée, la carte désarmée par
/// le canal. Faux : elle n'était plus en attente.
pub async fn withdraw(
    s: &Services,
    messenger: Option<&dyn Messenger>,
    a: &ApprovalRequest,
    why: &str,
) -> anyhow::Result<bool> {
    let id = a.id.as_str();
    if !s.approvals.withdraw(id, why).await? {
        return Ok(false);
    }
    s.kv_delete(&jobs::child_approval_key(id)).await?;
    tracing::info!(approval = id, tool = %a.subject, why, "demande d'accord retirée");
    let mut event = EventDraft::new(
        "approval.withdrawn",
        json!({"approval_id": id, "tool": a.subject, "why": why}),
    );
    if let Some(sid) = &a.session_id {
        event = event.session(sid);
    }
    let _ = s.events.append(event).await;
    if let Some(m) = messenger
        && let Err(e) = m.approval_withdrawn(id, why).await
    {
        tracing::warn!(approval = id, error = %e, "carte d'une demande retirée non désarmée");
    }
    Ok(true)
}

/// Passe sur les demandes en attente : celles dont le demandeur a disparu sont retirées.
/// Rend leurs identifiants.
pub async fn sweep(s: &Services, messenger: Option<&dyn Messenger>) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for a in s.approvals.pending(500).await? {
        if let Some(why) = gone(s, &a).await?
            && withdraw(s, messenger, &a, &why).await?
        {
            out.push(a.id.to_string());
        }
    }
    Ok(out)
}

/// Au moment de trancher `a` : la raison pour laquelle il n'y a plus rien à trancher,
/// la demande étant retirée ici si son demandeur vient de disparaître. `None` : elle
/// attend bien une décision, ou elle en a déjà reçu une.
pub async fn nothing_to_decide(
    s: &Services,
    messenger: Option<&dyn Messenger>,
    a: &ApprovalRequest,
) -> anyhow::Result<Option<String>> {
    match a.state {
        ApprovalState::Pending => {}
        ApprovalState::Expired => {
            let why = a.reason.clone().filter(|r| !r.is_empty());
            return Ok(Some(
                why.unwrap_or_else(|| "elle a expiré sans réponse".into()),
            ));
        }
        ApprovalState::Cancelled => {
            let why = a.reason.clone().filter(|r| !r.is_empty());
            return Ok(Some(why.unwrap_or_else(|| "elle a été annulée".into())));
        }
        _ => return Ok(None),
    }
    let Some(why) = gone(s, a).await? else {
        return Ok(None);
    };
    withdraw(s, messenger, a, &why).await?;
    Ok(Some(why))
}
