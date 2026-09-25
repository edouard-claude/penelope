//! Commandes de cycle de vie des sessions : `/start`, `/new`, `/title`, `/sessions`,
//! `/compact`, `/fork`, `/rewind`, `/export`, `/switch`, `/close`, `/purge`.

use super::*;

impl TelegramGateway {
    /// `/start` et `/help`.
    pub(super) async fn cmd_start(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        // `/start <charge>` : lien profond vers un écran (issue #30).
        if !args.is_empty() {
            return self
                .open_deep_link(chat_id, topic_id, message_id, args)
                .await;
        }
        return self
            .show_screen(chat_id, topic_id, reply_to, "help", &json!({}), None)
            .await;
    }

    /// `/new`.
    pub(super) async fn cmd_new(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        let text: String = {
            // `/new !titre` ferme l'ancienne session sans redemander, `/new ~titre` la
            // garde en fond : ce sont les deux boutons de la question ci-dessous.
            let (choice, args) = match args.trim_start().chars().next() {
                Some('!') => (Some(true), args.trim_start()[1..].trim()),
                Some('~') => (Some(false), args.trim_start()[1..].trim()),
                _ => (None, args),
            };
            if let Some(old) = s.sessions.find_by_topic(chat_id, topic_id).await? {
                let queued = s.turns.queued_for(old.id.as_str()).await?;
                // Une session qui travaille encore : dire ce que `/new` lui ferait,
                // avant de le faire (issue #112).
                if choice.is_none() && queued > 0 {
                    let rows = vec![
                        vec![
                            self.command_button_with(
                                "⏳ Garder l'ancienne en fond",
                                "new",
                                &format!("~{args}"),
                            )
                            .await?,
                        ],
                        vec![
                            self.command_button_with(
                                &format!("🗑 Fermer ({queued} tour(s) perdu(s))"),
                                "new",
                                &format!("!{args}"),
                            )
                            .await?,
                        ],
                    ];
                    let text = format!(
                        "La session « {} » a {queued} tour(s) en file ou en cours. \
                         `/new` la ferme et **ils seront perdus**. La garder en fond : \
                         elle finit son travail et ses réponses t'attendent à ton retour \
                         (`/sessions`).",
                        crate::titles::label(&old)
                    );
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            screens::Screen { text, rows },
                            None,
                        )
                        .await;
                }
                if choice != Some(false) {
                    crate::session_ops::silence(
                        &d.services,
                        &d.bus,
                        old.id.as_str(),
                        "nouvelle session",
                    )
                    .await?;
                    s.sessions.set_state(old.id.as_str(), "closed").await?;
                    // `/new` clôt aussi l'épisode en cours : il est relu (§6.6).
                    crate::episodes::spawn_ingest(
                        d.services.clone(),
                        d.providers.clone(),
                        old.id.to_string(),
                        old.episode_seq,
                        crate::episodes::Boundary::NewSession,
                    );
                }
            }
            let title = (!args.is_empty())
                .then(|| crate::titles::clean(args))
                .flatten();
            let sess = s
                .sessions
                .create(penelope_kernel::session::SessionKind::Chat, title.clone())
                .await?;
            s.sessions
                .bind_telegram(sess.id.as_str(), chat_id, topic_id)
                .await?;
            if let Some(title) = title {
                // Notes d'une session sur le même sujet : proposées (issue #32).
                let similar = crate::session_notes::similar(s, sess.id.as_str(), &title).await;
                let text = format!("🆕 Nouvelle session « {title} » (`{}`).", sess.id);
                if similar.is_empty() {
                    text
                } else {
                    let mut rows = Vec::new();
                    for (from, label) in &similar {
                        let t = s
                            .actions
                            .create(
                                k::SCREEN_DO,
                                "notes.adopt",
                                json!({"params": {"from": from, "to": sess.id.to_string()}, "back": null}),
                                24 * 3_600_000,
                                true,
                            )
                            .await?;
                        rows.push(vec![ButtonSpec::callback(
                            &format!(
                                "📓 Reprendre les notes de « {} »",
                                label.chars().take(40).collect::<String>()
                            ),
                            &t.token,
                            "",
                        )]);
                    }
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            screens::Screen {
                                text: format!(
                                    "{text}\nDes notes de travail existent sur un sujet proche."
                                ),
                                rows,
                            },
                            None,
                        )
                        .await;
                }
            } else {
                // Sans titre : le message sera complété quand le titre automatique arrive.
                let text = format!(
                    "🆕 Nouvelle session `{}`. Son titre suivra le premier échange.",
                    sess.id
                );
                let sent = self
                    .bot
                    .send_text(
                        chat_id,
                        topic_id,
                        &markdown_to_html(&text),
                        None,
                        Some(message_id),
                    )
                    .await;
                match sent
                    .ok()
                    .and_then(|v| v.get("message_id").and_then(|m| m.as_i64()))
                {
                    Some(mid) => {
                        s.kv_set(
                            &format!("tg.new_session.{}", sess.id),
                            &format!("{chat_id}:{mid}"),
                        )
                        .await?;
                        return Ok(());
                    }
                    None => text,
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/title`.
    pub(super) async fn cmd_title(
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
            let session = d.chat_session_for(&origin).await?;
            match crate::titles::clean(args) {
                None => {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "✏️ Nouveau titre de la session : `/title` suivi du titre.",
                                "✏️ Écrire le titre",
                                "/title ",
                            ),
                            None,
                        )
                        .await;
                }
                Some(title) => {
                    s.sessions.set_title(&session, &title, false).await?;
                    format!("✏️ Session renommée : « {title} ».")
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/sessions`.
    pub(super) async fn cmd_sessions(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        _message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let all = matches!(args, "all" | "toutes" | "tout");
        return self
            .send_sessions_menu(chat_id, topic_id, 0, all, None)
            .await;
    }

    /// `/compact`.
    pub(super) async fn cmd_compact(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        _args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        // Un résumé prend de quelques secondes à une minute : la file des updates
        // n'attend pas, le bilan arrive en réponse quand il est prêt.
        let session = d.chat_session_for(&origin).await?;
        self.react(chat_id, message_id, reaction::RECEIVED);
        let (daemon, messenger) = (d.clone(), d.hooks.messenger());
        tokio::spawn(async move {
            let text = match crate::compaction::compact(
                &crate::compaction::context_of(&daemon),
                &session,
                crate::compaction::Trigger::Manual,
                None,
            )
            .await
            {
                Ok(r) => crate::compaction::report_text(&r),
                Err(e) => format!("❌ {e}"),
            };
            if let Some(m) = messenger {
                let _ = m.send_text(&origin, &text).await;
            }
        });
        Ok(())
    }

    /// `/fork`.
    pub(super) async fn cmd_fork(
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
            let session = d.chat_session_for(&origin).await?;
            let title = (!args.is_empty()).then(|| args.to_string());
            match crate::session_ops::fork(&d.services, &session, title).await {
                Ok(v) => {
                    let fork = v["session"].as_str().unwrap_or_default().to_string();
                    let background = self.bind_chat(&fork, chat_id, topic_id).await?;
                    s.sessions.touch(&fork).await?;
                    let back = s
                        .actions
                        .create(
                            k::SESSION_SWITCH,
                            &session,
                            json!({"notice": true}),
                            7 * 24 * 3_600_000,
                            false,
                        )
                        .await?;
                    let screen = screens::Screen {
                        text: format!(
                            "🍴 Session dupliquée ({} messages) : la suite se passe dans \
                             `{fork}`.{}",
                            shown(&v["messages"]),
                            background_note(background)
                        ),
                        rows: vec![vec![ButtonSpec::callback(
                            "↪️ Revenir à l'original",
                            &back.token,
                            "",
                        )]],
                    };
                    return self
                        .send_screen(chat_id, topic_id, reply_to, screen, None)
                        .await;
                }
                Err(e) => format!("❌ {e}"),
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/rewind` sans argument : l'écran de choix.
    pub(super) async fn cmd_rewind_screen(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        _args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        return self
            .show_screen(chat_id, topic_id, reply_to, "rewind", &json!({}), None)
            .await;
    }

    /// `/rewind`.
    pub(super) async fn cmd_rewind(
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
        let reply_to = Some(message_id);
        let text: String = {
            let session = d.chat_session_for(&origin).await?;
            let turns = args.trim().parse::<usize>().unwrap_or(1);
            match crate::session_ops::rewind(&d.services, &d.bus, &session, turns).await {
                Ok(v) => format!(
                    "⏪ {turns} échange(s) défait(s) ({} messages mis de côté dans `{}`).",
                    shown(&v["removed"]),
                    v["archive"].as_str().unwrap_or("?")
                ),
                Err(e) => format!("❌ {e}"),
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/export`.
    pub(super) async fn cmd_export(
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
        let reply_to = Some(message_id);
        let session = if args.is_empty() {
            d.chat_session_for(&origin).await?
        } else {
            args.to_string()
        };
        // Écriture puis téléversement : détachés, la boucle des updates continue
        // de lire `/stop` et les boutons (issue #69).
        let (me, d2) = (self.clone(), d.clone());
        tokio::spawn(async move {
            let note =
                match crate::session_ops::export(&d2.services, "session", Some(&session)).await {
                    Ok(v) => {
                        let path = std::path::PathBuf::from(v["path"].as_str().unwrap_or_default());
                        match me
                            .bot
                            .send_document(chat_id, topic_id, &path, Some("Export JSONL"))
                            .await
                        {
                            Ok(_) => return,
                            Err(e) => format!(
                                "📦 Export écrit dans `{}`, envoi impossible : {e}",
                                path.display()
                            ),
                        }
                    }
                    Err(e) => format!("❌ {e}"),
                };
            let _ = me.reply(chat_id, topic_id, reply_to, &note).await;
        });
        Ok(())
    }

    /// `/switch`.
    pub(super) async fn cmd_switch(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        let text: String = {
            if args.is_empty() {
                return self
                    .send_sessions_menu(chat_id, topic_id, 0, false, None)
                    .await;
            } else {
                match crate::session_ops::resolve(s, args).await {
                    Err(e) => format!("❌ {e}"),
                    Ok(sess) => {
                        let id = sess.id.to_string();
                        if sess.state != "active" {
                            s.sessions.set_state(&id, "active").await?;
                        }
                        let background = self.bind_chat(&id, chat_id, topic_id).await?;
                        s.sessions.touch(&id).await?;
                        format!(
                            "↪️ Session « {} » reprise (`{id}`).{}",
                            crate::titles::label(&sess),
                            background_note(background)
                        )
                    }
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/close`.
    pub(super) async fn cmd_close(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        let text: String = {
            let target = if args.is_empty() {
                s.sessions
                    .find_by_topic(chat_id, topic_id)
                    .await?
                    .map(|x| x.id.to_string())
                    .ok_or_else(|| "aucune session liée à ce chat".to_string())
            } else {
                crate::session_ops::resolve(s, args)
                    .await
                    .map(|x| x.id.to_string())
            };
            match target {
                Err(e) => format!("❌ {e}"),
                Ok(id) => {
                    let label = match s.sessions.get(&id).await? {
                        Some(sess) => format!("« {} »", crate::titles::label(&sess)),
                        None => format!("`{id}`"),
                    };
                    let args = json!({
                        "op": "session.close", "params": {"session": id},
                        "question": format!(
                            "Fermer la session {label} ? Sa file d'attente est vidée."
                        ),
                        "back": null,
                    });
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                        .await;
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/purge`.
    pub(super) async fn cmd_purge(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        let text: String = {
            let target = if args.is_empty() {
                s.sessions
                    .find_by_topic(chat_id, topic_id)
                    .await?
                    .map(|x| x.id.to_string())
                    .ok_or_else(|| "aucune session liée à ce chat".to_string())
            } else {
                crate::session_ops::resolve(s, args)
                    .await
                    .map(|x| x.id.to_string())
            };
            match target {
                Err(e) => format!("❌ {e}"),
                Ok(id) => {
                    let label = match s.sessions.get(&id).await? {
                        Some(sess) => format!("« {} »", crate::titles::label(&sess)),
                        None => format!("`{id}`"),
                    };
                    // Les forks perdent leur début avec la mère (arbitrage 3 de la V1) :
                    // l'écran le dit avant la question.
                    let preview = penelope_daemon::purge::preview(s, &id).await?;
                    let warning = preview["avertissement"]
                        .as_str()
                        .map(|w| format!("⚠️ {w}\n\n"))
                        .unwrap_or_default();
                    let args = json!({
                        "op": "session.purge",
                        "params": {"session": id, "reason": "demande du propriétaire"},
                        "question": format!(
                            "{warning}Effacer le contenu de la session {label} ? Messages, \
                             résumés, artefacts et requêtes partent définitivement ; la \
                             chaîne d'audit garde ses lignes, sans leur contenu."
                        ),
                        "back": null,
                    });
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                        .await;
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }
}
