//! Un sous-agent en arrière-plan devant une carte (#422) : au lieu d'échouer, il pose la
//! carte au propriétaire, en disant qu'elle vient de lui et de quelle tâche, passe son job
//! en `input_required`, et attend la décision. Accordé, il reprend où il était ; refusé
//! ou expiré, il reçoit le refus comme résultat d'outil et continue ou conclut.
//!
//! Aucun tour du parent n'est lancé pendant l'attente : la garde de #396 (pas de réveil
//! avec une carte en attente) tient, et `job_status` montre l'état. La conversation du
//! sous-agent vit dans le daemon : un redémarrage pendant l'attente fait échouer le job
//! en le disant (`LOST_WAITING_ON_BOOT`). Arrêté ou perdu, sa demande est retirée
//! (`crate::withdrawn`, #464) : sa carte ne reste pas cliquable pour rien.

use super::*;
use penelope_executor::jobs;

/// Battement de la lecture de la décision.
const POLL: Duration = Duration::from_millis(250);

pub(super) struct Waiting<'a> {
    pub job: &'a str,
    pub run_id: Option<&'a str>,
    pub kind: &'a str,
    pub task: &'a str,
    pub approval_id: &'a str,
}

async fn send_card_once(d: &Context, run_id: Option<&str>, approval_id: &str) {
    let s = &d.services;
    let key = format!("wf.approval_sent.{approval_id}");
    if s.kv_get(&key).await.ok().flatten().is_some() {
        return;
    }
    let Some(m) = d.workflows.ports.messenger.get() else {
        return;
    };
    let origin = match run_id {
        Some(r) => origin_of(s, r).await,
        None => penelope_app::helpers::owner_origin_of(s),
    };
    if m.send_approval(&origin, approval_id).await.is_ok() {
        let _ = s.kv_set(&key, "1").await;
    }
}

/// Attend la décision du propriétaire. `Err` : le sous-agent a été arrêté pendant
/// l'attente.
pub(super) async fn await_owner(
    d: &Context,
    w: Waiting<'_>,
    cancel: &CancelToken,
) -> Result<(), String> {
    let s = &d.services;
    let store = jobs::store(s);
    let _ = s
        .kv_set(&jobs::child_approval_key(w.approval_id), w.job)
        .await;
    let _ = s.approvals.note_child(w.approval_id, w.kind, w.task).await;
    let _ = store.set_waiting(w.job, true).await;
    send_card_once(d, w.run_id, w.approval_id).await;
    tracing::info!(
        job = w.job,
        approval = w.approval_id,
        "sous-agent en attente d'accord"
    );
    loop {
        let asked = s.approvals.get(w.approval_id).await.ok().flatten();
        let pending = asked
            .as_ref()
            .is_some_and(|a| a.state == penelope_hitl::ApprovalState::Pending);
        if cancel.is_cancelled() {
            // Plus personne n'attend cette décision : la carte est retirée tout de suite,
            // elle ne reste pas cliquable pour rien (#464).
            if let Some(a) = asked.filter(|_| pending) {
                let messenger = d.workflows.ports.messenger.get();
                let why = "le sous-agent qui la posait a été arrêté";
                let _ = crate::withdrawn::withdraw(s, messenger.as_deref(), &a, why).await;
            }
            return Err("sous-agent interrompu pendant l'attente de ton accord".into());
        }
        if !pending {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    let _ = store.set_waiting(w.job, false).await;
    Ok(())
}
