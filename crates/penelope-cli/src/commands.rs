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
mod offline;
mod purge;
mod render;
mod restore;
mod route;
#[cfg(test)]
mod tests;
mod upgrade;
