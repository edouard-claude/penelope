//! Livraison du cœur vers le canal : `ChannelDelivery`, `Messenger` ; démarrage par `Gateway`.

use super::*;

#[async_trait::async_trait]
impl ChannelDelivery for TelegramGateway {
    /// La carte de rafale ne vaut que pour une conversation Telegram (issues #49, #161).
    fn burst_limits(&self, origin: &Origin) -> Option<penelope_app::bus::BurstLimits> {
        let cfg = self.daemon.services.config.config();
        matches!(origin, Origin::Telegram { .. }).then_some(penelope_app::bus::BurstLimits::new(
            cfg.telegram.burst_messages,
            cfg.telegram.burst_chars,
        ))
    }

    async fn describe_origin(&self, origin: &Origin) -> Option<String> {
        channel::place_name(&self.daemon.services, origin).await
    }

    /// Le nom du sujet de la session, quand il vaut projet : ni Général, ni le foyer (#301).
    async fn subject_of(&self, session_id: &str) -> Option<String> {
        self.session_subject(session_id).await
    }

    /// `telegram.trace` (#273) : en mode `narre`, le rôle `trace` a un modèle joignable.
    async fn doctor_checks(&self) -> Vec<penelope_kernel::api::DoctorCheck> {
        trace::narrate::doctor_check(&self.daemon.services)
            .await
            .into_iter()
            .collect()
    }

    async fn destination_for(&self, origin: &Origin) -> Result<Origin, String> {
        channel::destination_for(&self.daemon.services, origin)
    }

    async fn offer_burst(
        &self,
        session_id: &str,
        origin: &Origin,
        parts: Vec<String>,
    ) -> Result<(), String> {
        let chars = parts.iter().map(|part| part.chars().count()).sum();
        self.ask_about_burst(TextBurst {
            origin: origin.clone(),
            session: session_id.to_string(),
            parts,
            message_ids: Vec::new(),
            update_id: 0,
            chars,
            deadline: std::time::Instant::now(),
        })
        .await
        .map_err(|error| error.to_string())
    }

    /// `getMe` : une requête neuve, qui ne dépend pas du long poll resté pendu pendant la
    /// veille (#228).
    async fn probe(&self) -> Result<(), String> {
        self.bot
            .get_me()
            .await
            .map(|_| ())
            .map_err(|e| format!("Telegram injoignable : {e}"))
    }

    async fn schedule_alert(
        &self,
        origin: &Origin,
        schedule_id: &str,
        text: &str,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let ttl = 7 * 24 * 3_600_000;
        let rerun = self
            .actions
            .create(
                k::SCREEN_DO,
                "schedule.run",
                json!({"params": {"id": schedule_id}, "back": null}),
                ttl,
                true,
            )
            .await
            .map_err(|e| e.to_string())?;
        let show = self
            .actions
            .create(k::SCREEN, "schedules", json!({}), ttl, false)
            .await
            .map_err(|e| e.to_string())?;
        let rows = vec![vec![
            ButtonSpec::callback("🔁 Relancer maintenant", &rerun.token, ""),
            ButtonSpec::callback("📅 Voir la planification", &show.token, ""),
        ]];
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rows),
                "message_thread_id": topic_id,
            }),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn session_titled(&self, session_id: &str, title: &str) {
        let s = &self.daemon.services;
        let key = format!("tg.new_session.{session_id}");
        let Some((chat_id, message_id)) = s.kv_get(&key).await.ok().flatten().and_then(|v| {
            let (c, m) = v.split_once(':')?;
            Some((c.parse::<i64>().ok()?, m.parse::<i64>().ok()?))
        }) else {
            return;
        };
        let text = format!("🆕 Nouvelle session « {title} » (`{session_id}`).");
        if let Err(e) = self
            .bot
            .edit_text(chat_id, message_id, &markdown_to_html(&text), None)
            .await
        {
            tracing::debug!(error = %e, "message de nouvelle session non mis à jour");
        }
        let _ = s.kv_delete(&key).await;
    }

    async fn deliver(
        &self,
        turn_id: &str,
        session_id: &str,
        origin: &Origin,
        outcome: &TurnOutcome,
    ) {
        let s = &self.daemon.services;
        let Origin::Telegram {
            chat_id,
            topic_id,
            message_id,
        } = origin.clone()
        else {
            return;
        };
        // Session en arrière-plan : rien n'est écrit dans le chat (issue #10).
        if self.out_of_focus(session_id, chat_id, topic_id).await {
            let item = match outcome {
                TurnOutcome::Answered { text, .. } => Some(Held::Text {
                    text: text.clone(),
                    reply_to: message_id,
                    answer: true,
                    choices: Vec::new(),
                }),
                TurnOutcome::AwaitingApproval { approval_id } => Some(Held::Approval {
                    id: approval_id.clone(),
                }),
                TurnOutcome::LoopAborted {
                    answer, choices, ..
                } => Some(Held::Text {
                    text: answer.clone(),
                    reply_to: message_id,
                    answer: false,
                    choices: choices.clone(),
                }),
                TurnOutcome::Cancelled => None,
                TurnOutcome::BudgetExceeded {
                    scope,
                    spent_usd,
                    limit_usd,
                } => Some(Held::Text {
                    text: penelope_agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
                    reply_to: message_id,
                    answer: false,
                    choices: Vec::new(),
                }),
                TurnOutcome::Failed { error } => Some(Held::Failure {
                    error: error.clone(),
                    reply_to: message_id,
                }),
            };
            if let Some(item) = item
                && let Err(e) = self.hold(session_id, chat_id, topic_id, item).await
            {
                tracing::error!(error = %e, "sortie d'une session en arrière-plan perdue");
            }
            return;
        }
        // La bulle de trace des outils du tour part avant la réponse, par construction :
        // ses appels sont déjà sur le bus, la boucle les lit avant qu'on enfile (#222).
        self.trace_barrier().await;
        let result: anyhow::Result<()> = async {
            match outcome {
                TurnOutcome::Answered { text, .. } => {
                    self.reply(chat_id, topic_id, message_id, text).await?;
                    if let Some(mid) = message_id {
                        self.react(chat_id, mid, reaction::DONE);
                    }
                }
                TurnOutcome::AwaitingApproval { approval_id } => {
                    if let Some(a) = s.approvals.get(approval_id).await? {
                        self.send_approval_card(chat_id, topic_id, &a).await?;
                    }
                    if let Some(mid) = message_id {
                        self.react(chat_id, mid, reaction::WAITING_APPROVAL);
                    }
                }
                TurnOutcome::LoopAborted {
                    answer, choices, ..
                } => {
                    self.send_choices(chat_id, topic_id, message_id, session_id, answer, choices)
                        .await?;
                }
                TurnOutcome::Cancelled => {
                    self.reply(chat_id, topic_id, None, "⏹ Génération arrêtée.")
                        .await?;
                }
                TurnOutcome::BudgetExceeded {
                    scope,
                    spent_usd,
                    limit_usd,
                } => {
                    // Carte de relèvement si la demande est ouverte (issue #32), sinon le texte.
                    let pending = s.approvals.pending(50).await?.into_iter().find(|a| {
                        a.kind == penelope_hitl::ApprovalKind::BudgetExceeded
                            && a.session_id.as_deref() == Some(session_id)
                            && a.payload["budget"].as_bool() == Some(true)
                    });
                    match pending {
                        Some(a) => self.send_budget_card(chat_id, topic_id, &a).await?,
                        None => {
                            self.reply(
                                chat_id,
                                topic_id,
                                message_id,
                                &penelope_agent::budget_exceeded_text(
                                    scope, *spent_usd, *limit_usd,
                                ),
                            )
                            .await?
                        }
                    }
                }
                TurnOutcome::Failed { error } => {
                    let cost = s.budget.turn_totals(turn_id).await.ok().map(|(_, c)| c);
                    self.send_failure(chat_id, topic_id, message_id, session_id, error, cost)
                        .await?;
                    if let Some(mid) = message_id {
                        self.react(chat_id, mid, reaction::ERROR);
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::error!(error = %e, "livraison Telegram impossible");
        }
    }
}

/// Demandes d'élicitation MCP : une carte dans le chat privé du propriétaire.
impl TelegramGateway {
    /// Foyer du propriétaire : le chat et le sujet où arrivent les notifications qui
    /// n'appartiennent à aucune session (issue #143). Réglé par `telegram.home` ; sans
    /// lui, le chat privé, comme avant.
    ///
    /// Une session, elle, garde toujours son propre chat et son propre sujet : ce repli
    /// ne s'applique qu'à ce qui n'en a pas.
    pub fn home_chat(&self) -> (i64, Option<i64>) {
        self.daemon
            .services
            .config
            .config()
            .telegram
            .home
            .resolved()
            .unwrap_or((self.owner_id, None))
    }
}

#[async_trait::async_trait]
impl Messenger for TelegramGateway {
    async fn send_plan_card(
        &self,
        origin: &Origin,
        session: &str,
        draft: &penelope_workflow::plan::PlanDraft,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let version = draft.plan.version();
        // Le bouton porte la révision exacte : un clic sur une carte d'une autre révision
        // est refusé, même si son numéro de version est réutilisé par un plan suivant.
        let fingerprint = draft.fingerprint();
        let token = self
            .actions
            .create(
                k::SCREEN_DO,
                "wf.plan.go",
                json!({"params":{"session":session,"version":version,"fingerprint":fingerprint},"back":null}),
                24 * 3_600_000,
                true,
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut lines = vec![format!("📋 Plan v{version} · {}", draft.plan.goal())];
        for (index, step) in draft.plan.steps().iter().enumerate() {
            lines.push(format!(
                "{}. {} — {}",
                index + 1,
                step.phase.label(),
                step.title
            ));
        }
        lines.push(format!(
            "\nEmpreinte {} · une carte d'OK après la spécification, les tests et le code.\n\
             Réponds dans cette conversation pour corriger le plan, ou valide-le.",
            &fingerprint[..12]
        ));
        self.send_screen(
            chat_id,
            topic_id,
            None,
            screens::Screen {
                text: lines.join("\n"),
                rows: vec![vec![ButtonSpec::callback(
                    &format!("✅ Vas-y (v{version})"),
                    &token.token,
                    "",
                )]],
            },
            None,
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn send_text(&self, origin: &Origin, markdown: &str) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        // Un avis interne (digest, rapport de veille) qui déborde part en document
        // plutôt qu'en six bulles (issue #145).
        self.reply_or_document(chat_id, topic_id, markdown)
            .await
            .map_err(|e| e.to_string())
    }

    async fn send_file(
        &self,
        origin: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        self.bot
            .send_document(chat_id, topic_id, path, caption)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn send_session_text(
        &self,
        session_id: &str,
        origin: &Origin,
        markdown: &str,
    ) -> Result<(), String> {
        if let Some((chat_id, topic_id)) = origin.telegram_chat()
            && self.out_of_focus(session_id, chat_id, topic_id).await
        {
            let item = Held::Text {
                text: markdown.to_string(),
                reply_to: None,
                answer: false,
                choices: Vec::new(),
            };
            return self
                .hold(session_id, chat_id, topic_id, item)
                .await
                .map_err(|e| e.to_string());
        }
        self.send_text(origin, markdown).await
    }

    async fn send_session_file(
        &self,
        session_id: &str,
        origin: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        if let Some((chat_id, topic_id)) = origin.telegram_chat()
            && self.out_of_focus(session_id, chat_id, topic_id).await
        {
            let item = Held::File {
                path: path.display().to_string(),
                caption: caption.map(String::from),
            };
            return self
                .hold(session_id, chat_id, topic_id, item)
                .await
                .map_err(|e| e.to_string());
        }
        self.send_file(origin, path, caption).await
    }

    async fn send_session_voice(
        &self,
        session_id: &str,
        origin: &Origin,
        path: &Path,
        duration_s: u32,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        if self.out_of_focus(session_id, chat_id, topic_id).await {
            let item = Held::Voice {
                path: path.display().to_string(),
                duration_s,
                caption: caption.map(String::from),
            };
            return self
                .hold(session_id, chat_id, topic_id, item)
                .await
                .map_err(|e| e.to_string());
        }
        let reply_to = match origin {
            Origin::Telegram { message_id, .. } => *message_id,
            _ => None,
        };
        self.bot
            .send_voice(chat_id, topic_id, path, duration_s, caption, reply_to)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn send_question(
        &self,
        origin: &Origin,
        markdown: &str,
        run_id: &str,
        visit: &str,
        choices: &[String],
        wants_input: bool,
        form: Option<&Value>,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let ttl = 7 * 24 * 3_600_000;
        let labels: Vec<String> = if choices.is_empty() {
            vec![
                if wants_input {
                    "✏️ Répondre"
                } else {
                    "OK"
                }
                .to_string(),
            ]
        } else {
            choices.to_vec()
        };
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for label in &labels {
            let t = self
                .actions
                .create(
                    k::CHOICE,
                    run_id,
                    json!({
                        "visit": visit,
                        "choice": label,
                        "input": wants_input,
                        "form": form.is_some(),
                    }),
                    ttl,
                    true,
                )
                .await
                .map_err(|e| e.to_string())?;
            rows.push(vec![ButtonSpec::callback(label, &t.token, "")]);
        }
        self.bot
            .send_text(
                chat_id,
                topic_id,
                &markdown_to_html(markdown),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn upsert_card(&self, origin: &Origin, key: &str, markdown: &str) -> Result<(), String> {
        let s = &self.daemon.services;
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let html = markdown_to_html(markdown);
        let kv_key = format!("tg.card.{key}");
        let known = s
            .kv_get(&kv_key)
            .await
            .ok()
            .flatten()
            .and_then(|m| m.parse::<i64>().ok());
        if let Some(message_id) = known {
            match self.bot.edit_text(chat_id, message_id, &html, None).await {
                Ok(_) => return Ok(()),
                // Contenu identique : rien à faire.
                Err(e) if e.to_string().contains("not modified") => return Ok(()),
                // Message trop ancien ou supprimé : une nouvelle carte.
                Err(_) => {}
            }
        }
        let sent = self
            .bot
            .send_text(chat_id, topic_id, &html, None, None)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(id) = sent.get("message_id").and_then(|m| m.as_i64()) {
            let _ = s.kv_set(&kv_key, &id.to_string()).await;
        }
        Ok(())
    }

    async fn send_approval(&self, origin: &Origin, approval_id: &str) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let a = self
            .daemon
            .services
            .approvals
            .get(approval_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("demande {approval_id} introuvable"))?;
        self.send_approval_card(chat_id, topic_id, &a)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Démarrée par `Daemon::run`, qui la reçoit de la composition (`penelope-cli`).
#[async_trait::async_trait]
impl penelope_app::gateway::Gateway for TelegramGateway {
    fn name(&self) -> &'static str {
        "Telegram"
    }

    async fn start(self: Arc<Self>) -> Result<Vec<tokio::task::JoinHandle<()>>, String> {
        TelegramGateway::start(&self).await
    }
}
