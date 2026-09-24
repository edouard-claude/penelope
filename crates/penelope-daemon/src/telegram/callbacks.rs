//! Clics sur les boutons : aiguillage, décisions d'approbation, relances.

use super::*;

impl TelegramGateway {
    // ================================================================ boutons

    #[allow(clippy::too_many_lines)] // gel 0.17 : lot G (telegram/callbacks.rs)
    pub(super) async fn callback(
        self: &Arc<Self>,
        callback_id: &str,
        data: &str,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        from_id: i64,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let outcome = s.actions.click(data, from_id).await?;
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::MODEL_PIN
        {
            return self
                .model_pin_clicked(callback_id, action, chat_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && (action.action == k::OAUTH_RETRY || action.action == k::OAUTH_PASTED)
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            if action.action == k::OAUTH_RETRY {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                return self.send_oauth_card(chat_id, None, &action.target).await;
            }
            return self
                .reply(
                    chat_id,
                    None,
                    None,
                    "📋 Colle ici l'adresse complète affichée par le navigateur après \
                     l'autorisation (elle contient `code=` et `state=`).",
                )
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::SESSION_SWITCH
                    | k::SESSION_MENU
                    | k::SESSION_FORK
                    | k::SESSION_RENAME
                    | k::SESSION_CLOSE
                    | k::SESSIONS_PAGE
            )
        {
            return self
                .session_menu_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && (action.action == k::BUDGET_RAISE || action.action == k::BUDGET_STOP)
        {
            return self
                .budget_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::SAY
        {
            let text = action.args["text"].as_str().unwrap_or_default().to_string();
            let _ = self
                .bot
                .answer_callback(callback_id, Some(&text), false)
                .await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            let origin = Origin::Telegram {
                chat_id,
                topic_id,
                message_id: None,
            };
            self.reply(chat_id, topic_id, Some(message_id), &format!("➡️ {text}"))
                .await?;
            self.daemon
                .enqueue_message(&action.target, &text, &origin, None)
                .await?;
            return Ok(());
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::SCREEN | k::SCREEN_DO | k::RUN_COMMAND
            )
        {
            return self
                .screen_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::ONBOARD_START
                    | k::ONBOARD_ANSWER
                    | k::ONBOARD_PAUSE
                    | k::ONBOARD_WRITE
                    | k::ONBOARD_CANCEL
            )
        {
            return self
                .onboarding_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::ELICIT_RETRY
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return self.elicitation_retry_clicked(action, chat_id).await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::ELICIT_ACCEPT | k::ELICIT_DECLINE | k::ELICIT_CANCEL | k::ELICIT_DONE
            )
        {
            return self
                .elicitation_clicked(callback_id, action, chat_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::FORM_NEXT | k::FORM_PREV | k::FORM_SUBMIT | k::FORM_DECLINE
            )
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return self.form_clicked(action, chat_id, topic_id).await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::REGENERATE
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return self.retry_clicked(action, chat_id).await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::CHOICE
            && action.args.get("visit").is_some()
        {
            return self
                .workflow_choice_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        // `answerCallbackQuery` d'abord : Telegram attend une réponse sous une seconde.
        let notice = match &outcome {
            ClickOutcome::Accepted(_) => None,
            ClickOutcome::AlreadyHandled(_) => Some("Déjà traité."),
            ClickOutcome::Expired => Some("Ce bouton a expiré."),
            ClickOutcome::Unknown => Some("Action inconnue."),
            ClickOutcome::NotOwner => Some("Non autorisé."),
        };
        let _ = self.bot.answer_callback(callback_id, notice, false).await;

        let ClickOutcome::Accepted(action) = outcome else {
            if matches!(
                outcome,
                ClickOutcome::Expired | ClickOutcome::AlreadyHandled(_)
            ) {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            }
            return Ok(());
        };

        let approval_id = action.target.clone();
        let Some(approval) = s.approvals.get(&approval_id).await? else {
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return Ok(());
        };
        let (chat_id, topic_id) = self
            .approval_destination(&approval, chat_id, topic_id)
            .await;
        let double = approval.payload["double"].as_bool().unwrap_or(false);

        match action.action.as_str() {
            k::APPROVE if double => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.send_destructive_confirm(chat_id, topic_id, &approval)
                    .await?;
            }
            k::APPROVE | k::CONFIRM_DESTRUCTIVE => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.finalize_decision(
                    &approval_id,
                    &Decision::approve_once("telegram"),
                    chat_id,
                    topic_id,
                )
                .await?;
            }
            k::APPROVE_RUN => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let d = Decision {
                    window: PolicyWindow::Session,
                    choice: "Pour cette session".into(),
                    ..Decision::approve_once("telegram")
                };
                self.finalize_decision(&approval_id, &d, chat_id, topic_id)
                    .await?;
            }
            k::APPROVE_ALWAYS => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.finalize_decision(
                    &approval_id,
                    &Decision::approve_always("telegram"),
                    chat_id,
                    topic_id,
                )
                .await?;
            }
            k::DENY => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                // « Pas encore » d'un lancement de workflow porte sa raison (issue #35).
                let reason = action.args["reason"].as_str().map(String::from);
                self.finalize_decision(
                    &approval_id,
                    &Decision::deny("telegram", reason),
                    chat_id,
                    topic_id,
                )
                .await?;
            }
            // Contradiction (#145) : remplacer, garder les deux avec un contexte, ou
            // ignorer. Les trois passent par le même chemin d'application.
            k::MEMORY_ACCEPT | k::MEMORY_AS_EXCEPTION | k::MEMORY_REJECT
                if approval.payload["contradiction"].as_bool() == Some(true) =>
            {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let keep = action.action != k::MEMORY_REJECT;
                let decision = if keep {
                    Decision {
                        choice: if action.action == k::MEMORY_ACCEPT {
                            "Remplacer".into()
                        } else {
                            "Exception".into()
                        },
                        ..Decision::approve_once("telegram")
                    }
                } else {
                    Decision {
                        choice: "Ignorer".into(),
                        ..Decision::deny("telegram", None)
                    }
                };
                let won = decide_approval(s, &approval_id, &decision).await?;
                let note = if !won {
                    "ℹ️ Déjà tranché.".to_string()
                } else {
                    match crate::ingest::apply_contradiction(
                        &self.daemon,
                        &approval_id,
                        &action.action,
                    )
                    .await
                    {
                        Ok(note) => note,
                        Err(e) => format!("❌ {e}"),
                    }
                };
                self.reply(chat_id, topic_id, None, &note).await?;
            }
            k::MEMORY_ACCEPT | k::MEMORY_REJECT => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let accept = action.action == k::MEMORY_ACCEPT;
                let decision = if accept {
                    Decision {
                        choice: "Tout".into(),
                        ..Decision::approve_once("telegram")
                    }
                } else {
                    Decision {
                        choice: "Rien".into(),
                        ..Decision::deny("telegram", None)
                    }
                };
                let won = decide_approval(s, &approval_id, &decision).await?;
                let note = match (accept, won) {
                    (false, _) => "🗑 Propositions écartées : rien n'entre en mémoire.".to_string(),
                    (true, true) => {
                        let confirm = s
                            .approvals
                            .get(&approval_id)
                            .await?
                            .is_some_and(|a| a.payload["confirm"].as_bool() == Some(true));
                        match crate::ingest::apply_memory_proposal(&self.daemon, &approval_id).await
                        {
                            Ok(n) if confirm => format!(
                                "✅ {n} règle(s) confirmée(s) : elles entrent en mémoire à la \
                                 prochaine consolidation (`/dream` pour tout de suite)."
                            ),
                            Ok(n) => format!("🧠 {n} fait(s) ajouté(s) à `notes.md`."),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    (true, false) => "ℹ️ Déjà tranché.".to_string(),
                };
                self.reply(chat_id, topic_id, None, &note).await?;
            }
            // Effet incertain (#83) : la décision tranche le ledger, sans règle.
            k::EFFECT_VERIFY | k::EFFECT_RETRY | k::EFFECT_IGNORE => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let decision = match action.action.as_str() {
                    k::EFFECT_VERIFY => Decision {
                        choice: crate::agent::EFFECT_DONE.into(),
                        ..Decision::approve_once("telegram")
                    },
                    k::EFFECT_RETRY => Decision {
                        choice: crate::agent::EFFECT_RETRY.into(),
                        ..Decision::approve_once("telegram")
                    },
                    _ => Decision {
                        choice: crate::agent::EFFECT_IGNORE.into(),
                        ..Decision::deny("telegram", None)
                    },
                };
                self.finalize_decision(&approval_id, &decision, chat_id, topic_id)
                    .await?;
            }
            k::DENY_REASON => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                s.kv_set(&approval_reason_key(chat_id, topic_id), &approval_id)
                    .await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    "✏️ Donne la raison du refus en un message.",
                )
                .await?;
            }
            other => {
                tracing::warn!(action = other, "action Telegram non prise en charge");
            }
        }
        Ok(())
    }
    /// Tranche, confirme, et remet la suite du tour en file.
    pub(super) async fn finalize_decision(
        &self,
        approval_id: &str,
        decision: &Decision,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let won = decide_approval(s, approval_id, decision).await?;
        let a = s.approvals.get(approval_id).await?;
        let Some(a) = a else { return Ok(()) };

        // Une autre décision est passée avant (CLI) : on le dit, sans rien rejouer.
        let first = won == decision.approved && a.decided_via.as_deref() == Some("telegram");
        let checkpoint = a.payload["checkpoint"].as_bool() == Some(true);
        let launch = a.subject == "workflow_start";
        let effect = a.kind == penelope_hitl::ApprovalKind::EffectUnknown;
        let note = match (first, a.state) {
            (true, _) if effect => match decision.choice.as_str() {
                crate::agent::EFFECT_DONE => {
                    format!("✅ Noté : `{}` a eu lieu, je ne le relance pas.", a.subject)
                }
                crate::agent::EFFECT_RETRY => format!("🔁 Je relance `{}`.", a.subject),
                _ => format!("⏭ `{}` reste tel quel, sans relance.", a.subject),
            },
            (false, st) => format!(
                "ℹ️ Déjà tranché : {} via {}.",
                st.as_str(),
                a.decided_via.clone().unwrap_or_default()
            ),
            (true, ApprovalState::Approved) if checkpoint => "▶️ Je continue.".to_string(),
            (true, _) if checkpoint => "⏹ Tour arrêté.".to_string(),
            (true, ApprovalState::Approved) if launch => "▶️ Je lance.".to_string(),
            (true, _) if launch => "⏸ Pas encore : on continue d'en parler.".to_string(),
            (true, ApprovalState::Approved) => format!("✅ {} : `{}`.", decision.choice, a.subject),
            (true, _) => format!("❌ Refusé : `{}`.", a.subject),
        };
        self.reply(chat_id, topic_id, None, &note).await?;

        if first && let Some(session) = &a.session_id {
            let origin = Origin::Telegram {
                chat_id,
                topic_id,
                message_id: None,
            };
            self.daemon
                .enqueue_resume(session, approval_id, &origin)
                .await?;
        }
        Ok(())
    }
    pub(super) async fn recorded_approval_destination(
        &self,
        a: &ApprovalRequest,
    ) -> Option<(i64, Option<i64>)> {
        let s = &self.daemon.services;
        let key = format!("tg.approval_destination.{}", a.id.as_str());
        if let Ok(Some(raw)) = s.kv_get(&key).await
            && let Ok(v) = serde_json::from_str::<Value>(&raw)
            && let Some(chat) = v["chat_id"].as_i64()
        {
            return Some((chat, v["topic_id"].as_i64()));
        }
        if let Some(run_id) = a.run_id.as_deref()
            && let Some(destination) = crate::workflow::origin_of(&self.daemon, run_id)
                .await
                .telegram_chat()
        {
            return Some(destination);
        }
        if let Some(sid) = a.session_id.as_deref()
            && let Ok(Some(session)) = s.sessions.get(sid).await
            && let Some(chat) = session.tg_chat_id
        {
            return Some((chat, session.tg_topic_id));
        }
        None
    }
    pub(super) async fn approval_destination(
        &self,
        a: &ApprovalRequest,
        clicked_chat: i64,
        clicked_topic: Option<i64>,
    ) -> (i64, Option<i64>) {
        // Le message du callback donne la destination la plus précise. Telegram ne
        // répète parfois pas son sujet : la destination persistée de la carte tranche.
        if clicked_topic.is_some() {
            return (clicked_chat, clicked_topic);
        }
        self.recorded_approval_destination(a)
            .await
            .unwrap_or((clicked_chat, clicked_topic))
    }
    /// Bouton « Réessayer » : relance, sauf si la conversation a continué depuis.
    async fn retry_clicked(&self, action: &Action, chat_id: i64) -> anyhow::Result<()> {
        let d = &self.daemon;
        let session = action.target.clone();
        let topic_id = action.args["topic_id"].as_i64();
        let answered = d
            .services
            .context
            .history
            .last_entry(&session)
            .await?
            .is_some_and(|e| {
                e.message.role == penelope_llm::types::Role::Assistant
                    && e.message.tool_calls.is_empty()
            });
        if answered {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    None,
                    "La conversation a continué depuis cet échec : rien à relancer.",
                )
                .await;
        }
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        d.enqueue_retry(&session, &origin, &action.token).await?;
        Ok(())
    }
}
