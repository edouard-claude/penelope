//! Gardes d'un appel d'outil, avant la politique : liste blanche, décision antérieure,
//! garde de boucle, vérification des arguments.
//!
//! Une chaîne fixe, dans cet ordre, et monotone : une garde peut refuser, suspendre ou
//! arrêter le tour, jamais autoriser ce qu'une autre refuse. La seule sortie qui n'est
//! pas un refus est la décision déjà prise par le propriétaire : elle est définitive.

use super::*;

/// Identifiant d'un appel d'outil, tel que le modèle (ou le programme parent) l'a émis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallId(pub String);

/// Place d'un appel dans l'arbre des appels (couture PTC, décision 0016) : `root` est
/// l'appel émis par le modèle, `parent` celui qui a émis cet appel quand il est imbriqué
/// (sous-appel d'un programme `run_code`). Aujourd'hui, tout appel est une racine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallContext {
    pub call_id: CallId,
    pub parent: Option<CallId>,
    pub root: CallId,
}

impl CallContext {
    /// Un appel émis par le modèle.
    pub fn root(call_id: &str) -> Self {
        CallContext {
            call_id: CallId(call_id.to_string()),
            parent: None,
            root: CallId(call_id.to_string()),
        }
    }

    /// Un appel émis par celui-ci : même racine, parent = cet appel.
    pub fn child(&self, call_id: &str) -> Self {
        CallContext {
            call_id: CallId(call_id.to_string()),
            parent: Some(self.call_id.clone()),
            root: self.root.clone(),
        }
    }

    pub fn is_nested(&self) -> bool {
        self.parent.is_some()
    }
}

/// Un appel normalisé (issue #123) et ce qu'il faut savoir pour le décider.
pub(crate) struct DescribedCall {
    pub call: ToolCall,
    pub info: CallInfo,
    pub context: CallContext,
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
    /// Refusé par la politique (règle, déclaration du serveur), avec sa raison.
    Policy { reason: String },
    /// Appel imbriqué qui demanderait une approbation : un programme en vol ne se suspend
    /// pas, la demande est un refus (décision 0016).
    NestedApproval { reason: String },
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
            Refusal::Policy { reason } => format!("Refusé par la politique : {reason}"),
            Refusal::NestedApproval { reason } => not_run(&format!(
                "un appel imbriqué ne peut pas demander d'approbation ({reason})"
            )),
        }
    }
}

/// Un appel imbriqué dont la politique demande une approbation est refusé, sans carte :
/// seul l'appel racine peut suspendre le tour (décision 0016).
pub(crate) fn nested_ask(
    context: &CallContext,
    decision: PolicyDecision,
    reason: &str,
) -> Option<Refusal> {
    (context.is_nested() && matches!(decision, PolicyDecision::Ask | PolicyDecision::AskTwice))
        .then(|| Refusal::NestedApproval {
            reason: reason.to_string(),
        })
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
pub(crate) struct GuardContext<'a> {
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
        cx: &mut GuardContext<'_>,
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
    cx: &mut GuardContext<'_>,
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
        cx: &mut GuardContext<'_>,
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
        cx: &mut GuardContext<'_>,
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
        cx: &mut GuardContext<'_>,
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
        cx: &mut GuardContext<'_>,
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
