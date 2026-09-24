//! Politique d'un appel d'outil : les couches, dans l'ordre, chacune nommée.
//!
//! ```text
//!  1. règles du propriétaire (motif > outil > serveur > classe), sinon défaut de classe
//!  2. déclaration du serveur MCP : un refus prime, le reste cède à une règle
//!  3. autorisation déclarée d'avance (brouillon de plan, `tools.shell_allow[_network]`)
//!  4. mode de la session : `ask` redemande, `auto` laisse passer sauf le destructif
//!  5. réseau demandé : la raison le dit (la couche ne change pas)
//!  6. plancher : `config_set` sensible demande deux fois, malgré toute règle
//! ```
//!
//! `Verdict.reason` est la chaîne que la carte affiche et que les tests lisent : elle
//! reste celle d'avant les couches typées, à l'octet.

use super::*;

/// La couche qui a fixé la décision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VerdictLayer {
    /// Une règle du propriétaire.
    Rule { id: String },
    /// La politique par défaut de la classe de risque.
    Default,
    /// La déclaration du serveur MCP (`tool_policy`).
    ServerDeclaration,
    /// Une autorisation déclarée d'avance.
    DeclaredAllow,
    /// Le mode d'approbation de la session.
    SessionMode,
    /// Plancher : réglage sensible, double confirmation.
    SensitiveConfig,
}

/// Décision de la politique sur un appel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Verdict {
    pub decision: PolicyDecision,
    pub layer: VerdictLayer,
    pub reason: String,
}

/// L'étape de politique.
pub(crate) struct PolicyStage;

impl PolicyStage {
    /// Évalue un appel sur ses arguments **effectifs** : par `tool_call`, ceux de l'appel
    /// interne (#110), sinon une règle « Toujours » couvrirait l'outil entier.
    pub(crate) async fn evaluate(
        s: &Services,
        spec: &TurnSpec,
        workspace: Option<&std::path::Path>,
        info: &CallInfo,
        effective_args: &Value,
    ) -> anyhow::Result<Verdict> {
        let cfg = s.config.config();
        let base = s
            .policies
            .evaluate_in(
                &cfg.mcp.policy,
                &info.effective_name,
                server_of(&info.effective_name).as_deref(),
                effective_args,
                info.risk,
                spec.run_id.as_deref(),
                Some(&spec.session_id),
                workspace,
            )
            .await?;
        let rule_id = base.rule_id;
        let mut verdict = Verdict {
            decision: base.decision,
            layer: match &rule_id {
                Some(id) => VerdictLayer::Rule { id: id.clone() },
                None => VerdictLayer::Default,
            },
            reason: base.reason,
        };
        // La déclaration du serveur MCP peut imposer sa politique à un outil :
        // un refus l'emporte toujours, le reste cède à une règle du propriétaire.
        if let Some(forced) = info.policy
            && (forced == PolicyDecision::Deny || rule_id.is_none())
        {
            verdict.decision = forced;
            verdict.layer = VerdictLayer::ServerDeclaration;
            verdict.reason = format!(
                "politique de la déclaration du serveur : `{}`",
                forced.as_str()
            );
        }
        // Autorisation déclarée d'avance, puis mode de la session (#111).
        if rule_id.is_none()
            && verdict.decision != PolicyDecision::Deny
            && let Some(why) = crate::approval_mode::local_draft_allow(&info.effective_name)
                .or_else(|| {
                    crate::approval_mode::declared_allow(&cfg, &info.effective_name, effective_args)
                })
        {
            verdict.decision = PolicyDecision::Auto;
            verdict.layer = VerdictLayer::DeclaredAllow;
            verdict.reason = why;
        }
        match crate::approval_mode::of_session(s, &spec.session_id).await {
            crate::approval_mode::ApprovalMode::Ask
                if verdict.decision == PolicyDecision::Auto
                    && (info.effective_name == "shell_exec" || info.risk != RiskClass::Read) =>
            {
                verdict.decision = PolicyDecision::Ask;
                verdict.layer = VerdictLayer::SessionMode;
                verdict.reason = "mode « demander tout » de la session".into();
            }
            crate::approval_mode::ApprovalMode::Auto
                if verdict.decision == PolicyDecision::Ask
                    && rule_id.is_none()
                    && info.policy.is_none()
                    && info.risk != RiskClass::Destructive
                    && !(info.effective_name == "shell_exec"
                        && effective_args
                            .get("command")
                            .and_then(|c| c.as_str())
                            .is_none_or(penelope_tools::shell::may_destroy)) =>
            {
                verdict.decision = PolicyDecision::Auto;
                verdict.layer = VerdictLayer::SessionMode;
                verdict.reason = "mode « tout sauf le destructif » de la session".into();
            }
            _ => {}
        }
        // Réseau demandé par une commande : la carte le dit en toutes lettres.
        if crate::executor::wants_network(&info.effective_name, effective_args)
            && info.risk == RiskClass::External
        {
            verdict.reason = format!(
                "accès réseau demandé pour cette commande ({})",
                verdict.reason
            );
        }
        // Une règle « toujours » posée pour `config_set` vaut pour les réglages
        // ordinaires, jamais pour le bac à sable, les providers ou Telegram.
        if info.effective_name == "config_set"
            && info.risk == RiskClass::Destructive
            && verdict.decision != PolicyDecision::Deny
        {
            verdict.decision = PolicyDecision::AskTwice;
            verdict.layer = VerdictLayer::SensitiveConfig;
            verdict.reason = "réglage sensible : double confirmation à chaque fois".into();
        }
        Ok(verdict)
    }
}
