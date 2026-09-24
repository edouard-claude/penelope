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
        let d = self.daemon.clone();
        Ok(vec![
            crate::tasks::spawn_supervised(d.clone(), "telegram.poll", move || {
                a.clone().poll_loop()
            }),
            crate::tasks::spawn_supervised(d.clone(), "telegram.drafts", move || {
                b.clone().draft_loop()
            }),
            crate::tasks::spawn_supervised(d, "telegram.outbox", move || c.clone().outbox_loop()),
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
                    return match crate::onboarding::answer(&self.daemon, rel, n, Some(&text)).await
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
                    && crate::onboarding::profile_is_empty(&self.daemon).await
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

    // ================================================================ commandes

    #[allow(clippy::too_many_lines)] // gel 0.17 : table de dispatch des commandes, lot G (telegram/commands/*.rs)
    async fn command(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let rpc = crate::rpc::Rpc::new(d.clone());
        let args = args.trim();
        let reply_to = Some(message_id);

        let text: String = match command {
            "start" | "help" => {
                // `/start <charge>` : lien profond vers un écran (issue #30).
                if !args.is_empty() {
                    return self
                        .open_deep_link(chat_id, topic_id, message_id, args)
                        .await;
                }
                return self
                    .show_screen(chat_id, topic_id, reply_to, "help", &json!({}), None)
                    .await;
            }
            "new" => {
                // `/new !titre` ferme l'ancienne session sans redemander, `/new ~titre` la
                // garde en fond : ce sont les deux boutons de la question ci-dessous.
                let (choice, args) = match args.trim_start().chars().next() {
                    Some('!') => (Some(true), args.trim_start()[1..].trim()),
                    Some('~') => (Some(false), args.trim_start()[1..].trim()),
                    _ => (None, args),
                };
                if let Some(old) = s.sessions.find_by_topic(chat_id, topic_id).await? {
                    let queued = s.turns.queued_for(old.id.as_str()).await?;
                    // Une session qui travaille encore : dire ce que `/new` lui ferait,
                    // avant de le faire (issue #112).
                    if choice.is_none() && queued > 0 {
                        let rows = vec![
                            vec![
                                self.command_button_with(
                                    "⏳ Garder l'ancienne en fond",
                                    "new",
                                    &format!("~{args}"),
                                )
                                .await?,
                            ],
                            vec![
                                self.command_button_with(
                                    &format!("🗑 Fermer ({queued} tour(s) perdu(s))"),
                                    "new",
                                    &format!("!{args}"),
                                )
                                .await?,
                            ],
                        ];
                        let text = format!(
                            "La session « {} » a {queued} tour(s) en file ou en cours. \
                             `/new` la ferme et **ils seront perdus**. La garder en fond : \
                             elle finit son travail et ses réponses t'attendent à ton retour \
                             (`/sessions`).",
                            crate::titles::label(&old)
                        );
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                screens::Screen { text, rows },
                                None,
                            )
                            .await;
                    }
                    if choice != Some(false) {
                        crate::session_ops::silence(d, old.id.as_str(), "nouvelle session").await?;
                        s.sessions.set_state(old.id.as_str(), "closed").await?;
                        // `/new` clôt aussi l'épisode en cours : il est relu (§6.6).
                        crate::episodes::spawn_ingest(
                            d.clone(),
                            old.id.to_string(),
                            old.episode_seq,
                            crate::episodes::Boundary::NewSession,
                        );
                    }
                }
                let title = (!args.is_empty())
                    .then(|| crate::titles::clean(args))
                    .flatten();
                let sess = s
                    .sessions
                    .create(penelope_kernel::session::SessionKind::Chat, title.clone())
                    .await?;
                s.sessions
                    .bind_telegram(sess.id.as_str(), chat_id, topic_id)
                    .await?;
                if let Some(title) = title {
                    // Notes d'une session sur le même sujet : proposées (issue #32).
                    let similar = crate::session_notes::similar(s, sess.id.as_str(), &title).await;
                    let text = format!("🆕 Nouvelle session « {title} » (`{}`).", sess.id);
                    if similar.is_empty() {
                        text
                    } else {
                        let mut rows = Vec::new();
                        for (from, label) in &similar {
                            let t = s
                                .actions
                                .create(
                                    k::SCREEN_DO,
                                    "notes.adopt",
                                    json!({"params": {"from": from, "to": sess.id.to_string()}, "back": null}),
                                    24 * 3_600_000,
                                    true,
                                )
                                .await?;
                            rows.push(vec![ButtonSpec::callback(
                                &format!(
                                    "📓 Reprendre les notes de « {} »",
                                    label.chars().take(40).collect::<String>()
                                ),
                                &t.token,
                                "",
                            )]);
                        }
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                screens::Screen {
                                    text: format!(
                                        "{text}\nDes notes de travail existent sur un sujet proche."
                                    ),
                                    rows,
                                },
                                None,
                            )
                            .await;
                    }
                } else {
                    // Sans titre : le message sera complété quand le titre automatique arrive.
                    let text = format!(
                        "🆕 Nouvelle session `{}`. Son titre suivra le premier échange.",
                        sess.id
                    );
                    let sent = self
                        .bot
                        .send_text(
                            chat_id,
                            topic_id,
                            &markdown_to_html(&text),
                            None,
                            Some(message_id),
                        )
                        .await;
                    match sent
                        .ok()
                        .and_then(|v| v.get("message_id").and_then(|m| m.as_i64()))
                    {
                        Some(mid) => {
                            s.kv_set(
                                &format!("tg.new_session.{}", sess.id),
                                &format!("{chat_id}:{mid}"),
                            )
                            .await?;
                            return Ok(());
                        }
                        None => text,
                    }
                }
            }
            "title" => {
                let session = d.chat_session_for(&origin).await?;
                match crate::titles::clean(args) {
                    None => {
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                Self::typed_screen(
                                    "✏️ Nouveau titre de la session : `/title` suivi du titre.",
                                    "✏️ Écrire le titre",
                                    "/title ",
                                ),
                                None,
                            )
                            .await;
                    }
                    Some(title) => {
                        s.sessions.set_title(&session, &title, false).await?;
                        format!("✏️ Session renommée : « {title} ».")
                    }
                }
            }
            "sessions" => {
                let all = matches!(args, "all" | "toutes" | "tout");
                return self
                    .send_sessions_menu(chat_id, topic_id, 0, all, None)
                    .await;
            }
            "compact" => {
                // Un résumé prend de quelques secondes à une minute : la file des updates
                // n'attend pas, le bilan arrive en réponse quand il est prêt.
                let session = d.chat_session_for(&origin).await?;
                self.react(chat_id, message_id, reaction::RECEIVED);
                let (daemon, messenger) = (d.clone(), d.hooks.messenger());
                tokio::spawn(async move {
                    let text = match crate::compaction::compact(
                        &daemon,
                        &session,
                        crate::compaction::Trigger::Manual,
                        None,
                    )
                    .await
                    {
                        Ok(r) => crate::compaction::report_text(&r),
                        Err(e) => format!("❌ {e}"),
                    };
                    if let Some(m) = messenger {
                        let _ = m.send_text(&origin, &text).await;
                    }
                });
                return Ok(());
            }
            "fork" => {
                let session = d.chat_session_for(&origin).await?;
                let title = (!args.is_empty()).then(|| args.to_string());
                match crate::session_ops::fork(d, &session, title).await {
                    Ok(v) => {
                        let fork = v["session"].as_str().unwrap_or_default().to_string();
                        let background = self.bind_chat(&fork, chat_id, topic_id).await?;
                        s.sessions.touch(&fork).await?;
                        let back = s
                            .actions
                            .create(
                                k::SESSION_SWITCH,
                                &session,
                                json!({"notice": true}),
                                7 * 24 * 3_600_000,
                                false,
                            )
                            .await?;
                        let screen = screens::Screen {
                            text: format!(
                                "🍴 Session dupliquée ({} messages) : la suite se passe dans \
                                 `{fork}`.{}",
                                shown(&v["messages"]),
                                background_note(background)
                            ),
                            rows: vec![vec![ButtonSpec::callback(
                                "↪️ Revenir à l'original",
                                &back.token,
                                "",
                            )]],
                        };
                        return self
                            .send_screen(chat_id, topic_id, reply_to, screen, None)
                            .await;
                    }
                    Err(e) => format!("❌ {e}"),
                }
            }
            "rewind" if args.is_empty() => {
                return self
                    .show_screen(chat_id, topic_id, reply_to, "rewind", &json!({}), None)
                    .await;
            }
            "rewind" => {
                let session = d.chat_session_for(&origin).await?;
                let turns = args.trim().parse::<usize>().unwrap_or(1);
                match crate::session_ops::rewind(d, &session, turns).await {
                    Ok(v) => format!(
                        "⏪ {turns} échange(s) défait(s) ({} messages mis de côté dans `{}`).",
                        shown(&v["removed"]),
                        v["archive"].as_str().unwrap_or("?")
                    ),
                    Err(e) => format!("❌ {e}"),
                }
            }
            "export" => {
                let session = if args.is_empty() {
                    d.chat_session_for(&origin).await?
                } else {
                    args.to_string()
                };
                // Écriture puis téléversement : détachés, la boucle des updates continue
                // de lire `/stop` et les boutons (issue #69).
                let (me, d2) = (self.clone(), d.clone());
                tokio::spawn(async move {
                    let note = match crate::session_ops::export(&d2, "session", Some(&session))
                        .await
                    {
                        Ok(v) => {
                            let path =
                                std::path::PathBuf::from(v["path"].as_str().unwrap_or_default());
                            match me
                                .bot
                                .send_document(chat_id, topic_id, &path, Some("Export JSONL"))
                                .await
                            {
                                Ok(_) => return,
                                Err(e) => format!(
                                    "📦 Export écrit dans `{}`, envoi impossible : {e}",
                                    path.display()
                                ),
                            }
                        }
                        Err(e) => format!("❌ {e}"),
                    };
                    let _ = me.reply(chat_id, topic_id, reply_to, &note).await;
                });
                return Ok(());
            }
            "upgrade" => {
                // Installer ou revenir en arrière : toujours confirmé (issue #30).
                // Installation depuis les sources : la carte de bascule (issue #33).
                if args == "install"
                    && crate::helpers::running_binary()
                        .is_ok_and(|b| crate::helpers::is_source_build(&b))
                {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "upgrade.switch",
                            &json!({}),
                            None,
                        )
                        .await;
                }
                let confirm = match args {
                    "install" => Some((
                        "upgrade.install",
                        json!({}),
                        "Installer la dernière version publiée puis redémarrer ?".to_string(),
                    )),
                    "rollback" => Some((
                        "upgrade.rollback",
                        json!({}),
                        "Revenir au binaire précédent puis redémarrer ?".to_string(),
                    )),
                    tag if tag.starts_with('v') && !tag.contains(char::is_whitespace) => Some((
                        "upgrade.install",
                        json!({"tag": tag}),
                        format!("Installer {tag} puis redémarrer ?"),
                    )),
                    _ => None,
                };
                let args = match confirm {
                    Some((op, params, question)) => json!({
                        "op": op, "params": params, "question": question,
                        "back": {"screen": "upgrade", "args": {}},
                    }),
                    None => {
                        // L'écran s'affiche tout de suite ; la vérification suit en fond.
                        let daemon = d.clone();
                        tokio::spawn(async move {
                            let rpc = crate::rpc::Rpc::new(daemon.clone());
                            if let Ok(v) = rpc.call(m::UPGRADE, json!({"check": true})).await {
                                let cached =
                                    json!({"latest": v["latest"], "up_to_date": v["up_to_date"]});
                                let _ = daemon
                                    .services
                                    .kv_set("tg.upgrade.last_check", &cached.to_string())
                                    .await;
                            }
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "upgrade", &json!({}), None)
                            .await;
                    }
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                    .await;
            }
            "stop" => {
                // Arrêter, c'est aussi vider la file : sinon le flot reprend aussitôt
                // (issue #49).
                let session = d.chat_session_for(&origin).await?;
                let tout = matches!(args.trim(), "tout" | "all");
                let burst = self
                    .bursts
                    .lock()
                    .ok()
                    .and_then(|mut g| g.remove(&chat_id))
                    .map(|b| b.parts.len())
                    .unwrap_or(0);
                let running = d.bus.cancel_session(&session);
                // Ingestions en cours de cette session : comptées pour toute forme de
                // `/stop`, annulées par `/stop tout` (issue #155).
                let mut ingests = d.bus.ingests_of(&session);
                let mut cancelled_ingests = if tout {
                    d.bus.cancel_ingests(&session)
                } else {
                    0
                };
                // Un job d'outil tourne hors du tour : `cancel_session` ne le voit pas.
                // `/stop` coupe ceux de cette session (issue #204).
                let mut cancelled_jobs = s.jobs.cancel_session(&session);
                let mut queued = crate::session_ops::silence(d, &session, "arrêt demandé").await?;
                let mut sessions = 0;
                let mut runs = 0;
                // Les runs ouverts de ce chat, quel que soit leur état : un run `blocked`
                // paraît « en cours » au propriétaire, et c'est précisément celui que
                // `/stop tout` ignorait en répondant « Rien à arrêter » (issue #155).
                let open: Vec<penelope_workflow::runs::Run> = s
                    .runs
                    .list(None, 50)
                    .await?
                    .into_iter()
                    .filter(|r| {
                        matches!(
                            r.state,
                            penelope_workflow::RunState::Running
                                | penelope_workflow::RunState::Blocked
                                | penelope_workflow::RunState::Paused
                        )
                    })
                    .collect();
                let mut left: Vec<String> = Vec::new();
                if tout {
                    // Sessions du chat, **et** sous-agents dont le parent est dans ce chat :
                    // une session de sous-agent n'a pas de chat Telegram, elle était sautée.
                    let all = s.sessions.list(None, 200).await?;
                    let here: std::collections::HashSet<String> = all
                        .iter()
                        .filter(|x| x.tg_chat_id == Some(chat_id))
                        .map(|x| x.id.to_string())
                        .collect();
                    for other in &all {
                        let id = other.id.to_string();
                        if id == session {
                            continue;
                        }
                        let mine = other.tg_chat_id == Some(chat_id)
                            || other.parent_id.as_ref().is_some_and(|p| here.contains(p));
                        if !mine {
                            continue;
                        }
                        d.bus.cancel_session(&id);
                        ingests += d.bus.ingests_of(&id);
                        cancelled_ingests += d.bus.cancel_ingests(&id);
                        cancelled_jobs += s.jobs.cancel_session(&id);
                        let n = crate::session_ops::silence(d, &id, "arrêt demandé").await?;
                        if n > 0 || d.bus.is_active(&id) {
                            sessions += 1;
                        }
                        queued += n;
                    }
                    for run in &open {
                        if run.state == penelope_workflow::RunState::Running {
                            if crate::workflow::control(
                                d,
                                &run.id,
                                &penelope_workflow::Control::Pause,
                            )
                            .await
                            .is_ok()
                            {
                                runs += 1;
                            }
                        } else {
                            // Ni mis en pause (il l'est déjà ou il attend), ni annulé à la
                            // place du propriétaire : nommé, avec de quoi décider.
                            left.push(format!("`{}` ({})", run.id, run.state.as_str()));
                        }
                    }
                }
                let has_left = !left.is_empty();
                let note = StopReport {
                    running,
                    queued,
                    burst,
                    sessions,
                    paused: runs,
                    left,
                    open: open
                        .iter()
                        .map(|r| (r.id.clone(), r.state.as_str().to_string()))
                        .collect(),
                    ingests,
                    cancelled_ingests,
                    cancelled_jobs,
                    tout,
                }
                .render();
                // Des runs sont restés ouverts : l'écran `runs` porte un bouton par run
                // (⏸ ▶️ ⏹, l'arrêt sous confirmation). « Laisser », c'est ne pas cliquer.
                // Le propriétaire décide, la commande ne décide pas pour lui (issue #155).
                if tout && has_left {
                    let _ = self.reply(chat_id, topic_id, reply_to, &note).await;
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "runs", &json!({}), None)
                        .await;
                }
                note
            }
            "switch" => {
                if args.is_empty() {
                    return self
                        .send_sessions_menu(chat_id, topic_id, 0, false, None)
                        .await;
                } else {
                    match crate::session_ops::resolve(s, args).await {
                        Err(e) => format!("❌ {e}"),
                        Ok(sess) => {
                            let id = sess.id.to_string();
                            if sess.state != "active" {
                                s.sessions.set_state(&id, "active").await?;
                            }
                            let background = self.bind_chat(&id, chat_id, topic_id).await?;
                            s.sessions.touch(&id).await?;
                            format!(
                                "↪️ Session « {} » reprise (`{id}`).{}",
                                crate::titles::label(&sess),
                                background_note(background)
                            )
                        }
                    }
                }
            }
            "close" => {
                let target = if args.is_empty() {
                    s.sessions
                        .find_by_topic(chat_id, topic_id)
                        .await?
                        .map(|x| x.id.to_string())
                        .ok_or_else(|| "aucune session liée à ce chat".to_string())
                } else {
                    crate::session_ops::resolve(s, args)
                        .await
                        .map(|x| x.id.to_string())
                };
                match target {
                    Err(e) => format!("❌ {e}"),
                    Ok(id) => {
                        let label = match s.sessions.get(&id).await? {
                            Some(sess) => format!("« {} »", crate::titles::label(&sess)),
                            None => format!("`{id}`"),
                        };
                        let args = json!({
                            "op": "session.close", "params": {"session": id},
                            "question": format!(
                                "Fermer la session {label} ? Sa file d'attente est vidée."
                            ),
                            "back": null,
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                            .await;
                    }
                }
            }
            "purge" => {
                let target = if args.is_empty() {
                    s.sessions
                        .find_by_topic(chat_id, topic_id)
                        .await?
                        .map(|x| x.id.to_string())
                        .ok_or_else(|| "aucune session liée à ce chat".to_string())
                } else {
                    crate::session_ops::resolve(s, args)
                        .await
                        .map(|x| x.id.to_string())
                };
                match target {
                    Err(e) => format!("❌ {e}"),
                    Ok(id) => {
                        let label = match s.sessions.get(&id).await? {
                            Some(sess) => format!("« {} »", crate::titles::label(&sess)),
                            None => format!("`{id}`"),
                        };
                        let args = json!({
                            "op": "session.purge",
                            "params": {"session": id, "reason": "demande du propriétaire"},
                            "question": format!(
                                "Effacer le contenu de la session {label} ? Messages, résumés, \
                                 artefacts et requêtes partent définitivement ; la chaîne \
                                 d'audit garde ses lignes, sans leur contenu."
                            ),
                            "back": null,
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                            .await;
                    }
                }
            }
            "model" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let session = d.chat_session_for(&origin).await?;
                match parts.as_slice() {
                    // Sans argument : l'état de la session et un bouton par modèle.
                    [] => {
                        return self
                            .send_model_menu(chat_id, topic_id, reply_to, &session)
                            .await;
                    }
                    ["auto", switch] => {
                        let on = match *switch {
                            "on" | "oui" => Some(true),
                            "off" | "non" => Some(false),
                            _ => None,
                        };
                        match on {
                            None => "Usage : `/model auto on` ou `/model auto off`".into(),
                            Some(on) => {
                                rpc.call(
                                    m::CONFIG_SET,
                                    json!({"path": "models.routing.classifier", "value": on}),
                                )
                                .await?;
                                if on {
                                    "🔀 Routage adaptatif activé : le classifieur choisit l'alias à chaque message.".into()
                                } else {
                                    "📌 Routage fixe : les sessions non épinglées passent par `main`.".into()
                                }
                            }
                        }
                    }
                    // Connexion d'un fournisseur à compte (#142) : le code s'affiche
                    // ici, et Pénélope confirme dès qu'il est saisi. Ni le code ni les
                    // jetons ne passent par une carte ni par une demande (#134).
                    ["auth", rest @ ..] => {
                        let provider = rest
                            .iter()
                            .find(|w| !w.starts_with('-') && **w != "status" && **w != "logout")
                            .copied()
                            .unwrap_or("codex")
                            .to_string();
                        let action = if rest.iter().any(|w| w.trim_start_matches('-') == "logout") {
                            "logout"
                        } else if rest.iter().any(|w| w.trim_start_matches('-') == "status") {
                            "status"
                        } else {
                            "start"
                        };
                        let params = json!({"provider": provider, "action": action});
                        match (action, rpc.call(m::MODEL_AUTH, params).await) {
                            (_, Err(e)) => format!("❌ {e}"),
                            ("logout", Ok(_)) => {
                                format!("🔌 `{provider}` déconnecté : jeton révoqué et oublié.")
                            }
                            ("status", Ok(v)) => codex_status_text(&v),
                            (_, Ok(v)) => {
                                // L'attente se fait en fond : le tour Telegram ne reste
                                // pas suspendu un quart d'heure.
                                let g = self.clone();
                                let daemon = d.clone();
                                tokio::spawn(async move {
                                    let text = match crate::rpc::Rpc::new(daemon)
                                        .call(
                                            m::MODEL_AUTH,
                                            json!({"provider": "codex", "action": "wait"}),
                                        )
                                        .await
                                    {
                                        Ok(v) => format!(
                                            "✅ Connecté : plan {}, compte {}.\nDonner un \
                                             alias : `/model code codex:gpt-6-astra`",
                                            shown(&v["plan"]),
                                            shown(&v["account"])
                                        ),
                                        Err(e) => format!("❌ Connexion abandonnée : {e}"),
                                    };
                                    if let Err(e) = g.reply(chat_id, topic_id, None, &text).await {
                                        tracing::warn!(error = %e, "confirmation de connexion non envoyée");
                                    }
                                });
                                format!(
                                    "🔐 Ouvrir {}\net saisir le code : `{}`\n\nJe confirme ici \
                                     dès que c'est validé (quinze minutes).",
                                    shown(&v["url"]),
                                    shown(&v["user_code"])
                                )
                            }
                        }
                    }
                    // Un alias seul : l'épingler sur la session, `auto` pour revenir.
                    [alias] => match rpc
                        .call(
                            m::SESSION_MODEL,
                            json!({"session": session, "alias": alias}),
                        )
                        .await
                    {
                        Ok(v) => model_pin_notice(&v),
                        Err(e) => format!("❌ {e}"),
                    },
                    // Un alias et un modèle : changer ce que vise l'alias, partout.
                    [alias, model, ..] => {
                        let model = normalise_model_id(model);
                        match rpc
                            .call(m::MODEL_SET, json!({"alias": alias, "model": model}))
                            .await
                        {
                            Ok(v) => format!(
                                "✅ `{alias}` → `{model}` (génération {}).",
                                shown(&v["generation"])
                            ),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                }
            }
            // Foyer du propriétaire (issue #143) : ce qui n'appartient à aucune session
            // — alertes de budget, rappels, digest du rêve, cartes OAuth — arrive ici
            // plutôt que dans un chat privé que plus personne ne lit.
            "home" | "foyer" => {
                let reset = matches!(args.trim(), "off" | "non" | "privé" | "prive");
                let (chat, topic) = if reset {
                    (0, 0)
                } else {
                    (chat_id, topic_id.unwrap_or(0))
                };
                match rpc
                    .call(
                        m::CONFIG_SET,
                        json!({"path": "telegram.home", "value": {"chat": chat, "topic": topic}}),
                    )
                    .await
                {
                    Err(e) => format!("❌ {e}"),
                    Ok(_) if reset => "🏠 Foyer effacé : les avis sans session repartent \
                                       dans le chat privé."
                        .into(),
                    Ok(_) => format!(
                        "🏠 Foyer réglé sur ce {}. Les avis sans session (budget, rappels, \
                         digest, cartes MCP) arriveront ici. `/home off` pour revenir au \
                         chat privé.",
                        if topic_id.is_some() { "sujet" } else { "chat" }
                    ),
                }
            }
            "models" => {
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "models",
                        &json!({"filter": args}),
                        None,
                    )
                    .await;
            }
            // Sujet de travail de la session (issue #119) : sans argument, l'état et un
            // bouton par projet connu.
            "projet" => {
                let session = d.chat_session_for(&origin).await?;
                let wanted = args.trim();
                match rpc
                    .call(
                        m::SESSION_PROJECT,
                        json!({"session": session, "project": wanted}),
                    )
                    .await
                {
                    Err(e) => format!("❌ {e}"),
                    Ok(v) if !wanted.is_empty() => match v["project"].as_str() {
                        Some(p) => format!(
                            "📁 Sujet de la session : **{p}**. La mémoire d'office s'y limite dès \
                             le prochain message ; le reste reste au rappel."
                        ),
                        None => "📁 Session sans sujet : seules les entrées sans projet sont \
                                 injectées d'office."
                            .into(),
                    },
                    Ok(v) => {
                        let current = v["project"].as_str();
                        let mut rows = Vec::new();
                        for p in v["known"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .iter()
                            .take(12)
                        {
                            let p = p.as_str().unwrap_or_default();
                            let mark = if Some(p) == current { "✅ " } else { "" };
                            rows.push(vec![
                                self.command_button_with(&format!("{mark}{p}"), "projet", p)
                                    .await?,
                            ]);
                        }
                        rows.push(vec![
                            self.command_button_with(
                                if current.is_none() {
                                    "✅ Aucun"
                                } else {
                                    "Aucun"
                                },
                                "projet",
                                "aucun",
                            )
                            .await?,
                        ]);
                        let state = match (current, v["how"].as_str()) {
                            (Some(p), Some("explicite")) => format!("**{p}** (choisi)"),
                            (Some(p), Some(how)) => format!("**{p}** (déduit du {how})"),
                            (Some(p), None) => format!("**{p}**"),
                            (None, Some(_)) => "aucun (choisi)".into(),
                            (None, None) => "aucun pour l'instant".into(),
                        };
                        let text = format!(
                            "📁 Sujet de la session : {state}.\n\nLe profil et les entrées sans \
                             projet sont toujours là ; celles d'un projet n'entrent d'office que \
                             dans une session de ce projet. Les autres restent au rappel et à \
                             `mem_search`."
                        );
                        let mut payload = json!({
                            "chat_id": chat_id,
                            "text": markdown_to_html(&text),
                            "parse_mode": "HTML",
                            "reply_markup": inline_keyboard(&rows),
                            "message_thread_id": topic_id,
                        });
                        if let Some(r) = reply_to {
                            payload["reply_parameters"] =
                                json!({"message_id": r, "allow_sending_without_reply": true});
                        }
                        return self
                            .outbox_push(chat_id, topic_id, "sendMessage", payload)
                            .await;
                    }
                }
            }
            // Mode d'approbation de la session (issue #111) : sans argument, l'état et un
            // bouton par mode.
            "mode" => {
                let session = d.chat_session_for(&origin).await?;
                let wanted = args.trim();
                let v = rpc
                    .call(m::SESSION_MODE, json!({"session": session, "mode": wanted}))
                    .await;
                match v {
                    Err(e) => format!("❌ {e}"),
                    Ok(v) if !wanted.is_empty() => {
                        format!(
                            "🛡 Mode de la session : **{}**.",
                            v["label"].as_str().unwrap_or("?")
                        )
                    }
                    Ok(v) => {
                        let current = v["mode"].as_str().unwrap_or("reads");
                        let mut rows = Vec::new();
                        for (mode, label) in [
                            ("ask", "Demander tout"),
                            ("reads", "Lectures sans demande"),
                            ("auto", "Tout sauf le destructif"),
                        ] {
                            let mark = if mode == current { "✅ " } else { "" };
                            rows.push(vec![
                                self.command_button_with(&format!("{mark}{label}"), "mode", mode)
                                    .await?,
                            ]);
                        }
                        let text = format!(
                            "🛡 Mode de la session : **{}**.\n\nDemander tout : même une lecture \
                             du shell attend ton accord. Lectures sans demande (défaut) : `ls`, \
                             `cat`, `grep`, `git status` passent, le reste selon tes règles. \
                             Tout sauf le destructif : plus de demande, sauf suppression et \
                             réglages sensibles.",
                            v["label"].as_str().unwrap_or("?")
                        );
                        let mut payload = json!({
                            "chat_id": chat_id,
                            "text": markdown_to_html(&text),
                            "parse_mode": "HTML",
                            "reply_markup": inline_keyboard(&rows),
                            "message_thread_id": topic_id,
                        });
                        if let Some(r) = reply_to {
                            payload["reply_parameters"] =
                                json!({"message_id": r, "allow_sending_without_reply": true});
                        }
                        return self
                            .outbox_push(chat_id, topic_id, "sendMessage", payload)
                            .await;
                    }
                }
            }
            "schedules" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let by_id = |method: &'static str, id: &str| rpc.call(method, json!({"id": id}));
                match parts.as_slice() {
                    ["rm", id] => {
                        let args = json!({
                            "op": "schedule.rm", "params": {"id": id},
                            "question": format!("Supprimer le déclencheur `{id}` ?"),
                            "back": {"screen": "schedules", "args": {}},
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                            .await;
                    }
                    // Livrer ici, dans cette conversation et ce sujet (#124).
                    ["ici" | "here", id] => {
                        match crate::scheduler::retarget(
                            &self.daemon.services,
                            id,
                            chat_id,
                            topic_id,
                        )
                        .await
                        {
                            Ok(to) => format!("📍 `{id}` livrera désormais ici : {to}."),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    [op @ ("pause" | "resume" | "run"), id] => {
                        let method = match *op {
                            "pause" => m::SCHEDULE_PAUSE,
                            "resume" => m::SCHEDULE_RESUME,
                            "rm" => m::SCHEDULE_RM,
                            _ => m::SCHEDULE_RUN_NOW,
                        };
                        match by_id(method, id).await {
                            Ok(_) => match *op {
                                "pause" => format!("⏸ `{id}` en pause."),
                                "resume" => format!("▶️ `{id}` repris."),
                                "rm" => format!("🗑 `{id}` supprimé."),
                                _ => format!("⚡ `{id}` déclenché."),
                            },
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    _ => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "schedules", &json!({}), None)
                            .await;
                    }
                }
            }
            "mcp" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let call =
                    |method: &'static str, name: &str| rpc.call(method, json!({"name": name}));
                match parts.as_slice() {
                    [] => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "mcp", &json!({}), None)
                            .await;
                    }
                    ["auth", name] => {
                        return self.send_oauth_card(chat_id, topic_id, name).await;
                    }
                    ["restart", name] => match call(m::MCP_RESTART, name).await {
                        Ok(v) => format!(
                            "🔄 `{name}` redémarré : {} outil(s), état {}.",
                            shown(&v["tool_count"]),
                            v["state"].as_str().unwrap_or("?")
                        ),
                        Err(e) => format!("❌ {e}"),
                    },
                    ["logs", name] => match call(m::MCP_LOGS, name).await {
                        Ok(v) => {
                            let lines: Vec<String> = v["lines"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                                .iter()
                                .rev()
                                .take(30)
                                .rev()
                                .filter_map(|l| l.as_str().map(String::from))
                                .collect();
                            if lines.is_empty() {
                                format!("Aucune ligne de journal pour `{name}`.")
                            } else {
                                format!("```\n{}\n```", lines.join("\n").replace("```", "ʼʼʼ"))
                            }
                        }
                        Err(e) => format!("❌ {e}"),
                    },
                    ["test", name] => match call(m::MCP_TEST, name).await {
                        Ok(v) if v["ok"].as_bool() == Some(true) => format!(
                            "✅ `{name}` répond : protocole {}, {} outil(s){}, {} ms.",
                            v["protocol"].as_str().unwrap_or("?"),
                            shown(&v["tools"]),
                            match v["call"]["tool"].as_str() {
                                Some(t) => format!(", appel de `{t}` réussi"),
                                None => ", aucun outil en lecture sans argument à essayer".into(),
                            },
                            shown(&v["ms"])
                        ),
                        Ok(v) => {
                            format!("❌ `{name}` : {}", v["error"].as_str().unwrap_or("échec"))
                        }
                        Err(e) => format!("❌ {e}"),
                    },
                    [op @ ("enable" | "disable"), name] => {
                        let method = if *op == "enable" {
                            m::MCP_ENABLE
                        } else {
                            m::MCP_DISABLE
                        };
                        match call(method, name).await {
                            Ok(_) if *op == "enable" => format!("▶️ `{name}` activé."),
                            Ok(_) => format!("⏸ `{name}` désactivé."),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    [name] => {
                        return self
                            .show_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                "mcp.server",
                                &json!({"name": name}),
                                None,
                            )
                            .await;
                    }
                    _ => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "mcp", &json!({}), None)
                            .await;
                    }
                }
            }
            "budget" => {
                let session = d.chat_session_for(&origin).await?;
                match args.split_whitespace().collect::<Vec<_>>().as_slice() {
                    // Plafond propre à la session de travail (issue #32).
                    ["session", amount] => {
                        let usd = match *amount {
                            "off" | "défaut" | "defaut" | "0" => None,
                            raw => match raw.trim_end_matches('$').replace(',', ".").parse::<f64>()
                            {
                                Ok(v) if v > 0.0 => Some(v),
                                _ => {
                                    return self
                                        .send_screen(
                                            chat_id,
                                            topic_id,
                                            reply_to,
                                            Self::typed_screen(
                                                &format!("Montant illisible : `{raw}`. Un nombre de dollars, ou `off`."),
                                                "✏️ Écrire le plafond",
                                                "/budget session ",
                                            ),
                                            None,
                                        )
                                        .await;
                                }
                            },
                        };
                        s.sessions.set_budget(&session, usd).await?;
                        let cfg = s.config.config();
                        let (daily, limit, _) =
                            s.budget.limits(&cfg.budget, Some(&session), None).await?;
                        format!(
                            "💰 Plafond de cette session : {} $ ({}). Le jour reste plafonné à {} $.",
                            fmt_usd(limit),
                            if usd.is_some() {
                                "propre à la session"
                            } else {
                                "celui de la configuration"
                            },
                            fmt_usd(daily)
                        )
                    }
                    ["session"] => {
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                Self::typed_screen(
                                    "💰 **Plafond de la session** : `/budget session` suivi d'un montant en dollars (`off` pour revenir à la configuration).",
                                    "✏️ Écrire le plafond",
                                    "/budget session ",
                                ),
                                None,
                            )
                            .await;
                    }
                    _ => self.budget_text(&session, args).await?,
                }
            }
            "usage" => {
                let session = d.chat_session_for(&origin).await?;
                self.usage_text(&session, args).await?
            }
            "audit" => {
                // L'audit relit toute la mémoire : détaché (issue #69).
                let (me, d2) = (self.clone(), d.clone());
                tokio::spawn(async move {
                    let note = match crate::mem_audit::run(&d2).await {
                        Ok(a) => crate::mem_audit::summary(&a),
                        Err(e) => format!("❌ {e}"),
                    };
                    let _ = me.reply(chat_id, topic_id, reply_to, &note).await;
                });
                return Ok(());
            }
            "accueil" => {
                let part = crate::onboarding::Part::parse(args);
                if !args.is_empty() && part.is_none() {
                    "Partie inconnue : profil, outils, style ou limites.".to_string()
                } else {
                    return self.onboarding_next(chat_id, topic_id, part).await;
                }
            }
            "dream" => {
                // Une passe peut prendre une minute : le bilan arrive quand il est prêt.
                self.react(chat_id, message_id, reaction::RECEIVED);
                let (daemon, messenger) = (d.clone(), d.hooks.messenger());
                let dry_run = args.contains("dry");
                tokio::spawn(async move {
                    let text = match crate::dream::run(&daemon, dry_run).await {
                        Ok(o) => format!(
                            "🌙 {}{}",
                            if o.dry_run { "(à blanc) " } else { "" },
                            o.report.render()
                        ),
                        Err(e) => format!("❌ {e}"),
                    };
                    if let Some(m) = messenger {
                        let _ = m.send_text(&origin, &text).await;
                    }
                });
                return Ok(());
            }
            "appris" => {
                let days = args.trim().parse::<i64>().unwrap_or(7);
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "learned",
                        &json!({"days": days}),
                        None,
                    )
                    .await;
            }
            "pratique" => {
                let (screen, screen_args) = if args.is_empty() {
                    ("practices", json!({}))
                } else {
                    (
                        "practice",
                        json!({"slug": penelope_platform::slugify(args)}),
                    )
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, screen, &screen_args, None)
                    .await;
            }
            "retiens" => {
                if args.is_empty() {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "🧠 **Retenir** : `/retiens` suivi de ce qu'il faut garder en \
                                 mémoire de fond (une ligne, sans secret).",
                                "✏️ Écrire",
                                "/retiens ",
                            ),
                            None,
                        )
                        .await;
                } else {
                    let vault = crate::helpers::vault_dir(s);
                    let session = d.chat_session_for(&origin).await?;
                    match crate::vault_ops::remember(
                        s,
                        &vault,
                        penelope_memory::Level::Coeur,
                        args,
                        &session,
                    )
                    .await
                    {
                        Ok(uid) => format!("🧠 Retenu (`{uid}`)."),
                        Err(e) => format!("❌ {e}"),
                    }
                }
            }
            "oublie" => {
                if !args.is_empty()
                    && let Some(e) = s.memory.get(args).await?
                {
                    let confirm = json!({
                        "op": "mem.forget", "params": {"uid": args},
                        "question": format!("Oublier « {} » ?", e.text),
                        "back": {"screen": "forget", "args": {}},
                    });
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                        .await;
                }
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "forget",
                        &json!({"query": args}),
                        None,
                    )
                    .await;
            }
            "forget" => {
                if args.is_empty() {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "forget.sessions",
                            &json!({}),
                            None,
                        )
                        .await;
                }
                match crate::session_ops::resolve(s, args).await {
                    Err(e) => format!("❌ {e}"),
                    Ok(sess) => {
                        let confirm = json!({
                            "op": "session.forget", "params": {"session": sess.id.to_string()},
                            "question": format!(
                                "Oublier tout ce que la mémoire a retenu de la session « {} » ?",
                                crate::titles::label(&sess)
                            ),
                            "back": {"screen": "forget.sessions", "args": {}},
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                            .await;
                    }
                }
            }
            "secret" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                match parts.first().copied() {
                    None | Some("list") => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "secrets", &json!({}), None)
                            .await;
                    }
                    Some("rm") if parts.len() > 1 => {
                        let confirm = json!({
                            "op": "secret.rm", "params": {"name": parts[1]},
                            "question": format!("Supprimer le secret `{}` ?", parts[1]),
                            "back": {"screen": "secrets", "args": {}},
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                            .await;
                    }
                    _ => "Un secret ne se saisit **jamais** dans une conversation. En SSH : \
                          `penelope secret set <nom>` puis coller la valeur à l'invite"
                        .into(),
                }
            }
            "approvals" => {
                let pending = s.approvals.pending(20).await?;
                if pending.is_empty() {
                    "Aucune demande en attente.".into()
                } else {
                    for a in &pending {
                        self.send_approval_card(chat_id, topic_id, a).await?;
                    }
                    format!("{} demande(s) en attente.", pending.len())
                }
            }
            "recall" => {
                if args.is_empty() {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "🔎 **Rechercher en mémoire** : `/recall` suivi des mots-clés.",
                                "🔎 Chercher",
                                "/recall ",
                            ),
                            None,
                        )
                        .await;
                }
                let hits = rpc.call(m::MEM_SEARCH, json!({"query": args})).await?;
                let hits = hits.as_array().cloned().unwrap_or_default();
                if hits.is_empty() {
                    format!("🔎 Rien en mémoire pour « {args} ».")
                } else {
                    let mut t = format!(
                        "🔎 **Mémoire** : {} résultat(s) pour « {args} »\n",
                        hits.len()
                    );
                    for h in hits.iter().take(10) {
                        t.push_str(&format!(
                            "\n- {} _({})_",
                            h["text"].as_str().unwrap_or_default(),
                            h["file"].as_str().unwrap_or("?")
                        ));
                    }
                    t
                }
            }
            "run" => {
                let mut words = args.splitn(2, char::is_whitespace);
                match words.next().filter(|w| !w.is_empty()) {
                    None => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "wf", &json!({}), None)
                            .await;
                    }
                    Some(id) => {
                        let rest = words.next().unwrap_or_default().trim();
                        // Le plan précède toute exécution, même avec paramètres explicites.
                        if let Some(w) = s.workflows.get(id) {
                            return self
                                .run_by_conversation(chat_id, topic_id, message_id, &w, rest)
                                .await;
                        }
                        format!("❌ Workflow `{id}` introuvable.")
                    }
                }
            }
            "resume" => {
                if args.is_empty() {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "runs",
                            &json!({"filter": "stuck"}),
                            None,
                        )
                        .await;
                }
                match crate::workflow::control(d, args, &penelope_workflow::Control::Resume).await {
                    Ok(state) => format!("▶️ Run `{args}` : {}.", state.as_str()),
                    Err(e) => format!("❌ {e}"),
                }
            }
            "wf" | "runs" | "skills" | "intentions" | "policies" | "status" | "doctor"
            | "config" => {
                let screen = match command {
                    "wf" if !args.is_empty() => {
                        return self
                            .show_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                "wf.detail",
                                &json!({"id": args}),
                                None,
                            )
                            .await;
                    }
                    other => other,
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, screen, &json!({}), None)
                    .await;
            }
            "skill" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let (screen, screen_args) = match parts.as_slice() {
                    [] => ("skills", json!({})),
                    ["rollback", name] => (
                        "confirm",
                        json!({
                            "op": "skill.rollback", "params": {"name": name},
                            "question": format!("Revenir à la version précédente de `{name}` ?"),
                            "back": {"screen": "skills", "args": {}},
                        }),
                    ),
                    [name, ..] => ("skill", json!({"name": name})),
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, screen, &screen_args, None)
                    .await;
            }
            "logs" => {
                let component = args.split_whitespace().next().unwrap_or_default();
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "logs",
                        &json!({"component": component}),
                        None,
                    )
                    .await;
            }
            "restart" => {
                let confirm = json!({
                    "op": "restart", "params": {},
                    "question": "Redémarrer le daemon ? Les tours en cours reprennent au redémarrage.",
                    "back": null,
                });
                return self
                    .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                    .await;
            }
            "note" => {
                if args.is_empty() {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "📝 **Noter** : `/note` suivi du texte, rangé dans le journal du jour.",
                                "✏️ Écrire",
                                "/note ",
                            ),
                            None,
                        )
                        .await;
                }
                let vault = crate::helpers::vault_dir(s);
                let session = d.chat_session_for(&origin).await?;
                match crate::vault_ops::remember(
                    s,
                    &vault,
                    penelope_memory::Level::Episodic,
                    args,
                    &session,
                )
                .await
                {
                    Ok(_) => "📝 Noté dans le journal du jour.".into(),
                    Err(e) => format!("❌ {e}"),
                }
            }
            "mien" => "📄 Envoie un document avec la légende `/mien` : il est ingéré comme rédigé \
                       par toi, donc fiable et rappelable. Sans légende, un document reçu reste \
                       une source non fiable."
                .into(),
            "p" => {
                let mut words = args.splitn(3, char::is_whitespace);
                match (words.next().filter(|w| !w.is_empty()), words.next()) {
                    (None, _) => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "prompts", &json!({}), None)
                            .await;
                    }
                    (Some(server), None) => {
                        return self
                            .show_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                "prompts",
                                &json!({"server": server}),
                                None,
                            )
                            .await;
                    }
                    (Some(server), Some(prompt)) => {
                        let params = parse_params(words.next().unwrap_or_default());
                        match self
                            .run_mcp_prompt(chat_id, topic_id, server, prompt, params)
                            .await
                        {
                            Ok(n) => format!(
                                "💬 Prompt `{prompt}` de `{server}` : {n} message(s) envoyé(s) au modèle."
                            ),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                }
            }
            "quiet" => {
                if args.is_empty() {
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "quiet", &json!({}), None)
                        .await;
                }
                let range = if matches!(args, "off" | "non" | "aucune") {
                    ""
                } else {
                    args
                };
                match rpc.call(m::QUIET, json!({"range": range})).await {
                    Ok(_) if range.is_empty() => "🔔 Heures silencieuses désactivées.".into(),
                    Ok(_) => format!("🌙 Heures silencieuses : {range}."),
                    Err(e) => format!("❌ {e}"),
                }
            }
            other => format!("Commande inconnue : `/{other}`. Voir `/help`."),
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }
}

#[cfg(test)]
mod tests;
