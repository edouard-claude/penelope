//! Commandes du cadre de travail : `/home`, `/projet`, `/mode`, `/quiet`, `/note`, `/mien`,
//! `/p`.

use super::*;

impl TelegramGateway {
    /// Foyer du propriétaire (issue #143) : ce qui n'appartient à aucune session
    /// — alertes de budget, rappels, digest du rêve, cartes OAuth — arrive ici
    /// plutôt que dans un chat privé que plus personne ne lit.
    pub(super) async fn cmd_home(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            let reset = matches!(args.trim(), "off" | "non" | "privé" | "prive");
            let (chat, topic) = if reset {
                (0, 0)
            } else {
                (chat_id, topic_id.unwrap_or(0))
            };
            match rpc
                .call(
                    m::CONFIG_SET,
                    json!({"path": "telegram.home", "value": {"chat": chat, "topic": topic}}),
                )
                .await
            {
                Err(e) => format!("❌ {e}"),
                Ok(_) if reset => "🏠 Foyer effacé : les avis sans session repartent \
                                   dans le chat privé."
                    .into(),
                Ok(_) => format!(
                    "🏠 Foyer réglé sur ce {}. Les avis sans session (budget, rappels, \
                     digest, cartes MCP) arriveront ici. `/home off` pour revenir au \
                     chat privé.",
                    if topic_id.is_some() { "sujet" } else { "chat" }
                ),
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// Sujet de travail de la session (issue #119) : sans argument, l'état et un
    /// bouton par projet connu.
    pub(super) async fn cmd_projet(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            let session = d.chat_session_for(&origin).await?;
            let wanted = args.trim();
            match rpc
                .call(
                    m::SESSION_PROJECT,
                    json!({"session": session, "project": wanted}),
                )
                .await
            {
                Err(e) => format!("❌ {e}"),
                Ok(v) if !wanted.is_empty() => match v["project"].as_str() {
                    Some(p) => format!(
                        "📁 Sujet de la session : **{p}**. La mémoire d'office s'y limite dès \
                         le prochain message ; le reste reste au rappel."
                    ),
                    None => "📁 Session sans sujet : seules les entrées sans projet sont \
                             injectées d'office."
                        .into(),
                },
                Ok(v) => {
                    let current = v["project"].as_str();
                    let mut rows = Vec::new();
                    for p in v["known"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .take(12)
                    {
                        let p = p.as_str().unwrap_or_default();
                        let mark = if Some(p) == current { "✅ " } else { "" };
                        rows.push(vec![
                            self.command_button_with(&format!("{mark}{p}"), "projet", p)
                                .await?,
                        ]);
                    }
                    rows.push(vec![
                        self.command_button_with(
                            if current.is_none() {
                                "✅ Aucun"
                            } else {
                                "Aucun"
                            },
                            "projet",
                            "aucun",
                        )
                        .await?,
                    ]);
                    let state = match (current, v["how"].as_str()) {
                        (Some(p), Some("explicite")) => format!("**{p}** (choisi)"),
                        (Some(p), Some(how)) => format!("**{p}** (déduit du {how})"),
                        (Some(p), None) => format!("**{p}**"),
                        (None, Some(_)) => "aucun (choisi)".into(),
                        (None, None) => "aucun pour l'instant".into(),
                    };
                    let text = format!(
                        "📁 Sujet de la session : {state}.\n\nLe profil et les entrées sans \
                         projet sont toujours là ; celles d'un projet n'entrent d'office que \
                         dans une session de ce projet. Les autres restent au rappel et à \
                         `mem_search`."
                    );
                    let mut payload = json!({
                        "chat_id": chat_id,
                        "text": markdown_to_html(&text),
                        "parse_mode": "HTML",
                        "reply_markup": inline_keyboard(&rows),
                        "message_thread_id": topic_id,
                    });
                    if let Some(r) = reply_to {
                        payload["reply_parameters"] =
                            json!({"message_id": r, "allow_sending_without_reply": true});
                    }
                    return self
                        .outbox_push(chat_id, topic_id, "sendMessage", payload)
                        .await;
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// Mode d'approbation de la session (issue #111) : sans argument, l'état et un
    /// bouton par mode.
    pub(super) async fn cmd_mode(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            let session = d.chat_session_for(&origin).await?;
            let wanted = args.trim();
            let v = rpc
                .call(m::SESSION_MODE, json!({"session": session, "mode": wanted}))
                .await;
            match v {
                Err(e) => format!("❌ {e}"),
                Ok(v) if !wanted.is_empty() => {
                    format!(
                        "🛡 Mode de la session : **{}**.",
                        v["label"].as_str().unwrap_or("?")
                    )
                }
                Ok(v) => {
                    let current = v["mode"].as_str().unwrap_or("reads");
                    let mut rows = Vec::new();
                    for (mode, label) in [
                        ("ask", "Demander tout"),
                        ("reads", "Lectures sans demande"),
                        ("auto", "Tout sauf le destructif"),
                    ] {
                        let mark = if mode == current { "✅ " } else { "" };
                        rows.push(vec![
                            self.command_button_with(&format!("{mark}{label}"), "mode", mode)
                                .await?,
                        ]);
                    }
                    let text = format!(
                        "🛡 Mode de la session : **{}**.\n\nDemander tout : même une lecture \
                         du shell attend ton accord. Lectures sans demande (défaut) : `ls`, \
                         `cat`, `grep`, `git status` passent, le reste selon tes règles. \
                         Tout sauf le destructif : plus de demande, sauf suppression et \
                         réglages sensibles.",
                        v["label"].as_str().unwrap_or("?")
                    );
                    let mut payload = json!({
                        "chat_id": chat_id,
                        "text": markdown_to_html(&text),
                        "parse_mode": "HTML",
                        "reply_markup": inline_keyboard(&rows),
                        "message_thread_id": topic_id,
                    });
                    if let Some(r) = reply_to {
                        payload["reply_parameters"] =
                            json!({"message_id": r, "allow_sending_without_reply": true});
                    }
                    return self
                        .outbox_push(chat_id, topic_id, "sendMessage", payload)
                        .await;
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/note`.
    pub(super) async fn cmd_note(
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
            if args.is_empty() {
                return self
                    .send_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        Self::typed_screen(
                            "📝 **Noter** : `/note` suivi du texte, rangé dans le journal du jour.",
                            "✏️ Écrire",
                            "/note ",
                        ),
                        None,
                    )
                    .await;
            }
            let vault = penelope_app::helpers::vault_dir(s);
            let session = d.chat_session_for(&origin).await?;
            match penelope_vault::vault_ops::remember(
                s,
                &vault,
                penelope_memory::Level::Episodic,
                args,
                &session,
            )
            .await
            {
                Ok(_) => "📝 Noté dans le journal du jour.".into(),
                Err(e) => format!("❌ {e}"),
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/mien`.
    pub(super) async fn cmd_mien(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        _args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let text: String =
            "📄 Envoie un document avec la légende `/mien` : il est ingéré comme rédigé \
               par toi, donc fiable et rappelable. Sans légende, un document reçu reste \
               une source non fiable."
                .into();
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/p`.
    pub(super) async fn cmd_p(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let text: String = {
            let mut words = args.splitn(3, char::is_whitespace);
            match (words.next().filter(|w| !w.is_empty()), words.next()) {
                (None, _) => {
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "prompts", &json!({}), None)
                        .await;
                }
                (Some(server), None) => {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "prompts",
                            &json!({"server": server}),
                            None,
                        )
                        .await;
                }
                (Some(server), Some(prompt)) => {
                    let params = parse_params(words.next().unwrap_or_default());
                    match self
                        .run_mcp_prompt(chat_id, topic_id, server, prompt, params)
                        .await
                    {
                        Ok(n) => format!(
                            "💬 Prompt `{prompt}` de `{server}` : {n} message(s) envoyé(s) au modèle."
                        ),
                        Err(e) => format!("❌ {e}"),
                    }
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/quiet`.
    pub(super) async fn cmd_quiet(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            if args.is_empty() {
                return self
                    .show_screen(chat_id, topic_id, reply_to, "quiet", &json!({}), None)
                    .await;
            }
            let range = if matches!(args, "off" | "non" | "aucune") {
                ""
            } else {
                args
            };
            match rpc.call(m::QUIET, json!({"range": range})).await {
                Ok(_) if range.is_empty() => "🔔 Heures silencieuses désactivées.".into(),
                Ok(_) => format!("🌙 Heures silencieuses : {range}."),
                Err(e) => format!("❌ {e}"),
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }
}
