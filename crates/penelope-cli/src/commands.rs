//! Surface de commandes (§15).
//!
//! Deux familles : celles qui parlent au daemon par RPC, et celles qui fonctionnent
//! **hors daemon** (`wf validate`, `config validate`, `paths`) pour l'édition en SSH et
//! la CI.

use crate::client::{CliError, CliResult, call, socket_path};
use crate::output;
use approval_stats::ApprovalsCmd;
#[cfg(test)]
use clap::Parser;
pub use cli::{
    AuditCmd, Cli, Command, ConfigCmd, HistoryCmd, ImportCmd, McpCmd, MemCmd, ModelCmd,
    ScheduleCmd, SecretCmd, SessionCmd, SkillCmd, StoreCmd, VaultCmd, WfCmd,
};
use interactive::{chat, model_auth, onboard};
#[cfg(test)]
use logs::filter_log_lines;
use logs::logs;
use offline::{doctor, eval_local, paths, service, set_secret, validate_config, validate_workflow};
use penelope_kernel::api::method as m;
use render::*;
use restore::{restore_all, restore_offline};
#[cfg(test)]
use route::parse_scalar;
pub use route::route;
use serde_json::{Value, json};
use std::path::PathBuf;
use upgrade::upgrade;

/// Exécute la commande.
pub async fn run(cli: Cli) -> CliResult<()> {
    crate::client::set_timeout(cli.timeout);
    // Les commandes hors daemon d'abord : elles doivent marcher sans socket.
    match &cli.command {
        Command::Paths => return paths(&cli),
        Command::Approvals {
            cmd: Some(ApprovalsCmd::Stats { days }),
        } => return approval_stats::run(&cli, *days),
        Command::Doctor => return doctor(&cli).await,
        Command::Config(ConfigCmd::Validate { file }) => {
            return validate_config(&cli, file.clone());
        }
        Command::Wf(WfCmd::Validate { file }) => return validate_workflow(&cli, file.clone()),
        Command::Eval { suite } => return eval_local(suite).await,
        Command::Restore { file } => return restore_offline(&cli, file).await,
        Command::RestoreAll { source, dry_run } => {
            return restore_all(&cli, source.clone(), *dry_run).await;
        }
        Command::Secret(SecretCmd::Set { name, value }) => {
            if value.is_some() {
                return Err(CliError::Usage(format!(
                    "la valeur d'un secret ne se passe jamais en argument : elle reste dans \
                     l'historique du shell et apparaît dans `ps`. Rien n'a été enregistré.\n\
                     → relancer sans valeur : `penelope secret set {name}`, puis coller la \
                     valeur à l'invite\n\
                     → si la vraie valeur a été tapée, la considérer comme exposée : en \
                     générer une nouvelle (pour un bot : /revoke chez @BotFather)"
                )));
            }
            return set_secret(&cli, name.clone());
        }
        Command::Install | Command::Uninstall | Command::Start | Command::Stop => {
            return service(&cli);
        }
        Command::Daemon => return daemon(&cli).await,
        Command::Logs {
            turn,
            session,
            lines,
        } => return logs(&cli, turn.as_deref(), session.as_deref(), *lines),
        Command::Upgrade { .. } => return upgrade(&cli).await,
        Command::Chat { session, message } => {
            return chat(&cli, session.clone(), message.clone()).await;
        }
        Command::Onboard { part } => return onboard(&cli, part.clone()).await,
        // Connexion d'un compte : le code s'affiche, puis Pénélope attend la validation.
        Command::Model(ModelCmd::Auth {
            provider,
            logout,
            status,
        }) if !cli.json && !*logout && !*status => {
            return model_auth(&cli, provider.clone()).await;
        }
        _ => {}
    }

    // Purge : effacement sans retour, confirmé à l'invite sauf `--yes` (issue #46), forks
    // nommés avant (arbitrage 3 de la V1).
    if let Command::Session(SessionCmd::Purge { session, yes, .. }) = &cli.command
        && !yes
        && !purge::confirm(&cli, session).await?
    {
        return Ok(());
    }

    let socket = socket_path(cli.home.clone())?;
    let (method, params) = route(&cli.command)?;
    let value = call(&socket, method, params).await?;

    match &cli.command {
        Command::Metrics if !cli.json => {
            print!("{}", value["text"].as_str().unwrap_or_default());
        }
        Command::Doctor => {
            let checks: Vec<penelope_kernel::api::DoctorCheck> =
                serde_json::from_value(value.clone()).unwrap_or_default();
            if cli.json {
                output::print(&value, true);
            } else {
                print!("{}", penelope_ops::doctor::render(&checks));
            }
            if checks.iter().any(|c| !c.ok && c.severity == "error") {
                return Err(CliError::Validation(
                    "des contrôles critiques sont en échec".into(),
                ));
            }
        }
        Command::Model(ModelCmd::List { .. }) if !cli.json => {
            println!("{}", render_model_list(&value));
        }
        Command::Mcp(McpCmd::List) if !cli.json => {
            println!("{}", render_mcp_list(&value));
        }
        Command::Schedule(ScheduleCmd::List) if !cli.json => {
            println!("{}", render_schedule_list(&value));
        }
        Command::Session(SessionCmd::List) if !cli.json => {
            println!("{}", render_session_list(&value));
        }
        Command::Session(SessionCmd::Compact { .. })
        | Command::Mem(MemCmd::Dream { .. })
        | Command::Mem(MemCmd::Diff { .. })
        | Command::Vault(VaultCmd::Lint)
        | Command::Import(_)
            if !cli.json =>
        {
            println!("{}", value["text"].as_str().unwrap_or_default());
        }
        Command::Skill(SkillCmd::Install { .. }) if !cli.json => {
            println!("{}", value["report"].as_str().unwrap_or_default());
        }
        Command::Mcp(McpCmd::Logs { .. }) if !cli.json => {
            for l in value["lines"].as_array().cloned().unwrap_or_default() {
                println!("{}", l.as_str().unwrap_or_default());
            }
        }
        Command::Mcp(_) => output::print(&value, true),
        Command::History(HistoryCmd::Verify { .. }) => {
            output::print(&value, cli.json);
            if value["ok"] != json!(true) {
                return Err(CliError::Validation(format!(
                    "{} divergence(s) entre le journal et les caches",
                    value["divergences"].as_array().map_or(0, Vec::len)
                )));
            }
        }
        Command::History(HistoryCmd::Reindex { .. }) => {
            output::print(&value, cli.json);
            if value["ok"] != json!(true) {
                return Err(CliError::Validation(
                    "des sessions n'ont pas pu être refondues".into(),
                ));
            }
        }
        _ => output::print(&value, cli.json),
    }
    Ok(())
}

async fn daemon(cli: &Cli) -> CliResult<()> {
    use penelope_ops::upgrade::{self, Boot};
    // Nouveau binaire à l'essai : ce démarrage est compté avant d'ouvrir quoi que ce soit,
    // pour qu'un plantage plus loin mène aussi au retour arrière.
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    match upgrade::on_boot_now(&dirs.state(), penelope_daemon::VERSION) {
        Boot::RolledBack { from, to } => {
            // L'ancien binaire est au même chemin : `KeepAlive` le relance (issue #36).
            return Err(CliError::Io(format!(
                "la version {from} n'a pas confirmé son démarrage : binaire {to} remis en \
                 place, le service repart avec lui"
            )));
        }
        Boot::Trial { attempt } => {
            eprintln!(
                "mise à jour {} à l'essai (démarrage {attempt})",
                penelope_daemon::VERSION
            );
            upgrade::arm_watchdog(upgrade::WATCHDOG);
        }
        Boot::Normal => {}
    }
    let d = penelope_daemon::runtime::Daemon::new(
        cli.home.clone(),
        Some(penelope_gateway_telegram::cards),
    )
    .await
    .map_err(|e| CliError::Io(e.to_string()))?;
    let cfg = d.services.config.config();
    // Sous launchd, stderr est `daemon.err.log`, jamais tourné : le JSON à rétention suffit,
    // `penelope logs` le relit. Une panique y reste visible, elle passe par le crochet de
    // panique et non par `tracing` (issue #103).
    let service = std::env::var_os("PENELOPE_SERVICE").is_some_and(|v| v == "1");
    penelope_observe::init(
        &d.services.platform.dirs.logs(),
        &cfg.observability.log_level,
        cfg.observability.log_retention_days,
        !service,
    );
    tracing::info!(version = penelope_daemon::VERSION, "Pénélope démarre");
    let d = std::sync::Arc::new(d);
    let gw = penelope_gateway_telegram::compose(&d).await; // avant `run` (#12, T29)
    d.run(gw).await.map_err(|e| CliError::Io(e.to_string()))
}

mod approval_stats;
mod cli;
mod interactive;
mod logs;
mod offline;
mod purge;
mod render;
mod restore;
mod route;
#[cfg(test)]
mod tests;
mod upgrade;
