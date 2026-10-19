//! `penelope plugin list|show` (#393) : ce qu'apportent les plugins déposés dans
//! `{data}/plugins`, lu sur disque, sans daemon.

use super::*;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    /// Les plugins : skills, sous-agents, serveurs MCP autorisés et refusés.
    List,
    /// Le détail d'un plugin.
    Show { name: String },
}

pub(super) fn run(cli: &Cli, cmd: &PluginCmd) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let policy = std::fs::read_to_string(dirs.config_file())
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(c, _)| c.plugins)
        .unwrap_or_default();
    let report = penelope_app::plugin_report::report(&dirs.data(), &policy);
    let picked: Vec<serde_json::Value> = match cmd {
        PluginCmd::List => report,
        PluginCmd::Show { name } => {
            let one: Vec<_> = report.into_iter().filter(|p| p["name"] == *name).collect();
            if one.is_empty() {
                return Err(CliError::Usage(format!(
                    "aucun plugin `{name}` dans {}",
                    dirs.data().join("plugins").display()
                )));
            }
            one
        }
    };
    if cli.json || matches!(cmd, PluginCmd::Show { .. }) {
        println!(
            "{}",
            serde_json::to_string_pretty(&picked).unwrap_or_default()
        );
        return Ok(());
    }
    if picked.is_empty() {
        println!(
            "Aucun plugin dans {}.",
            dirs.data().join("plugins").display()
        );
    }
    let count = |v: &serde_json::Value| v.as_array().map(Vec::len).unwrap_or(0);
    for p in &picked {
        println!(
            "{:<24} {} skill(s), {} agent(s), {} serveur(s) MCP, {} hook(s), {} refusé(s)",
            p["name"].as_str().unwrap_or_default(),
            count(&p["skills"]),
            count(&p["agents"]),
            count(&p["mcp_servers"]),
            count(&p["hooks"]),
            count(&p["mcp_refused"]) + count(&p["hooks_refused"]),
        );
    }
    Ok(())
}
