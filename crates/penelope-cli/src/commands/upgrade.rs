//! `penelope upgrade` : par le daemon s'il tourne, sinon hors ligne, ici.

use super::*;

/// `penelope upgrade` : par le daemon s'il tourne (il redémarre ensuite), sinon ici.
pub(super) async fn upgrade(cli: &Cli) -> CliResult<()> {
    let (method, params) = route(&cli.command)?;
    let socket = socket_path(cli.home.clone())?;
    let (value, offline) = match call(&socket, method, params.clone()).await {
        Ok(v) => (v, false),
        Err(CliError::DaemonUnreachable(_)) => (upgrade_offline(cli, &params).await?, true),
        Err(e) => return Err(e),
    };
    if cli.json {
        output::print(&value, true);
        return Ok(());
    }
    println!("{}", penelope_ops::upgrade::render(&value));
    let changed = value["installed"].is_string() || value["rolled_back"].as_bool() == Some(true);
    if changed && offline {
        println!("Daemon arrêté : `penelope start` pour démarrer la nouvelle version.");
    } else if changed {
        println!("Le daemon redémarre : `penelope status` dans quelques secondes.");
    }
    Ok(())
}

async fn upgrade_offline(cli: &Cli, p: &Value) -> CliResult<Value> {
    use penelope_ops::upgrade as up;
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    // Sans daemon, la configuration est lue sur disque (clé minisign, adresse des releases).
    let cfg = std::fs::read_to_string(dirs.config_file())
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(cfg, _)| cfg)
        .unwrap_or_default();
    let source = up::Source::from_config(&cfg);
    if p["check"].as_bool().unwrap_or(false) {
        return up::check(&source).await.map_err(CliError::Io);
    }
    let state = dirs.state();
    if p["switch"].as_bool() == Some(true) {
        let install_dir = dirs.expand(&cfg.upgrade.install_dir);
        let current = up::running_binary().map_err(CliError::Io)?;
        return up::switch_to_releases(up::Switch {
            source: &source,
            tag: p["tag"].as_str(),
            current: &current,
            install_dir: &install_dir,
            state_dir: &state,
            now: chrono::Utc::now().to_rfc3339(),
            codesign: up::codesign_of(&cfg),
            host: &up::SystemHost,
        })
        .await
        .map_err(CliError::Io);
    }
    let binary = up::installed_binary().map_err(CliError::Usage)?;
    if p["rollback"].as_bool().unwrap_or(false) {
        return up::manual_rollback(&binary, &state).map_err(CliError::Io);
    }
    up::install(up::Install {
        source: &source,
        tag: p["tag"].as_str(),
        force: p["force"].as_bool().unwrap_or(false),
        binary: &binary,
        state_dir: &state,
        now: chrono::Utc::now().to_rfc3339(),
        codesign: up::codesign_of(&cfg),
    })
    .await
    .map_err(CliError::Io)
}
