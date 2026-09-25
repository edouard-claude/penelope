//! Publication d'un résumé et état persisté (résumé en attente, cooldown), sortie de
//! `compaction.rs`.

use super::*;

/// Publie un résumé validé : nœud LCM, canonique marqué, événement.
pub(super) async fn publish(
    d: &Context,
    session_id: &str,
    pending: &Pending,
    trigger: Trigger,
    report: &mut Report,
) -> anyhow::Result<()> {
    let s = &d.services;
    let job = &pending.job;
    let node_id = s
        .context
        .apply_summary_as(job, &pending.summary, &pending.model, trigger.as_str())
        .await?;
    let tokens_summary = s
        .context
        .lcm
        .get(&node_id)
        .await?
        .map(|n| n.tokens_self)
        .unwrap_or(0);

    // Frontière sûre pour le cache : le préfixe change de toute façon, les instantanés
    // mémoire T2 se rafraîchissent au tour suivant (§6.6).
    crate::episodes::refresh_snapshot(s, session_id).await;

    report.published += 1;
    report.messages += job.messages();
    report.node = Some(node_id.clone());
    report.covered_to = job.to_seq;
    report.tokens_src += job.tokens_src;
    report.tokens_summary = tokens_summary;
    report.model = Some(pending.model.clone());

    // Observation en shadow du contexte réellement rendu : ni le résumé accepté
    // ni le lot source ne sont retouchés. Les métriques ne portent aucun texte.
    let fidelity = observe_fidelity(job, &pending.summary);
    if fidelity.actions_missing > 0 || fidelity.identifiers_missing > 0 {
        tracing::warn!(
            session = %session_id,
            actions_missing = fidelity.actions_missing,
            identifiers_missing = fidelity.identifiers_missing,
            "indices explicites absents du contexte compacté"
        );
    }
    for (kind, count) in [
        ("action", fidelity.actions_missing),
        ("identifier", fidelity.identifiers_missing),
    ] {
        if count > 0 {
            penelope_observe::metrics::counter_inc(
                "penelope_compaction_missing_evidence_total",
                &[("kind", kind)],
                count as f64,
            );
        }
    }

    s.events
        .append(
            EventDraft::new(
                "context.compacted",
                json!({
                    "node": node_id,
                    "from_seq": job.from_seq,
                    "to_seq": job.to_seq,
                    "chunk_from_seq": job.chunk_from_seq,
                    "messages": job.messages(),
                    "tokens_src": job.tokens_src,
                    "tokens_summary": tokens_summary,
                    "updated": job.previous_node_id,
                    "model": pending.model,
                    "trigger": trigger.as_str(),
                    "evidence": fidelity,
                }),
            )
            .session(session_id),
        )
        .await?;
    penelope_observe::metrics::counter_inc(
        "penelope_compactions_total",
        &[("trigger", trigger.as_str()), ("outcome", "published")],
        1.0,
    );
    Ok(())
}

/// Publie un résumé mis de côté. Il est retiré d'abord : un résumé périmé ne revient
/// pas en boucle.
pub(super) async fn publish_saved(
    d: &Context,
    session_id: &str,
    pending: Pending,
    trigger: Trigger,
    report: &mut Report,
) {
    if let Err(e) = d.services.kv_delete(&pending_key(session_id)).await {
        tracing::warn!(session = %session_id, error = %e, "résumé en attente non retiré");
        return;
    }
    if let Err(e) = publish(d, session_id, &pending, trigger, report).await {
        tracing::warn!(session = %session_id, error = %e, "résumé en attente abandonné");
    }
}

pub(super) async fn load_pending(
    s: &Services,
    session_id: &str,
) -> anyhow::Result<Option<Pending>> {
    let Some(raw) = s.kv_get(&pending_key(session_id)).await? else {
        return Ok(None);
    };
    match serde_json::from_str(&raw) {
        Ok(p) => Ok(Some(p)),
        Err(e) => {
            tracing::warn!(session = %session_id, error = %e, "résumé en attente illisible, écarté");
            s.kv_delete(&pending_key(session_id)).await?;
            Ok(None)
        }
    }
}

pub(super) async fn save_pending(
    s: &Services,
    session_id: &str,
    pending: &Pending,
) -> anyhow::Result<()> {
    s.kv_set(&pending_key(session_id), &serde_json::to_string(pending)?)
        .await
}

pub(super) async fn load_cooldown(s: &Services, session_id: &str) -> Cooldown {
    s.kv_get(&cooldown_key(session_id))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub(super) async fn save_cooldown(s: &Services, session_id: &str, cooldown: &Cooldown) {
    let raw = serde_json::to_string(cooldown).unwrap_or_default();
    if let Err(e) = s.kv_set(&cooldown_key(session_id), &raw).await {
        tracing::warn!(session = %session_id, error = %e, "cooldown de compaction non enregistré");
    }
}
