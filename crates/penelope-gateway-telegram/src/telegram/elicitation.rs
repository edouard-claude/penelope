//! Élicitations MCP relayées au propriétaire (`OwnerChannel`).

use super::*;

impl TelegramGateway {
    // ============================================================ élicitation MCP

    /// Texte d'une carte d'élicitation : le serveur est nommé, son message cité et échappé,
    /// un lien montré en entier avec son domaine (§8.4, issue #12).
    fn elicitation_html(r: &penelope_app::elicitation::Request) -> String {
        use penelope_app::elicitation::Kind;
        use penelope_telegram::render::escape_html;
        let server = escape_html(&r.server);
        let quote = match r.message.trim() {
            "" => String::new(),
            m => format!("\n\n<blockquote>{}</blockquote>", escape_html(m)),
        };
        match &r.kind {
            Kind::Form { schema } if r.field_count() > 0 => {
                let fields = penelope_telegram::forms::fields_from_schema(schema)
                    .map(|f| {
                        f.iter()
                            .map(|f| format!("{}{}", f.title, if f.required { " *" } else { "" }))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                format!(
                    "📝 <b>Le serveur MCP <code>{server}</code> demande des informations</b>\
                     {quote}\nChamps : {}",
                    escape_html(&fields)
                )
            }
            Kind::Form { .. } => format!(
                "🔐 <b>Le serveur MCP <code>{server}</code> demande ta confirmation</b>{quote}"
            ),
            Kind::Url { url, .. } => {
                let host = r.host().unwrap_or_default();
                let warning = if host.split('.').any(|l| l.starts_with("xn--")) {
                    "\n⚠️ Domaine en Punycode : ses caractères peuvent imiter un autre site."
                } else {
                    ""
                };
                format!(
                    "🌐 <b>Le serveur MCP <code>{server}</code> demande d'ouvrir un lien</b>\
                     {quote}\nDomaine : <b>{}</b>{warning}\n<code>{}</code>",
                    escape_html(&host),
                    escape_html(url)
                )
            }
        }
    }
    async fn elicit_button(
        &self,
        label: &str,
        action: &str,
        r: &penelope_app::elicitation::Request,
    ) -> anyhow::Result<ButtonSpec> {
        let ttl = r.timeout.as_millis() as i64 + 3_600_000;
        let t = self
            .actions
            .create(action, &r.id, json!({}), ttl, true)
            .await?;
        Ok(ButtonSpec::callback(label, &t.token, ""))
    }
    /// Remplace la carte (texte d'origine, puis l'issue) ; à défaut, un nouveau message.
    async fn elicitation_update(
        &self,
        r: &penelope_app::elicitation::Request,
        card: Option<i64>,
        note: &str,
        keyboard: Option<Value>,
    ) -> anyhow::Result<()> {
        let html = format!(
            "{}\n\n{}",
            Self::elicitation_html(r),
            markdown_to_html(note)
        );
        let (chat_id, topic_id) = self.elicitation_chat(r).await;
        if let Some(id) = card
            && self
                .bot
                .edit_text(chat_id, id, &html, keyboard.clone())
                .await
                .is_ok()
        {
            return Ok(());
        }
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": keyboard,
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    /// Bouton « Relancer » d'une demande annulée.
    async fn elicit_retry_button(
        &self,
        r: &penelope_app::elicitation::Request,
        session: &str,
    ) -> Option<ButtonSpec> {
        let t = self
            .actions
            .create(
                k::ELICIT_RETRY,
                &r.id,
                json!({
                    "session": session,
                    "server": r.server,
                    "what": penelope_app::elicitation::first_line(&r.message),
                }),
                24 * 3_600_000,
                true,
            )
            .await
            .ok()?;
        Some(ButtonSpec::callback("🔄 Relancer", &t.token, ""))
    }
    /// Relance demandée depuis le message d'annulation : le texte repart comme un
    /// message du propriétaire dans **sa** session, et le modèle refait l'appel (#143).
    pub(super) async fn elicitation_retry_clicked(
        &self,
        action: &penelope_telegram::actions::Action,
        chat_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let session = action.args["session"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let server = action.args["server"].as_str().unwrap_or("le serveur");
        let what = action.args["what"].as_str().unwrap_or_default();
        if session.is_empty() {
            let (c, t) = self.home_chat();
            return self
                .reply(
                    c,
                    t,
                    None,
                    "Cette demande n'a pas de conversation à relancer.",
                )
                .await
                .map(|_| ());
        }
        let topic_id = d
            .services
            .sessions
            .get(&session)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.tg_topic_id);
        let text = format!(
            "Relance la demande `{server}` qui a expiré{}. Cette fois je réponds tout de \
             suite à la confirmation.",
            if what.is_empty() {
                String::new()
            } else {
                format!(" ({what})")
            }
        );
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        d.enqueue_message(&session, &text, &origin, None).await?;
        Ok(())
    }
    /// Où poser une carte d'élicitation : la conversation de l'appel, sinon le foyer du
    /// propriétaire (issue #143).
    async fn elicitation_chat(&self, r: &penelope_app::elicitation::Request) -> (i64, Option<i64>) {
        match r.to.origin.as_ref().and_then(|o| o.telegram_chat()) {
            Some(chat) => chat,
            None => self.home_chat(),
        }
    }
    /// Répond au serveur, met la carte à jour et le dit dans le chat. `note` : `{server}`
    /// est remplacé par le nom du serveur.
    /// `card` : le message à modifier en place. La file ne rend pas d'identifiant à
    /// l'envoi (issue #143) : celui du message cliqué fait l'affaire, et l'issue reste
    /// écrite là où la carte se trouve.
    pub(super) async fn finish_elicitation(
        &self,
        chat_id: i64,
        id: &str,
        answer: penelope_app::elicitation::Action,
        note: &str,
        echo: bool,
        card: Option<i64>,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let broker = &s.elicitations;
        let card = broker.request(id).and_then(|(_, c)| c).or(card);
        match broker.resolve(id, answer) {
            Ok(request) => {
                let topic = self.elicitation_chat(&request).await.1;
                if let Some(raw) = self.form_pending(chat_id, topic).await?
                    && serde_json::from_str::<Value>(&raw)
                        .is_ok_and(|p| p["elicitation"].as_str() == Some(id))
                {
                    s.kv_set(&form_key(chat_id, topic), "").await?;
                }
                let note = note.replace("{server}", &request.server);
                self.elicitation_update(&request, card, &note, None).await?;
                // La carte est plus haut dans le chat : l'issue est redite en bas.
                if echo && card.is_some() {
                    self.reply(chat_id, None, None, &note).await?;
                }
                Ok(())
            }
            Err(e) => self.reply(chat_id, None, None, &format!("ℹ️ {e}")).await,
        }
    }
    /// Boutons d'une carte d'élicitation.
    pub(super) async fn elicitation_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        use penelope_app::elicitation::{Action as Answer, Kind};
        let broker = s.elicitations.clone();
        let Some((request, card)) = broker.request(&action.target) else {
            let _ = self
                .bot
                .answer_callback(callback_id, Some("Demande expirée ou déjà traitée."), false)
                .await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return Ok(());
        };
        let _ = self.bot.answer_callback(callback_id, None, false).await;
        let elsewhere = card != Some(message_id);
        if elsewhere {
            // Bouton d'un autre message (récapitulatif du formulaire) : il a servi.
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        }
        let card = card.or(Some(message_id));
        let (answer, note) = match action.action.as_str() {
            k::ELICIT_DECLINE => (Answer::Decline, "🚫 Refusé : `{server}` en est informé."),
            k::ELICIT_CANCEL => (Answer::Cancel, "✖️ Annulé : `{server}` en est informé."),
            k::ELICIT_DONE => (Answer::Accept(None), "✅ Terminé : `{server}` reprend."),
            _ => match &request.kind {
                Kind::Form { schema } if request.field_count() > 0 => {
                    let state =
                        match penelope_telegram::forms::FormState::new(&request.id, schema.clone())
                        {
                            Ok(st) => st,
                            Err(e) => {
                                let _ = broker.resolve(&request.id, Answer::Cancel);
                                return self
                                    .elicitation_update(
                                        &request,
                                        card,
                                        &format!(
                                            "❌ Formulaire illisible ({e}) : demande annulée."
                                        ),
                                        None,
                                    )
                                    .await;
                            }
                        };
                    let title: String = request.message.chars().take(80).collect();
                    // Le formulaire vit là où la carte est arrivée (#143), pas dans le
                    // chat tout entier (#149).
                    let topic = self.elicitation_chat(&request).await.1;
                    let pending = json!({
                        "elicitation": request.id,
                        "choice": format!("{} · {title}", request.server),
                        "state": state,
                        "topic": topic,
                        "since": s.clock.now_rfc3339(),
                    });
                    s.kv_set(&form_key(chat_id, topic), &pending.to_string())
                        .await?;
                    let rows = vec![vec![
                        self.elicit_button("🚫 Refuser", k::ELICIT_DECLINE, &request)
                            .await?,
                        self.elicit_button("✖️ Annuler", k::ELICIT_CANCEL, &request)
                            .await?,
                    ]];
                    self.elicitation_update(
                        &request,
                        card,
                        "📝 Formulaire en cours ci-dessous.",
                        Some(inline_keyboard(&rows)),
                    )
                    .await?;
                    return self.send_form_step(chat_id, &pending).await;
                }
                Kind::Form { .. } => (
                    Answer::Accept(Some(json!({}))),
                    "✅ Accepté : `{server}` continue.",
                ),
                Kind::Url {
                    url,
                    elicitation_id,
                } => {
                    let host = request.host().unwrap_or_default();
                    let open = ButtonSpec::url(&format!("🌐 Ouvrir {host}"), url);
                    if elicitation_id.is_none() {
                        // MRTR : le serveur reprend quand le propriétaire a terminé.
                        let rows = vec![
                            vec![open],
                            vec![
                                self.elicit_button("✅ J'ai terminé", k::ELICIT_DONE, &request)
                                    .await?,
                                self.elicit_button("✖️ Annuler", k::ELICIT_CANCEL, &request)
                                    .await?,
                            ],
                        ];
                        return self
                            .elicitation_update(
                                &request,
                                card,
                                "Ouvre le lien, puis « J'ai terminé ».",
                                Some(inline_keyboard(&rows)),
                            )
                            .await;
                    }
                    if let Err(e) = broker.resolve(&request.id, Answer::Accept(None)) {
                        return self.reply(chat_id, None, None, &format!("ℹ️ {e}")).await;
                    }
                    return self
                        .elicitation_update(
                            &request,
                            card,
                            &format!(
                                "✅ Accepté : ouvre le lien, `{}` signalera la fin.",
                                request.server
                            ),
                            Some(inline_keyboard(&[vec![open]])),
                        )
                        .await;
                }
            },
        };
        self.finish_elicitation(chat_id, &request.id, answer, note, elsewhere, card)
            .await
    }
}

#[async_trait::async_trait]
impl penelope_app::elicitation::OwnerChannel for TelegramGateway {
    fn place(&self) -> String {
        "sur Telegram".into()
    }

    /// Un formulaire se présente champ par champ (§8.4) : ce que Telegram ne sait pas
    /// demander est refusé au serveur avant toute carte.
    fn check_form(&self, schema: &Value) -> Result<(), String> {
        penelope_telegram::forms::fields_from_schema(schema)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn show(&self, r: &penelope_app::elicitation::Request) -> Result<Option<i64>, String> {
        use penelope_app::elicitation::Kind;
        let accept = match &r.kind {
            Kind::Form { .. } if r.field_count() > 0 => "📝 Remplir",
            Kind::Form { .. } => "✅ Accepter",
            Kind::Url { .. } => "🌐 Ouvrir le lien",
        };
        let button = |label, action| self.elicit_button(label, action, r);
        let rows = vec![
            vec![
                button(accept, k::ELICIT_ACCEPT)
                    .await
                    .map_err(|e| e.to_string())?,
                button("🚫 Refuser", k::ELICIT_DECLINE)
                    .await
                    .map_err(|e| e.to_string())?,
            ],
            vec![
                button("✖️ Annuler", k::ELICIT_CANCEL)
                    .await
                    .map_err(|e| e.to_string())?,
            ],
        ];
        let html = format!(
            "{}\n\n<i>Sans réponse d'ici {}, la demande est annulée.</i>",
            Self::elicitation_html(r),
            penelope_app::elicitation::human(r.timeout)
        );
        // La carte va **dans la conversation qui a déclenché l'appel** (issue #143) :
        // avant, elle partait dans le chat privé, que le propriétaire ne lit plus depuis
        // qu'il travaille dans un sujet de groupe. Elle passe par la file, comme les
        // cartes d'approbation : une coupure réseau ne la perd plus (#101).
        let (chat_id, topic_id) = self.elicitation_chat(r).await;
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rows),
                "message_thread_id": topic_id,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
        // La file ne rend pas l'identifiant du message : la carte ne sera pas modifiée
        // en place, l'issue arrive en message dans le même sujet.
        Ok(None)
    }

    async fn close(
        &self,
        r: &penelope_app::elicitation::Request,
        card: Option<i64>,
        markdown: &str,
        retry: bool,
    ) {
        // Une demande annulée par le délai se relance d'un bouton, dans la conversation
        // où elle a échoué : trois demandes identiques en une session coûtaient trois
        // cartes et autant d'attentes (issue #143).
        let keyboard = match (retry, r.to.session_id.as_deref()) {
            (true, Some(session)) => self
                .elicit_retry_button(r, session)
                .await
                .map(|b| inline_keyboard(&[vec![b]])),
            _ => None,
        };
        if let Err(e) = self.elicitation_update(r, card, markdown, keyboard).await {
            tracing::warn!(error = %e, "carte d'élicitation non mise à jour");
        }
    }

    /// Rappel à mi-délai, dans la conversation où la carte a été posée (issue #143).
    async fn remind(&self, r: &penelope_app::elicitation::Request, _card: Option<i64>) {
        let (chat_id, topic_id) = self.elicitation_chat(r).await;
        let text = format!(
            "⏳ La confirmation demandée par `{}` attend toujours ({} restantes) : {}",
            r.server,
            penelope_app::elicitation::human(r.timeout / 2),
            penelope_app::elicitation::first_line(&r.message)
        );
        if let Err(e) = self.reply(chat_id, topic_id, None, &text).await {
            tracing::warn!(error = %e, "rappel d'élicitation non envoyé");
        }
    }
}
