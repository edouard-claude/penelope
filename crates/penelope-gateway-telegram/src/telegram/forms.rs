//! Formulaires : un champ par écran, saisie tapée ou cliquée (issues #30, #35).

use super::*;

/// Clé du formulaire en cours, **par sujet** (issue #149). Un seul formulaire par chat,
/// quel que soit le sujet, faisait qu'un formulaire ouvert dans un sujet avalait le texte
/// tapé dans un autre, et répondait dans Général.
pub(super) fn form_key(chat_id: i64, topic_id: Option<i64>) -> String {
    match topic_id {
        Some(t) => format!("tg.form.{chat_id}.{t}"),
        None => format!("tg.form.{chat_id}"),
    }
}

/// Sujet où vit un formulaire, d'après sa charge : toutes ses phrases y retournent.
fn form_topic(pending: &Value) -> Option<i64> {
    pending["topic"].as_i64()
}

impl TelegramGateway {
    /// Écran courant d'un formulaire : le champ à remplir, ou le récapitulatif à envoyer.
    pub(super) async fn send_form_step(&self, chat_id: i64, pending: &Value) -> anyhow::Result<()> {
        use penelope_telegram::forms::{FieldKind, FormState};
        let state: FormState = serde_json::from_value(pending["state"].clone())?;
        let s = &self.daemon.services;
        let ttl = 24 * 3_600_000;
        let target = chat_id.to_string();
        let button = |label: &str, action: &str, args: Value| {
            let (label, action, target) = (label.to_string(), action.to_string(), target.clone());
            async move {
                s.actions
                    .create(&action, &target, args, ttl, true)
                    .await
                    .map(|t| ButtonSpec::callback(&label, &t.token, ""))
            }
        };
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        let text = if state.done {
            rows.push(vec![
                button("✅ Envoyer", k::FORM_SUBMIT, json!({})).await?,
                button("↩️ Modifier", k::FORM_PREV, json!({})).await?,
            ]);
            let mut last = vec![button("✖️ Abandonner", k::FORM_DECLINE, json!({})).await?];
            // Élicitation MCP : refuser reste possible jusqu'à l'envoi.
            if let Some(id) = pending["elicitation"].as_str() {
                let t = s
                    .actions
                    .create(k::ELICIT_DECLINE, id, json!({}), ttl, true)
                    .await?;
                last.push(ButtonSpec::callback("🚫 Refuser", &t.token, ""));
            }
            rows.push(last);
            format!(
                "📝 « {} »\n\n{}",
                pending["choice"].as_str().unwrap_or_default(),
                state.summary()
            )
        } else {
            let Some(field) = state.current() else {
                return Ok(());
            };
            for label in field.button_labels() {
                rows.push(vec![
                    button(&label, k::FORM_NEXT, json!({"answer": label})).await?,
                ]);
            }
            let mut nav = Vec::new();
            if state.cursor > 0 {
                nav.push(button("↩️ Précédent", k::FORM_PREV, json!({})).await?);
            }
            if !field.required || state.values.contains_key(&field.name) {
                nav.push(button("⏭ Passer", k::FORM_NEXT, json!({})).await?);
            }
            nav.push(button("✖️ Abandonner", k::FORM_DECLINE, json!({})).await?);
            rows.push(nav);
            let hint = match &field.kind {
                FieldKind::Enum { multi: true, .. } => {
                    "Un bouton, ou plusieurs options séparées par des virgules."
                }
                FieldKind::Enum { .. } | FieldKind::Boolean => "Un bouton.",
                FieldKind::Number { integer: true } => "Un nombre entier, en un message.",
                FieldKind::Number { .. } => "Un nombre, en un message.",
                FieldKind::Text { .. } => "En un message.",
            };
            let current = state
                .values
                .get(&field.name)
                .map(|v| {
                    format!(
                        "\nValeur actuelle : `{}`",
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string())
                    )
                })
                .unwrap_or_default();
            format!(
                "📝 {} · **{}**{}\n{}{}{current}",
                state.progress(),
                field.title,
                if field.required { " *" } else { "" },
                if field.description.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", field.description)
                },
                hint
            )
        };
        self.bot
            .send_text(
                chat_id,
                pending["topic"].as_i64(),
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
    /// Formulaire en cours dans ce sujet. Une clé d'avant #149 (`tg.form.{chat}`, tous
    /// sujets confondus) est reprise une dernière fois, puis réécrite par sujet.
    pub(super) async fn form_pending(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<Option<String>> {
        let s = &self.daemon.services;
        let here = s.kv_get(&form_key(chat_id, topic_id)).await?;
        if here.as_deref().is_some_and(|r| !r.is_empty()) || topic_id.is_none() {
            return Ok(here);
        }
        let Some(legacy) = s
            .kv_get(&form_key(chat_id, None))
            .await?
            .filter(|r| !r.is_empty())
        else {
            return Ok(here);
        };
        let mut pending: Value = serde_json::from_str(&legacy)?;
        pending["topic"] = json!(topic_id);
        s.kv_set(&form_key(chat_id, None), "").await?;
        s.kv_set(&form_key(chat_id, topic_id), &pending.to_string())
            .await?;
        Ok(Some(pending.to_string()))
    }
    /// Une réponse au champ courant (message ou bouton ; `None` : passer le champ).
    pub(super) async fn form_input(
        &self,
        chat_id: i64,
        raw: &str,
        answer: Option<&str>,
    ) -> anyhow::Result<()> {
        use penelope_telegram::forms::FormState;
        let mut pending: Value = serde_json::from_str(raw)?;
        let topic = form_topic(&pending);
        let mut state: FormState = serde_json::from_value(pending["state"].clone())?;
        let applied = match answer {
            Some(a) => state.answer(a),
            None => state.skip(),
        };
        if let Err(e) = applied {
            // L'erreur de validation retourne là où la carte vit, et dit où répondre :
            // un formulaire ne lit que son propre sujet (issue #149).
            let here = if topic.is_some() {
                " — réponds **ici**, ou « ✖️ Abandonner »"
            } else {
                ""
            };
            self.reply(chat_id, topic, None, &format!("⚠️ {e}{here}"))
                .await?;
            return self.send_form_step(chat_id, &pending).await;
        }
        pending["state"] = serde_json::to_value(&state)?;
        self.daemon
            .services
            .kv_set(&form_key(chat_id, topic), &pending.to_string())
            .await?;
        self.send_form_step(chat_id, &pending).await
    }
    /// Boutons d'un formulaire : passer, revenir, envoyer, abandonner.
    pub(super) async fn form_clicked(
        &self,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<()> {
        use penelope_telegram::forms::FormState;
        let d = &self.daemon;
        let Some(raw) = self
            .form_pending(chat_id, topic_id)
            .await?
            .filter(|r| !r.is_empty())
        else {
            return self
                .reply(chat_id, topic_id, None, "ℹ️ aucun formulaire en cours")
                .await;
        };
        let mut pending: Value = serde_json::from_str(&raw)?;
        // Le sujet du formulaire prime sur celui du clic : la carte peut être ailleurs.
        let topic_id = form_topic(&pending).or(topic_id);
        let mut state: FormState = serde_json::from_value(pending["state"].clone())?;
        match action.action.as_str() {
            k::FORM_NEXT => {
                return self
                    .form_input(chat_id, &raw, action.args["answer"].as_str())
                    .await;
            }
            k::FORM_PREV => {
                state.prev();
                pending["state"] = serde_json::to_value(&state)?;
                d.services
                    .kv_set(&form_key(chat_id, topic_id), &pending.to_string())
                    .await?;
                self.send_form_step(chat_id, &pending).await
            }
            k::FORM_DECLINE => {
                d.services.kv_set(&form_key(chat_id, topic_id), "").await?;
                if pending["workflow"].is_string() || pending["prompt"].is_object() {
                    return self
                        .reply(
                            chat_id,
                            None,
                            None,
                            "✖️ Formulaire abandonné : rien n'est lancé.",
                        )
                        .await;
                }
                if let Some(id) = pending["elicitation"].as_str() {
                    return self
                        .finish_elicitation(
                            chat_id,
                            id,
                            crate::elicitation::Action::Cancel,
                            "✖️ Formulaire abandonné : `{server}` reçoit une annulation.",
                            true,
                            None,
                        )
                        .await;
                }
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    "✖️ Formulaire abandonné : la question du workflow reste ouverte \
                     (`/runs`).",
                )
                .await
            }
            _ => {
                let values = match state.submit() {
                    Ok(v) => v,
                    Err(e) => {
                        self.reply(chat_id, topic_id, None, &format!("⚠️ {e}"))
                            .await?;
                        return self.send_form_step(chat_id, &pending).await;
                    }
                };
                d.services.kv_set(&form_key(chat_id, topic_id), "").await?;
                // Paramètres d'un workflow lancé depuis `/wf` ou `/run` (issue #30).
                if let Some(workflow) = pending["workflow"].as_str() {
                    let origin = Origin::Telegram {
                        chat_id,
                        topic_id,
                        message_id: None,
                    };
                    let note =
                        match crate::workflow::start_run(d, workflow, values, &origin, None, 0)
                            .await
                        {
                            Ok(run) => format!(
                                "▶️ Run `{}` lancé (« {} »).",
                                run.id,
                                pending["choice"].as_str().unwrap_or(workflow)
                            ),
                            Err(e) => format!("❌ {e}"),
                        };
                    return self.reply(chat_id, topic_id, None, &note).await;
                }
                // Arguments d'un prompt MCP (`/p`).
                if let (Some(server), Some(prompt)) = (
                    pending["prompt"]["server"].as_str(),
                    pending["prompt"]["name"].as_str(),
                ) {
                    let note = match self
                        .run_mcp_prompt(chat_id, topic_id, server, prompt, values)
                        .await
                    {
                        Ok(n) => {
                            format!("💬 Prompt `{prompt}` : {n} message(s) envoyé(s) au modèle.")
                        }
                        Err(e) => format!("❌ {e}"),
                    };
                    return self.reply(chat_id, topic_id, None, &note).await;
                }
                if let Some(id) = pending["elicitation"].as_str() {
                    return self
                        .finish_elicitation(
                            chat_id,
                            id,
                            crate::elicitation::Action::Accept(Some(values)),
                            "✔️ Formulaire envoyé à `{server}`.",
                            true,
                            None,
                        )
                        .await;
                }
                let note = match crate::workflow::answer(
                    d,
                    pending["run"].as_str().unwrap_or_default(),
                    pending["visit"].as_str().unwrap_or_default(),
                    pending["choice"].as_str().unwrap_or_default(),
                    Some(&values.to_string()),
                )
                .await
                {
                    Ok(()) => "✔️ Formulaire transmis au workflow.".to_string(),
                    Err(e) => format!("ℹ️ {e}"),
                };
                self.reply(chat_id, topic_id, None, &note).await
            }
        }
    }
}
