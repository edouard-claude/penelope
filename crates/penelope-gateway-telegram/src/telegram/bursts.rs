//! Rafales de texte et messages retenus hors du sujet actif (issue #49).

use super::*;

/// Sortie d'une session en arrière-plan, mise de côté jusqu'à son retour au focus du chat
/// (issue #10).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Held {
    Text {
        text: String,
        reply_to: Option<i64>,
        /// Réponse finale d'un tour : réaction ✅ sur le message d'origine.
        answer: bool,
        /// Suites proposées en boutons (boucle arrêtée, issue #31).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        choices: Vec<String>,
    },
    Approval {
        id: String,
    },
    Failure {
        error: String,
        reply_to: Option<i64>,
    },
    File {
        path: String,
        caption: Option<String>,
    },
    /// Message vocal (issue #41).
    Voice {
        path: String,
        duration_s: u32,
        caption: Option<String>,
    },
}

pub(super) fn held_key(session_id: &str) -> String {
    format!("tg.held.{session_id}")
}

/// « 1 réponse », « 3 approbations ».
fn count_of(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n > 1 { many } else { one })
}

/// Un long texte collé arrive découpé par Telegram en messages de 4 096 caractères : un
/// morceau de cette taille appelle la suite sans séparateur (issue #49).
pub(super) const TELEGRAM_TEXT_LIMIT: usize = 4_000;

/// Silence qui clôt une rafale après un message court, sa fin probable (issue #96).
const TAIL_QUIET: Duration = Duration::from_millis(300);

/// Morceaux d'un même envoi, en attente de leur fin de fenêtre (issue #49).
#[derive(Debug, Clone)]
pub(super) struct TextBurst {
    pub(super) origin: Origin,
    pub(super) session: String,
    pub(super) parts: Vec<String>,
    pub(super) message_ids: Vec<i64>,
    /// `update_id` du premier morceau : clé de déduplication du tour.
    pub(super) update_id: i64,
    pub(super) chars: usize,
    /// Instant à partir duquel la rafale est considérée comme finie.
    pub(super) deadline: std::time::Instant,
}

impl TextBurst {
    /// Recolle les morceaux : un morceau à la limite de Telegram est la suite du
    /// précédent, les autres sont des messages distincts.
    fn joined(&self) -> String {
        let mut out = String::new();
        for (i, part) in self.parts.iter().enumerate() {
            if i > 0 && self.parts[i - 1].chars().count() < TELEGRAM_TEXT_LIMIT {
                out.push_str("\n\n");
            }
            out.push_str(part);
        }
        out
    }
}

impl TelegramGateway {
    // ---------------------------------------------------------- rafales (#49)

    /// Met un morceau de côté le temps de la fenêtre de regroupement. Tant que la rafale
    /// n'est pas finie, un nouveau message s'y ajoute au lieu de créer un tour de plus.
    /// Fenêtre adaptative (issue #96) : seul un morceau qui ressemble à une coupure de
    /// Telegram (≥ 4 000 caractères) ou un message transféré ouvre ou prolonge une rafale ;
    /// un message court tapé part aussitôt, ou ferme la rafale ouverte après un court
    /// silence s'il en est la fin.
    ///
    /// ```text
    ///  court, rien d'ouvert ─────────────► tour tout de suite
    ///  long ou transféré ────────────────► rafale, attente de la fenêtre (2 s)
    ///  court, rafale ouverte ────────────► dernier morceau probable : 300 ms de silence
    /// ```
    pub(super) async fn buffer_text(
        self: &Arc<Self>,
        origin: &Origin,
        session: &str,
        message_id: i64,
        update_id: i64,
        text: String,
        forwarded: bool,
    ) -> anyhow::Result<()> {
        let cfg = self.daemon.services.config.config();
        let window = cfg.telegram.text_group_window_ms;
        let chat_id = match origin {
            Origin::Telegram { chat_id, .. } => *chat_id,
            _ => 0,
        };
        let piece = forwarded || text.chars().count() >= TELEGRAM_TEXT_LIMIT;
        let open = self
            .bursts
            .lock()
            .map(|g| g.contains_key(&chat_id))
            .unwrap_or(false);
        if window == 0 || (!piece && !open) {
            self.daemon
                .enqueue_message(session, &text, origin, Some(format!("tg:{update_id}")))
                .await?;
            // Signe de vie dès la mise en file, avant que le tour démarre (issue #121).
            if let Some((chat, topic)) = origin.telegram_chat() {
                self.chat_action(chat, topic, "typing");
            }
            return Ok(());
        }
        let wait = if piece {
            Duration::from_millis(window)
        } else {
            TAIL_QUIET.min(Duration::from_millis(window))
        };
        let deadline = std::time::Instant::now() + wait;
        let first = {
            let mut g = self
                .bursts
                .lock()
                .map_err(|_| anyhow::anyhow!("rafales verrouillées"))?;
            match g.get_mut(&chat_id) {
                Some(b) => {
                    b.chars += text.chars().count();
                    b.parts.push(text);
                    b.message_ids.push(message_id);
                    b.deadline = deadline;
                    false
                }
                None => {
                    g.insert(
                        chat_id,
                        TextBurst {
                            origin: origin.clone(),
                            session: session.to_string(),
                            chars: text.chars().count(),
                            parts: vec![text],
                            message_ids: vec![message_id],
                            update_id,
                            deadline,
                        },
                    );
                    true
                }
            }
        };
        if first {
            let me = self.clone();
            tokio::spawn(async move { me.flush_burst(chat_id).await });
        }
        Ok(())
    }
    /// Attend la fin de la rafale, puis en fait un tour unique (ou demande quoi en faire).
    async fn flush_burst(self: Arc<Self>, chat_id: i64) {
        loop {
            let left = {
                let g = match self.bursts.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                match g.get(&chat_id) {
                    Some(b) => b
                        .deadline
                        .saturating_duration_since(std::time::Instant::now()),
                    None => return,
                }
            };
            if left.is_zero() {
                break;
            }
            tokio::time::sleep(left).await;
        }
        let burst = match self.bursts.lock() {
            Ok(mut g) => g.remove(&chat_id),
            Err(_) => None,
        };
        let Some(burst) = burst else { return };
        if let Err(e) = self.deliver_burst(burst).await {
            tracing::warn!(error = %e, "rafale Telegram non transmise");
        }
    }
    /// Un seul tour pour toute la rafale, sauf si elle dépasse les seuils : Pénélope
    /// demande alors quoi en faire avant de dépenser quoi que ce soit.
    async fn deliver_burst(self: &Arc<Self>, burst: TextBurst) -> anyhow::Result<()> {
        let cfg = self.daemon.services.config.config();
        let too_many =
            cfg.telegram.burst_messages > 0 && burst.parts.len() >= cfg.telegram.burst_messages;
        let too_long = cfg.telegram.burst_chars > 0 && burst.chars >= cfg.telegram.burst_chars;
        if too_many || too_long {
            return self.ask_about_burst(burst).await;
        }
        self.daemon
            .enqueue_message(
                &burst.session,
                &burst.joined(),
                &burst.origin,
                Some(format!("tg:{}", burst.update_id)),
            )
            .await?;
        if let Some((chat, topic)) = burst.origin.telegram_chat() {
            self.chat_action(chat, topic, "typing");
        }
        Ok(())
    }
    /// Carte de rafale : ce qui est arrivé, et quatre façons de le traiter.
    pub(super) async fn ask_about_burst(&self, burst: TextBurst) -> anyhow::Result<()> {
        let (chat_id, topic_id, reply_to) = match burst.origin {
            Origin::Telegram {
                chat_id,
                topic_id,
                message_id,
            } => (chat_id, topic_id, message_id),
            _ => (0, None, None),
        };
        let id = penelope_kernel::ids::Ulid::new().to_string();
        let stored = json!({
            "session": burst.session,
            "message_id": reply_to,
            "parts": burst.parts,
            "joined": burst.joined(),
        });
        self.daemon
            .services
            .kv_set(&format!("tg.burst.{id}"), &stored.to_string())
            .await?;
        let screen = screens::Screen {
            text: format!(
                "📥 Tu m'as envoyé {} messages ({} caractères). Qu'est-ce que j'en fais ?",
                burst.parts.len(),
                burst.chars
            ),
            rows: vec![
                vec![
                    self.op(
                        "📄 Un seul document",
                        "burst.one",
                        json!({"id": id}),
                        Value::Null,
                    )
                    .await?,
                ],
                vec![
                    self.op(
                        "📥 Ingérer sans répondre",
                        "burst.ingest",
                        json!({"id": id}),
                        Value::Null,
                    )
                    .await?,
                ],
                vec![
                    self.op("1️⃣ Un par un", "burst.each", json!({"id": id}), Value::Null)
                        .await?,
                    self.op(
                        "🗑 Tout annuler",
                        "burst.drop",
                        json!({"id": id}),
                        Value::Null,
                    )
                    .await?,
                ],
            ],
        };
        self.send_screen(chat_id, topic_id, reply_to, screen, None)
            .await
    }
    /// Vrai quand une autre session a le focus du chat : la sortie de `session_id` est
    /// alors mise de côté. Seules les sessions de conversation actives sont concernées, et un
    /// chat sans session liée reçoit tout.
    pub(super) async fn out_of_focus(
        &self,
        session_id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> bool {
        let s = &self.daemon.services;
        match s.sessions.find_by_topic(chat_id, topic_id).await {
            Ok(Some(focused)) if focused.id.as_str() != session_id => {}
            _ => return false,
        }
        matches!(
            s.sessions.get(session_id).await,
            Ok(Some(sess)) if sess.kind == penelope_kernel::session::SessionKind::Chat
                && sess.state == "active"
        )
    }
    /// Met une sortie de côté et tient à jour l'unique notification de la session, avec
    /// son bouton pour basculer.
    pub(super) async fn hold(
        &self,
        session_id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
        item: Held,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        use penelope_telegram::render::escape_html;
        let _guard = self.held_lock.lock().await;
        let key = held_key(session_id);
        let mut held: Value = s
            .kv_get(&key)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| json!({"chat_id": chat_id, "topic_id": topic_id, "items": []}));
        let mut items: Vec<Held> =
            serde_json::from_value(held["items"].clone()).unwrap_or_default();
        items.push(item);
        held["items"] = serde_json::to_value(&items)?;

        let approvals = items
            .iter()
            .filter(|h| matches!(h, Held::Approval { .. }))
            .count();
        let mut parts = Vec::new();
        if items.len() > approvals {
            parts.push(count_of(items.len() - approvals, "réponse", "réponses"));
        }
        if approvals > 0 {
            parts.push(count_of(approvals, "approbation", "approbations"));
        }
        let title = s
            .sessions
            .get(session_id)
            .await?
            .and_then(|sess| sess.title)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "(sans titre)".into());
        let html = format!(
            "📬 {} en attente dans « {} »",
            parts.join(" et "),
            escape_html(&title)
        );
        let token = s
            .actions
            .create(
                k::SESSION_SWITCH,
                session_id,
                json!({"notice": true}),
                7 * 24 * 3_600_000,
                true,
            )
            .await?;
        let keyboard =
            inline_keyboard(&[vec![ButtonSpec::callback("↪️ Basculer", &token.token, "")]]);
        let edited = match held["notice"].as_i64() {
            Some(id) => self
                .bot
                .edit_text(chat_id, id, &html, Some(keyboard.clone()))
                .await
                .is_ok(),
            None => false,
        };
        if !edited {
            let mut payload = json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "disable_notification": true,
                "reply_markup": keyboard,
            });
            if let Some(t) = topic_id {
                payload["message_thread_id"] = json!(t);
            }
            let sent = self
                .bot
                .call(
                    penelope_telegram::api::method::SEND_MESSAGE,
                    Some(chat_id),
                    payload,
                )
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            held["notice"] = sent["message_id"].clone();
        }
        s.kv_set(&key, &held.to_string()).await?;
        Ok(())
    }
    /// Retour au focus : les sorties mises de côté partent dans l'ordre, approbations encore
    /// ouvertes comprises. Renvoie le nombre de sorties envoyées.
    pub(super) async fn flush_held(&self, session_id: &str) -> anyhow::Result<usize> {
        let s = &self.daemon.services;
        let _guard = self.held_lock.lock().await;
        let key = held_key(session_id);
        let Some(held) = s
            .kv_get(&key)
            .await?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            return Ok(0);
        };
        s.kv_delete(&key).await?;
        let chat_id = held["chat_id"].as_i64().unwrap_or(self.owner_id);
        let topic_id = held["topic_id"].as_i64();
        let items: Vec<Held> = serde_json::from_value(held["items"].clone()).unwrap_or_default();
        if let Some(notice) = held["notice"].as_i64() {
            let _ = self
                .bot
                .edit_text(
                    chat_id,
                    notice,
                    "📬 Réponses mises de côté : envoyées ci-dessous.",
                    None,
                )
                .await;
        }
        for item in &items {
            match item {
                Held::Text {
                    text,
                    reply_to,
                    answer,
                    choices,
                } => {
                    if choices.is_empty() {
                        self.reply(chat_id, topic_id, *reply_to, text).await?;
                    } else {
                        self.send_choices(chat_id, topic_id, *reply_to, session_id, text, choices)
                            .await?;
                    }
                    if let (true, Some(mid)) = (answer, reply_to) {
                        self.react(chat_id, *mid, reaction::DONE);
                    }
                }
                Held::Approval { id } => {
                    if let Some(a) = s.approvals.get(id).await?
                        && a.state == ApprovalState::Pending
                    {
                        self.send_approval_card(chat_id, topic_id, &a).await?;
                    }
                }
                Held::Failure { error, reply_to } => {
                    self.send_failure(chat_id, topic_id, *reply_to, session_id, error, None)
                        .await?;
                }
                Held::File { path, caption } => {
                    if let Err(e) = self
                        .bot
                        .send_document(chat_id, topic_id, Path::new(path), caption.as_deref())
                        .await
                    {
                        tracing::warn!(error = %e, "fichier mis de côté non envoyé");
                    }
                }
                Held::Voice {
                    path,
                    duration_s,
                    caption,
                } => {
                    if let Err(e) = self
                        .bot
                        .send_voice(
                            chat_id,
                            topic_id,
                            Path::new(path),
                            *duration_s,
                            caption.as_deref(),
                            None,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "vocal mis de côté non envoyé");
                    }
                }
            }
        }
        Ok(items.len())
    }
}
