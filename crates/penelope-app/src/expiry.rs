//! La suite d'une approbation échue (#399) : reprendre le tour une fois, ou s'arrêter.

use crate::bus::Origin;
use crate::engine::TurnIntake;
use crate::ports::Messenger;
use crate::services::Services;
use penelope_kernel::event::EventDraft;
use serde_json::json;

/// Ce que devient une approbation échue (#399).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExpiry {
    /// Le tour reprend et apprend que la demande a expiré (§9.2).
    Resume,
    /// Rien : pas d'appel en attente, ou la session est close.
    Nothing,
    /// Le tour a déjà repris une fois sur une expiration et a redemandé : il s'arrête là,
    /// sinon il redemanderait chaque jour indéfiniment.
    Abandon,
}

/// Décide de la suite d'une approbation échue.
pub async fn on_expiry(
    s: &Services,
    a: &penelope_hitl::ApprovalRequest,
) -> anyhow::Result<OnExpiry> {
    let Some(sid) = &a.session_id else {
        return Ok(OnExpiry::Nothing);
    };
    if a.payload.get("call_id").is_none() {
        return Ok(OnExpiry::Nothing);
    }
    match s.sessions.get(sid).await? {
        Some(sess) if sess.state != "closed" => {}
        _ => return Ok(OnExpiry::Nothing),
    }
    Ok(if s.approvals.earlier_expiries(a).await? > 0 {
        OnExpiry::Abandon
    } else {
        OnExpiry::Resume
    })
}

/// Donne sa suite à une approbation échue : reprise du tour, ou arrêt annoncé dans la
/// conversation de la session (`origin`, que le canal déduit de la session).
pub async fn settle(
    s: &Services,
    intake: &dyn TurnIntake,
    messenger: Option<&dyn Messenger>,
    a: &penelope_hitl::ApprovalRequest,
    origin: &Origin,
) -> anyhow::Result<()> {
    let on_expiry = on_expiry(s, a).await?;
    let Some(sid) = a
        .session_id
        .as_deref()
        .filter(|_| on_expiry != OnExpiry::Nothing)
    else {
        return Ok(());
    };
    if on_expiry == OnExpiry::Resume {
        intake.enqueue_resume(sid, a.id.as_str(), origin).await?;
        return Ok(());
    }
    let _ = s
        .events
        .append(
            EventDraft::new(
                "approval.abandoned",
                json!({"approval_id": a.id.as_str(), "tool": a.subject}),
            )
            .session(sid),
        )
        .await;
    if let Some(m) = messenger {
        let text = format!(
            "⏹ Deuxième demande sans réponse pour `{}` : la tâche s'arrête ici. Écris dans \
             ce sujet pour la reprendre.",
            a.subject
        );
        let _ = m.send_text(origin, &text).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
