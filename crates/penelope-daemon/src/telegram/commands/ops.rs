//! Commandes d'exploitation : `/upgrade`, `/stop`, `/schedules`, `/mcp`, `/secret`,
//! `/approvals`, `/logs`, `/restart`.

use super::*;

impl TelegramGateway {
    /// `/upgrade`.
    pub(super) async fn cmd_upgrade(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let reply_to = Some(message_id);
        // Installer ou revenir en arrière : toujours confirmé (issue #30).
        // Installation depuis les sources : la carte de bascule (issue #33).
        if args == "install"
            && crate::helpers::running_binary().is_ok_and(|b| crate::helpers::is_source_build(&b))
        {
            return self
                .show_screen(
                    chat_id,
                    topic_id,
                    reply_to,
                    "upgrade.switch",
                    &json!({}),
                    None,
                )
                .await;
        }
        let confirm = match args {
            "install" => Some((
                "upgrade.install",
                json!({}),
                "Installer la dernière version publiée puis redémarrer ?".to_string(),
            )),
            "rollback" => Some((
                "upgrade.rollback",
                json!({}),
                "Revenir au binaire précédent puis redémarrer ?".to_string(),
            )),
            tag if tag.starts_with('v') && !tag.contains(char::is_whitespace) => Some((
                "upgrade.install",
                json!({"tag": tag}),
                format!("Installer {tag} puis redémarrer ?"),
            )),
            _ => None,
        };
        let args = match confirm {
            Some((op, params, question)) => json!({
                "op": op, "params": params, "question": question,
                "back": {"screen": "upgrade", "args": {}},
            }),
            None => {
                // L'écran s'affiche tout de suite ; la vérification suit en fond.
                let daemon = d.clone();
                tokio::spawn(async move {
                    let rpc = crate::rpc::Rpc::new(daemon.clone());
                    if let Ok(v) = rpc.call(m::UPGRADE, json!({"check": true})).await {
                        let cached = json!({"latest": v["latest"], "up_to_date": v["up_to_date"]});
                        let _ = daemon
                            .services
                            .kv_set("tg.upgrade.last_check", &cached.to_string())
                            .await;
                    }
                });
                return self
                    .show_screen(chat_id, topic_id, reply_to, "upgrade", &json!({}), None)
                    .await;
            }
        };
        return self
            .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
            .await;
    }

    /// `/stop`.
    pub(super) async fn cmd_stop(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let reply_to = Some(message_id);
        let text: String = {
            // Arrêter, c'est aussi vider la file : sinon le flot reprend aussitôt
            // (issue #49).
            let session = d.chat_session_for(&origin).await?;
            let tout = matches!(args.trim(), "tout" | "all");
            let burst = self
                .bursts
                .lock()
                .ok()
                .and_then(|mut g| g.remove(&chat_id))
                .map(|b| b.parts.len())
                .unwrap_or(0);
            let running = d.bus.cancel_session(&session);
            // Ingestions en cours de cette session : comptées pour toute forme de
            // `/stop`, annulées par `/stop tout` (issue #155).
            let mut ingests = d.bus.ingests_of(&session);
            let mut cancelled_ingests = if tout {
                d.bus.cancel_ingests(&session)
            } else {
                0
            };
            // Un job d'outil tourne hors du tour : `cancel_session` ne le voit pas.
            // `/stop` coupe ceux de cette session (issue #204).
            let mut cancelled_jobs = s.jobs.cancel_session(&session);
            let mut queued =
                crate::session_ops::silence(&d.services, &d.bus, &session, "arrêt demandé").await?;
            let mut sessions = 0;
            let mut runs = 0;
            // Les runs ouverts de ce chat, quel que soit leur état : un run `blocked`
            // paraît « en cours » au propriétaire, et c'est précisément celui que
            // `/stop tout` ignorait en répondant « Rien à arrêter » (issue #155).
            let open: Vec<penelope_workflow::runs::Run> = s
                .runs
                .list(None, 50)
                .await?
                .into_iter()
                .filter(|r| {
                    matches!(
                        r.state,
                        penelope_workflow::RunState::Running
                            | penelope_workflow::RunState::Blocked
                            | penelope_workflow::RunState::Paused
                    )
                })
                .collect();
            let mut left: Vec<String> = Vec::new();
            if tout {
                // Sessions du chat, **et** sous-agents dont le parent est dans ce chat :
                // une session de sous-agent n'a pas de chat Telegram, elle était sautée.
                let all = s.sessions.list(None, 200).await?;
                let here: std::collections::HashSet<String> = all
                    .iter()
                    .filter(|x| x.tg_chat_id == Some(chat_id))
                    .map(|x| x.id.to_string())
                    .collect();
                for other in &all {
                    let id = other.id.to_string();
                    if id == session {
                        continue;
                    }
                    let mine = other.tg_chat_id == Some(chat_id)
                        || other.parent_id.as_ref().is_some_and(|p| here.contains(p));
                    if !mine {
                        continue;
                    }
                    d.bus.cancel_session(&id);
                    ingests += d.bus.ingests_of(&id);
                    cancelled_ingests += d.bus.cancel_ingests(&id);
                    cancelled_jobs += s.jobs.cancel_session(&id);
                    let n = crate::session_ops::silence(&d.services, &d.bus, &id, "arrêt demandé")
                        .await?;
                    if n > 0 || d.bus.is_active(&id) {
                        sessions += 1;
                    }
                    queued += n;
                }
                for run in &open {
                    if run.state == penelope_workflow::RunState::Running {
                        if crate::workflow::control(d, &run.id, &penelope_workflow::Control::Pause)
                            .await
                            .is_ok()
                        {
                            runs += 1;
                        }
                    } else {
                        // Ni mis en pause (il l'est déjà ou il attend), ni annulé à la
                        // place du propriétaire : nommé, avec de quoi décider.
                        left.push(format!("`{}` ({})", run.id, run.state.as_str()));
                    }
                }
            }
            let has_left = !left.is_empty();
            let note = StopReport {
                running,
                queued,
                burst,
                sessions,
                paused: runs,
                left,
                open: open
                    .iter()
                    .map(|r| (r.id.clone(), r.state.as_str().to_string()))
                    .collect(),
                ingests,
                cancelled_ingests,
                cancelled_jobs,
                tout,
            }
            .render();
            // Des runs sont restés ouverts : l'écran `runs` porte un bouton par run
            // (⏸ ▶️ ⏹, l'arrêt sous confirmation). « Laisser », c'est ne pas cliquer.
            // Le propriétaire décide, la commande ne décide pas pour lui (issue #155).
            if tout && has_left {
                let _ = self.reply(chat_id, topic_id, reply_to, &note).await;
                return self
                    .show_screen(chat_id, topic_id, reply_to, "runs", &json!({}), None)
                    .await;
            }
            note
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/schedules`.
    pub(super) async fn cmd_schedules(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let rpc = crate::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            let parts: Vec<&str> = args.split_whitespace().collect();
            let by_id = |method: &'static str, id: &str| rpc.call(method, json!({"id": id}));
            match parts.as_slice() {
                ["rm", id] => {
                    let args = json!({
                        "op": "schedule.rm", "params": {"id": id},
                        "question": format!("Supprimer le déclencheur `{id}` ?"),
                        "back": {"screen": "schedules", "args": {}},
                    });
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                        .await;
                }
                // Livrer ici, dans cette conversation et ce sujet (#124).
                ["ici" | "here", id] => {
                    match crate::scheduler::retarget(&self.daemon.services, id, chat_id, topic_id)
                        .await
                    {
                        Ok(to) => format!("📍 `{id}` livrera désormais ici : {to}."),
                        Err(e) => format!("❌ {e}"),
                    }
                }
                [op @ ("pause" | "resume" | "run"), id] => {
                    let method = match *op {
                        "pause" => m::SCHEDULE_PAUSE,
                        "resume" => m::SCHEDULE_RESUME,
                        "rm" => m::SCHEDULE_RM,
                        _ => m::SCHEDULE_RUN_NOW,
                    };
                    match by_id(method, id).await {
                        Ok(_) => match *op {
                            "pause" => format!("⏸ `{id}` en pause."),
                            "resume" => format!("▶️ `{id}` repris."),
                            "rm" => format!("🗑 `{id}` supprimé."),
                            _ => format!("⚡ `{id}` déclenché."),
                        },
                        Err(e) => format!("❌ {e}"),
                    }
                }
                _ => {
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "schedules", &json!({}), None)
                        .await;
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/mcp`.
    pub(super) async fn cmd_mcp(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let rpc = crate::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            let parts: Vec<&str> = args.split_whitespace().collect();
            let call = |method: &'static str, name: &str| rpc.call(method, json!({"name": name}));
            match parts.as_slice() {
                [] => {
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "mcp", &json!({}), None)
                        .await;
                }
                ["auth", name] => {
                    return self.send_oauth_card(chat_id, topic_id, name).await;
                }
                ["restart", name] => match call(m::MCP_RESTART, name).await {
                    Ok(v) => format!(
                        "🔄 `{name}` redémarré : {} outil(s), état {}.",
                        shown(&v["tool_count"]),
                        v["state"].as_str().unwrap_or("?")
                    ),
                    Err(e) => format!("❌ {e}"),
                },
                ["logs", name] => match call(m::MCP_LOGS, name).await {
                    Ok(v) => {
                        let lines: Vec<String> = v["lines"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .iter()
                            .rev()
                            .take(30)
                            .rev()
                            .filter_map(|l| l.as_str().map(String::from))
                            .collect();
                        if lines.is_empty() {
                            format!("Aucune ligne de journal pour `{name}`.")
                        } else {
                            format!("```\n{}\n```", lines.join("\n").replace("```", "ʼʼʼ"))
                        }
                    }
                    Err(e) => format!("❌ {e}"),
                },
                ["test", name] => match call(m::MCP_TEST, name).await {
                    Ok(v) if v["ok"].as_bool() == Some(true) => format!(
                        "✅ `{name}` répond : protocole {}, {} outil(s){}, {} ms.",
                        v["protocol"].as_str().unwrap_or("?"),
                        shown(&v["tools"]),
                        match v["call"]["tool"].as_str() {
                            Some(t) => format!(", appel de `{t}` réussi"),
                            None => ", aucun outil en lecture sans argument à essayer".into(),
                        },
                        shown(&v["ms"])
                    ),
                    Ok(v) => {
                        format!("❌ `{name}` : {}", v["error"].as_str().unwrap_or("échec"))
                    }
                    Err(e) => format!("❌ {e}"),
                },
                [op @ ("enable" | "disable"), name] => {
                    let method = if *op == "enable" {
                        m::MCP_ENABLE
                    } else {
                        m::MCP_DISABLE
                    };
                    match call(method, name).await {
                        Ok(_) if *op == "enable" => format!("▶️ `{name}` activé."),
                        Ok(_) => format!("⏸ `{name}` désactivé."),
                        Err(e) => format!("❌ {e}"),
                    }
                }
                [name] => {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "mcp.server",
                            &json!({"name": name}),
                            None,
                        )
                        .await;
                }
                _ => {
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "mcp", &json!({}), None)
                        .await;
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/secret`.
    pub(super) async fn cmd_secret(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let text: String = {
            let parts: Vec<&str> = args.split_whitespace().collect();
            match parts.first().copied() {
                None | Some("list") => {
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "secrets", &json!({}), None)
                        .await;
                }
                Some("rm") if parts.len() > 1 => {
                    let confirm = json!({
                        "op": "secret.rm", "params": {"name": parts[1]},
                        "question": format!("Supprimer le secret `{}` ?", parts[1]),
                        "back": {"screen": "secrets", "args": {}},
                    });
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                        .await;
                }
                _ => "Un secret ne se saisit **jamais** dans une conversation. En SSH : \
                      `penelope secret set <nom>` puis coller la valeur à l'invite"
                    .into(),
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/approvals`.
    pub(super) async fn cmd_approvals(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        _args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        let text: String = {
            let pending = s.approvals.pending(20).await?;
            if pending.is_empty() {
                "Aucune demande en attente.".into()
            } else {
                for a in &pending {
                    self.send_approval_card(chat_id, topic_id, a).await?;
                }
                format!("{} demande(s) en attente.", pending.len())
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/logs`.
    pub(super) async fn cmd_logs(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let component = args.split_whitespace().next().unwrap_or_default();
        return self
            .show_screen(
                chat_id,
                topic_id,
                reply_to,
                "logs",
                &json!({"component": component}),
                None,
            )
            .await;
    }

    /// `/restart`.
    pub(super) async fn cmd_restart(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        _args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let confirm = json!({
            "op": "restart", "params": {},
            "question": "Redémarrer le daemon ? Les tours en cours reprennent au redémarrage.",
            "back": null,
        });
        return self
            .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
            .await;
    }
}
