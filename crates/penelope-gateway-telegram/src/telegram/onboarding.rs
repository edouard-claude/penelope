//! Accueil : questions du profil au premier contact.

use super::*;

impl TelegramGateway {
    // ============================================================ accueil

    pub(super) async fn propose_onboarding(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<()> {
        let t = self
            .actions
            .create(k::ONBOARD_START, "", json!({}), 7 * 24 * 3_600_000, true)
            .await?;
        let rows = vec![vec![ButtonSpec::callback(
            "📋 Commencer l'accueil",
            &t.token,
            "",
        )]];
        self.bot
            .send_text(
                chat_id,
                topic_id,
                &markdown_to_html(
                    "👋 Ton profil est encore vide. Neuf questions (rôle, projets, outils, style, \
                     limites) et je te connais dès aujourd'hui ; chacune peut être passée.",
                ),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
    /// `/accueil [partie]` : reprend la séance en cours ou en ouvre une (issue #21).
    pub(super) async fn onboarding_next(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        part: Option<penelope_dream::onboarding::Part>,
    ) -> anyhow::Result<()> {
        let sitting = penelope_dream::onboarding::start(&self.daemon.services, part).await?;
        self.onboarding_ask(chat_id, topic_id, &sitting).await
    }
    /// Pose la question suivante, ou montre le récapitulatif à valider.
    pub(super) async fn onboarding_ask(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        sitting: &penelope_dream::onboarding::Sitting,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let key = format!("tg.onboard.{chat_id}");
        let ttl = 7 * 24 * 3_600_000;
        let button = |label: String, action: &'static str, args: Value| {
            let rel = sitting.rel.clone();
            async move {
                self.actions
                    .create(action, &rel, args, ttl, true)
                    .await
                    .map(|t| ButtonSpec::callback(&label, &t.token, ""))
            }
        };
        let Some(q) = sitting.next() else {
            s.kv_set(&key, "").await?;
            let plan = penelope_dream::onboarding::plan(&d.services, sitting).await?;
            if plan.is_empty() && plan.keep.is_empty() {
                penelope_dream::onboarding::cancel(&d.services).await?;
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        "Aucune réponse à retenir : rien n'est écrit.",
                    )
                    .await;
            }
            let rows = vec![vec![
                button("✅ Écrire".into(), k::ONBOARD_WRITE, json!({})).await?,
                button("✖️ Annuler".into(), k::ONBOARD_CANCEL, json!({})).await?,
            ]];
            return self
                .bot
                .send_text(
                    chat_id,
                    topic_id,
                    &markdown_to_html(&penelope_dream::onboarding::plan_text(&plan)),
                    Some(inline_keyboard(&rows)),
                    None,
                )
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!(e.to_string()));
        };
        s.kv_set(&key, &json!({"rel": sitting.rel, "n": q.n}).to_string())
            .await?;
        let (i, total) = sitting.position(q.n);
        let mut hint = q.hint.to_string();
        if q.n == 4
            && let Some(sup) = d.hooks.mcp_supervisor()
        {
            let servers: Vec<String> = sup.statuses().await.into_iter().map(|st| st.name).collect();
            if !servers.is_empty() {
                hint.push_str(&format!(" Serveurs MCP déclarés : {}.", servers.join(", ")));
            }
        }
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        if !q.choices.is_empty() {
            let mut row = Vec::new();
            for c in q.choices {
                row.push(
                    button(
                        c.to_string(),
                        k::ONBOARD_ANSWER,
                        json!({"n": q.n, "answer": c}),
                    )
                    .await?,
                );
            }
            rows.push(row);
        }
        rows.push(vec![
            button(
                "⏭ Passer".into(),
                k::ONBOARD_ANSWER,
                json!({"n": q.n, "answer": null}),
            )
            .await?,
            button("⏸ Plus tard".into(), k::ONBOARD_PAUSE, json!({})).await?,
        ]);
        let mut text = format!("📋 **Accueil · {i}/{total}**\n\n{}", q.text);
        if !hint.trim().is_empty() {
            text.push_str(&format!("\n_{}_", hint.trim()));
        }
        if q.choices.is_empty() {
            text.push_str("\n\nRéponds en un message.");
        }
        self.bot
            .send_text(
                chat_id,
                topic_id,
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
    /// Boutons de l'accueil.
    pub(super) async fn onboarding_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let _ = self.bot.answer_callback(callback_id, None, false).await;
        let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        let rel = action.target.as_str();
        match action.action.as_str() {
            k::ONBOARD_START => self.onboarding_next(chat_id, topic_id, None).await,
            k::ONBOARD_ANSWER => {
                let n = action.args["n"].as_u64().unwrap_or(0) as u32;
                match penelope_dream::onboarding::answer(
                    &d.services,
                    rel,
                    n,
                    action.args["answer"].as_str(),
                )
                .await
                {
                    Ok(sitting) => self.onboarding_ask(chat_id, topic_id, &sitting).await,
                    Err(e) => {
                        self.reply(chat_id, topic_id, None, &format!("⚠️ {e}"))
                            .await
                    }
                }
            }
            k::ONBOARD_PAUSE => {
                d.services
                    .kv_set(&format!("tg.onboard.{chat_id}"), "")
                    .await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    "⏸ Accueil en pause : `/accueil` reprend à la première question sans réponse.",
                )
                .await
            }
            k::ONBOARD_WRITE => {
                let Some(sitting) = penelope_dream::onboarding::load(&d.services, rel) else {
                    return self
                        .reply(chat_id, topic_id, None, "ℹ️ séance d'accueil introuvable")
                        .await;
                };
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: None,
                };
                let session = d.chat_session_for(&origin).await?;
                let (added, replaced) =
                    penelope_dream::onboarding::write(&d.services, &sitting, &session).await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!(
                        "✅ Accueil enregistré : {added} ajout(s), {replaced} remplacement(s) \
                         dans `profil.md` et `memoire.md`. `/accueil limites` (ou profil, \
                         outils, style) pour revenir sur une partie."
                    ),
                )
                .await
            }
            _ => {
                penelope_dream::onboarding::cancel(&d.services).await?;
                d.services
                    .kv_set(&format!("tg.onboard.{chat_id}"), "")
                    .await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!("✖️ Rien n'est écrit ; la séance reste lisible dans `{rel}`."),
                )
                .await
            }
        }
    }
}
