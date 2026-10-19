//! `penelope plugin list|show|add|remove` (#393) : ce qu'apportent les plugins déposés
//! dans `{data}/plugins`, et leur dépôt, sur disque, sans daemon.

use super::*;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    /// Les plugins : skills, sous-agents, serveurs MCP autorisés et refusés.
    List,
    /// Le détail d'un plugin.
    Show { name: String },
    /// Dépose un plugin : un dossier, ou un dépôt git (cloné). Rien n'exécute avant la
    /// politique du propriétaire.
    Add {
        source: String,
        /// Nom du plugin ; par défaut, le dernier segment de la source.
        #[arg(long)]
        name: Option<String>,
    },
    /// Retire un plugin.
    Remove { name: String },
    /// Installe `<plugin>@<marketplace>` depuis une marketplace inscrite.
    Install { spec: String },
    /// Marketplaces au format Claude Code (`.claude-plugin/marketplace.json`).
    #[command(subcommand)]
    Marketplace(MarketCmd),
}

#[derive(Subcommand, Debug)]
pub enum MarketCmd {
    /// Inscrit une marketplace : un dossier, ou un dépôt git.
    Add { source: String },
    /// Les marketplaces inscrites et leurs plugins.
    List,
}

pub(super) fn run(cli: &Cli, cmd: &PluginCmd) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let policy = std::fs::read_to_string(dirs.config_file())
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(c, _)| c.plugins)
        .unwrap_or_default();
    match cmd {
        PluginCmd::Add { source, name } => {
            let dest = penelope_app::plugin_install::add(&dirs.data(), source, name.as_deref())
                .map_err(CliError::Usage)?;
            println!(
                "Plugin déposé dans {}. Ses skills et sous-agents sont lus aussitôt ; ses \
                 serveurs MCP et ses hooks attendent `plugins.allowed_mcp_servers` et \
                 `plugins.allowed_hooks` (`penelope plugin show` dit lesquels).",
                dest.display()
            );
            return Ok(());
        }
        PluginCmd::Remove { name } => {
            penelope_app::plugin_install::remove(&dirs.data(), name).map_err(CliError::Usage)?;
            penelope_app::plugin_market::forget(&dirs.data(), name).map_err(CliError::Usage)?;
            println!("Plugin `{name}` retiré.");
            return Ok(());
        }
        PluginCmd::Install { spec } => {
            let dest = penelope_app::plugin_market::install(&dirs.data(), spec)
                .map_err(CliError::Usage)?;
            println!(
                "Plugin `{spec}` installé dans {}. Serveurs MCP et hooks attendent la \
                 politique (`penelope plugin show`).",
                dest.display()
            );
            return Ok(());
        }
        PluginCmd::Marketplace(MarketCmd::Add { source }) => {
            let name = penelope_app::plugin_market::add_marketplace(&dirs.data(), source)
                .map_err(CliError::Usage)?;
            println!("Marketplace `{name}` inscrite : `penelope plugin marketplace list`.");
            return Ok(());
        }
        PluginCmd::Marketplace(MarketCmd::List) => {
            let all = penelope_app::plugin_market::list(&dirs.data());
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
                return Ok(());
            }
            for m in all.as_array().into_iter().flatten() {
                let name = m["name"].as_str().unwrap_or_default();
                println!("{name}");
                for p in m["plugins"].as_array().into_iter().flatten() {
                    println!(
                        "  {}@{name}  {}",
                        p["name"].as_str().unwrap_or_default(),
                        p["description"].as_str().unwrap_or_default()
                    );
                }
            }
            return Ok(());
        }
        _ => {}
    }
    let report = penelope_app::plugin_report::report(&dirs.data(), &policy);
    let picked: Vec<serde_json::Value> = match cmd {
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
        _ => report,
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
