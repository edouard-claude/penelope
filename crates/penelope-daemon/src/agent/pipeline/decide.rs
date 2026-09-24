//! Gardes d'un appel d'outil, avant la politique : liste blanche, décision antérieure,
//! garde de boucle, vérification des arguments.
//!
//! Une chaîne fixe, dans cet ordre, et monotone : une garde peut refuser, suspendre ou
//! arrêter le tour, jamais autoriser ce qu'une autre refuse. La seule sortie qui n'est
//! pas un refus est la décision déjà prise par le propriétaire : elle est définitive.

use super::*;

/// Un appel normalisé (issue #123) et ce qu'il faut savoir pour le décider.
pub(crate) struct DescribedCall {
    pub call: ToolCall,
    pub info: CallInfo,
}

/// Un appel refusé sans être exécuté : son texte revient au modèle comme résultat.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Refusal {
    /// Hors de la liste blanche de l'étape ou de la skill.
    Allowlist { tool: String },
    /// Refusé par le propriétaire, avec sa raison éventuelle.
    Denied { reason: Option<String> },
    /// La demande a expiré sans réponse.
    Expired,
    /// La garde de boucle avertit : l'appel n'est pas exécuté.
    LoopWarn(String),
    /// Arguments invalides (issue #117), déjà formulés pour le modèle.
    Invalid(String),
}

impl Refusal {
    /// Le texte renvoyé au modèle, tel que les tests et le modèle le lisent.
    pub(crate) fn text(&self) -> String {
        match self {
            Refusal::Allowlist { tool } => format!("Refusé : `{tool}` n'est pas autorisé ici."),
            Refusal::Denied { reason } => not_run(
                &reason
                    .as_ref()
                    .map(|r| format!("le propriétaire a refusé : {r}"))
                    .unwrap_or_else(|| "le propriétaire a refusé".to_string()),
            ),
            Refusal::Expired => not_run("la demande a expiré sans réponse"),
            Refusal::LoopWarn(m) => format!("[avertissement du harnais] {m}"),
            Refusal::Invalid(text) => text.clone(),
        }
    }
}

fn not_run(why: &str) -> String {
    format!("Non exécuté : {why}. Propose une autre approche ou demande.")
}

/// Le tour s'arrête sur une demande d'approbation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Suspension {
    /// Une demande déjà posée pour cet appel attend sa décision.
    Prior { approval_id: String },
}

/// Ce qui arrête la chaîne de gardes pour un appel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GuardStop {
    Refuse(Refusal),
    Suspend(Suspension),
    /// La garde de boucle arrête le tour : le message du détecteur.
    LoopAbort(String),
    /// Le propriétaire a déjà approuvé cet appel : il s'exécute sans redemander, ni
    /// garde de boucle, ni politique.
    Approved,
}

/// Ce que voit une garde d'appel.
pub(crate) struct CallContext<'a> {
    pub agent: &'a AgentLoop,
    pub spec: &'a TurnSpec,
    pub execute: &'a (dyn ToolExecutor + Send + Sync),
    pub detector: &'a mut LoopDetector,
}

#[async_trait::async_trait]
pub(crate) trait CallGuard: Send + Sync {
    /// Nom stable, pour le test d'ordre.
    fn name(&self) -> &'static str;
    async fn check(
        &self,
        call: &DescribedCall,
        cx: &mut CallContext<'_>,
    ) -> anyhow::Result<Option<GuardStop>>;
}

/// La chaîne fixe, dans l'ordre.
pub(crate) fn call_chain() -> [&'static dyn CallGuard; 4] {
    [&Allowlist, &PriorDecision, &LoopGuard, &Precheck]
}

/// Passe la chaîne : la première garde qui s'arrête décide.
pub(crate) async fn run_call_guards(
    chain: &[&dyn CallGuard],
    call: &DescribedCall,
    cx: &mut CallContext<'_>,
) -> anyhow::Result<Option<GuardStop>> {
    for guard in chain {
        if let Some(stop) = guard.check(call, cx).await? {
            tracing::debug!(guard = guard.name(), tool = %call.call.name, "appel arrêté par une garde");
            return Ok(Some(stop));
        }
    }
    Ok(None)
}

/// Liste blanche de l'étape ou de la skill.
pub(crate) struct Allowlist;

#[async_trait::async_trait]
impl CallGuard for Allowlist {
    fn name(&self) -> &'static str {
        "allowlist"
    }

    async fn check(
        &self,
        d: &DescribedCall,
        cx: &mut CallContext<'_>,
    ) -> anyhow::Result<Option<GuardStop>> {
        Ok(
            (!penelope_tools::is_allowed(&d.call.name, &cx.spec.allowed_tools)).then(|| {
                GuardStop::Refuse(Refusal::Allowlist {
                    tool: d.call.name.clone(),
                })
            }),
        )
    }
}

/// Une décision a-t-elle déjà été prise pour cet appel précis ?
pub(crate) struct PriorDecision;

#[async_trait::async_trait]
impl CallGuard for PriorDecision {
    fn name(&self) -> &'static str {
        "prior_decision"
    }

    async fn check(
        &self,
        d: &DescribedCall,
        cx: &mut CallContext<'_>,
    ) -> anyhow::Result<Option<GuardStop>> {
        let prior = cx
            .agent
            .services
            .approvals
            .find_for_call(&cx.spec.session_id, &d.call.id)
            .await?;
        let Some(a) = prior else {
            return Ok(None);
        };
        Ok(Some(match a.state {
            ApprovalState::Pending => GuardStop::Suspend(Suspension::Prior {
                approval_id: a.id.0.clone(),
            }),
            ApprovalState::Approved => {
                // Approuvé : on exécute, sans redemander.
                let _ = cx.detector.observe(&d.call.name, &d.call.arguments);
                GuardStop::Approved
            }
            ApprovalState::Expired => GuardStop::Refuse(Refusal::Expired),
            _ => GuardStop::Refuse(Refusal::Denied { reason: a.reason }),
        }))
    }
}

/// Détecteur de boucles, sans l'intention : la reformuler ne change pas l'appel (#116).
pub(crate) struct LoopGuard;

#[async_trait::async_trait]
impl CallGuard for LoopGuard {
    fn name(&self) -> &'static str {
        "loop_guard"
    }

    async fn check(
        &self,
        d: &DescribedCall,
        cx: &mut CallContext<'_>,
    ) -> anyhow::Result<Option<GuardStop>> {
        Ok(
            match cx
                .detector
                .observe(&d.call.name, &without_intention(&d.call.arguments))
            {
                LoopVerdict::Ok => None,
                LoopVerdict::Warn(m) => Some(GuardStop::Refuse(Refusal::LoopWarn(m))),
                LoopVerdict::Abort(m) => Some(GuardStop::LoopAbort(m)),
            },
        )
    }
}

/// Arguments vérifiés avant toute carte : un appel invalide revient au modèle avec les
/// paramètres attendus, et compte pour la garde de boucle (issue #117).
pub(crate) struct Precheck;

#[async_trait::async_trait]
impl CallGuard for Precheck {
    fn name(&self) -> &'static str {
        "precheck"
    }

    async fn check(
        &self,
        d: &DescribedCall,
        cx: &mut CallContext<'_>,
    ) -> anyhow::Result<Option<GuardStop>> {
        let Err(e) = cx.execute.precheck(&d.call.name, &d.call.arguments).await else {
            return Ok(None);
        };
        let mut text = e.for_model();
        match cx.detector.observe_invalid(&d.info.effective_name) {
            LoopVerdict::Ok => {}
            LoopVerdict::Warn(m) => {
                text.push_str(&format!("\n\n[avertissement du harnais] {m}"));
            }
            LoopVerdict::Abort(m) => return Ok(Some(GuardStop::LoopAbort(m))),
        }
        Ok(Some(GuardStop::Refuse(Refusal::Invalid(text))))
    }
}
