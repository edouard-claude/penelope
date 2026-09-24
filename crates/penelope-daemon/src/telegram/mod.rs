//! Passerelle Telegram (§14) : réception, commandes, approbations, brouillons, envoi.
//!
//! Trois boucles :
//! - **scrutation** : `getUpdates` en long polling, offset et updates persistés, chaque
//!   update traité une seule fois même après un crash ;
//! - **brouillons** : les fragments du bus deviennent un aperçu `sendMessageDraft`,
//!   limité en fréquence, jamais bloquant ;
//! - **envoi** : les réponses finales, cartes et retours de commandes passent par
//!   `tg_outbox`, dans l'ordre, avec reprise et repli en texte brut.

use crate::agent::{TurnEvent, TurnOutcome, decide_approval};
use crate::bus::{BusKind, ChannelDelivery, Origin};
use crate::executor::Messenger;
use crate::helpers::{
    BOT_USERNAME_KEY, SEEN_CHATS_KEY, chat_title_key, seen_chats, shown, topic_name_key,
};
use crate::runtime::Daemon;
use penelope_hitl::{ApprovalRequest, ApprovalState, Decision};
use penelope_kernel::api::method as m;
use penelope_kernel::risk::PolicyWindow;
use penelope_store::rusqlite::params;
use penelope_telegram::actions::{Action, ClickOutcome, kind as k};
use penelope_telegram::api::{BotTransport, HttpTransport, inline_keyboard, reaction};
use penelope_telegram::render::ButtonSpec;
use penelope_telegram::{Bot, Incoming, TgError, classify, html_to_plain, markdown_to_html};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

mod approvals;
mod bursts;
mod callbacks;
mod cards;
mod commands;
mod delivery;
mod drafts;
mod elicitation;
mod forms;
mod keys;
mod media;
mod menus;
mod onboarding;
mod outbox;
mod screens;
mod sessions_menu;
mod usage;

use bursts::{Held, TextBurst};
#[cfg(test)]
use bursts::{TELEGRAM_TEXT_LIMIT, held_key};
pub(crate) use cards::approval_card;
pub use cards::render_value;
use cards::{
    background_note, cancelled_note, codex_status_text, context_line, fmt_usd, mcp_list_text,
    mcp_show_text, mcp_state_icon, routing_text, schedules_text,
};
use drafts::Activity;
pub(crate) use drafts::StopReport;
use forms::form_key;
use keys::{
    approval_reason_key, chat_title_of, model_pin_notice, normalise_model_id, recent_log_lines,
    short_model, substitute, topic_name_of,
};
pub use keys::{parse_params, record_seen_chat};
use media::Album;
#[cfg(test)]
use media::{ALBUM_WINDOW, audio_filename};
#[cfg(test)]
use outbox::shorten_failure;

pub struct TelegramGateway {
    pub daemon: Arc<Daemon>,
    pub bot: Arc<Bot>,
    owner_id: i64,
    draft_interval: Duration,
    poll_timeout_s: u64,
    outbox_wake: Notify,
    /// Albums en cours de réception, par `media_group_id`.
    albums: Arc<std::sync::Mutex<HashMap<String, Album>>>,
    /// Morceaux d'un même envoi texte en cours de réception, par chat (issue #49).
    bursts: Arc<std::sync::Mutex<HashMap<i64, TextBurst>>>,
    /// Sorties mises de côté des sessions en arrière-plan : lues et réécrites sous verrou.
    held_lock: tokio::sync::Mutex<()>,
    /// Indicateurs d'activité des tours en cours, par tour (issue #121).
    activities: Arc<std::sync::Mutex<HashMap<String, Activity>>>,
    /// Intervalle de renvoi de l'indicateur, en millisecondes (Telegram l'efface au bout
    /// de cinq secondes).
    activity_every_ms: std::sync::atomic::AtomicU64,
}

impl TelegramGateway {
    /// Construit la passerelle depuis la configuration. `Ok(None)` : Telegram n'est
    /// pas configuré (pas de propriétaire ou pas de jeton), ce n'est pas une erreur.
    pub async fn from_config(daemon: Arc<Daemon>) -> Result<Option<Arc<Self>>, String> {
        let s = &daemon.services;
        let cfg = s.config.config();
        if cfg.owner.telegram_user_id == 0 {
            return Ok(None);
        }
        let token = match s.platform.secrets.expand(&cfg.telegram.token) {
            Ok(t) if !t.trim().is_empty() => t,
            _ => return Ok(None),
        };
        let transport =
            HttpTransport::new(&cfg.telegram.api_base, &token).map_err(|e| e.to_string())?;
        Ok(Some(Self::with_transport(
            daemon.clone(),
            Arc::new(transport),
        )))
    }

    pub fn with_transport(daemon: Arc<Daemon>, transport: Arc<dyn BotTransport>) -> Arc<Self> {
        let s = &daemon.services;
        let cfg = s.config.config();
        let bot = Arc::new(Bot::new(
            transport,
            cfg.telegram.rate_per_chat_per_s,
            s.clock.clone(),
        ));
        Arc::new(TelegramGateway {
            owner_id: cfg.owner.telegram_user_id,
            draft_interval: Duration::from_millis(cfg.telegram.draft_interval_ms.max(300)),
            poll_timeout_s: cfg.telegram.poll_timeout_s,
            outbox_wake: Notify::new(),
            albums: Arc::new(std::sync::Mutex::new(HashMap::new())),
            bursts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            held_lock: tokio::sync::Mutex::new(()),
            activities: Arc::new(std::sync::Mutex::new(HashMap::new())),
            activity_every_ms: std::sync::atomic::AtomicU64::new(4_000),
            daemon,
            bot,
        })
    }

    /// Branche la passerelle dans le daemon (messages, livraison).
    pub fn register(self: &Arc<Self>) {
        let hooks = &self.daemon.hooks;
        if let Ok(mut g) = hooks.messenger.write() {
            *g = Some(self.clone());
        }
        if let Ok(mut g) = hooks.telegram.write() {
            *g = Some(self.clone());
        }
        self.daemon.services.elicitations.attach(self.clone());
    }

    /// Vérifie le jeton, publie les commandes, lance les boucles.
    pub async fn start(self: &Arc<Self>) -> Result<Vec<tokio::task::JoinHandle<()>>, String> {
        let me =
            self.bot.get_me().await.map_err(|e| {
                format!("jeton Telegram refusé ({e}) : vérifier `telegram_bot_token`")
            })?;
        let username = me.get("username").and_then(|u| u.as_str()).unwrap_or("?");
        tracing::info!(bot = username, "Telegram connecté");
        // Liens profonds des textes longs (digest, audit) vers un écran précis (issue #30).
        let _ = self
            .daemon
            .services
            .kv_set(BOT_USERNAME_KEY, username)
            .await;
        if let Err(e) = self
            .bot
            .set_commands(penelope_telegram::commands::to_bot_commands())
            .await
        {
            tracing::warn!(error = %e, "setMyCommands refusé");
        }
        self.register();
        // Effets restés incertains après un arrêt brutal : la question part sans attendre
        // que le propriétaire pense à `/approvals` (#83).
        if let Err(e) = self.announce_uncertain_effects().await {
            tracing::warn!(error = %e, "effets incertains non annoncés");
        }
        // Surveillées : une panique relance la boucle au lieu de rendre le bot muet (#84).
        let (a, b, c) = (self.clone(), self.clone(), self.clone());
        let sup = self.daemon.supervision();
        Ok(vec![
            crate::tasks::spawn_supervised(&sup, "telegram.poll", move || a.clone().poll_loop()),
            crate::tasks::spawn_supervised(&sup, "telegram.drafts", move || b.clone().draft_loop()),
            crate::tasks::spawn_supervised(&sup, "telegram.outbox", move || {
                c.clone().outbox_loop()
            }),
        ])
    }

    fn shutting_down(&self) -> bool {
        self.daemon.handle.is_shutting_down()
    }

    /// Pousse chaque demande `effect_unknown` en attente, une fois : dans le chat de sa
    /// session, sinon en privé au propriétaire.
    pub async fn announce_uncertain_effects(&self) -> anyhow::Result<usize> {
        let s = &self.daemon.services;
        let mut sent = 0;
        for a in s.approvals.pending(200).await? {
            if a.kind != penelope_hitl::ApprovalKind::EffectUnknown {
                continue;
            }
            let flag = format!("tg.card.effect.{}", a.id.as_str());
            if s.kv_get(&flag).await?.is_some() {
                continue;
            }
            let (chat_id, topic_id) = self
                .recorded_approval_destination(&a)
                .await
                .unwrap_or_else(|| self.home_chat());
            self.send_approval_card(chat_id, topic_id, &a).await?;
            s.kv_set(&flag, "1").await?;
            sent += 1;
        }
        Ok(sent)
    }

    // ================================================================ réception

    async fn poll_loop(self: Arc<Self>) {
        let s = &self.daemon.services;
        let mut backoff = Duration::from_secs(1);
        while !self.shutting_down() {
            let offset = s
                .kv_get("tg.offset")
                .await
                .ok()
                .flatten()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            match self.bot.get_updates(offset, self.poll_timeout_s).await {
                Ok(updates) => {
                    backoff = Duration::from_secs(1);
                    for u in updates {
                        let id = u.get("update_id").and_then(|v| v.as_i64()).unwrap_or(0);
                        if let Err(e) = self.process_update(&u).await {
                            tracing::error!(update = id, error = %e, "update Telegram en échec");
                            // Jamais de silence : le propriétaire voit l'erreur au lieu de
                            // retaper ou de croire que c'est fait (issue #71).
                            self.report_failure(&u, &e).await;
                        }
                        let _ = s.kv_set("tg.offset", &(id + 1).to_string()).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "getUpdates en échec, nouvelle tentative");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    /// Dit au propriétaire qu'un update n'a pas pu être traité, dans la conversation où il
    /// l'a envoyé (issue #71).
    async fn report_failure(&self, update: &Value, error: &anyhow::Error) {
        let msg = update
            .get("message")
            .or_else(|| update.get("edited_message"))
            .or_else(|| update.pointer("/callback_query/message"));
        let chat_id = msg
            .and_then(|m| m.pointer("/chat/id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(self.owner_id);
        let topic_id = msg
            .and_then(|m| m.get("message_thread_id"))
            .and_then(|v| v.as_i64());
        let reply_to = msg
            .and_then(|m| m.get("message_id"))
            .and_then(|v| v.as_i64());
        let text = format!("⚠️ Ta demande n'a pas pu être traitée : {error}");
        if let Err(e) = self.reply(chat_id, topic_id, reply_to, &text).await {
            tracing::warn!(error = %e, "échec non signalé au propriétaire");
        }
    }

    /// Traite un update. Idempotent : un update déjà traité est ignoré.
    pub async fn process_update(self: &Arc<Self>, update: &Value) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let update_id = update
            .get("update_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let (now, raw) = (s.clock.now_rfc3339(), update.to_string());
        let already = s
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO tg_updates(update_id, received_at, processed, payload)
                     VALUES(?1, ?2, 0, ?3)",
                    params![update_id, now, raw],
                )?;
                let processed: i64 = tx.query_row(
                    "SELECT processed FROM tg_updates WHERE update_id = ?1",
                    [update_id],
                    |r| r.get(0),
                )?;
                Ok(processed == 1)
            })
            .await?;
        if already {
            return Ok(());
        }

        // Conversations autorisées relues à chaque update : `config set` s'applique à
        // chaud (issue #113).
        let access = penelope_telegram::Access {
            owner_id: self.owner_id,
            allowed_chats: s.config.config().telegram.allowed_chats.clone(),
        };
        // Nom du sujet Telegram : il peut donner son sujet de travail à la session (#119).
        // Titre du groupe : il nomme où livre une planification (#124).
        let names = topic_name_of(update)
            .map(|(chat, topic, name)| (chat, topic_name_key(chat, topic), name))
            .into_iter()
            .chain(chat_title_of(update).map(|(chat, t)| (chat, chat_title_key(chat), t)));
        for (chat, key, name) in names {
            if access.allowed_chats.contains(&chat)
                && self
                    .daemon
                    .services
                    .kv_get(&key)
                    .await
                    .ok()
                    .flatten()
                    .as_deref()
                    != Some(name.as_str())
            {
                let _ = s.kv_set(&key, &name).await;
            }
        }
        let incoming = classify(update, &access);
        self.handle(incoming).await?;

        // Traité : seul `update_id` sert encore (déduplication). Le texte intégral n'a
        // plus de raison d'être gardé (issue #46).
        s.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE tg_updates SET processed = 1, payload = '{}' WHERE update_id = ?1",
                    [update_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // gel 0.17 : lot G (telegram/mod.rs)
    async fn handle(self: &Arc<Self>, incoming: Incoming) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        match incoming {
            Incoming::Text {
                update_id,
                chat_id,
                message_id,
                topic_id,
                text,
                forwarded,
                ..
            } => {
                // Renommage demandé depuis le menu `/sessions` il y a moins de 5 min : ce
                // message est le titre.
                let title_key = format!("tg.await_title.{chat_id}");
                if let Some(raw) = s.kv_get(&title_key).await?
                    && let Some((target, at)) = raw.split_once(' ')
                    && s.clock.now_ms() - at.parse::<i64>().unwrap_or(0) < 5 * 60_000
                {
                    let target = target.to_string();
                    s.kv_set(&title_key, "").await?;
                    let note = match crate::titles::clean(&text) {
                        Some(title) => {
                            s.sessions.set_title(&target, &title, false).await?;
                            format!("✏️ Session renommée : « {title} ».")
                        }
                        None => "Titre vide : rien n'a changé.".to_string(),
                    };
                    return self.reply(chat_id, topic_id, Some(message_id), &note).await;
                }
                // Entretien d'accueil en cours : ce message répond à la question posée.
                let onboard_key = format!("tg.onboard.{chat_id}");
                if let Some(raw) = s.kv_get(&onboard_key).await?
                    && let Ok(v) = serde_json::from_str::<Value>(&raw)
                    && let Some(rel) = v["rel"].as_str()
                {
                    let n = v["n"].as_u64().unwrap_or(0) as u32;
                    return match crate::onboarding::answer(
                        &self.daemon.services,
                        rel,
                        n,
                        Some(&text),
                    )
                    .await
                    {
                        Ok(sitting) => self.onboarding_ask(chat_id, topic_id, &sitting).await,
                        Err(e) => {
                            self.reply(chat_id, topic_id, Some(message_id), &format!("⚠️ {e}"))
                                .await
                        }
                    };
                }
                // Un formulaire d'étape `user` est en cours : ce message remplit le champ.
                if let Some(raw) = self.form_pending(chat_id, topic_id).await?
                    && !raw.is_empty()
                {
                    return self.form_input(chat_id, &raw, Some(&text)).await;
                }
                // Une saisie était attendue par une étape `user` de workflow.
                let input_key = format!("tg.await_input.{chat_id}");
                if let Some(raw) = s.kv_get(&input_key).await?
                    && !raw.is_empty()
                {
                    s.kv_set(&input_key, "").await?;
                    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                    let note = match crate::workflow::answer(
                        &self.daemon,
                        v["run"].as_str().unwrap_or_default(),
                        v["visit"].as_str().unwrap_or_default(),
                        v["choice"].as_str().unwrap_or_default(),
                        Some(&text),
                    )
                    .await
                    {
                        Ok(()) => "✔️ Réponse transmise au workflow.".to_string(),
                        Err(e) => format!("ℹ️ {e}"),
                    };
                    return self.reply(chat_id, topic_id, Some(message_id), &note).await;
                }

                // Une raison de refus était attendue : ce message la donne.
                let reason_key = approval_reason_key(chat_id, topic_id);
                if let Some(approval_id) = s.kv_get(&reason_key).await?
                    && !approval_id.is_empty()
                {
                    s.kv_set(&reason_key, "").await?;
                    let d = Decision::deny("telegram", Some(text.clone()));
                    self.finalize_decision(&approval_id, &d, chat_id, topic_id)
                        .await?;
                    return Ok(());
                }

                // Profil vide : l'accueil est proposé une fois, sans retenir le message.
                if s.kv_get("tg.onboard.proposed").await?.is_none()
                    && crate::onboarding::profile_is_empty(&self.daemon.services).await
                {
                    s.kv_set("tg.onboard.proposed", &s.clock.now_rfc3339())
                        .await?;
                    self.propose_onboarding(chat_id, topic_id).await?;
                }
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: Some(message_id),
                };
                let session = self.daemon.chat_session_for(&origin).await?;
                let content = if forwarded {
                    penelope_observe::injection::wrap_untrusted("message transféré", &text)
                } else {
                    text
                };
                self.react(chat_id, message_id, reaction::RECEIVED);
                // Les morceaux d'un même envoi attendent la fin de la fenêtre et partent
                // en un seul tour (issue #49) ; un message court tapé part tout de suite
                // (issue #96).
                self.buffer_text(&origin, &session, message_id, update_id, content, forwarded)
                    .await?;
            }
            Incoming::Command {
                chat_id,
                topic_id,
                message_id,
                command,
                args,
                ..
            } => {
                Box::pin(self.command(chat_id, topic_id, message_id, &command, &args)).await?;
            }
            Incoming::Callback {
                callback_id,
                data,
                message_id,
                chat_id,
                topic_id,
                from_id,
                ..
            } => {
                self.callback(&callback_id, &data, chat_id, topic_id, message_id, from_id)
                    .await?;
            }
            Incoming::StoppedGeneration { draft_id, .. } => {
                if let Some(session) = self.daemon.bus.session_for_draft(draft_id) {
                    self.daemon.bus.cancel_session(&session);
                }
            }
            Incoming::Voice {
                update_id,
                chat_id,
                message_id,
                topic_id,
                file_id,
                file_name,
                mime_type,
                file_size,
                ..
            } => {
                // Téléchargement puis transcription : détachés, sinon `/stop` et les
                // boutons attendent la fin (issue #69).
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me
                        .voice(
                            update_id,
                            chat_id,
                            topic_id,
                            message_id,
                            &file_id,
                            file_name.as_deref(),
                            mime_type.as_deref(),
                            file_size,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "vocal Telegram non traité");
                    }
                });
            }
            photo @ Incoming::Photo { .. } => {
                // Téléchargement de la photo : détaché aussi (issue #69).
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.photo(photo).await {
                        tracing::warn!(error = %e, "photo Telegram non traitée");
                    }
                });
            }
            document @ Incoming::Document { .. } => {
                // Téléchargement du document : détaché comme vocaux et photos, sinon
                // `/stop` et les boutons attendent jusqu'à 20 Mo (issue #98).
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.document(document).await {
                        tracing::warn!(error = %e, "document Telegram non traité");
                    }
                });
            }
            Incoming::OAuthCallback { chat_id, url, .. } => {
                // Adresse de retour collée (§8.5, `paste_back`) : elle ne sert qu'une fois.
                match crate::mcp_auth::complete(&self.daemon, &url).await {
                    Ok(server) => crate::mcp_auth::reconnect_and_tell(&self.daemon, &server).await,
                    Err(e) => {
                        self.reply(
                            chat_id,
                            None,
                            None,
                            &format!("🔐 Autorisation impossible : {e}"),
                        )
                        .await?
                    }
                }
            }
            Incoming::Edited { .. } => {}
            Incoming::Unauthorized { from_id, .. } => {
                // Aucune réponse : ne pas confirmer l'existence du bot à un inconnu.
                tracing::warn!(from = from_id, "message Telegram d'un inconnu ignoré");
            }
            Incoming::ForeignChat {
                chat_id,
                chat_type,
                title,
                from_id,
                ..
            } => {
                // Silence dans la conversation, mais l'identifiant est dit : c'est ce qu'il
                // faut ajouter à `telegram.allowed_chats` (issue #113).
                tracing::warn!(
                    chat = chat_id,
                    kind = %chat_type,
                    title = %title,
                    from = from_id,
                    "conversation Telegram non autorisée ignorée : \
                     telegram.allowed_chats"
                );
                record_seen_chat(s, chat_id, &chat_type, &title).await;
            }
            Incoming::Ignored { reason, .. } => {
                tracing::debug!(%reason, "update ignoré");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
