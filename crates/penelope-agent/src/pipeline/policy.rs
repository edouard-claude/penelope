//! Politique d'un appel d'outil : les couches, dans l'ordre, chacune nommée.
//!
//! ```text
//!  1. règles du propriétaire (motif > outil > serveur > classe), sinon défaut de classe
//!  2. déclaration du serveur MCP : un refus prime, le reste cède à une règle
//!  3. autorisation déclarée d'avance (brouillon de plan, `tools.shell_allow[_network]`)
//!  4. mode de la session : `ask` redemande, `auto` laisse passer sauf le destructif
//!  5. réseau demandé : la raison le dit (la couche ne change pas)
//!  6. plancher : `config_set` sensible (ou sur `approval.*`) demande deux fois, malgré
//!     toute règle
//! ```
//!
//! Le juge (#203, `judge.rs`) vient après, sur le verdict final.
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
    /// Le juge d'approbation, sous contrôle déterministe (#203).
    Judge,
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
        s: &AgentServices,
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
            && let Some(why) = local_draft_allow(&info.effective_name)
                .or_else(|| declared_allow(&cfg, &info.effective_name, effective_args))
        {
            verdict.decision = PolicyDecision::Auto;
            verdict.layer = VerdictLayer::DeclaredAllow;
            verdict.reason = why;
        }
        match s.modes.of_session(&spec.session_id).await {
            ApprovalMode::Ask
                if verdict.decision == PolicyDecision::Auto
                    && (info.effective_name == "shell_exec" || info.risk != RiskClass::Read) =>
            {
                verdict.decision = PolicyDecision::Ask;
                verdict.layer = VerdictLayer::SessionMode;
                verdict.reason = "mode « demander tout » de la session".into();
            }
            ApprovalMode::Auto
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
        if crate::wants_network(&info.effective_name, effective_args)
            && info.risk == RiskClass::External
        {
            verdict.reason = format!(
                "accès réseau demandé pour cette commande ({})",
                verdict.reason
            );
        }
        // Une règle « toujours » posée pour `config_set` vaut pour les réglages
        // ordinaires, jamais pour le bac à sable, les providers ou Telegram.
        // Le juge d'approbation (`approval.*`, #203) en est un : le modèle ne choisit pas
        // qui le juge.
        let judge_setting = effective_args
            .get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p == "approval" || p.starts_with("approval."));
        if info.effective_name == "config_set"
            && (info.risk == RiskClass::Destructive || judge_setting)
            && verdict.decision != PolicyDecision::Deny
        {
            verdict.decision = PolicyDecision::AskTwice;
            verdict.layer = VerdictLayer::SensitiveConfig;
            verdict.reason = "réglage sensible : double confirmation à chaque fois".into();
        }
        Ok(verdict)
    }
}

/// Mode d'approbation d'une session (issue #111) : ce qui part sans demande.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    Ask,
    Reads,
    Auto,
}

impl ApprovalMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "ask" | "demander" | "tout" => ApprovalMode::Ask,
            "reads" | "lectures" | "defaut" | "défaut" => ApprovalMode::Reads,
            "auto" => ApprovalMode::Auto,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalMode::Ask => "ask",
            ApprovalMode::Reads => "reads",
            ApprovalMode::Auto => "auto",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ApprovalMode::Ask => "demander tout",
            ApprovalMode::Reads => "lectures sans demande",
            ApprovalMode::Auto => "tout sauf le destructif",
        }
    }
}

/// Un brouillon de plan ne lance rien et reste révisable : sa persistance locale
/// est autorisée d'avance. Le gate « vas-y » garde l'approbation du propriétaire.
pub fn local_draft_allow(tool: &str) -> Option<String> {
    (tool == "workflow_plan").then(|| "brouillon local sans exécution".into())
}

/// Autorisation déclarée d'avance dans la configuration (issue #111) : une commande
/// `shell_exec` d'une famille de `tools.shell_allow`, ou de `tools.shell_allow_network`
/// quand elle demande le réseau. Renvoie la raison, pour la trace.
pub fn declared_allow(
    cfg: &penelope_kernel::config::Config,
    tool: &str,
    args: &serde_json::Value,
) -> Option<String> {
    if tool != "shell_exec" {
        return None;
    }
    let command = args.get("command").and_then(|v| v.as_str())?.trim();
    let (families, key) = if crate::wants_network(tool, args) {
        (&cfg.tools.shell_allow_network, "tools.shell_allow_network")
    } else {
        (&cfg.tools.shell_allow, "tools.shell_allow")
    };
    // Une liste `a && b` est autorisée d'avance quand **chaque** étape l'est, comme pour
    // les règles (issue #150) : une famille déclarée ne couvre pas ses voisines.
    let list = penelope_hitl::cmdline::list(command)?;
    let mut matched: Vec<String> = Vec::new();
    for step in &list.steps {
        // Une seule commande : la famille déclarée doit la couvrir, lecture comprise —
        // en mode « demander tout », `tools.shell_allow` vaut aussi pour `ls` (#111).
        if list.steps.len() > 1 && penelope_hitl::cmdline::needs_no_rule(step) {
            continue;
        }
        let f = families
            .iter()
            .find(|f| penelope_hitl::policy::family_covers(f.trim(), step))?;
        if !matched.contains(f) {
            matched.push(f.clone());
        }
    }
    if matched.is_empty() {
        return None;
    }
    Some(format!(
        "autorisé d'avance : famille(s) « {} » de `{key}`",
        matched.join(" », « ")
    ))
}
