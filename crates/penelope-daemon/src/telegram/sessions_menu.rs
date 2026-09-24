//! Menu des sessions et rattachement d'un chat.

use super::*;

impl TelegramGateway {
    /// Menu `/sessions` : un bouton par session (bascule), un « ⋯ » par session (forker,
    /// renommer, fermer), pagination, sessions fermées masquées sauf `all` (issue #14).
    /// `edit` : message à remplacer plutôt qu'un nouvel envoi.
    pub(super) async fn send_sessions_menu(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        page: usize,
        all: bool,
        edit: Option<i64>,
    ) -> anyhow::Result<()> {
        const PER_PAGE: usize = 12;
        let d = &self.daemon;
        let s = &d.services;
        let current = s
            .sessions
            .find_by_topic(chat_id, topic_id)
            .await?
            .map(|x| x.id.to_string());
        let sessions: Vec<_> = s
            .sessions
            .list(Some(penelope_kernel::session::SessionKind::Chat), 1_000)
            .await?
            .into_iter()
            .filter(|x| all || x.state != "closed")
            .collect();
        let busy: std::collections::BTreeMap<String, i64> = s
            .store
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT session_id, COUNT(*) FROM turn_queue
                     WHERE state IN ('pending', 'leased') GROUP BY session_id",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        let pages = sessions.len().div_ceil(PER_PAGE).max(1);
        let page = page.min(pages - 1);
        let ttl = 24 * 3_600_000;
        let make = |label: String, action: &'static str, target: String, args: Value| async move {
            s.actions
                .create(action, &target, args, ttl, true)
                .await
                .map(|t| ButtonSpec::callback(&label, &t.token, ""))
        };
        let nav = json!({"page": page, "all": all});
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for (i, sess) in sessions
            .iter()
            .enumerate()
            .skip(page * PER_PAGE)
            .take(PER_PAGE)
        {
            let id = sess.id.to_string();
            let mut label = String::new();
            if current.as_deref() == Some(id.as_str()) {
                label.push_str("▶️ ");
            } else if sess.state == "closed" {
                label.push_str("🔒 ");
            }
            // Une session qui travaille en fond, avec sa file (issue #112).
            if let Some(n) = busy.get(&id).filter(|n| **n > 0) {
                label.push_str(&format!("⏳{n} "));
            }
            let title = crate::titles::label(sess);
            label.push_str(&title.chars().take(48).collect::<String>());
            // Son sujet de travail, qui filtre sa mémoire d'office (#119).
            if let (Some(p), _) = crate::session_project::of_session(s, &id).await {
                label.push_str(&format!(" · 📁{p}"));
            }
            // La plus récente porte aussi l'heure de sa dernière activité.
            if i == 0
                && let Some(hm) = sess.last_activity.as_deref().and_then(|t| t.get(11..16))
            {
                label.push_str(&format!(" {hm}"));
            }
            rows.push(vec![
                make(label, k::SESSION_SWITCH, id.clone(), nav.clone()).await?,
                make("⋯".into(), k::SESSION_MENU, id, nav.clone()).await?,
            ]);
        }
        let mut footer = Vec::new();
        if page > 0 {
            footer.push(
                make(
                    "« Plus récentes".into(),
                    k::SESSIONS_PAGE,
                    String::new(),
                    json!({"page": page - 1, "all": all}),
                )
                .await?,
            );
        }
        if page + 1 < pages {
            footer.push(
                make(
                    "Plus anciennes »".into(),
                    k::SESSIONS_PAGE,
                    String::new(),
                    json!({"page": page + 1, "all": all}),
                )
                .await?,
            );
        }
        footer.push(
            make(
                if all {
                    "Masquer les fermées".into()
                } else {
                    "Voir les fermées".into()
                },
                k::SESSIONS_PAGE,
                String::new(),
                json!({"page": 0, "all": !all}),
            )
            .await?,
        );
        rows.push(footer);
        let text = if sessions.is_empty() {
            "**Sessions** : aucune.".to_string()
        } else {
            format!(
                "**Sessions** ({} · page {}/{pages})\nUn clic bascule ce chat sur la session ; \
                 « ⋯ » pour forker, renommer ou fermer. ▶️ session de ce chat, ⏳ tour en cours \
                 ou en attente.",
                sessions.len(),
                page + 1
            )
        };
        let html = markdown_to_html(&text);
        let keyboard = inline_keyboard(&rows);
        if let Some(message_id) = edit {
            match self
                .bot
                .edit_text(chat_id, message_id, &html, Some(keyboard.clone()))
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if e.to_string().contains("not modified") => return Ok(()),
                Err(_) => {}
            }
        }
        self.bot
            .send_text(chat_id, topic_id, &html, Some(keyboard), None)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
    /// Boutons du menu `/sessions`.
    pub(super) async fn session_menu_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let page = action.args["page"].as_u64().unwrap_or(0) as usize;
        let all = action.args["all"].as_bool().unwrap_or(false);
        let target = action.target.clone();
        let toast = match action.action.as_str() {
            k::SESSION_SWITCH => {
                let Some(sess) = s.sessions.get(&target).await? else {
                    let _ = self
                        .bot
                        .answer_callback(callback_id, Some("Session introuvable."), false)
                        .await;
                    return self
                        .send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
                        .await;
                };
                if sess.state != "active" {
                    s.sessions.set_state(&target, "active").await?;
                }
                let background = self.bind_chat(&target, chat_id, topic_id).await?;
                s.sessions.touch(&target).await?;
                let mut t = format!("Session « {} »", crate::titles::label(&sess));
                if background > 0 {
                    t.push_str(&format!(
                        " ({background} tour(s) continuent en fond ailleurs)"
                    ));
                }
                // Bouton d'une notification « réponses en attente » : pas de menu à redessiner.
                if action.args["notice"].as_bool() == Some(true) {
                    let _ = self.bot.answer_callback(callback_id, Some(&t), false).await;
                    let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                    return Ok(());
                }
                Some(t)
            }
            k::SESSION_FORK => match crate::session_ops::fork(d, &target, None).await {
                Ok(v) => {
                    let fork = v["session"].as_str().unwrap_or_default().to_string();
                    self.bind_chat(&fork, chat_id, topic_id).await?;
                    s.sessions.touch(&fork).await?;
                    Some("Session dupliquée : la suite se passe dans le fork.".to_string())
                }
                Err(e) => Some(format!("Fork impossible : {e}")),
            },
            k::SESSION_CLOSE => match crate::session_ops::close(d, &target).await {
                Ok(v) => Some(format!(
                    "Session fermée{}",
                    match v["cancelled"].as_u64().unwrap_or(0) {
                        0 => String::new(),
                        n => format!(", {n} en attente annulé(s)"),
                    }
                )),
                Err(e) => Some(format!("Fermeture impossible : {e}")),
            },
            k::SESSION_RENAME => {
                s.kv_set(
                    &format!("tg.await_title.{chat_id}"),
                    &format!("{target} {}", s.clock.now_ms()),
                )
                .await?;
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        "✏️ Envoie le nouveau titre de la session en un message (dans les \
                         5 minutes).",
                    )
                    .await;
            }
            k::SESSION_MENU => {
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                return self
                    .send_session_actions(chat_id, topic_id, message_id, &target, page, all)
                    .await;
            }
            _ => None,
        };
        let _ = self
            .bot
            .answer_callback(callback_id, toast.as_deref(), false)
            .await;
        self.send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
            .await
    }
    /// Sous-menu d'une session : basculer, forker, renommer, fermer, retour.
    async fn send_session_actions(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        session_id: &str,
        page: usize,
        all: bool,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let Some(sess) = s.sessions.get(session_id).await? else {
            return self
                .send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
                .await;
        };
        let ttl = 24 * 3_600_000;
        let nav = json!({"page": page, "all": all});
        let mut rows = Vec::new();
        for pair in [
            [
                ("↪️ Basculer", k::SESSION_SWITCH),
                ("🍴 Forker", k::SESSION_FORK),
            ],
            [
                ("✏️ Renommer", k::SESSION_RENAME),
                ("🔒 Fermer", k::SESSION_CLOSE),
            ],
        ] {
            let mut row = Vec::new();
            for (label, action) in pair {
                let t = s
                    .actions
                    .create(action, session_id, nav.clone(), ttl, true)
                    .await?;
                row.push(ButtonSpec::callback(label, &t.token, ""));
            }
            rows.push(row);
        }
        let back = s
            .actions
            .create(k::SESSIONS_PAGE, "", nav, ttl, true)
            .await?;
        rows.push(vec![ButtonSpec::callback("↩️ Retour", &back.token, "")]);
        let text = format!(
            "**{}**\n`{}` · {}",
            crate::titles::label(&sess),
            sess.id,
            if sess.state == "closed" {
                "fermée"
            } else {
                "active"
            }
        );
        self.bot
            .edit_text(
                chat_id,
                message_id,
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
    /// Lie une session au chat : elle a le focus. Celles qui le perdent finissent leur tour
    /// en cours, dont la sortie est mise de côté, et leur file est annulée ; la session liée
    /// reçoit ce qui l'attendait (issue #10). Renvoie le nombre de tours annulés.
    /// Donne le fil à `session_id`. La session qu'il quitte garde sa file : ses tours
    /// s'exécutent en fond et ce qu'ils produisent est retenu jusqu'au retour, comme la
    /// réponse du tour en vol (issue #112). Renvoie le nombre de tours qu'elle a encore.
    pub(super) async fn bind_chat(
        &self,
        session_id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<usize> {
        let s = &self.daemon.services;
        let mut background = 0;
        for other in s
            .sessions
            .bind_telegram(session_id, chat_id, topic_id)
            .await?
        {
            background += s.turns.queued_for(&other).await? as usize;
        }
        self.flush_held(session_id).await?;
        Ok(background)
    }
}
