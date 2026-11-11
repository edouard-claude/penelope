//! Les règles qu'une double confirmation rend mortes (#445).
//!
//! Un appel de classe destructive que la politique fait confirmer deux fois ne passe plus
//! sous une règle `Auto` : celles qu'une carte double avait posées par « Toujours » restent
//! en base sans jamais s'appliquer. `doctor` les nomme, pour que le propriétaire sache
//! pourquoi des cartes sont revenues et retire ce qui ne sert plus.

use crate::services::Services;
use penelope_hitl::{PolicyEngine, PolicyRule};
use penelope_kernel::api::DoctorCheck;
use penelope_kernel::risk::{PolicyDecision, PolicyWindow, RiskClass};

/// La classe d'un outil quand elle ne dépend pas de ses arguments : celle de sa
/// spécification pour un outil natif, celle du registre pour un outil MCP.
async fn static_risk(s: &Services, tool: &str) -> RiskClass {
    if tool.starts_with("mcp__") {
        return match s.mcp_tools.get(tool).await {
            Ok(Some(t)) => t.risk,
            _ => RiskClass::Unknown,
        };
    }
    penelope_tools::effective_risk(tool, &Default::default())
}

/// Les règles `Auto` durables posées sur un outil destructif, quand la politique fait
/// confirmer cette classe deux fois : le moteur les ignore.
pub async fn dead_rules(s: &Services) -> anyhow::Result<Vec<PolicyRule>> {
    let cfg = s.config.config();
    if !PolicyEngine::asks_twice(&cfg.mcp.policy, RiskClass::Destructive) {
        return Ok(vec![]);
    }
    let mut dead = vec![];
    for r in s.policies.active_rules().await? {
        if r.decision != PolicyDecision::Auto || r.window != PolicyWindow::Always {
            continue;
        }
        let Some(tool) = r.tool.as_deref() else {
            continue;
        };
        if static_risk(s, tool).await == RiskClass::Destructive {
            dead.push(r);
        }
    }
    Ok(dead)
}

/// Le contrôle de `doctor` : aucune règle morte, ou leur liste et quoi en faire.
pub async fn doctor_check(s: &Services) -> DoctorCheck {
    const ID: &str = "policy_dead_rules";
    const LABEL: &str = "Règles sous une double confirmation";
    let dead = match dead_rules(s).await {
        Ok(d) => d,
        Err(e) => return DoctorCheck::fail(ID, LABEL, format!("règles illisibles : {e}"), None),
    };
    if dead.is_empty() {
        return DoctorCheck::ok(
            ID,
            LABEL,
            "aucune règle « Toujours » sur un outil à double confirmation",
        );
    }
    let list = dead
        .iter()
        .map(|r| format!("`{}` ({})", r.tool.as_deref().unwrap_or("?"), r.id))
        .collect::<Vec<_>>()
        .join(", ");
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "{} règle(s) « Toujours » ne s'applique(nt) plus, l'outil se confirme deux fois \
             à chaque appel : {list}",
            dead.len()
        ),
        Some(
            "un outil MCP annoncé destructif à tort : corriger sa classe dans la déclaration du \
             serveur (`[tool_risk]`, par exemple `outil = \"write\"`) et la règle vaut à \
             nouveau ; sinon la retirer par `/policies`. `mcp.policy.destructive = \"ask\"` \
             ramène toute la classe à une confirmation et rend leur effet aux règles"
                .into(),
        ),
    )
}

#[cfg(test)]
mod tests;
