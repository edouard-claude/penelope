//! Hooks `pre_tool` d'un appel (#384), et ce qu'une réécriture d'arguments engage (#446).
//!
//! Un hook qui réécrit les arguments ne change que l'appel en mémoire : le transcript
//! garde ce que le modèle a émis. Deux conséquences, tenues ici :
//!
//! - l'accord du propriétaire vaut pour les arguments que sa carte a montrés
//!   ([`args_digest`], enregistré dans la demande) ; à la reprise, l'appel relu dans le
//!   transcript repasse ses hooks, et l'accord n'est retrouvé que si la réécriture
//!   redonne exactement ces arguments ([`AgentLoop::approved_as`]) ;
//! - le résultat dit au modèle ce qui est vraiment parti ([`rewrite_note`]).
//!
//! Avant, la carte montrait `git push --force-with-lease` et, l'accord donné, c'est
//! `git push --force` qui partait : la décision antérieure était retrouvée par le seul
//! identifiant de l'appel, avant les hooks.

use super::*;
use penelope_app::hook_gate::PreToolGate;
use policy::{HookPull, VerdictLayer, stricter, with_hook};

/// L'issue des hooks `pre_tool` d'un appel (#384).
pub(super) enum Gated {
    /// L'appel continue, avec sa décision resserrée ; `rewritten_by` : le hook qui en a
    /// modifié les arguments.
    Go {
        call: ToolCall,
        args: Value,
        verdict: policy::Verdict,
        rewritten_by: Option<String>,
    },
    /// L'appel s'arrête là ; le texte va au modèle.
    Refused(ToolCall, String),
}

/// Empreinte des arguments qu'une carte montre, prise avant leur masquage : deux appels
/// qui feraient la même chose ont la même, quel que soit l'ordre de leurs clés.
pub(crate) fn args_digest(args: &Value) -> String {
    penelope_kernel::canonical::canonical_hash(args)
}

/// Vrai si l'accord `approval` porte sur ces arguments. Une demande d'avant la 1.0.100
/// n'a pas d'empreinte : elle vaut comme avant, une fois.
pub(crate) fn covers(approval: &penelope_hitl::ApprovalRequest, args: &Value) -> bool {
    approval
        .payload
        .get("args_digest")
        .and_then(Value::as_str)
        .is_none_or(|digest| digest == args_digest(args))
}

/// Ce que le modèle lit en tête du résultat d'un appel réécrit : le transcript garde
/// l'appel qu'il a émis, cette ligne dit ce qui est parti à sa place.
pub(super) fn rewrite_note(hook: &str, args: &Value) -> String {
    format!(
        "[hook `{hook}`] Arguments modifiés avant l'exécution. Exécuté : {}",
        penelope_observe::redact_json(args)
    )
}

impl AgentLoop {
    /// Vrai si le propriétaire a déjà approuvé cet appel **sous ces arguments** : la
    /// reprise d'un appel qu'un hook réécrit, une fois ses hooks repassés (#446).
    pub(super) async fn approved_as(
        &self,
        spec: &TurnSpec,
        context: &CallContext,
        args: &Value,
    ) -> anyhow::Result<bool> {
        let prior = self
            .services
            .approvals
            .find_for_call(&spec.session_id, &context.call_id.0)
            .await?;
        Ok(prior.is_some_and(|a| {
            a.state == ApprovalState::Approved
                && a.payload.get("args_digest").is_some()
                && covers(&a, args)
        }))
    }

    /// Les hooks `pre_tool` d'un appel (#384) : refus, carte forcée (en tour autonome, la
    /// mission passe bloquée), ou arguments modifiés qui repassent toute la politique, le
    /// plus strict l'emportant toujours.
    pub(super) async fn pre_tool_hooks(
        &self,
        spec: &TurnSpec,
        workspace: Option<&std::path::Path>,
        info: &CallInfo,
        call: ToolCall,
        effective_args: Value,
        verdict: policy::Verdict,
    ) -> anyhow::Result<Gated> {
        let s = &self.services;
        let gate = s
            .hooks
            .pre_tool(
                &spec.session_id,
                spec.run_id.as_deref(),
                &info.effective_name,
                &effective_args,
            )
            .await;
        match gate {
            PreToolGate::Continue => {
                let decision = with_hook(verdict.decision, HookPull::Continue);
                Ok(Gated::Go {
                    call,
                    args: effective_args,
                    verdict: policy::Verdict {
                        decision,
                        ..verdict
                    },
                    rewritten_by: None,
                })
            }
            PreToolGate::Deny { hook, reason } => {
                debug_assert_eq!(
                    with_hook(verdict.decision, HookPull::Deny),
                    PolicyDecision::Deny
                );
                tracing::info!(session = %spec.session_id, hook = %hook, "appel refusé par un hook");
                Ok(Gated::Refused(
                    call,
                    Refusal::Policy {
                        reason: format!("refusé par le hook `{hook}` : {reason}"),
                    }
                    .text(),
                ))
            }
            PreToolGate::Ask { hook, reason } => {
                let unattended = s
                    .modes
                    .of_turn(&spec.session_id, spec.run_id.as_deref())
                    .await
                    == ApprovalMode::Unattended;
                if unattended {
                    // Personne pour répondre à la carte : la mission le dit et s'arrête.
                    let why = format!("le hook `{hook}` demande l'avis du propriétaire : {reason}");
                    s.hooks
                        .blocked(&spec.session_id, spec.run_id.as_deref(), &why)
                        .await;
                    return Ok(Gated::Refused(
                        call,
                        Refusal::Policy {
                            reason: format!("{why} ; rien ne part sans lui"),
                        }
                        .text(),
                    ));
                }
                let decision = with_hook(verdict.decision, HookPull::Ask);
                Ok(Gated::Go {
                    call,
                    args: effective_args,
                    verdict: policy::Verdict {
                        decision,
                        layer: VerdictLayer::Hook { name: hook.clone() },
                        reason: format!("hook `{hook}` : {reason}"),
                    },
                    rewritten_by: None,
                })
            }
            PreToolGate::Args { hook, args } => {
                // Le même outil seulement : par `tool_call`, l'outil visé ne change pas.
                let renamed = call.name == "tool_call"
                    && args
                        .get("name")
                        .is_some_and(|n| Some(n) != call.arguments.get("name"));
                if renamed {
                    return Ok(Gated::Refused(
                        call,
                        Refusal::Policy {
                            reason: format!(
                                "le hook `{hook}` change l'outil appelé : erreur de hook, appel refusé"
                            ),
                        }
                        .text(),
                    ));
                }
                let mut modified = call.clone();
                if call.name == "tool_call" {
                    modified.arguments["args"] = args.clone();
                } else {
                    modified.arguments = args.clone();
                }
                let new_args = crate::effective_arguments(&modified.name, &modified.arguments);
                let again = PolicyStage::evaluate(s, spec, workspace, info, &new_args).await?;
                tracing::info!(session = %spec.session_id, hook = %hook, before = %effective_args, after = %new_args, "arguments modifiés par un hook");
                let decision = stricter(verdict.decision, again.decision);
                Ok(Gated::Go {
                    call: modified,
                    args: new_args,
                    verdict: policy::Verdict {
                        decision,
                        layer: VerdictLayer::Hook { name: hook.clone() },
                        reason: format!(
                            "hook `{hook}` : arguments modifiés (avant : {}) ; {}",
                            penelope_observe::redact_json(&effective_args),
                            again.reason
                        ),
                    },
                    rewritten_by: Some(hook),
                })
            }
        }
    }
}
