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

/// Verdict d'une garde de tour : laisser passer, suspendre sur une approbation, arrêter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GuardVerdict {
    Proceed,
    /// Le tour attend la décision `approval_id` et reprendra au même point.
    Suspend {
        approval_id: String,
    },
    Stop(TurnOutcome),
}

impl GuardVerdict {
    /// L'issue du tour si la garde l'interrompt.
    pub(crate) fn outcome(self) -> Option<TurnOutcome> {
        match self {
            GuardVerdict::Proceed => None,
            GuardVerdict::Suspend { approval_id } => {
                Some(TurnOutcome::AwaitingApproval { approval_id })
            }
            GuardVerdict::Stop(outcome) => Some(outcome),
        }
    }
}

/// Ce que voit une garde, avant chaque appel au modèle.
pub(crate) struct TurnContext<'a> {
    pub agent: &'a AgentLoop,
    pub spec: &'a TurnSpec,
    pub sink: &'a dyn TurnSink,
    /// Itération en cours, à partir de 0.
    pub iteration: u32,
    /// Coût des appels de cette exécution du tour (hors reprises).
    pub cost: f64,
}

/// Une garde de tour. Monotone : elle peut suspendre ou arrêter le tour, jamais ajouter
/// un message ni autoriser ce qu'une autre refuse.
#[async_trait::async_trait]
pub(crate) trait TurnGuard: Send + Sync {
    /// Nom stable, pour les journaux et le test d'ordre.
    fn name(&self) -> &'static str;
    async fn check(&self, cx: &TurnContext<'_>) -> anyhow::Result<GuardVerdict>;
}

/// La chaîne fixe, dans l'ordre : l'arrêt demandé d'abord, puis les plafonds de budget
/// (issue #32), le plafond d'appels du tour et le palier de coût (issue #19).
pub(crate) fn default_chain() -> [&'static dyn TurnGuard; 4] {
    [
        &CancelGuard,
        &BudgetGuard,
        &CallCapGuard,
        &CostCheckpointGuard,
    ]
}

/// Passe la chaîne : la première garde qui ne laisse pas passer décide.
pub(crate) async fn run_guards(
    chain: &[&dyn TurnGuard],
    cx: &TurnContext<'_>,
) -> anyhow::Result<Option<TurnOutcome>> {
    for guard in chain {
        if let Some(outcome) = guard.check(cx).await?.outcome() {
            tracing::debug!(guard = guard.name(), session = %cx.spec.session_id, "tour interrompu par une garde");
            return Ok(Some(outcome));
        }
    }
    Ok(None)
}

/// `/stop`, bouton d'arrêt ou annulation du run.
pub(crate) struct CancelGuard;

#[async_trait::async_trait]
impl TurnGuard for CancelGuard {
    fn name(&self) -> &'static str {
        "cancel"
    }

    async fn check(&self, cx: &TurnContext<'_>) -> anyhow::Result<GuardVerdict> {
        Ok(if cx.spec.cancel.is_cancelled() {
            GuardVerdict::Stop(TurnOutcome::Cancelled)
        } else {
            GuardVerdict::Proceed
        })
    }
}

/// Plafonds du jour, de la session et du run : vérifiés **avant** chaque appel, pas après
/// coup.
pub(crate) struct BudgetGuard;

#[async_trait::async_trait]
impl TurnGuard for BudgetGuard {
    fn name(&self) -> &'static str {
        "budget"
    }

    async fn check(&self, cx: &TurnContext<'_>) -> anyhow::Result<GuardVerdict> {
        let s = &cx.agent.services;
        let spec = cx.spec;
        let cfg = s.config.config();
        let statuses = s
            .budget
            .status(&cfg.budget, Some(&spec.session_id), spec.run_id.as_deref())
            .await?;
        let Some(exceeded) = statuses.iter().find(|b| b.exceeded) else {
            return Ok(GuardVerdict::Proceed);
        };
        let stop = TurnOutcome::BudgetExceeded {
            scope: exceeded.scope.as_str().to_string(),
            spent_usd: exceeded.spent_usd,
            limit_usd: exceeded.limit_usd,
        };
        // Une carte « continuer ? » par plafond atteint (issue #32) : une demande
        // déjà tranchée sans relèvement arrête le tour, une demande en attente est
        // réutilisée plutôt que dupliquée.
        let call_id = format!(
            "budget:{}:{}",
            exceeded.scope.as_str(),
            (exceeded.limit_usd * 100.0).round() as i64
        );
        let approval_id = match s
            .approvals
            .find_for_call(&spec.session_id, &call_id)
            .await?
        {
            Some(a) if a.state == ApprovalState::Pending => a.id.0.clone(),
            Some(_) => return Ok(GuardVerdict::Stop(stop)),
            None => {
                s.approvals
                    .create(
                        ApprovalKind::BudgetExceeded,
                        exceeded.scope.as_str(),
                        RiskClass::Unknown,
                        json!({
                            "budget": true,
                            "scope": exceeded.scope.as_str(),
                            "spent": exceeded.spent_usd,
                            "limit": exceeded.limit_usd,
                            "call_id": call_id,
                            "turn_id": spec.turn_id,
                            "run_id": spec.run_id,
                        }),
                        vec!["+5 $".into(), "+20 $".into(), "Arrêter".into()],
                        Some(&spec.session_id),
                        spec.run_id.as_deref(),
                        false,
                    )
                    .await?
                    .id
                    .0
            }
        };
        // Session ouverte par le propriétaire : le tour se suspend et reprend là où
        // il s'est arrêté si le plafond est relevé. Le jour et les runs s'arrêtent.
        let owner_session = exceeded.scope == penelope_kernel::budget::BudgetScope::Session
            && spec.run_id.is_none()
            && s.sessions
                .get(&spec.session_id)
                .await?
                .is_some_and(|x| x.kind == penelope_kernel::session::SessionKind::Chat);
        if owner_session {
            return Ok(GuardVerdict::Suspend { approval_id });
        }
        Ok(GuardVerdict::Stop(stop))
    }
}

/// Plafond d'appels au modèle, compté sur tout le tour (reprises après approbation
/// comprises).
pub(crate) struct CallCapGuard;

#[async_trait::async_trait]
impl TurnGuard for CallCapGuard {
    fn name(&self) -> &'static str {
        "call_cap"
    }

    async fn check(&self, cx: &TurnContext<'_>) -> anyhow::Result<GuardVerdict> {
        let Some(turn_id) = &cx.spec.turn_id else {
            return Ok(GuardVerdict::Proceed);
        };
        let max = cx.agent.max_iterations;
        let (calls, _) = cx.agent.services.budget.turn_totals(turn_id).await?;
        if calls >= max as i64 {
            return Ok(GuardVerdict::Stop(TurnOutcome::Failed {
                error: format!(
                    "{CALLS_EXHAUSTED} en {max} appels au modèle (reprises après \
                     approbation comprises)"
                ),
            }));
        }
        Ok(GuardVerdict::Proceed)
    }
}

/// Point de contrôle de coût d'un tour déjà long (issue #19) : une carte « je continue ? »
/// par palier franchi, reprises comprises.
pub(crate) struct CostCheckpointGuard;

#[async_trait::async_trait]
impl TurnGuard for CostCheckpointGuard {
    fn name(&self) -> &'static str {
        "cost_checkpoint"
    }

    async fn check(&self, cx: &TurnContext<'_>) -> anyhow::Result<GuardVerdict> {
        let s = &cx.agent.services;
        let spec = cx.spec;
        let cfg = s.config.config();
        let Some(turn_id) = &spec.turn_id else {
            return Ok(GuardVerdict::Proceed);
        };
        let step = cfg.budget.turn_checkpoint_usd;
        // Un run de workflow a son propre plafond (`budget.run_usd`).
        if step <= 0.0 || spec.run_id.is_some() {
            return Ok(GuardVerdict::Proceed);
        }
        let (calls, turn_cost) = s.budget.turn_totals(turn_id).await?;
        if turn_cost < step {
            return Ok(GuardVerdict::Proceed);
        }
        let level = (turn_cost / step).floor() as i64;
        let call_id = format!("checkpoint:{turn_id}:{level}");
        let usd = crate::budget_alert::usd;
        let prior = s
            .approvals
            .find_for_call(&spec.session_id, &call_id)
            .await?;
        match prior.map(|a| (a.state, a.id.0)) {
            Some((ApprovalState::Approved, _)) => Ok(GuardVerdict::Proceed),
            Some((ApprovalState::Pending, id)) => Ok(GuardVerdict::Suspend { approval_id: id }),
            Some(_) => Ok(GuardVerdict::Stop(TurnOutcome::Answered {
                text: format!(
                    "⏹ Tour arrêté à ta demande après {} ({calls} appels au modèle).",
                    usd(turn_cost)
                ),
                iterations: cx.iteration,
                cost_usd: cx.cost,
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
                cx.sink.emit(TurnEvent::Approval {
                    id: approval.id.0.clone(),
                    tool: "tour".into(),
                    risk: RiskClass::Unknown,
                    arguments,
                    reason,
                    double: false,
                });
                Ok(GuardVerdict::Suspend {
                    approval_id: approval.id.0,
                })
            }
        }
    }
}

impl AgentLoop {
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
