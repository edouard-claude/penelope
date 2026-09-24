//! Définitions des outils offerts au modèle et description d'un appel natif.

use super::*;

/// Risque et nom effectif d'un appel d'outil natif. `shell_network` : le réseau est
/// ouvert à toutes les commandes par la configuration.
pub(super) fn native_info(name: &str, args: &Value, shell_network: bool) -> CallInfo {
    match name {
        // Réseau demandé pour une commande : une action externe, approuvée comme telle.
        "shell_exec" if wants_network(name, args) && !shell_network => CallInfo {
            effective_name: name.to_string(),
            risk: RiskClass::External,
            idempotent: false,
            policy: None,
        },
        // Une lecture simple (`ls`, `cat`, `grep`, `git status`…) est une lecture : elle ne
        // demande rien, comme `fs_read` (issue #111).
        "shell_exec"
            if !wants_network(name, args)
                && args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .is_some_and(penelope_tools::shell::is_read_command) =>
        {
            CallInfo {
                effective_name: name.to_string(),
                risk: RiskClass::Read,
                idempotent: true,
                policy: None,
            }
        }
        "config_set" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            CallInfo {
                effective_name: name.to_string(),
                risk: if crate::selfknow::sensitive_path(path) {
                    RiskClass::Destructive
                } else {
                    RiskClass::Write
                },
                idempotent: true,
                policy: None,
            }
        }
        _ => CallInfo {
            effective_name: name.to_string(),
            risk: penelope_tools::effective_risk(name, &Default::default()),
            idempotent: penelope_tools::tool_spec(name)
                .map(|t| t.idempotent)
                .unwrap_or(false),
            policy: None,
        },
    }
}

/// Outils d'un tour de conversation (issue #104) : le noyau, les outils à la demande que
/// la session a découverts, et les méta-outils qui mènent aux autres. Trié : la liste ne
/// change qu'avec ce que la session découvre ou oublie.
pub fn chat_tool_defs(discovered: &[String]) -> Vec<penelope_llm::ToolDef> {
    let mut specs = penelope_tools::core_exposed();
    for d in discovered {
        if let Some(t) = penelope_tools::tool_spec(d)
            && !specs.iter().any(|s| s.name == t.name)
        {
            specs.push(t);
        }
    }
    specs.sort_by(|a, b| a.name.cmp(b.name));
    let mut v: Vec<penelope_llm::ToolDef> = specs
        .into_iter()
        .map(|t| penelope_llm::ToolDef::new(t.name, t.description, t.schema))
        .collect();
    for (name, desc, schema) in penelope_mcp::registry::ToolRegistry::meta_tools() {
        v.push(penelope_llm::ToolDef::new(name, desc, schema));
    }
    v
}

/// Définitions d'outils offertes au modèle pour une session.
pub fn tool_defs(in_workflow: bool, with_mcp: bool) -> Vec<penelope_llm::ToolDef> {
    let mut v: Vec<penelope_llm::ToolDef> = penelope_tools::all_tools()
        .into_iter()
        .filter(|t| in_workflow || !t.workflow_only)
        .map(|t| penelope_llm::ToolDef::new(t.name, t.description, t.schema))
        .collect();
    if with_mcp {
        for (name, desc, schema) in penelope_mcp::registry::ToolRegistry::meta_tools() {
            v.push(penelope_llm::ToolDef::new(name, desc, schema));
        }
    }
    v
}

/// Rendu d'un résultat d'outil MCP : texte des blocs, sinon contenu structuré.
pub fn render_mcp_result(v: &Value) -> String {
    let mut out = String::new();
    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                out.push_str(t);
                out.push('\n');
            } else if let Some(uri) = b.get("uri").and_then(|u| u.as_str()) {
                out.push_str(&format!("[ressource {uri}]\n"));
            }
        }
    }
    if out.trim().is_empty() {
        if let Some(sc) = v.get("structuredContent") {
            return serde_json::to_string_pretty(sc).unwrap_or_default();
        }
        return serde_json::to_string_pretty(v).unwrap_or_default();
    }
    out
}

pub(crate) fn shell_override(raw: &str) -> Option<(String, Vec<String>)> {
    let mut parts = raw.split_whitespace();
    let program = parts.next()?.to_string();
    let mut args: Vec<String> = parts.map(String::from).collect();
    if args.is_empty() {
        args.push("-c".into());
    }
    Some((program, args))
}
