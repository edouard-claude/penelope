//! Ce que l'archive ne transporte pas mais dont la machine neuve aura besoin (#329) : les
//! commandes des serveurs MCP et les serveurs d'inférence en LaunchAgent. Le manifeste les
//! enregistre ; la restauration compare avec la machine et ne dit que ce qui manque.

use serde_json::{Value, json};
use std::path::Path;

/// Les commandes des serveurs MCP `stdio` actifs : `{server, command}`.
pub fn mcp_commands(mcp_d: &Path) -> Vec<Value> {
    let (servers, _) = penelope_mcp::config::load_dir(mcp_d);
    servers
        .into_iter()
        .filter(|c| c.enabled && !c.command.trim().is_empty())
        .map(|c| json!({"server": c.name, "command": c.command}))
        .collect()
}

/// Les serveurs d'inférence installés en LaunchAgent (`com.penelope.inference.*`), avec
/// leurs arguments : la restauration les réinstalle tels quels.
pub fn services(home: Option<&Path>) -> Vec<Value> {
    let Some(dir) = home.map(|h| h.join("Library/LaunchAgents")) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let label = name.strip_suffix(".plist")?;
            label.starts_with("com.penelope.inference.").then_some(())?;
            let plist = std::fs::read_to_string(e.path()).ok()?;
            let args = penelope_platform::service::launchd_args(&plist);
            (!args.is_empty()).then(|| json!({"label": label, "args": args}))
        })
        .collect();
    out.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));
    out
}

/// Les commandes MCP du manifeste introuvables sur cette machine.
pub fn missing_commands(manifest: &Value, which: impl Fn(&str) -> bool) -> Vec<Value> {
    manifest["mcp_commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["command"].as_str().is_some_and(|cmd| !which(cmd)))
        .cloned()
        .collect()
}
