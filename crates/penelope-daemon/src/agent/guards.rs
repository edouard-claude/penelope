//! Bornes d'un tour : plafonds de budget, plafond d'appels, palier de coût.

use super::*;

/// Message d'un plafond atteint : la clé à relever est celle du périmètre atteint, et
/// ce qui ne débloque rien est dit (issue #4).
pub fn budget_exceeded_text(scope: &str, spent_usd: f64, limit_usd: f64) -> String {
    let (label, key) = match scope {
        "session" => ("de la session", "budget.session_usd"),
        "run" => ("du run", "budget.run_usd"),
        _ => ("du jour", "budget.daily_usd"),
    };
    let raised = (limit_usd * 2.0).max(spent_usd + 1.0).ceil();
    let mut out = format!(
        "💸 Budget {label} atteint : {spent_usd:.2} $ dépensés pour un plafond de \
         {limit_usd:.2} $. Tour suspendu.\n\n"
    );
    match scope {
        "session" => out.push_str(&format!(
            "`/new` repart de zéro dans une nouvelle session. Pour continuer celle-ci, relever \
             le plafond : `penelope config set {key} {raised}`."
        )),
        "run" => out.push_str(&format!(
            "Pour laisser le run continuer, relever le plafond : `penelope config set {key} \
             {raised}`."
        )),
        _ => out.push_str(&format!(
            "La dépense du jour repart de zéro à minuit. D'ici là, relever le plafond : \
             `penelope config set {key} {raised}`."
        )),
    }
    out.push_str(
        "\n`/compact` allège le contexte des prochains tours mais ne rembourse pas ce qui est \
         déjà dépensé.",
    );
    out
}

impl AgentLoop {
    /// Plafond d'appels et point de contrôle de coût d'un tour, comptés sur tout le tour
    /// (reprises après approbation comprises) : `Some` arrête ou suspend le tour.
    pub(super) async fn turn_limits(
        &self,
        spec: &TurnSpec,
        sink: &dyn TurnSink,
        iteration: u32,
        cost: f64,
    ) -> anyhow::Result<Option<TurnOutcome>> {
        let s = &self.services;
        let cfg = s.config.config();
        let Some(turn_id) = &spec.turn_id else {
            return Ok(None);
        };
        let (calls, turn_cost) = s.budget.turn_totals(turn_id).await?;
        if calls >= self.max_iterations as i64 {
            return Ok(Some(TurnOutcome::Failed {
                error: format!(
                    "{CALLS_EXHAUSTED} en {} appels au modèle (reprises après \
                     approbation comprises)",
                    self.max_iterations
                ),
            }));
        }
        let step = cfg.budget.turn_checkpoint_usd;
        // Un run de workflow a son propre plafond (`budget.run_usd`).
        if step <= 0.0 || spec.run_id.is_some() || turn_cost < step {
            return Ok(None);
        }
        let level = (turn_cost / step).floor() as i64;
        let call_id = format!("checkpoint:{turn_id}:{level}");
        let usd = crate::budget_alert::usd;
        let prior = s
            .approvals
            .find_for_call(&spec.session_id, &call_id)
            .await?;
        match prior.map(|a| (a.state, a.id.0)) {
            Some((ApprovalState::Approved, _)) => Ok(None),
            Some((ApprovalState::Pending, id)) => {
                Ok(Some(TurnOutcome::AwaitingApproval { approval_id: id }))
            }
            Some(_) => Ok(Some(TurnOutcome::Answered {
                text: format!(
                    "⏹ Tour arrêté à ta demande après {} ({calls} appels au modèle).",
                    usd(turn_cost)
                ),
                iterations: iteration,
                cost_usd: cost,
            })),
            None => {
                let reason = format!(
                    "Ce tour a coûté {} ({calls} appels au modèle), je continue ?",
                    usd(turn_cost)
                );
                let arguments = json!({"cost_usd": turn_cost, "calls": calls});
                let approval = s
                    .approvals
                    .create(
                        ApprovalKind::BudgetExceeded,
                        "tour",
                        RiskClass::Unknown,
                        json!({
                            "checkpoint": true,
                            "call_id": call_id,
                            "turn_id": turn_id,
                            "arguments": arguments,
                            "reason": reason,
                        }),
                        vec!["Continuer".into(), "Arrêter".into()],
                        Some(&spec.session_id),
                        None,
                        false,
                    )
                    .await?;
                sink.emit(TurnEvent::Approval {
                    id: approval.id.0.clone(),
                    tool: "tour".into(),
                    risk: RiskClass::Unknown,
                    arguments,
                    reason,
                    double: false,
                });
                Ok(Some(TurnOutcome::AwaitingApproval {
                    approval_id: approval.id.0,
                }))
            }
        }
    }

    /// Tous les `delegate_after_calls` appels au modèle d'un tour, le résultat d'outil
    /// suivant rappelle de regrouper les commandes ou de déléguer (issue #19).
    pub(super) async fn delegation_nudge(&self, spec: &TurnSpec) -> anyhow::Result<Option<String>> {
        let s = &self.services;
        let every = s.config.config().budget.delegate_after_calls as i64;
        let Some(turn_id) = &spec.turn_id else {
            return Ok(None);
        };
        if every == 0 {
            return Ok(None);
        }
        let (calls, turn_cost) = s.budget.turn_totals(turn_id).await?;
        if calls == 0 || calls % every != 0 {
            return Ok(None);
        }
        Ok(Some(format!(
            "\n\n[Harnais : {calls} appels au modèle dans ce tour ({}), chacun renvoie tout le \
             contexte. Regroupe les commandes restantes dans un seul `shell_exec`, ou confie la \
             suite à `sub_agent_spawn`, qui ne rend que sa conclusion. Mets aussi à jour tes \
             notes de travail (`session_notes` : plan, décisions, prochaine étape).]",
            crate::budget_alert::usd(turn_cost)
        )))
    }
}
