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
#[cfg(test)]
use logs::filter_log_lines;
use logs::logs;
use penelope_kernel::api::method as m;
use render::*;
use serde_json::{Value, json};
use std::path::PathBuf;

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

/// Associe une commande à sa méthode RPC (CA 15 : parité Telegram ↔ CLI).
#[allow(clippy::too_many_lines)] // gel 0.17 : table de routage des commandes vers les méthodes RPC
pub fn route(cmd: &Command) -> CliResult<(&'static str, Value)> {
    Ok(match cmd {
        Command::Status => (m::STATUS, json!({})),
        Command::Metrics => (m::METRICS, json!({})),
        Command::Doctor => (m::DOCTOR, json!({})),
        Command::Restart => (m::RESTART, json!({})),
        Command::Import(ImportCmd::Hermes {
            path,
            dry_run,
            no_test,
        }) => (
            m::IMPORT_HERMES,
            json!({
                "path": path.as_ref().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())),
                "apply": !dry_run,
                "test": !no_test,
            }),
        ),
        Command::Upgrade {
            check,
            rollback,
            tag,
            force,
            switch,
        } => (
            m::UPGRADE,
            json!({"check": check, "rollback": rollback, "tag": tag, "force": force, "switch": switch}),
        ),

        Command::Session(SessionCmd::List) => (m::SESSION_LIST, json!({})),
        Command::Session(SessionCmd::New { title }) => (m::SESSION_NEW, json!({"title": title})),
        Command::Session(SessionCmd::Close { session }) => {
            (m::SESSION_CLOSE, json!({"session": session}))
        }
        Command::Session(SessionCmd::Purge {
            session, reason, ..
        }) => (
            m::SESSION_PURGE,
            json!({"session": session, "reason": reason}),
        ),
        Command::Session(SessionCmd::Title { session, title }) => (
            m::SESSION_TITLE,
            json!({"session": session, "title": title.join(" ")}),
        ),
        Command::Session(SessionCmd::Budget { session, usd }) => (
            m::SESSION_BUDGET,
            json!({
                "session": session,
                "usd": match usd.as_deref() {
                    None => Value::Null,
                    Some("off") => json!(0),
                    Some(x) => json!(x),
                },
            }),
        ),
        Command::Session(SessionCmd::Model { alias, session }) => (
            m::SESSION_MODEL,
            json!({"alias": alias, "session": session}),
        ),
        Command::Session(SessionCmd::Export { session }) => {
            (m::SESSION_EXPORT, json!({"session": session}))
        }
        Command::Session(SessionCmd::Compact { session }) => {
            (m::SESSION_COMPACT, json!({"session": session}))
        }
        Command::Session(SessionCmd::Mode { mode, session }) => {
            (m::SESSION_MODE, json!({"mode": mode, "session": session}))
        }
        Command::Session(SessionCmd::Project { project, session }) => (
            m::SESSION_PROJECT,
            json!({"project": project, "session": session}),
        ),

        Command::Config(ConfigCmd::Get) => (m::CONFIG_GET, json!({})),
        Command::Config(ConfigCmd::Status) => (m::CONFIG_STATUS, json!({})),
        Command::Config(ConfigCmd::Reload) => (m::CONFIG_RELOAD, json!({})),
        Command::Config(ConfigCmd::Set { path, value }) => (
            m::CONFIG_SET,
            json!({"path": path, "value": parse_scalar(value)}),
        ),

        Command::Secret(SecretCmd::List) => (m::SECRET_LIST, json!({})),
        Command::Secret(SecretCmd::Backend) => (m::SECRET_BACKEND, json!({})),
        Command::Secret(SecretCmd::Rm { name }) => (m::SECRET_RM, json!({"name": name})),

        Command::Model(ModelCmd::List { filter }) => (m::MODEL_LIST, json!({"filter": filter})),
        Command::Mcp(McpCmd::List) => (m::MCP_LIST, json!({})),
        Command::Mcp(McpCmd::Show { name }) => (m::MCP_SHOW, json!({"name": name})),
        Command::Mcp(McpCmd::Auth { name, callback }) => {
            (m::MCP_AUTH, json!({"name": name, "callback": callback}))
        }
        Command::Mcp(McpCmd::Add { file, name }) => {
            (m::MCP_ADD, json!({"toml": read_toml(file)?, "name": name}))
        }
        Command::Mcp(McpCmd::Edit { name, field, value }) => (
            m::MCP_EDIT,
            json!({"name": name, "patch": {field.clone(): parse_scalar(value)}}),
        ),
        Command::Mcp(McpCmd::Rm { name }) => (m::MCP_RM, json!({"name": name})),
        Command::Mcp(McpCmd::Enable { name }) => (m::MCP_ENABLE, json!({"name": name})),
        Command::Mcp(McpCmd::Disable { name }) => (m::MCP_DISABLE, json!({"name": name})),
        Command::Mcp(McpCmd::Restart { name }) => (m::MCP_RESTART, json!({"name": name})),
        Command::Mcp(McpCmd::Test { name, file }) => match file {
            Some(f) => (m::MCP_TEST, json!({"toml": read_toml(f)?, "name": name})),
            None => (m::MCP_TEST, json!({"name": name})),
        },
        Command::Mcp(McpCmd::Logs { name, lines }) => {
            (m::MCP_LOGS, json!({"name": name, "lines": lines}))
        }
        Command::Model(ModelCmd::Set { alias, model }) => {
            (m::MODEL_SET, json!({"alias": alias, "model": model}))
        }
        // `model auth` sans option passe par `model_auth` (code affiché, puis attente) ;
        // cette route sert la parité CLI↔RPC et le mode `--json`.
        Command::Model(ModelCmd::Auth {
            provider,
            logout,
            status,
        }) => (
            m::MODEL_AUTH,
            json!({
                "provider": provider,
                "action": if *logout { "logout" } else if *status { "status" } else { "start" },
            }),
        ),

        Command::Wf(WfCmd::List) => (m::WF_LIST, json!({})),
        Command::Wf(WfCmd::Show { id }) => (m::WF_SHOW, json!({"id": id})),
        Command::Wf(WfCmd::Runs) => (m::WF_RUNS, json!({})),
        Command::Wf(WfCmd::Trace { run }) => (m::WF_TRACE, json!({"run": run})),
        Command::Wf(WfCmd::Run { id, params }) => (
            m::WF_RUN,
            json!({"id": id, "params": penelope_gateway_telegram::parse_params(&params.join(" "))}),
        ),
        Command::Wf(WfCmd::Control {
            run,
            op,
            choice,
            input,
            usd,
            tokens,
        }) => (
            m::WF_CONTROL,
            json!({"run": run, "op": op, "choice": choice, "input": input,
                   "usd": usd, "tokens": tokens}),
        ),

        Command::Schedule(ScheduleCmd::List) => (m::SCHEDULE_LIST, json!({})),
        Command::Schedule(ScheduleCmd::Pause { id }) => (m::SCHEDULE_PAUSE, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Resume { id }) => (m::SCHEDULE_RESUME, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Rm { id }) => (m::SCHEDULE_RM, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Run { id }) => (m::SCHEDULE_RUN_NOW, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Move {
            id,
            chat,
            topic,
            private,
        }) => {
            if !private && chat.is_none() {
                return Err(CliError::Usage(
                    "où l'envoyer : `--private`, ou `--chat <id>` (et `--topic <id>`)".into(),
                ));
            }
            (
                m::SCHEDULE_MOVE,
                json!({"id": id, "private": private, "chat_id": chat, "topic_id": topic}),
            )
        }
        Command::Schedule(ScheduleCmd::Add {
            kind,
            spec,
            target,
            dedup,
        }) => {
            let json_arg = |name: &str, raw: &str| {
                serde_json::from_str::<Value>(raw)
                    .map_err(|e| CliError::Usage(format!("--{name} n'est pas du JSON : {e}")))
            };
            (
                m::SCHEDULE_ADD,
                json!({
                    "kind": kind,
                    "spec": json_arg("spec", spec)?,
                    "target": json_arg("target", target)?,
                    "dedup": match dedup {
                        Some(d) => json_arg("dedup", d)?,
                        None => json!({}),
                    },
                }),
            )
        }

        Command::Mem(MemCmd::Search { query }) => (m::MEM_SEARCH, json!({"query": query})),
        Command::Mem(MemCmd::Show { uid }) => (m::MEM_SHOW, json!({"uid": uid})),
        Command::Mem(MemCmd::History { uid, file }) => {
            (m::MEM_HISTORY, json!({"uid": uid, "file": file}))
        }
        Command::Mem(MemCmd::Restore { id }) => (m::MEM_RESTORE, json!({"id": id})),
        Command::Mem(MemCmd::Reindex { embeddings }) => {
            (m::MEM_REINDEX, json!({"embeddings": embeddings}))
        }
        Command::Mem(MemCmd::Forget { uid }) => (m::MEM_FORGET, json!({"uid": uid})),
        Command::Mem(MemCmd::Candidates) => (m::MEM_CANDIDATES, json!({})),
        Command::Mem(MemCmd::Split { uid }) => (m::MEM_SPLIT, json!({"uid": uid})),
        Command::Mem(MemCmd::Audit) => (m::MEM_AUDIT, json!({})),
        Command::Mem(MemCmd::RetryRejected) => (m::MEM_RETRY_REJECTED, json!({})),
        Command::Mem(MemCmd::Diff { since }) => (m::MEM_DIFF, json!({"since": since})),
        Command::Mem(MemCmd::Dream { dry_run }) => (m::MEM_DREAM, json!({"dry_run": dry_run})),
        Command::Mem(MemCmd::Learned { days }) => (m::MEM_LEARNED, json!({"days": days})),
        Command::Mem(MemCmd::Signals { uid }) => (m::MEM_SIGNALS, json!({"uid": uid})),
        Command::Vault(VaultCmd::Sync) => (m::VAULT_SYNC, json!({})),
        Command::Vault(VaultCmd::Check) => (m::VAULT_CHECK, json!({})),
        Command::Vault(VaultCmd::Lint) => (m::VAULT_LINT, json!({})),

        Command::Skill(SkillCmd::List) => (m::SKILL_LIST, json!({})),
        Command::Skill(SkillCmd::Show { name }) => (m::SKILL_SHOW, json!({"name": name})),
        Command::Skill(SkillCmd::Rollback { name }) => (m::SKILL_ROLLBACK, json!({"name": name})),
        Command::Skill(SkillCmd::Reload) => (m::SKILL_RELOAD, json!({})),
        Command::Skill(SkillCmd::Install { source, force }) => {
            (m::SKILL_INSTALL, json!({"source": source, "force": force}))
        }

        Command::Jobs { all } => (m::JOBS, json!({"all": all})),
        Command::Approvals { .. } => (m::APPROVALS, json!({})),
        Command::Approve { id, always, effect } => (
            m::APPROVE,
            json!({"id": id, "always": always, "effect": effect}),
        ),
        Command::Deny { id, reason } => (m::DENY, json!({"id": id, "reason": reason})),
        Command::Policies => (m::POLICIES, json!({})),

        Command::Usage {
            by,
            session,
            since,
            limit,
        } => (
            m::USAGE,
            json!({"by": by, "session": session, "since": since, "limit": limit}),
        ),
        Command::AuditVerify => (m::AUDIT_VERIFY, json!({})),
        Command::History(HistoryCmd::Verify { session }) => {
            (m::HISTORY_VERIFY, json!({"session": session}))
        }
        Command::History(HistoryCmd::Reindex { session }) => {
            (m::HISTORY_REINDEX, json!({"session": session}))
        }
        Command::Audit(AuditCmd::Show { turn, session }) => {
            if turn.is_none() && session.is_none() {
                return Err(CliError::Usage(
                    "préciser `--turn <id>` ou `--session <id>`".into(),
                ));
            }
            (m::AUDIT_SHOW, json!({"turn": turn, "session": session}))
        }
        Command::Backup { push, full, media } => (
            m::BACKUP,
            json!({"push": push, "full": *full || *push, "media": media}),
        ),
        Command::Export { what, id } => (m::EXPORT, json!({"what": what, "id": id})),
        Command::Store(StoreCmd::Rebuild) => (m::STORE_REBUILD, json!({})),
        Command::Session(SessionCmd::Fork { session, title }) => {
            (m::SESSION_FORK, json!({"session": session, "title": title}))
        }
        Command::Session(SessionCmd::Rewind { turns, session }) => (
            m::SESSION_REWIND,
            json!({"session": session, "turns": turns}),
        ),

        other => {
            return Err(CliError::Usage(format!("commande non routée : {other:?}")));
        }
    })
}

/// `penelope upgrade` : par le daemon s'il tourne (il redémarre ensuite), sinon ici.
async fn upgrade(cli: &Cli) -> CliResult<()> {
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

/// Suite d'évaluation depuis les sources : `cargo test` avec le filtre de la suite.
async fn eval_local(suite: &str) -> CliResult<()> {
    let Some((sub, args)) = penelope_evals::suites::cargo_filter(suite) else {
        let known: Vec<String> = penelope_evals::suites::all_suites()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect();
        return Err(CliError::Usage(format!(
            "suite inconnue : `{suite}` (suites : {})",
            known.join(", ")
        )));
    };
    let needed = penelope_evals::suites::required_env(suite);
    if !needed.is_empty() {
        let missing: Vec<&str> = needed
            .iter()
            .copied()
            .filter(|v| {
                std::env::var(v)
                    .map(|x| x.trim().is_empty())
                    .unwrap_or(true)
            })
            .collect();
        if !missing.is_empty() {
            return Err(CliError::Usage(format!(
                "suite réseau `{suite}` : variable(s) à exporter d'abord : {}",
                missing.join(", ")
            )));
        }
        eprintln!("⚠️ suite réseau `{suite}` : services réels, appels facturés.");
    }
    let root = std::env::var("PENELOPE_SOURCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    if !manifest.contains("[workspace]") || !root.join("crates/penelope-evals").exists() {
        return Err(CliError::Usage(
            "à lancer depuis le dépôt de Pénélope (ou `PENELOPE_SOURCE_DIR`)".into(),
        ));
    }
    let status = tokio::process::Command::new("cargo")
        .arg(sub)
        .args(&args)
        .current_dir(&root)
        .status()
        .await
        .map_err(|e| CliError::Io(format!("cargo : {e}")))?;
    if status.success() {
        println!("✅ suite `{suite}` verte");
        Ok(())
    } else {
        Err(CliError::Validation(format!("suite `{suite}` en échec")))
    }
}

/// Restauration hors ligne : refusée daemon en marche, base actuelle mise de côté.
async fn restore_offline(cli: &Cli, file: &std::path::Path) -> CliResult<()> {
    let socket = socket_path(cli.home.clone())?;
    if call(&socket, m::STATUS, json!({})).await.is_ok() {
        return Err(CliError::Usage(
            "le daemon tourne : `penelope stop` d'abord, puis relancer la restauration".into(),
        ));
    }
    let raw = std::fs::read(file).map_err(|e| CliError::Io(format!("{} : {e}", file.display())))?;
    if !raw.starts_with(b"SQLite format 3\0") {
        return Err(CliError::Validation(format!(
            "{} n'est pas une base SQLite (sauvegarde `penelope backup` attendue)",
            file.display()
        )));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let db = dirs.db_path();
    if db.exists() {
        let aside = dirs.data().join("backups").join(format!(
            "avant-restauration-{}.db",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        ));
        std::fs::create_dir_all(aside.parent().unwrap_or(std::path::Path::new(".")))
            .map_err(|e| CliError::Io(e.to_string()))?;
        std::fs::copy(&db, &aside).map_err(|e| CliError::Io(e.to_string()))?;
        println!("Base actuelle mise de côté : {}", aside.display());
    }
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", db.display()));
        let _ = std::fs::remove_file(side);
    }
    std::fs::write(&db, &raw).map_err(|e| CliError::Io(e.to_string()))?;
    println!(
        "✅ Restauré depuis {} : `penelope start` pour relancer.",
        file.display()
    );
    Ok(())
}

/// `penelope restore-all` : remonte une instance entière depuis une sauvegarde chiffrée
/// (issue #42). Se fait daemon arrêté, sur une machine où il n'y a encore rien.
async fn restore_all(cli: &Cli, source: Option<String>, dry_run: bool) -> CliResult<()> {
    let socket = socket_path(cli.home.clone())?;
    if !dry_run && call(&socket, m::STATUS, json!({})).await.is_ok() {
        return Err(CliError::Usage(
            "le daemon tourne : `penelope stop` d'abord, puis relancer la restauration".into(),
        ));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let work = dirs.data().join("backups").join("restore");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| CliError::Io(e.to_string()))?;

    // Source : une archive locale, ou un dépôt à cloner.
    let source = source.ok_or_else(|| {
        CliError::Usage(
            "donner l'archive `.tar.gz.enc` ou le dépôt privé des sauvegardes : \
             `penelope restore-all git@github.com:moi/penelope-backups.git`"
                .into(),
        )
    })?;
    let archive = if source.ends_with(".enc") {
        PathBuf::from(&source)
    } else {
        let repo = work.join("depot");
        println!("Clonage de {source}…");
        penelope_platform::process::git_sync_repo(&repo, &source)
            .map_err(|e| CliError::Io(e.to_string()))?;
        let mut found: Vec<PathBuf> = std::fs::read_dir(&repo)
            .map_err(|e| CliError::Io(e.to_string()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".tar.gz.enc"))
            .collect();
        found.sort();
        found.pop().ok_or_else(|| {
            CliError::Validation(format!("aucune sauvegarde chiffrée dans {source}"))
        })?
    };
    if !archive.is_file() {
        return Err(CliError::Validation(format!(
            "{} introuvable",
            archive.display()
        )));
    }

    // Phrase de passe : demandée à l'invite, jamais en argument.
    eprint!("Phrase de passe de la sauvegarde : ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut pass = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut pass)
        .map_err(|e| CliError::Io(e.to_string()))?;
    let pass = pass.trim().to_string();

    let tar = work.join("sauvegarde.tar.gz");
    penelope_platform::archive::open(&archive, &tar, &pass)
        .map_err(|e| CliError::Validation(e.to_string()))?;
    penelope_platform::process::extract_tar_gz(&tar, &work)
        .map_err(|e| CliError::Io(e.to_string()))?;
    let root = work.join("penelope");
    let manifest: Value = std::fs::read_to_string(root.join("MANIFEST.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);

    // Ce qui serait écrit, dans l'ordre.
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
    if root.join("penelope.db").is_file() {
        plan.push((root.join("penelope.db"), dirs.db_path()));
    }
    for (name, dst) in [
        ("vault", dirs.data().join("vault")),
        ("skills", dirs.data().join("skills")),
        ("workflows", dirs.data().join("workflows")),
        ("templates", dirs.data().join("templates")),
        ("mcp.d", dirs.data().join("mcp.d")),
        ("artifacts", dirs.data().join("artifacts")),
        ("media", dirs.data().join("media")),
        ("config.toml", dirs.config_file()),
    ] {
        let src = root.join(name);
        if src.exists() {
            plan.push((src, dst));
        }
    }

    println!(
        "Sauvegarde du {} (version {}) :",
        manifest["created_at"].as_str().unwrap_or("?"),
        manifest["version"].as_str().unwrap_or("?")
    );
    for (src, dst) in &plan {
        println!(
            "  {} → {}",
            src.file_name().unwrap_or_default().to_string_lossy(),
            dst.display()
        );
    }
    if dry_run {
        println!("\n(--dry-run : rien n'a été écrit)");
        return Ok(());
    }

    for (src, dst) in &plan {
        if dst.exists() {
            let aside = dst.with_extension(format!(
                "avant-restauration-{}",
                chrono::Utc::now().format("%Y%m%dT%H%M%S")
            ));
            let _ = std::fs::rename(dst, &aside);
            println!("Existant mis de côté : {}", aside.display());
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| CliError::Io(e.to_string()))?;
        }
        copy_tree(src, dst).map_err(|e| CliError::Io(e.to_string()))?;
    }
    // Journal WAL d'une base copiée : retiré, la base restaurée est cohérente.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!(
            "{}{suffix}",
            dirs.db_path().display()
        )));
    }

    let secrets: Vec<String> = manifest["secrets_expected"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    println!("\n✅ Fichiers restaurés. Il reste à faire, dans cet ordre :");
    println!("  1. `penelope install` puis `penelope start` (service).");
    if !secrets.is_empty() {
        println!("  2. Ressaisir les secrets, qui ne sont jamais sauvegardés :");
        for name in &secrets {
            println!("       penelope secret set {name}");
        }
    }
    println!("  3. `penelope doctor` : serveurs MCP à réautoriser, modèle de transcription à");
    println!("     télécharger, phrase de passe de sauvegarde à reposer.");
    Ok(())
}

/// Copie récursive, fichier ou répertoire.
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if src.is_file() {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        copy_tree(&e.path(), &dst.join(e.file_name()))?;
    }
    Ok(())
}

/// Contenu d'un fichier de déclaration MCP.
fn read_toml(path: &std::path::Path) -> CliResult<String> {
    std::fs::read_to_string(path).map_err(|e| CliError::Io(format!("{} : {e}", path.display())))
}

/// `"12"` devient un nombre, `"true"` un booléen, le reste une chaîne.
fn parse_scalar(raw: &str) -> Value {
    if let Ok(b) = raw.parse::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(i) = raw.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return json!(f);
    }
    if ((raw.starts_with('{') && raw.ends_with('}'))
        || (raw.starts_with('[') && raw.ends_with(']')))
        && let Ok(v) = serde_json::from_str::<Value>(raw)
    {
        return v;
    }
    Value::String(raw.to_string())
}

fn paths(cli: &Cli) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let v = json!({
        "config": dirs.config(),
        "data": dirs.data(),
        "state": dirs.state(),
        "logs": dirs.logs(),
        "cache": dirs.cache(),
        "db": dirs.db_path(),
        "socket": dirs.socket_path(),
        "vault": dirs.vault(),
        "skills": dirs.skills(),
        "workflows": dirs.workflows(),
        "templates": dirs.templates(),
        "mcp.d": dirs.mcp_d(),
    });
    output::print(&v, cli.json);
    Ok(())
}

/// `penelope secret set <nom>` : la valeur vient de l'entrée standard, jamais d'un
/// argument, et n'est **jamais** réaffichée.
fn set_secret(cli: &Cli, name: String) -> CliResult<()> {
    penelope_platform::validate_secret_name(&name).map_err(|e| CliError::Usage(e.to_string()))?;

    let raw = penelope_platform::terminal::read_secret(&format!(
        "Colle la valeur de `{name}` puis Entrée (rien ne s'affiche) : "
    ))
    .map_err(|e| CliError::Io(format!("lecture de la valeur : {e}")))?;
    // Un copier-coller traîne presque toujours un retour à la ligne ou une espace.
    let value = raw.trim();
    if value.is_empty() {
        return Err(CliError::Usage(format!(
            "valeur vide : relancer `penelope secret set {name}` et coller la valeur à \
             l'invite"
        )));
    }

    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    dirs.ensure_all().map_err(|e| CliError::Io(e.to_string()))?;
    let store = penelope_platform::backend::secret_store(dirs.as_ref())
        .map_err(|e| CliError::Io(e.to_string()))?;
    store
        .set(&name, value)
        .map_err(|e| CliError::Io(e.to_string()))?;

    output::print(
        &json!({
            "name": name,
            "backend": store.backend(),
            "bytes": value.len(),
            "stored": true,
        }),
        cli.json,
    );
    Ok(())
}
/// `doctor` en deux temps (issue #99) : ce qui se vérifie sans le daemon d'abord, puis
/// ses propres contrôles. Un daemon muet ou absent est un contrôle en échec, en tête du
/// rapport, pas une commande qui pend.
async fn doctor(cli: &Cli) -> CliResult<()> {
    use penelope_kernel::api::DoctorCheck;
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let socket = dirs.socket_path();
    let mut checks: Vec<DoctorCheck> = Vec::new();

    let remote = call(&socket, m::DOCTOR, json!({})).await;
    checks.push(match &remote {
        Ok(_) => DoctorCheck::ok("daemon", "Daemon", "répond"),
        Err(e @ CliError::DaemonUnresponsive(_)) => DoctorCheck::fail(
            "daemon",
            "Daemon",
            e.to_string(),
            Some("penelope restart".into()),
        )
        .critical(),
        Err(e) => DoctorCheck::fail(
            "daemon",
            "Daemon",
            e.to_string(),
            Some("penelope start".into()),
        )
        .critical(),
    });
    checks.push(DoctorCheck::ok(
        "binary",
        "Binaire",
        format!("penelope {}", env!("CARGO_PKG_VERSION")),
    ));
    let config = dirs.config_file();
    checks.push(match std::fs::read_to_string(&config) {
        Ok(raw) => match penelope_kernel::Config::parse(&raw) {
            Ok((cfg, _)) => match cfg.validate() {
                Ok(()) => DoctorCheck::ok("config.file", "Fichier de configuration", "valide"),
                Err(e) => DoctorCheck::fail(
                    "config.file",
                    "Fichier de configuration",
                    e.to_string(),
                    Some("penelope config validate".into()),
                ),
            },
            Err(e) => DoctorCheck::fail(
                "config.file",
                "Fichier de configuration",
                e.to_string(),
                Some("penelope config validate".into()),
            )
            .critical(),
        },
        Err(e) => DoctorCheck::fail(
            "config.file",
            "Fichier de configuration",
            format!("{} : {e}", config.display()),
            None,
        ),
    });
    if let Ok(v) = remote {
        let from_daemon: Vec<DoctorCheck> = serde_json::from_value(v).unwrap_or_default();
        checks.extend(from_daemon);
    }
    if cli.json {
        output::print(&json!(checks), true);
    } else {
        print!("{}", penelope_ops::doctor::render(&checks));
    }
    if checks.iter().any(|c| !c.ok && c.severity == "error") {
        return Err(CliError::Validation(
            "des contrôles critiques sont en échec".into(),
        ));
    }
    Ok(())
}

fn validate_config(cli: &Cli, file: Option<PathBuf>) -> CliResult<()> {
    let path = match file {
        Some(p) => p,
        None => penelope_platform::resolve_directories(cli.home.clone())
            .map_err(|e| CliError::Io(e.to_string()))?
            .config_file(),
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| CliError::Io(format!("{} : {e}", path.display())))?;
    // Tolérant comme le daemon (#76) : une clé inconnue est nommée, pas fatale.
    let (cfg, unknown) =
        penelope_kernel::Config::parse(&raw).map_err(|e| CliError::Validation(e.to_string()))?;
    cfg.validate()
        .map_err(|e| CliError::Validation(e.to_string()))?;
    for k in &unknown {
        println!(
            "⚠️ clé ignorée par cette version : {k} (écrite par une version plus récente, ou \
             faute de frappe)"
        );
    }
    let found = penelope_kernel::coherence::contradictions(&cfg);
    let refusals: Vec<String> = found
        .iter()
        .filter(|c| c.gravity == penelope_kernel::coherence::Gravity::Refus)
        .map(|c| format!("{} ({})", c.message, c.keys.join(", ")))
        .collect();
    if !refusals.is_empty() {
        return Err(CliError::Validation(format!(
            "réglages qui s'annulent :\n- {}",
            refusals.join("\n- ")
        )));
    }
    for c in &found {
        println!("⚠️ {} ({})", c.message, c.keys.join(", "));
    }
    output::ok(&format!("{} est valide", path.display()), cli.json);
    Ok(())
}

fn validate_workflow(cli: &Cli, file: PathBuf) -> CliResult<()> {
    let raw = std::fs::read_to_string(&file)
        .map_err(|e| CliError::Io(format!("{} : {e}", file.display())))?;
    let w = penelope_workflow::Workflow::from_json(&raw)
        .map_err(|e| CliError::Validation(format!("JSON invalide : {e}")))?;
    let stem = file
        .file_name()
        .map(|n| n.to_string_lossy().replace(".workflow.json", ""));

    // Hors daemon : on valide sur ce qui est connu statiquement.
    let known = penelope_workflow::Known {
        workflow_ids: penelope_workflow::bundled::all()
            .into_iter()
            .map(|x| x.metadata.id)
            .collect(),
        native_tools: penelope_tools::all_tools()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect(),
        templates: penelope_telegram::templates::CATALOG
            .iter()
            .map(|s| s.to_string())
            .collect(),
        max_depth: 3,
        ..Default::default()
    };
    let report = penelope_workflow::validate(&w, stem.as_deref(), &known);

    if cli.json {
        output::print(
            &json!({
                "valid": report.is_valid(),
                "issues": report.issues.iter().map(|i| json!({
                    "path": i.path, "message": i.message,
                    "severity": format!("{:?}", i.severity).to_lowercase(),
                })).collect::<Vec<_>>(),
            }),
            true,
        );
    } else {
        println!("{}", report.render());
    }
    if report.is_valid() {
        Ok(())
    } else {
        Err(CliError::Validation(format!(
            "{} : {} erreur(s)",
            file.display(),
            report.errors().len()
        )))
    }
}

fn service(cli: &Cli) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let mgr = penelope_platform::backend::service_manager(dirs.as_ref())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let exe = std::env::current_exe().map_err(|e| CliError::Io(e.to_string()))?;

    let out = match cli.command {
        Command::Install => {
            let p = mgr
                .install(&exe, cli.home.as_deref())
                .map_err(|e| CliError::Io(e.to_string()))?;
            json!({"installed": true, "unit": p, "mechanism": mgr.mechanism()})
        }
        Command::Uninstall => {
            mgr.uninstall().map_err(|e| CliError::Io(e.to_string()))?;
            json!({"uninstalled": true})
        }
        Command::Start => {
            mgr.start().map_err(|e| CliError::Io(e.to_string()))?;
            json!({"started": true})
        }
        Command::Stop => {
            mgr.stop().map_err(|e| CliError::Io(e.to_string()))?;
            json!({"stopped": true})
        }
        _ => unreachable!("routage service"),
    };
    output::print(&out, cli.json);
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
    let d = penelope_daemon::Daemon::new(cli.home.clone(), Some(penelope_gateway_telegram::cards))
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

/// `penelope chat` : un message, ou une conversation interactive.
/// `penelope onboard` : une question à la fois, réponse vide pour passer, `q` pour
/// reprendre plus tard ; le récapitulatif est validé avant écriture.
async fn onboard(cli: &Cli, part: Option<String>) -> CliResult<()> {
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let socket = socket_path(cli.home.clone())?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut read = async || -> CliResult<Option<String>> {
        let _ = std::io::stdout().flush();
        lines
            .next_line()
            .await
            .map_err(|e| CliError::Io(e.to_string()))
    };
    loop {
        let v = call(&socket, m::ONBOARD_NEXT, json!({"part": part})).await?;
        if v["done"].as_bool() == Some(true) {
            println!("\n{}", v["text"].as_str().unwrap_or_default());
            print!("Écrire dans le profil et la mémoire ? [o/N] ");
            let ok = read()
                .await?
                .is_some_and(|l| matches!(l.trim(), "o" | "O" | "oui" | "y"));
            if ok {
                let w = call(&socket, m::ONBOARD_WRITE, json!({"rel": v["rel"]})).await?;
                println!(
                    "Enregistré : {} ajout(s), {} remplacement(s).",
                    w["added"], w["replaced"]
                );
            } else {
                println!("Rien n'est écrit.");
            }
            return Ok(());
        }
        let q = &v["question"];
        println!(
            "\n[{}/{}] {}",
            q["position"],
            q["total"],
            q["text"].as_str().unwrap_or_default()
        );
        if let Some(h) = q["hint"].as_str().filter(|h| !h.is_empty()) {
            println!("  {h}");
        }
        if let Some(choices) = q["choices"].as_array().filter(|c| !c.is_empty()) {
            let list: Vec<&str> = choices.iter().filter_map(|c| c.as_str()).collect();
            println!("  Choix : {}", list.join(", "));
        }
        let multi = q["list"].as_bool() == Some(true);
        if multi {
            println!("  Une réponse par ligne, ligne vide pour finir.");
        }
        print!("› ");
        let mut answer = String::new();
        loop {
            let Some(line) = read().await? else {
                return Ok(());
            };
            if line.trim() == "q" && answer.is_empty() {
                println!("Accueil en pause : `penelope onboard` reprend ici.");
                return Ok(());
            }
            if line.trim().is_empty() {
                break;
            }
            answer.push_str(line.trim());
            answer.push('\n');
            if !multi {
                break;
            }
            print!("› ");
        }
        let answer = answer.trim();
        let params = json!({
            "rel": q["rel"],
            "n": q["n"],
            "answer": (!answer.is_empty()).then_some(answer),
        });
        if let Err(e) = call(&socket, m::ONBOARD_ANSWER, params).await {
            println!("⚠️ {e}");
        }
    }
}

/// Connexion d'un fournisseur à compte (issue #142) : Pénélope demande un code
/// d'appareil, l'affiche avec l'adresse à ouvrir, puis attend que le propriétaire l'ait
/// saisi. Le code ne vaut que quinze minutes.
async fn model_auth(cli: &Cli, provider: String) -> CliResult<()> {
    let socket = socket_path(cli.home.clone())?;
    let start = call(
        &socket,
        m::MODEL_AUTH,
        json!({"provider": provider, "action": "start"}),
    )
    .await?;
    println!(
        "🔐 Ouvrir {}
   et saisir le code : {}
",
        start["url"].as_str().unwrap_or_default(),
        start["user_code"].as_str().unwrap_or_default()
    );
    println!("J'attends la validation (quinze minutes)…");
    // L'attente dure autant que le propriétaire : pas de délai côté client.
    crate::client::set_timeout(Some(0));
    let done = call(
        &socket,
        m::MODEL_AUTH,
        json!({"provider": provider, "action": "wait"}),
    )
    .await?;
    println!(
        "✅ Connecté : plan {}, compte {}",
        done["plan"].as_str().unwrap_or("?"),
        done["account"].as_str().unwrap_or("?")
    );
    println!(
        "Le fournisseur `{provider}` est actif. Pour lui donner un alias :\n  \
         penelope model set code codex:gpt-6-astra"
    );
    Ok(())
}

async fn chat(cli: &Cli, session: Option<String>, message: Vec<String>) -> CliResult<()> {
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let socket = socket_path(cli.home.clone())?;
    if !message.is_empty() {
        let text = message.join(" ");
        return chat_turn(&socket, &text, session.as_deref(), false).await;
    }

    println!(
        "Pénélope : conversation (Ctrl-D pour quitter, /new pour une nouvelle session, /stop pour arrêter)"
    );
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut session = session;
    loop {
        print!("\n› ");
        let _ = std::io::stdout().flush();
        let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| CliError::Io(e.to_string()))?
        else {
            println!();
            break;
        };
        let line = line.trim();
        match line {
            "" => continue,
            "/quit" | "/exit" => break,
            "/new" => {
                let v = call(&socket, m::SESSION_NEW, json!({"title": "CLI"})).await?;
                let id = v["id"].as_str().unwrap_or_default().to_string();
                call(&socket, m::SESSION_SWITCH, json!({"session": id})).await?;
                println!("nouvelle session {id}");
                session = Some(id);
                continue;
            }
            "/stop" => {
                call(&socket, m::CHAT_STOP, json!({"session": session})).await?;
                continue;
            }
            _ => {}
        }
        if let Err(e) = chat_turn(&socket, line, session.as_deref(), true).await {
            eprintln!("erreur : {e}");
            if let Some(h) = e.hint() {
                eprintln!("→ {h}");
            }
        }
    }
    Ok(())
}

/// Un tour, affiché au fil de l'eau. En mode interactif, une approbation est demandée
/// sur place, puis la suite du tour est suivie jusqu'à sa fin.
async fn chat_turn(
    socket: &std::path::Path,
    text: &str,
    session: Option<&str>,
    interactive: bool,
) -> CliResult<()> {
    use std::io::Write;

    let mut streamed = false;
    let mut on_event = |ev: &Value| match ev["type"].as_str() {
        Some("delta") => {
            print!("{}", ev["text"].as_str().unwrap_or(""));
            let _ = std::io::stdout().flush();
            streamed = true;
        }
        Some("tool_call") => {
            eprintln!("\n⚙️  {}", ev["name"].as_str().unwrap_or("?"));
        }
        Some("tool_result") if ev["ok"] == false => {
            eprintln!("   ✗ {}", ev["preview"].as_str().unwrap_or(""));
        }
        _ => {}
    };
    let stream = crate::client::call_stream(
        socket,
        m::CHAT_STREAM,
        json!({"text": text, "session": session}),
        &mut on_event,
    );
    // Ctrl-C arrête le tour côté daemon, pas seulement l'affichage (issue #100) ; un
    // second Ctrl-C quitte sans attendre la confirmation.
    let result = tokio::select! {
        r = stream => r?,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\n⏹ arrêt demandé");
            tokio::select! {
                _ = call(socket, m::CHAT_STOP, json!({"session": session})) => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return Err(CliError::Interrupted);
        }
    };
    let session_id = result["session"].as_str().unwrap_or_default().to_string();
    finish_turn(socket, &result, streamed, &session_id, interactive).await
}

async fn finish_turn(
    socket: &std::path::Path,
    result: &Value,
    streamed: bool,
    session_id: &str,
    interactive: bool,
) -> CliResult<()> {
    match result["outcome"].as_str().unwrap_or("") {
        "answered" => {
            if streamed {
                println!();
            } else {
                println!("{}", result["text"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        "awaiting_approval" => {
            let id = result["approval_id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let pending = call(socket, m::APPROVALS, json!({})).await?;
            let detail = pending
                .as_array()
                .and_then(|a| a.iter().find(|x| x["id"] == id.as_str()))
                .cloned()
                .unwrap_or(Value::Null);
            println!(
                "\n⚠️  approbation requise : {} (risque {})",
                detail["subject"].as_str().unwrap_or("?"),
                detail["risk"].as_str().unwrap_or("?")
            );
            if !detail["payload"]["arguments"].is_null() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&detail["payload"]["arguments"])
                        .unwrap_or_default()
                );
            }
            // « Toujours » sur une commande composée n'écrit aucune règle : le dire
            // avant le clic, comme la carte Telegram (issue #141).
            let no_rule = penelope_agent::always_creates_no_rule(
                detail["subject"].as_str().unwrap_or_default(),
                detail["payload"].get("arguments"),
            );
            if no_rule {
                println!(
                    "ℹ️  commande composée : « toujours » l'autorise cette fois, sans créer \
                     de règle."
                );
            }
            if !interactive {
                println!("→ penelope approve {id}   ou   penelope deny {id}");
                return Ok(());
            }
            print!("Autoriser ? [o]ui / [n]on / [t]oujours : ");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            let (method, params) = match answer.trim().to_lowercase().as_str() {
                "o" | "oui" | "y" | "yes" => (m::APPROVE, json!({"id": id})),
                "t" | "toujours" | "a" | "always" => {
                    (m::APPROVE, json!({"id": id, "always": true}))
                }
                _ => (m::DENY, json!({"id": id})),
            };
            // Suivre la suite du tour avant de trancher, pour ne rien manquer.
            follow_session(socket, session_id, method, params).await
        }
        "failed" => {
            eprintln!("\n❌ {}", result["error"].as_str().unwrap_or("échec"));
            Ok(())
        }
        "cancelled" => {
            eprintln!("\n⏹ arrêté");
            Ok(())
        }
        "loop_aborted" => {
            eprintln!("\n⛔ boucle détectée, tour arrêté");
            Ok(())
        }
        "budget_exceeded" => {
            eprintln!(
                "\n💸 budget `{}` atteint",
                result["scope"].as_str().unwrap_or("?")
            );
            Ok(())
        }
        other => {
            eprintln!("\nissue inattendue : {other}");
            Ok(())
        }
    }
}

/// Tranche une approbation puis affiche la suite du tour de la session.
async fn follow_session(
    socket: &std::path::Path,
    session_id: &str,
    method: &str,
    params: Value,
) -> CliResult<()> {
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = penelope_platform::ipc::connect(socket)
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;
    let (read, mut write) = stream.into_split();
    let mut body = serde_json::to_string(&crate::client::request(socket, m::TAIL, json!({})))
        .unwrap_or_default();
    body.push('\n');
    write
        .write_all(body.as_bytes())
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;

    call(socket, method, params).await?;
    if method == m::DENY {
        println!("refusé ; le modèle en est informé.");
    }

    let mut lines = BufReader::new(read).lines();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1800);
    let mut streamed = false;
    loop {
        let next = tokio::time::timeout_at(deadline, lines.next_line()).await;
        let Ok(Ok(Some(line))) = next else { break };
        let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let ev = &v["params"];
        let mine = ev["session_id"].as_str() == Some(session_id);
        match ev["type"].as_str() {
            Some("delta") if mine => {
                print!("{}", ev["text"].as_str().unwrap_or(""));
                let _ = std::io::stdout().flush();
                streamed = true;
            }
            Some("tool_call") if mine => eprintln!("\n⚙️  {}", ev["name"].as_str().unwrap_or("?")),
            Some("done") if mine => {
                if !streamed {
                    println!("{}", ev["text"].as_str().unwrap_or(""));
                } else {
                    println!();
                }
                break;
            }
            Some("error") if mine => {
                eprintln!("\n❌ {}", ev["message"].as_str().unwrap_or(""));
                break;
            }
            Some("approval") => {
                println!(
                    "\n⚠️  nouvelle approbation requise : {} → penelope approve {}",
                    ev["subject"].as_str().unwrap_or("?"),
                    ev["id"].as_str().unwrap_or("?")
                );
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

mod approval_stats;
mod cli;
mod logs;
mod purge;
mod render;
#[cfg(test)]
mod tests;
