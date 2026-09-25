//! Les commandes interactives avec le daemon : `chat` (un tour suivi jusqu'à sa réponse),
//! `onboard` (l'entretien d'accueil) et `model auth` (connexion d'un compte).

use super::*;

/// `penelope chat` : un message, ou une conversation interactive.
/// `penelope onboard` : une question à la fois, réponse vide pour passer, `q` pour
/// reprendre plus tard ; le récapitulatif est validé avant écriture.
pub(super) async fn onboard(cli: &Cli, part: Option<String>) -> CliResult<()> {
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
pub(super) async fn model_auth(cli: &Cli, provider: String) -> CliResult<()> {
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

pub(super) async fn chat(
    cli: &Cli,
    session: Option<String>,
    message: Vec<String>,
) -> CliResult<()> {
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
