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
    AuditCmd, BackupCmd, Cli, Command, ConfigCmd, HistoryCmd, ImportCmd, LocalCmd, McpCmd, MemCmd,
    ModelCmd, ProfileCmd, ScheduleCmd, SecretCmd, SessionCmd, SkillCmd, StoreCmd, VaultCmd, WfCmd,
};
use dataset::DatasetCmd;
use interactive::{chat, model_auth, onboard};
#[cfg(test)]
use logs::filter_log_lines;
use logs::logs;
#[cfg(test)]
use offline::store_secret;
use offline::{doctor, eval_local, paths, service, set_secret, validate_config, validate_workflow};
use penelope_kernel::api::method as m;
use render::*;
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
        // Le serveur d'agenda (#295) : lancé par Pénélope elle-même, depuis `mcp.d`.
        Command::AgendaMcp => {
            return penelope_agenda_mcp::run_stdio()
                .await
                .map_err(CliError::Usage);
        }
        Command::Approvals {
            cmd: Some(ApprovalsCmd::Stats { days }),
        } => return approval_stats::run(&cli, *days),
        Command::Dataset(cmd) => return dataset::run(&cli, cmd),
        Command::Doctor => return doctor(&cli).await,
        Command::Config(ConfigCmd::Validate { file }) => {
            return validate_config(&cli, file.clone());
        }
        Command::Wf(WfCmd::Validate { file }) => return validate_workflow(&cli, file.clone()),
        Command::Eval { suite } => return eval_local(suite).await,
        Command::Local(cmd) => return local::run(&cli, cmd),
        Command::Backup { cmd: Some(cmd), .. } => return backup_setup::run(&cli, cmd).await,
        Command::Restore {
            source,
            dry_run,
            list,
            archive,
            endpoint,
            region,
            no_start,
        } => {
            let args = restore::RestoreArgs {
                source: source.clone(),
                dry_run: *dry_run,
                list: *list,
                archive: archive.clone(),
                endpoint: endpoint.clone(),
                region: region.clone(),
                no_start: *no_start,
            };
            return restore::restore(&cli, args).await;
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
            return set_secret(&cli, name.clone()).await;
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
        Command::Wf(WfCmd::Runs) if !cli.json => {
            println!("{}", render_run_list(&value));
        }
        Command::Session(SessionCmd::List) if !cli.json => {
            println!("{}", render_session_list(&value));
        }
        Command::Session(SessionCmd::Compact { .. })
        | Command::Mem(MemCmd::Dream { .. })
        | Command::Mem(MemCmd::Diff { .. })
        | Command::Mem(MemCmd::Reclaim { .. })
        | Command::Vault(VaultCmd::Lint)
        | Command::Import(_)
        | Command::Context { .. }
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
    use penelope_ops::lifecycle;
    use penelope_ops::upgrade::{self, Boot};
    // Nouveau binaire à l'essai : ce démarrage est compté avant d'ouvrir quoi que ce soit,
    // pour qu'un plantage plus loin mène aussi au retour arrière.
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    match upgrade::on_boot_now(&dirs.state(), penelope_daemon::VERSION) {
        Boot::RolledBack { from, to } => {
            // L'ancien binaire est au même chemin : `KeepAlive` le relance (issue #36). Le
            // journal n'est pas encore ouvert : la trace de vie le lui dira (#225).
            let why = format!("retour arrière automatique, {from} non confirmée");
            let stop = lifecycle::Stop::new("mise à jour", why, true);
            lifecycle::on_stop(&dirs.state(), &from, Some(&stop));
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
    // Le démarrage dit ce qui l'a précédé : arrêt demandé (par qui) ou non propre (#225).
    let state = d.services.platform.dirs.state();
    let previous = lifecycle::on_start(&state, penelope_daemon::VERSION);
    tracing::info!(
        version = penelope_daemon::VERSION,
        arret_precedent = previous.as_deref().unwrap_or("aucun (premier démarrage)"),
        "Pénélope démarre"
    );
    let d = std::sync::Arc::new(d);
    let gw = penelope_gateway_telegram::compose(&d).await; // avant `run` (#12, T29)
    let handle = d.handle.clone();
    let run = d.run(gw).await;
    let stop = handle.stop_reason();
    let why = stop.as_ref().map(|s| s.to_string());
    tracing::info!(
        version = penelope_daemon::VERSION,
        arret = why
            .as_deref()
            .unwrap_or("sans demande (sortie de la boucle principale)"),
        "Pénélope s'arrête"
    );
    lifecycle::on_stop(&state, penelope_daemon::VERSION, stop.as_ref());
    run.map_err(|e| CliError::Io(e.to_string()))
}

mod approval_stats;
mod backup_setup;
#[cfg(test)]
mod backup_tests;
mod cli;
mod console;
mod dataset;
mod interactive;
mod local;
mod logs;
mod offline;
mod purge;
mod render;
mod restore;
mod route;
#[cfg(test)]
mod tests;
mod upgrade;
