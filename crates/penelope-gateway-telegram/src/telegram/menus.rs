//! Menus de choix : suites proposées, modèles, workflows, épinglage.

use super::*;

impl TelegramGateway {
    pub(super) async fn send_choices(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        session_id: &str,
        text: &str,
        choices: &[String],
    ) -> anyhow::Result<()> {
        let mut rows = Vec::new();
        for choice in choices {
            let t = self
                .actions
                .create(
                    k::SAY,
                    session_id,
                    json!({"text": choice}),
                    7 * 24 * 3_600_000,
                    true,
                )
                .await?;
            rows.push(vec![ButtonSpec::callback(choice, &t.token, "")]);
        }
        let screen = screens::Screen {
            text: text.to_string(),
            rows,
        };
        self.send_screen(chat_id, topic_id, reply_to, screen, None)
            .await
    }
    /// Menu `/model` : état du modèle de la session et un bouton par choix.
    pub(super) async fn send_model_menu(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        session: &str,
    ) -> anyhow::Result<()> {
        let view = self.daemon.session_model_view(session).await?;
        let (html, keyboard) = self.model_menu(&view).await?;
        let mut payload = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "reply_markup": keyboard,
            "message_thread_id": topic_id,
        });
        if let Some(r) = reply_to {
            payload["reply_parameters"] =
                json!({"message_id": r, "allow_sending_without_reply": true});
        }
        self.outbox_push(chat_id, topic_id, "sendMessage", payload)
            .await
    }
    /// Texte et boutons du menu. Les jetons sont réutilisables : on peut changer d'avis
    /// depuis le même message pendant une semaine.
    async fn model_menu(&self, view: &Value) -> anyhow::Result<(String, Value)> {
        let session = view["session"].as_str().unwrap_or_default();
        let pinned = view["pinned"].as_str();
        let ttl = 7 * 24 * 3_600_000;

        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for c in view["choices"].as_array().cloned().unwrap_or_default() {
            let alias = c["alias"].as_str().unwrap_or("?");
            let model = short_model(c["model"].as_str().unwrap_or("?"));
            let token = self
                .actions
                .create(k::MODEL_PIN, session, json!({"alias": alias}), ttl, false)
                .await?;
            let mark = if pinned == Some(alias) { "✅ " } else { "" };
            rows.push(vec![ButtonSpec::callback(
                &format!("{mark}{alias} · {model}"),
                &token.token,
                "",
            )]);
        }
        let auto = self
            .actions
            .create(k::MODEL_PIN, session, json!({"alias": null}), ttl, false)
            .await?;
        let mark = if pinned.is_none() { "✅ " } else { "" };
        rows.push(vec![ButtonSpec::callback(
            &format!("{mark}🔀 Automatique"),
            &auto.token,
            "",
        )]);

        let state = match pinned {
            Some(alias) => format!(
                "Épinglé sur `{alias}` · `{}` : tous les messages de la session l'utilisent.",
                short_model(view["pinned_model"].as_str().unwrap_or("?"))
            ),
            None => {
                let how = if view["classifier"].as_bool().unwrap_or(false) {
                    "le classifieur choisit à chaque message"
                } else {
                    "tout passe par `main`"
                };
                let why = view["last_boundary"]
                    .as_str()
                    .map(|b| format!(" Reclassé à une frontière : {b}."))
                    .unwrap_or_default();
                match view["last_alias"].as_str() {
                    Some(last) => format!(
                        "Automatique ({how}). Dernier message : `{last}` · `{}`.{why}",
                        short_model(view["last_model"].as_str().unwrap_or("?"))
                    ),
                    None => format!("Automatique ({how})."),
                }
            }
        };
        let markdown = format!("**Modèle de cette session**\n\n{state}");
        Ok((markdown_to_html(&markdown), inline_keyboard(&rows)))
    }
    /// Clic sur un bouton du menu `/model`.
    /// Bouton d'une question de workflow : le choix part au run, ou attend la saisie.
    pub(super) async fn workflow_choice_clicked(
        &self,
        callback_id: &str,
        action: &penelope_telegram::actions::Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let run = action.target.as_str();
        let visit = action.args["visit"].as_str().unwrap_or_default();
        let choice = action.args["choice"].as_str().unwrap_or_default();
        let wants_input = action.args["input"].as_bool().unwrap_or(false);
        let _ = self
            .bot
            .answer_callback(callback_id, Some(choice), false)
            .await;
        let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        if action.args["form"].as_bool().unwrap_or(false) {
            let Some(schema) = crate::workflow::form_of(&self.daemon, run, visit).await else {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        "ℹ️ cette question n'est plus d'actualité",
                    )
                    .await;
            };
            let state = match penelope_telegram::forms::FormState::new(visit, schema) {
                Ok(st) => st,
                Err(e) => {
                    return self
                        .reply(chat_id, topic_id, None, &format!("❌ {e}"))
                        .await;
                }
            };
            let pending = json!({"run": run, "visit": visit, "choice": choice, "state": state,
                       "topic": topic_id, "since": s.clock.now_rfc3339()});
            s.kv_set(&form_key(chat_id, topic_id), &pending.to_string())
                .await?;
            return self.send_form_step(chat_id, &pending).await;
        }
        let note = if wants_input {
            s.kv_set(
                &format!("tg.await_input.{chat_id}"),
                &json!({"run": run, "visit": visit, "choice": choice}).to_string(),
            )
            .await?;
            format!("✏️ « {choice} » : précise en un message.")
        } else {
            match crate::workflow::answer(&self.daemon, run, visit, choice, None).await {
                Ok(()) => format!("✔️ « {choice} »"),
                Err(e) => format!("ℹ️ {e}"),
            }
        };
        self.reply(chat_id, topic_id, None, &note).await
    }
    pub(super) async fn model_pin_clicked(
        &self,
        callback_id: &str,
        action: &penelope_telegram::actions::Action,
        chat_id: i64,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let session = action.target.as_str();
        let alias = action.args.get("alias").and_then(|a| a.as_str());
        let rpc = crate::rpc::Rpc::new(self.daemon.clone());
        let result = rpc
            .call(
                m::SESSION_MODEL,
                json!({"session": session, "alias": alias.unwrap_or("auto")}),
            )
            .await;
        match result {
            Ok(view) => {
                let toast = match alias {
                    Some(a) => format!("Session épinglée sur {a}"),
                    None => "Session en automatique".to_string(),
                };
                let _ = self
                    .bot
                    .answer_callback(callback_id, Some(&toast), false)
                    .await;
                let (html, keyboard) = self.model_menu(&view).await?;
                let _ = self
                    .bot
                    .edit_text(chat_id, message_id, &html, Some(keyboard))
                    .await;
            }
            Err(e) => {
                let _ = self
                    .bot
                    .answer_callback(callback_id, Some(&e.to_string()), true)
                    .await;
            }
        }
        Ok(())
    }
}
