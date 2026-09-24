//! Textes et rendus : erreurs expliquées, sonde, résultat d'outil en JSON.

use super::*;

/// Configuration sérialisée telle qu'enregistrée : sert à détecter un changement.
pub(super) fn config_json(cfg: &ServerConfig) -> String {
    serde_json::to_string(cfg).unwrap_or_default()
}

pub(super) fn unknown(name: &str) -> String {
    format!("serveur MCP inconnu : `{name}` (voir `penelope mcp list`)")
}

pub(super) fn multi_file(name: &str) -> String {
    format!(
        "`{name}` est déclaré dans un fichier qui en contient plusieurs : modifier ce \
         fichier à la main dans mcp.d"
    )
}

/// Erreurs JSON-RPC qui disent que la requête elle-même est fautive (en-têtes, forme) :
/// `mcp test` échoue sur elles, pas sur un refus de l'outil (issue #126).
pub(super) const PROTOCOL_FAULTS: [i32; 2] = [
    penelope_mcp::protocol::HEADER_MISMATCH,
    penelope_mcp::protocol::INVALID_REQUEST,
];

/// Outil qu'essaie `penelope mcp test` : en lecture d'après ses annotations (et sans
/// surcharge contraire de la déclaration), sans argument requis, jamais refusé par la
/// politique ; les `list`, `get` et `whoami` d'abord.
pub(super) fn probe_tool(
    cfg: &ServerConfig,
    tools: &[penelope_mcp::protocol::ToolDescriptor],
) -> Option<String> {
    let mut candidates: Vec<&penelope_mcp::protocol::ToolDescriptor> = tools
        .iter()
        .filter(|t| {
            t.annotations["readOnlyHint"] == true && t.annotations["destructiveHint"] != true
        })
        .filter(|t| {
            t.input_schema["required"]
                .as_array()
                .is_none_or(|r| r.is_empty())
        })
        .filter(|t| cfg.tool_risk.get(&t.name).is_none_or(|r| r == "read"))
        .filter(|t| cfg.tool_policy.get(&t.name).is_none_or(|p| p != "deny"))
        .collect();
    let rank = |n: &str| {
        let n = n.to_lowercase();
        if ["whoami", "list", "get"].iter().any(|k| n.contains(k)) {
            0
        } else {
            1
        }
    };
    candidates.sort_by_key(|t| (rank(&t.name), t.name.clone()));
    candidates.first().map(|t| t.name.clone())
}

/// Erreur d'un appel d'outil, pour le modèle et pour `mcp test` : un désaccord d'en-têtes
/// (-32020) est un défaut du client, pas des arguments (issue #126).
pub(super) fn call_error(e: &McpError) -> String {
    match e {
        McpError::Rpc { code, .. } if *code == penelope_mcp::protocol::HEADER_MISMATCH => {
            format!(
                "{e}\n[Pénélope : le serveur refuse les en-têtes HTTP de la requête (-32020). \
                 C'est un défaut du client MCP de Pénélope, pas des arguments : ne réessaie \
                 pas et ne reformule pas l'appel, signale-le au propriétaire.]"
            )
        }
        other => other.to_string(),
    }
}

/// Message d'échec de connexion, avec les dernières lignes de stderr et, si le bac à
/// sable semble en cause, la marche à suivre.
pub(super) fn explain(cfg: &ServerConfig, e: &McpError, logs: &[String]) -> String {
    let mut msg = e.to_string();
    // Les lignes ajoutées par le transport (fin du processus, sortie vide) sont déjà dans
    // l'erreur (issue #114).
    let tail: Vec<&String> = logs
        .iter()
        .filter(|l| !l.starts_with("(rien sur la sortie d'erreur") && !l.starts_with("(processus "))
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !tail.is_empty() {
        msg.push_str(" ; stderr : ");
        msg.push_str(
            &tail
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    let blocked = logs.iter().any(|l| {
        let l = l.to_lowercase();
        l.contains("operation not permitted") || l.contains("permission denied")
    });
    if blocked && cfg.sandbox_profile != "full" {
        msg.push_str(&format!(
            " → le bac à sable `{0}` bloque peut-être une écriture : `sandbox_profile = \
             \"full\"` dans sa déclaration, puis `penelope config set \
             sandbox.allow_full_for '[\"{1}\"]'`",
            cfg.sandbox_profile, cfg.name
        ));
    }
    msg
}

/// Résultat d'outil au format MCP, sans les données binaires : une image ou un audio
/// n'entre pas dans le transcript en base64.
pub fn result_json(r: &ToolResult) -> Value {
    let content: Vec<Value> = r
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({"type": "text", "text": text}),
            ContentBlock::Image { data, mime_type } => json!({
                "type": "text",
                "text": format!("[image {mime_type}, {} octets en base64, non transmise]", data.len()),
            }),
            ContentBlock::Audio { data, mime_type } => json!({
                "type": "text",
                "text": format!("[audio {mime_type}, {} octets en base64, non transmis]", data.len()),
            }),
            ContentBlock::ResourceLink {
                uri,
                name,
                description,
            } => json!({"type": "resource_link", "uri": uri, "name": name, "description": description}),
            ContentBlock::Resource {
                uri,
                text,
                mime_type,
                ..
            } => match text {
                Some(t) => json!({"type": "resource", "uri": uri, "text": t, "mimeType": mime_type}),
                None => json!({"type": "resource", "uri": uri, "mimeType": mime_type}),
            },
            ContentBlock::Other(v) => v.clone(),
        })
        .collect();
    json!({
        "content": content,
        "structuredContent": r.structured,
        "isError": r.is_error,
    })
}
