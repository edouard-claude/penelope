//! Décisions du propriétaire : approbations et effets incertains.

use super::*;

/// Choix d'une demande `effect_unknown` (#83) : l'appel a eu lieu (vérifié par le
/// propriétaire), il faut le relancer, ou il reste tel quel.
pub const EFFECT_DONE: &str = "C'est fait";
pub const EFFECT_RETRY: &str = "Relancer";
pub const EFFECT_IGNORE: &str = "Ignorer";

/// Tranche un effet incertain : la décision passe au ledger, **jamais** dans une règle.
/// La transition est faite avant que le canal ne remette le tour en file : la reprise
/// trouve l'effet `completed` (rejoué), `planned` (relancé) ou la demande refusée.
async fn decide_uncertain_effect(
    s: &AgentServices,
    a: &penelope_hitl::ApprovalRequest,
    decision: &Decision,
) -> anyhow::Result<bool> {
    use penelope_kernel::effects::UnknownDecision;
    use penelope_kernel::ids::EffectId;
    let ledger = match (decision.approved, decision.choice.as_str()) {
        (true, EFFECT_DONE) => UnknownDecision::MarkCompleted(json!({
            "statut": "fait",
            "note": "Le propriétaire a vérifié : cet appel avait bien eu lieu avant l'arrêt \
                     du daemon. Il n'est pas relancé.",
        })),
        (true, EFFECT_RETRY) => UnknownDecision::Retry,
        (false, _) => UnknownDecision::Ignore,
        (true, other) => anyhow::bail!(
            "effet incertain : choisir « {EFFECT_DONE} », « {EFFECT_RETRY} » ou « \
             {EFFECT_IGNORE} » (reçu « {other} ») ; en ligne de commande, `penelope approve \
             <id> --effect done|retry` ou `penelope deny <id>`"
        ),
    };
    let effect_id = a.payload["effect_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("demande {} sans effet", a.id.0))?
        .to_string();
    // Pas de règle, quelle que soit la fenêtre demandée.
    let recorded = Decision {
        window: PolicyWindow::Once,
        reason: decision.reason.clone().or_else(|| {
            (!decision.approved)
                .then(|| "effet incertain laissé tel quel, sans relance".to_string())
        }),
        ..decision.clone()
    };
    match s.approvals.decide(a.id.as_str(), &recorded).await {
        Ok(_) => {
            s.effects
                .resolve_unknown(&EffectId(effect_id.clone()), ledger)
                .await?;
            s.events
                .append(TurnEventKind::ApprovalDecided.draft(json!({
                    "id": a.id.as_str(),
                    "approved": decision.approved,
                    "via": decision.via,
                    "window": "once",
                    "effect": effect_id,
                    "choice": decision.choice,
                })))
                .await?;
            Ok(decision.approved)
        }
        Err(penelope_hitl::HitlError::AlreadyDecided { .. }) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Tranche une approbation : la première décision gagne, une fenêtre crée une règle.
pub async fn decide_approval(
    s: &AgentServices,
    approval_id: &str,
    decision: &Decision,
) -> anyhow::Result<bool> {
    if let Some(a) = s.approvals.get(approval_id).await?
        && a.kind == penelope_hitl::ApprovalKind::EffectUnknown
    {
        return decide_uncertain_effect(s, &a, decision).await;
    }
    match s.approvals.decide(approval_id, decision).await {
        Ok(a) => {
            // Une règle ne naît que d'un appel d'outil dont on a vu les arguments : une
            // demande sans arguments (budget, effet incertain…) n'ouvre jamais une
            // autorisation sur l'outil entier (#83, régression de #67).
            let rule_allowed = a.payload.get("arguments").is_some()
                && !matches!(
                    a.kind,
                    penelope_hitl::ApprovalKind::BudgetExceeded
                        | penelope_hitl::ApprovalKind::EffectUnknown
                );
            // « Toujours », « pour ce run », « pour cette session » : règle visible et
            // révocable dans `/policies`.
            let window_ref = match decision.window {
                PolicyWindow::Run => a.run_id.clone().or_else(|| a.session_id.clone()),
                PolicyWindow::Session => a.session_id.clone(),
                _ => None,
            };
            if decision.approved && decision.window != PolicyWindow::Once && rule_allowed {
                let window = if decision.window == PolicyWindow::Run && a.run_id.is_none() {
                    PolicyWindow::Session
                } else {
                    decision.window
                };
                // Un « toujours » est borné au contexte de l'appel (famille de commandes,
                // répertoire, remote), pas à l'outil entier (issue #67). Une liste
                // `a && b` en demande une par famille (issue #150).
                let patterns = arg_patterns(&a.subject, a.payload.get("arguments"));
                // Une commande sans famille (composée) : autorisée cette fois, jamais le
                // shell entier (issue #111).
                if patterns.is_empty() && matches!(a.subject.as_str(), "shell_exec" | "git_clone") {
                    tracing::info!(
                        approval = approval_id,
                        "appel sans motif sûr : autorisé une fois, sans règle"
                    );
                    s.approvals.note_rules(approval_id, 0).await?;
                } else {
                    // Sans motif (outils MCP), la règle porte sur l'outil : une seule.
                    let patterns: Vec<Option<Value>> = if patterns.is_empty() {
                        vec![None]
                    } else {
                        patterns.into_iter().map(Some).collect()
                    };
                    let created = patterns.len();
                    for pattern in patterns {
                        s.policies
                            .create_rule(
                                penelope_hitl::RuleScope::Tool,
                                Some(&a.subject),
                                server_of(&a.subject).as_deref(),
                                pattern,
                                PolicyDecision::Auto,
                                window,
                                window_ref.as_deref(),
                            )
                            .await?;
                    }
                    s.approvals.note_rules(approval_id, created).await?;
                }
            } else if !decision.approved && decision.window.creates_rule() && rule_allowed {
                let pattern = if a.subject == "git_clone" {
                    arg_pattern(&a.subject, a.payload.get("arguments"))
                } else {
                    None
                };
                if a.subject != "git_clone" || pattern.is_some() {
                    s.policies
                        .create_rule(
                            penelope_hitl::RuleScope::Tool,
                            Some(&a.subject),
                            server_of(&a.subject).as_deref(),
                            pattern,
                            PolicyDecision::Deny,
                            decision.window,
                            None,
                        )
                        .await?;
                }
            }
            s.events
                .append(TurnEventKind::ApprovalDecided.draft(json!({
                    "id": approval_id,
                    "approved": decision.approved,
                    "via": decision.via,
                    "window": decision.window.as_str(),
                })))
                .await?;
            Ok(decision.approved)
        }
        // Déjà tranché par l'autre canal : la première décision gagne.
        Err(penelope_hitl::HitlError::AlreadyDecided { .. }) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
