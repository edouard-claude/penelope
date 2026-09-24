//! Envoi : réponses, documents longs, file `tg_outbox` et ses échecs.

use super::*;

/// Longueur d'un fragment Markdown avant conversion HTML : marge pour les balises.
const FRAGMENT_CHARS: usize = 3_500;

const MAX_ATTEMPTS: i64 = 6;

/// Durée de validité du bouton « Réessayer » d'un tour échoué.
const RETRY_TTL_MS: i64 = 24 * 3600 * 1000;

/// Préfixe des notes d'échec d'envoi : elles n'en appellent pas d'autres (issue #101).
const FAILURE_NOTE: &str = "failnote-";

impl TelegramGateway {
    /// Échec d'un tour, avec un bouton « Réessayer » qui relance la réponse sur le même
    /// transcript.
    pub(super) async fn send_failure(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        session_id: &str,
        error: &str,
        turn_cost: Option<f64>,
    ) -> anyhow::Result<()> {
        let token = self
            .daemon
            .services
            .actions
            .create(
                k::REGENERATE,
                session_id,
                json!({"topic_id": topic_id}),
                RETRY_TTL_MS,
                true,
            )
            .await?;
        let error: String = error.chars().take(3_500).collect();
        // Plafond d'appels atteint : ce n'est pas un échec, et le même bouton continue
        // avec tout ce qui est déjà fait (issue #139).
        let (text, label) = if error.starts_with(crate::agent::CALLS_EXHAUSTED) {
            let n = crate::agent::TURN_CALLS;
            (
                format!(
                    "⏸ J'ai utilisé mes {n} appels pour ce tour et je m'arrête là. « Continuer » \
                     m'en redonne {n} : je reprends avec ce que j'ai déjà fait, dernier résultat \
                     compris.{} Pour une tâche longue, je peux aussi déléguer à un sous-agent.",
                    turn_cost
                        .filter(|c| *c > 0.0)
                        .map(|c| format!(" Coût de ce tour : {c:.2} $."))
                        .unwrap_or_default()
                ),
                format!("▶️ Continuer ({n} appels de plus)"),
            )
        } else {
            (format!("❌ {error}"), "🔁 Réessayer".to_string())
        };
        let mut payload = json!({
            "chat_id": chat_id,
            "text": markdown_to_html(&text),
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
            "reply_markup": inline_keyboard(&[vec![ButtonSpec::callback(
                &label,
                &token.token,
                "",
            )]]),
        });
        if let Some(t) = topic_id {
            payload["message_thread_id"] = json!(t);
        }
        if let Some(r) = reply_to {
            payload["reply_parameters"] =
                json!({"message_id": r, "allow_sending_without_reply": true});
        }
        self.outbox_push(chat_id, topic_id, "sendMessage", payload)
            .await
    }
    // ================================================================ envoi

    /// Répond en Markdown, découpé en fragments, par la file d'envoi.
    pub async fn reply(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        markdown: &str,
    ) -> anyhow::Result<()> {
        let body = if markdown.trim().is_empty() {
            "(réponse vide)".to_string()
        } else {
            markdown.to_string()
        };
        let fragments = penelope_telegram::split_message(&body, FRAGMENT_CHARS);
        for (i, fragment) in fragments.iter().enumerate() {
            let mut payload = json!({
                "chat_id": chat_id,
                "text": markdown_to_html(fragment),
                "parse_mode": "HTML",
                "link_preview_options": {"is_disabled": true},
            });
            if let Some(t) = topic_id {
                payload["message_thread_id"] = json!(t);
            }
            if let (0, Some(r)) = (i, reply_to) {
                payload["reply_parameters"] =
                    json!({"message_id": r, "allow_sending_without_reply": true});
            }
            self.outbox_push(chat_id, topic_id, "sendMessage", payload)
                .await?;
        }
        Ok(())
    }
    /// Envoie un texte, ou le joint en document quand il déborderait en chapelet de
    /// messages (§14.1, `telegram.max_fragments`). Le digest du matin s'en sert : six
    /// messages dont un qui coupe un identifiant en deux ne se lisent pas (issue #145).
    pub async fn reply_or_document(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        markdown: &str,
    ) -> anyhow::Result<()> {
        let fragments = penelope_telegram::split_message(markdown, FRAGMENT_CHARS);
        let max_fragments = self.daemon.services.config.config().telegram.max_fragments;
        if max_fragments > 0
            && penelope_telegram::render::should_send_as_document(fragments.len(), max_fragments)
            && let Some(first) = fragments.first()
            && self
                .send_long_as_document(chat_id, topic_id, markdown, first)
                .await
                .is_ok()
        {
            return Ok(());
        }
        self.reply(chat_id, topic_id, None, markdown).await
    }
    /// Texte trop long pour une bulle : un fichier joint, et une ligne qui dit ce que
    /// c'est (issue #145). Le fichier vit dans le répertoire de données, pas dans `/tmp`.
    async fn send_long_as_document(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        body: &str,
        head: &str,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let dirs = &s.platform.dirs;
        let dir = dirs.data().join("outgoing");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "penelope-{}.md",
            s.clock.now_rfc3339().replace(':', "-")
        ));
        std::fs::write(&path, penelope_observe::redact(body))?;
        let caption: String = head
            .lines()
            .next()
            .unwrap_or("Rapport")
            .chars()
            .take(180)
            .collect();
        // Un document part en `multipart`, pas en JSON : il ne passe pas par la file.
        let sent = self
            .bot
            .send_document(
                chat_id,
                topic_id,
                &path,
                Some(&format!("{caption} (texte complet en pièce jointe)")),
            )
            .await;
        // Telegram garde le fichier : le nôtre n'a plus de raison de traîner.
        let _ = std::fs::remove_file(&path);
        sent.map(|_| ()).map_err(|e| anyhow::anyhow!(e.to_string()))
    }
    pub(super) async fn outbox_push(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        method: &str,
        mut payload: Value,
    ) -> anyhow::Result<()> {
        if let Some(o) = payload.as_object_mut() {
            o.retain(|_, v| !v.is_null());
            // Ce qui part sur Telegram et reste dans la file est rédigé comme les
            // journaux : clés, mots de passe, valeurs lues (issue #134).
            for k in ["text", "caption"] {
                if let Some(v) = o.get_mut(k)
                    && let Some(t) = v.as_str()
                {
                    let red = penelope_observe::redact(t);
                    // Un message d'échec ne se lit pas sur cinq bulles, et une erreur
                    // bavarde recopie ce qu'elle n'aurait pas dû voir : le détail reste
                    // au journal (issue #148).
                    let cut = shorten_failure(&red);
                    if cut != t {
                        if cut != red {
                            tracing::warn!(error = %red, "échec tronqué avant envoi");
                        }
                        *v = Value::String(cut);
                    }
                }
            }
        }
        let s = &self.daemon.services;
        let (id, method, now) = (
            format!("o_{}", penelope_kernel::ids::Ulid::new()),
            method.to_string(),
            s.clock.now_rfc3339(),
        );
        s.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, topic_id, method, payload, state,
                        attempts, created_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6)",
                    params![id, chat_id, topic_id, method, payload.to_string(), now],
                )?;
                Ok(())
            })
            .await?;
        self.outbox_wake.notify_one();
        Ok(())
    }
    pub(super) async fn outbox_loop(self: Arc<Self>) {
        while !self.shutting_down() {
            match self.flush_outbox().await {
                Ok(0) => {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(1), self.outbox_wake.notified())
                            .await;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "file d'envoi Telegram");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
    /// Envoie ce qui est dû, dans l'ordre. Renvoie le nombre de lignes traitées.
    pub async fn flush_outbox(&self) -> anyhow::Result<usize> {
        let s = &self.daemon.services;
        let now = s.clock.now_rfc3339();
        let rows: Vec<(String, i64, String, String, i64)> = s
            .store
            .read(move |c| {
                // Tête de file par chat (issue #101) : un message qui attend sa nouvelle
                // tentative retient ceux qui le suivent dans le même chat, sinon la
                // réponse se lit dans le désordre. Les autres chats passent.
                let mut st = c.prepare(
                    "SELECT o.id, o.chat_id, o.method, o.payload, o.attempts FROM tg_outbox o
                     WHERE o.state = 'pending' AND (o.not_before IS NULL OR o.not_before <= ?1)
                       AND NOT EXISTS (
                         SELECT 1 FROM tg_outbox p
                         WHERE p.chat_id = o.chat_id AND p.state = 'pending'
                           AND p.not_before > ?1
                           AND (p.created_at < o.created_at
                                OR (p.created_at = o.created_at AND p.rowid < o.rowid)))
                     ORDER BY o.created_at, o.rowid LIMIT 25",
                )?;
                let rows = st.query_map([now], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?;
        let n = rows.len();
        let mut held: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for (id, chat_id, method, payload, attempts) in rows {
            // Un envoi de ce chat a échoué pendant ce passage : la suite attend.
            if held.contains(&chat_id) {
                continue;
            }
            let body: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            let result = match self.bot.call(&method, Some(chat_id), body.clone()).await {
                Err(TgError::Api {
                    code: 400,
                    description,
                }) if description.to_lowercase().contains("parse")
                    && body.get("parse_mode").is_some() =>
                {
                    // Telegram refuse le HTML : même contenu, en texte brut.
                    let mut plain = body.clone();
                    if let Some(o) = plain.as_object_mut() {
                        o.remove("parse_mode");
                        let t = o
                            .get("text")
                            .and_then(|t| t.as_str())
                            .map(html_to_plain)
                            .unwrap_or_default();
                        o.insert("text".into(), json!(t));
                    }
                    self.bot.call(&method, Some(chat_id), plain).await
                }
                other => other,
            };
            let now = s.clock.now_rfc3339();
            match result {
                Ok(v) => {
                    let message_id = v.get("message_id").and_then(|m| m.as_i64());
                    s.store
                        .write(move |tx| {
                            tx.execute(
                                "UPDATE tg_outbox SET state='sent', sent_at=?2, message_id=?3,
                                    attempts=attempts+1 WHERE id=?1",
                                params![id, now, message_id],
                            )?;
                            Ok(())
                        })
                        .await?;
                }
                Err(e) => {
                    let attempts = attempts + 1;
                    let give_up = attempts >= MAX_ATTEMPTS
                        || matches!(e, TgError::Api { code, .. } if (400..500).contains(&code) && code != 429);
                    let delay_ms = penelope_telegram::api::backoff_ms(attempts as u32) as i64;
                    let not_before =
                        chrono::DateTime::from_timestamp_millis(s.clock.now_ms() + delay_ms)
                            .unwrap_or_default()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    let err = e.to_string();
                    tracing::warn!(error = %err, attempts, give_up, "envoi Telegram en échec");
                    let row = id.clone();
                    let stored = err.clone();
                    s.store
                        .write(move |tx| {
                            tx.execute(
                                "UPDATE tg_outbox SET attempts=?2, error=?3, not_before=?4,
                                    state = CASE WHEN ?5 = 1 THEN 'failed' ELSE state END
                                 WHERE id=?1",
                                params![row, attempts, stored, not_before, give_up as i64],
                            )?;
                            Ok(())
                        })
                        .await?;
                    if !give_up {
                        held.insert(chat_id);
                    } else if !id.starts_with(FAILURE_NOTE) {
                        // Jamais de silence (#71) : le propriétaire sait qu'un message
                        // n'est pas parti, en texte brut, sans rien qui puisse être refusé
                        // à nouveau. Une note qui échoue n'en appelle pas une autre.
                        self.push_failure_note(chat_id, &err).await;
                    }
                }
            }
        }
        Ok(n)
    }
    /// Note d'échec définitif d'un envoi, en texte brut (issue #101).
    async fn push_failure_note(&self, chat_id: i64, error: &str) {
        let s = &self.daemon.services;
        let id = format!("{FAILURE_NOTE}{}", penelope_kernel::ids::Ulid::new());
        let text = format!(
            "⚠️ Un message n'a pas pu être envoyé ({}). Le détail est dans le journal du \
             daemon ; `/status` compte ces échecs.",
            error.chars().take(200).collect::<String>()
        );
        let payload = json!({"chat_id": chat_id, "text": text}).to_string();
        let now = s.clock.now_rfc3339();
        let _ = s
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                     VALUES(?1, ?2, 'sendMessage', ?3, 'pending', ?4)",
                    params![id, chat_id, payload, now],
                )?;
                Ok(())
            })
            .await;
    }
    /// Réaction d'état sur le message du propriétaire, sans jamais bloquer.
    pub(super) fn react(&self, chat_id: i64, message_id: i64, emoji: &'static str) {
        let bot = self.bot.clone();
        tokio::spawn(async move {
            let _ = bot.set_reaction(chat_id, message_id, emoji).await;
        });
    }
}

/// Longueur au-delà de laquelle un message d'échec est tronqué (issue #148).
const MAX_FAILURE_CHARS: usize = 500;

/// Tronque un message d'échec — par convention, ceux qui commencent par ❌ — en renvoyant
/// au journal pour le détail. Les autres messages passent intacts : une réponse longue du
/// modèle part en document, elle ne se coupe pas ici.
pub(super) fn shorten_failure(text: &str) -> String {
    if !text.trim_start().starts_with('❌') || text.chars().count() <= MAX_FAILURE_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_FAILURE_CHARS).collect();
    format!("{head}…\n\n(message tronqué ; détail dans `penelope logs`)")
}
