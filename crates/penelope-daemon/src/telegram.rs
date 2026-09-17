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

mod screens;

/// Longueur d'un fragment Markdown avant conversion HTML : marge pour les balises.
const FRAGMENT_CHARS: usize = 3_500;
const MAX_ATTEMPTS: i64 = 6;
/// Mention des messages en attente abandonnés par un changement de session.
fn cancelled_note(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "\n⏹ 1 message en attente dans l'ancienne session a été abandonné.".into(),
        n => format!("\n⏹ {n} messages en attente dans l'ancienne session ont été abandonnés."),
    }
}

/// Formulaire d'étape `user` en cours dans un chat.
const BOT_USERNAME_KEY: &str = "tg.bot_username";

/// Lien `https://t.me/<bot>?start=<charge>` vers un écran ou une commande, quand le bot est
/// connu (issue #30).
pub async fn deep_link(d: &Daemon, payload: &str) -> Option<String> {
    let bot = d.kv_get(BOT_USERNAME_KEY).await.ok().flatten()?;
    (!bot.is_empty() && bot != "?").then(|| penelope_telegram::render::deep_link(&bot, payload))
}

fn form_key(chat_id: i64) -> String {
    format!("tg.form.{chat_id}")
}

/// Durée de validité du bouton « Réessayer » d'un tour échoué.
const RETRY_TTL_MS: i64 = 24 * 3600 * 1000;

pub struct TelegramGateway {
    pub daemon: Arc<Daemon>,
    pub bot: Arc<Bot>,
    owner_id: i64,
    allow_groups: bool,
    draft_interval: Duration,
    poll_timeout_s: u64,
    outbox_wake: Notify,
    /// Albums en cours de réception, par `media_group_id`.
    albums: Arc<std::sync::Mutex<HashMap<String, Album>>>,
    /// Sorties mises de côté des sessions en arrière-plan : lues et réécrites sous verrou.
    held_lock: tokio::sync::Mutex<()>,
}

/// Sortie d'une session en arrière-plan, mise de côté jusqu'à son retour au focus du chat
/// (issue #10).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Held {
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
}

fn held_key(session_id: &str) -> String {
    format!("tg.held.{session_id}")
}

/// « 1 réponse », « 3 approbations ».
fn count_of(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n > 1 { many } else { one })
}

/// Photos d'un même album, regroupées en un seul tour (§14.4).
struct Album {
    origin: Origin,
    images: Vec<std::path::PathBuf>,
    caption: Option<String>,
    update_id: i64,
}

/// Fenêtre de regroupement d'un album : Telegram envoie ses photos une par une.
const ALBUM_WINDOW: Duration = Duration::from_millis(1_500);

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
        let cfg = daemon.services.config.config();
        let bot = Arc::new(Bot::new(
            transport,
            cfg.telegram.rate_per_chat_per_s,
            daemon.services.clock.clone(),
        ));
        Arc::new(TelegramGateway {
            owner_id: cfg.owner.telegram_user_id,
            allow_groups: cfg.telegram.allow_groups,
            draft_interval: Duration::from_millis(cfg.telegram.draft_interval_ms.max(300)),
            poll_timeout_s: cfg.telegram.poll_timeout_s,
            outbox_wake: Notify::new(),
            albums: Arc::new(std::sync::Mutex::new(HashMap::new())),
            held_lock: tokio::sync::Mutex::new(()),
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
        let _ = self.daemon.kv_set(BOT_USERNAME_KEY, username).await;
        if let Err(e) = self
            .bot
            .set_commands(penelope_telegram::commands::to_bot_commands())
            .await
        {
            tracing::warn!(error = %e, "setMyCommands refusé");
        }
        self.register();
        Ok(vec![
            tokio::spawn(self.clone().poll_loop()),
            tokio::spawn(self.clone().draft_loop()),
            tokio::spawn(self.clone().outbox_loop()),
        ])
    }

    fn shutting_down(&self) -> bool {
        self.daemon.handle.is_shutting_down()
    }

    // ================================================================ réception

    async fn poll_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        while !self.shutting_down() {
            let offset = self
                .daemon
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
                        }
                        let _ = self.daemon.kv_set("tg.offset", &(id + 1).to_string()).await;
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

    /// Traite un update. Idempotent : un update déjà traité est ignoré.
    pub async fn process_update(&self, update: &Value) -> anyhow::Result<()> {
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

        let incoming = classify(update, self.owner_id, self.allow_groups);
        self.handle(incoming).await?;

        s.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE tg_updates SET processed = 1 WHERE update_id = ?1",
                    [update_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn handle(&self, incoming: Incoming) -> anyhow::Result<()> {
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
                if let Some(raw) = self.daemon.kv_get(&title_key).await?
                    && let Some((target, at)) = raw.split_once(' ')
                    && self.daemon.services.clock.now_ms() - at.parse::<i64>().unwrap_or(0)
                        < 5 * 60_000
                {
                    let target = target.to_string();
                    self.daemon.kv_set(&title_key, "").await?;
                    let note = match crate::titles::clean(&text) {
                        Some(title) => {
                            self.daemon
                                .services
                                .sessions
                                .set_title(&target, &title, false)
                                .await?;
                            format!("✏️ Session renommée : « {title} ».")
                        }
                        None => "Titre vide : rien n'a changé.".to_string(),
                    };
                    return self.reply(chat_id, topic_id, Some(message_id), &note).await;
                }
                // Entretien d'accueil en cours : ce message répond à la question posée.
                let onboard_key = format!("tg.onboard.{chat_id}");
                if let Some(raw) = self.daemon.kv_get(&onboard_key).await?
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
                if let Some(raw) = self.daemon.kv_get(&form_key(chat_id)).await?
                    && !raw.is_empty()
                {
                    return self.form_input(chat_id, &raw, Some(&text)).await;
                }
                // Une saisie était attendue par une étape `user` de workflow.
                let input_key = format!("tg.await_input.{chat_id}");
                if let Some(raw) = self.daemon.kv_get(&input_key).await?
                    && !raw.is_empty()
                {
                    self.daemon.kv_set(&input_key, "").await?;
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
                let reason_key = format!("tg.await_reason.{chat_id}");
                if let Some(approval_id) = self.daemon.kv_get(&reason_key).await?
                    && !approval_id.is_empty()
                {
                    self.daemon.kv_set(&reason_key, "").await?;
                    let d = Decision::deny("telegram", Some(text.clone()));
                    self.finalize_decision(&approval_id, &d, chat_id, topic_id)
                        .await?;
                    return Ok(());
                }

                // Profil vide : l'accueil est proposé une fois, sans retenir le message.
                if self.daemon.kv_get("tg.onboard.proposed").await?.is_none()
                    && crate::onboarding::profile_is_empty(&self.daemon).await
                {
                    self.daemon
                        .kv_set(
                            "tg.onboard.proposed",
                            &self.daemon.services.clock.now_rfc3339(),
                        )
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
                self.daemon
                    .enqueue_message(&session, &content, &origin, Some(format!("tg:{update_id}")))
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
                self.voice(
                    update_id,
                    chat_id,
                    topic_id,
                    message_id,
                    &file_id,
                    file_name.as_deref(),
                    mime_type.as_deref(),
                    file_size,
                )
                .await?;
            }
            photo @ Incoming::Photo { .. } => self.photo(photo).await?,
            document @ Incoming::Document { .. } => self.document(document).await?,
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
            Incoming::Ignored { reason, .. } => {
                tracing::debug!(%reason, "update ignoré");
            }
        }
        Ok(())
    }

    // ================================================================ commandes

    async fn command(
        &self,
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
                if let Some(old) = s.sessions.find_by_topic(chat_id, topic_id).await? {
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
                    format!("🆕 Nouvelle session « {title} » (`{}`).", sess.id)
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
                            d.kv_set(
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
                        let cancelled = self.bind_chat(&fork, chat_id, topic_id).await?;
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
                                v["messages"],
                                cancelled_note(cancelled)
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
                        v["removed"],
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
                match crate::session_ops::export(d, "session", Some(&session)).await {
                    Ok(v) => {
                        let path = std::path::PathBuf::from(v["path"].as_str().unwrap_or_default());
                        match self
                            .bot
                            .send_document(chat_id, topic_id, &path, Some("Export JSONL"))
                            .await
                        {
                            Ok(_) => return Ok(()),
                            Err(e) => format!(
                                "📦 Export écrit dans `{}`, envoi impossible : {e}",
                                path.display()
                            ),
                        }
                    }
                    Err(e) => format!("❌ {e}"),
                }
            }
            "upgrade" => {
                // Installer ou revenir en arrière : toujours confirmé (issue #30).
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
                let session = d.chat_session_for(&origin).await?;
                if d.bus.cancel_session(&session) {
                    "⏹ Arrêt demandé.".into()
                } else {
                    "Rien à arrêter.".into()
                }
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
                            let cancelled = self.bind_chat(&id, chat_id, topic_id).await?;
                            s.sessions.touch(&id).await?;
                            format!(
                                "↪️ Session « {} » reprise (`{id}`).{}",
                                crate::titles::label(&sess),
                                cancelled_note(cancelled)
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
                                v["generation"]
                            ),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
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
                            v["tool_count"],
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
                            "✅ `{name}` répond : protocole {}, {} outil(s), {} ms.",
                            v["protocol"].as_str().unwrap_or("?"),
                            v["tools"],
                            v["ms"]
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
                self.budget_text(&session, args).await?
            }
            "usage" => {
                let session = d.chat_session_for(&origin).await?;
                self.usage_text(&session, args).await?
            }
            "audit" => {
                let audit = crate::mem_audit::run(d).await?;
                crate::mem_audit::summary(&audit)
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
                    let vault = crate::conversation::vault_dir(s);
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
                        // Paramètres déclarés et rien de saisi : le formulaire les demande.
                        if rest.is_empty()
                            && let Some(w) = s.workflows.get(id)
                            && !w.metadata.parameters.is_empty()
                        {
                            let title = if w.metadata.name.is_empty() {
                                id.to_string()
                            } else {
                                w.metadata.name.clone()
                            };
                            return self.start_workflow_form(chat_id, id, &title).await;
                        }
                        let params = parse_params(rest);
                        match crate::workflow::start_run(d, id, params, &origin, None, 0).await {
                            Ok(run) => format!("▶️ Run `{}` lancé.", run.id),
                            Err(e) => format!("❌ {e}"),
                        }
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
                let vault = crate::conversation::vault_dir(s);
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

    /// Réponse suivie d'un bouton par suite proposée ; un clic envoie la suite comme message
    /// du propriétaire dans la session (issue #31).
    async fn send_choices(
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
                .daemon
                .services
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
    async fn send_model_menu(
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
        let s = &self.daemon.services;
        let session = view["session"].as_str().unwrap_or_default();
        let pinned = view["pinned"].as_str();
        let ttl = 7 * 24 * 3_600_000;

        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for c in view["choices"].as_array().cloned().unwrap_or_default() {
            let alias = c["alias"].as_str().unwrap_or("?");
            let model = short_model(c["model"].as_str().unwrap_or("?"));
            let token = s
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
        let auto = s
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
                match view["last_alias"].as_str() {
                    Some(last) => format!(
                        "Automatique ({how}). Dernier message : `{last}` · `{}`.",
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
    async fn workflow_choice_clicked(
        &self,
        callback_id: &str,
        action: &penelope_telegram::actions::Action,
        chat_id: i64,
        message_id: i64,
    ) -> anyhow::Result<()> {
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
                        None,
                        None,
                        "ℹ️ cette question n'est plus d'actualité",
                    )
                    .await;
            };
            let state = match penelope_telegram::forms::FormState::new(visit, schema) {
                Ok(st) => st,
                Err(e) => return self.reply(chat_id, None, None, &format!("❌ {e}")).await,
            };
            let pending = json!({"run": run, "visit": visit, "choice": choice, "state": state});
            self.daemon
                .kv_set(&form_key(chat_id), &pending.to_string())
                .await?;
            return self.send_form_step(chat_id, &pending).await;
        }
        let note = if wants_input {
            self.daemon
                .kv_set(
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
        self.reply(chat_id, None, None, &note).await
    }

    async fn model_pin_clicked(
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

    /// Vocal ou fichier audio (§14.4) : téléchargement, transcription par le rôle `stt`,
    /// texte montré en citation, puis traité comme un message tapé.
    /// Photo : enregistrée, puis confiée au tour (vision, §10.4). Les photos d'un album
    /// attendent leurs voisines pendant [`ALBUM_WINDOW`] et partent ensemble.
    async fn photo(&self, incoming: Incoming) -> anyhow::Result<()> {
        let Incoming::Photo {
            update_id,
            chat_id,
            message_id,
            topic_id,
            file_ids,
            file_size,
            media_group,
            caption,
            ..
        } = incoming
        else {
            return Ok(());
        };
        let reply_to = Some(message_id);
        let Some(file_id) = file_ids.last() else {
            return Ok(());
        };
        if file_size.unwrap_or(0) as usize > crate::media::IMAGE_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "📷 Photo trop lourde (10 Mo au plus).",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let saved = match self.bot.download_file(file_id).await {
            Ok((bytes, _)) => crate::media::save_photo(&self.daemon.services, &bytes),
            Err(e) => Err(format!("téléchargement impossible : {e}")),
        };
        let path = match saved {
            Ok(p) => p,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("📷 Photo ignorée : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let Some(group) = media_group else {
            return enqueue_photos(&self.daemon, &origin, vec![path], caption, update_id).await;
        };
        let first = {
            let mut albums = self
                .albums
                .lock()
                .map_err(|_| anyhow::anyhow!("albums verrouillés"))?;
            match albums.get_mut(&group) {
                Some(album) => {
                    album.images.push(path);
                    if album.caption.is_none() {
                        album.caption = caption;
                    }
                    false
                }
                None => {
                    albums.insert(
                        group.clone(),
                        Album {
                            origin,
                            images: vec![path],
                            caption,
                            update_id,
                        },
                    );
                    true
                }
            }
        };
        if first {
            let (daemon, albums) = (self.daemon.clone(), self.albums.clone());
            tokio::spawn(async move {
                tokio::time::sleep(ALBUM_WINDOW).await;
                let album = albums.lock().ok().and_then(|mut g| g.remove(&group));
                if let Some(a) = album
                    && let Err(e) =
                        enqueue_photos(&daemon, &a.origin, a.images, a.caption, a.update_id).await
                {
                    tracing::warn!(error = %e, "album Telegram non transmis");
                }
            });
        }
        Ok(())
    }

    /// Document : ingéré dans le vault s'il est lisible (§6.13), sinon rangé comme pièce
    /// jointe. Une légende est une demande : elle part en tour, document joint. `/mien` en
    /// tête de légende déclare un document rédigé par le propriétaire.
    async fn document(&self, incoming: Incoming) -> anyhow::Result<()> {
        let Incoming::Document {
            update_id,
            chat_id,
            message_id,
            topic_id,
            file_id,
            file_name,
            file_size,
            caption,
            ..
        } = incoming
        else {
            return Ok(());
        };
        let reply_to = Some(message_id);
        if file_size.unwrap_or(0) as usize > penelope_telegram::api::DOWNLOAD_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "📄 Fichier trop gros : un bot Telegram ne télécharge pas plus de 20 Mo. \
                     Le déposer dans `vault/inbox/` fonctionne aussi.",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let bytes = match self.bot.download_file(&file_id).await {
            Ok((b, _)) => b,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("📄 Téléchargement impossible : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = self.daemon.chat_session_for(&origin).await?;
        let caption = caption.unwrap_or_default();
        let caption = caption.trim();
        let (owner, request) = match caption.strip_prefix("/mien") {
            Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                (true, rest.trim().to_string())
            }
            _ => (false, caption.to_string()),
        };
        // L'ingestion appelle le modèle : la file des updates n'attend pas.
        let daemon = self.daemon.clone();
        tokio::spawn(async move {
            let dedup = Some(format!("tg:{update_id}"));
            let messenger = daemon.hooks.messenger();
            let say = |text: String| {
                let (m, o) = (messenger.clone(), origin.clone());
                async move {
                    if let Some(m) = m {
                        let _ = m.send_text(&o, &text).await;
                    }
                }
            };
            if penelope_memory::ingest::is_ingestible(&file_name) {
                let trust = if owner {
                    penelope_memory::Origin::Owner
                } else {
                    penelope_memory::Origin::Untrusted
                };
                match crate::ingest::ingest(
                    &daemon,
                    &file_name,
                    bytes,
                    "telegram",
                    trust,
                    Some(&session),
                )
                .await
                {
                    Ok(doc) => {
                        say(doc.report()).await;
                        if let (Some(m), Some(id)) = (&messenger, &doc.approval_id) {
                            let _ = m.send_approval(&origin, id).await;
                        }
                        if !request.is_empty() {
                            let text = doc.turn_text(&request);
                            if let Err(e) = daemon
                                .enqueue_message(&session, &text, &origin, dedup)
                                .await
                            {
                                tracing::warn!(error = %e, "demande sur document non transmise");
                            }
                        }
                    }
                    Err(e) => say(format!("📄 `{file_name}` non ingéré : {e}")).await,
                }
                return;
            }
            let (note, joined) = store_attachment(&daemon, &session, &file_name, &bytes).await;
            say(note).await;
            if let (false, Some(joined)) = (request.is_empty(), joined) {
                let text = format!("{request}\n\n{joined}");
                if let Err(e) = daemon
                    .enqueue_message(&session, &text, &origin, dedup)
                    .await
                {
                    tracing::warn!(error = %e, "demande sur pièce jointe non transmise");
                }
            }
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn voice(
        &self,
        update_id: i64,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        file_id: &str,
        file_name: Option<&str>,
        mime_type: Option<&str>,
        file_size: Option<i64>,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        if file_size.unwrap_or(0) as usize > penelope_telegram::api::DOWNLOAD_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "🎙️ Fichier trop gros : un bot Telegram ne télécharge pas plus de 20 Mo.",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let (audio, path) = match self.bot.download_file(file_id).await {
            Ok(x) => x,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("🎙️ Téléchargement du vocal impossible : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = self.daemon.chat_session_for(&origin).await?;
        let filename = audio_filename(&path, file_name, mime_type);
        let text = match self.daemon.transcribe(audio, &filename, &session).await {
            Ok(t) => t,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("🎙️ Transcription impossible : {e}"),
                    )
                    .await;
            }
        };
        if text.trim().is_empty() {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "🎙️ Rien d'audible dans ce vocal.",
                )
                .await;
        }
        let quoted: String = text
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.reply(chat_id, topic_id, reply_to, &format!("🎙️\n{quoted}"))
            .await?;
        self.daemon
            .enqueue_message(
                &session,
                &format!("(message vocal transcrit) {text}"),
                &origin,
                Some(format!("tg:{update_id}")),
            )
            .await?;
        Ok(())
    }

    /// `/budget` : dépense du jour et de la session, requêtes les plus chères.
    /// `/budget sessions|requêtes|modèles|jours` : un regroupement précis.
    /// `/usage [session|turn|model|day|role|upstream]` : tokens d'entrée, part en cache,
    /// sortie et coût (issue #20). `turn` se limite à la session du chat.
    async fn usage_text(&self, session: &str, args: &str) -> anyhow::Result<String> {
        let s = &self.daemon.services;
        let by = match args.trim() {
            "" | "sessions" => "session",
            "requêtes" | "requetes" | "tours" | "turns" => "turn",
            "modèles" | "modeles" | "models" => "model",
            other => other,
        };
        if !penelope_kernel::budget::USAGE_AXES.contains(&by) {
            return Ok(format!(
                "Regroupement inconnu `{by}`. Choix : {}.",
                penelope_kernel::budget::USAGE_AXES.join(", ")
            ));
        }
        let today: String = s.clock.now_rfc3339().chars().take(10).collect();
        let (scope, since, title) = match by {
            "turn" => (Some(session), None, "Requêtes de la session"),
            "day" => (None, None, "Par jour"),
            _ => (None, Some(today.as_str()), "Aujourd'hui"),
        };
        let rows = s.budget.report(by, scope, since, 10).await?;
        if rows.is_empty() {
            return Ok("Aucune consommation enregistrée.".into());
        }
        let k = |n: i64| {
            if n >= 1_000_000 {
                format!("{:.1} M", n as f64 / 1e6).replace('.', ",")
            } else if n >= 1_000 {
                format!("{} k", n / 1_000)
            } else {
                n.to_string()
            }
        };
        let mut t = format!("**{title}, par {by}**\n");
        for r in &rows {
            let label = r
                .label
                .as_deref()
                .map(|l| format!("« {l} »"))
                .unwrap_or_else(|| format!("`{}`", if r.key.is_empty() { "?" } else { &r.key }));
            t.push_str(&format!(
                "\n- {} · {label} · {} appel(s) · entrée {} (cache {:.0} %) · sortie {}",
                crate::budget_alert::usd(r.cost_usd),
                r.calls,
                k(r.prompt),
                r.cache_ratio() * 100.0,
                k(r.completion)
            ));
        }
        Ok(t)
    }

    async fn budget_text(&self, session: &str, args: &str) -> anyhow::Result<String> {
        let s = &self.daemon.services;
        let cfg = s.config.config();
        let usd = |x: f64| format!("{:.4} $", x).replace('.', ",");
        let axis = match args.trim() {
            "" => None,
            "sessions" | "session" => Some(("session", None, "Sessions les plus chères")),
            "requêtes" | "requetes" | "tours" | "turn" => {
                Some(("turn", Some(session), "Requêtes les plus chères (session)"))
            }
            "modèles" | "modeles" | "model" => Some(("model", None, "Par modèle")),
            "jours" | "day" => Some(("day", None, "Par jour")),
            "rôles" | "roles" | "role" => Some(("role", None, "Par usage")),
            other => {
                return Ok(format!(
                    "Regroupement inconnu `{other}`. Choix : sessions, requêtes, modèles, jours, rôles."
                ));
            }
        };

        let row_line = |r: &penelope_kernel::budget::UsageRow| {
            let label = r
                .label
                .as_deref()
                .map(|l| format!(" « {l} »"))
                .unwrap_or_default();
            let key = if r.key.is_empty() {
                "?"
            } else {
                r.key.as_str()
            };
            let est = if r.estimated > 0 { " (estimé)" } else { "" };
            format!(
                "- {}{est} · `{key}`{label} · {} appel(s)\n",
                usd(r.cost_usd),
                r.calls
            )
        };

        if let Some((by, scope, title)) = axis {
            let rows = s.budget.report(by, scope, None, 15).await?;
            if rows.is_empty() {
                return Ok("Aucune consommation enregistrée.".into());
            }
            let mut t = format!("**{title}**\n\n");
            for r in &rows {
                t.push_str(&row_line(r));
            }
            return Ok(t);
        }

        let today = s.budget.spent_today().await?;
        let in_session = s.budget.spent_session(session).await?;
        let mut t = format!(
            "💶 Aujourd'hui : {} sur {} · session : {} sur {}\n",
            usd(today),
            usd(cfg.budget.daily_usd),
            usd(in_session),
            usd(cfg.budget.session_usd)
        );
        let view = crate::compaction::context_view(s, session, None).await?;
        if let Some(prompt) = view["last_prompt_tokens"].as_i64() {
            let cached = view["last_cached_tokens"].as_i64().unwrap_or(0);
            t.push_str(&format!(
                "📏 Contexte : {} k tokens au dernier appel ({:.0} % en cache), compaction de \
                 fond vers {} k\n",
                prompt / 1000,
                if prompt > 0 {
                    cached as f64 * 100.0 / prompt as f64
                } else {
                    0.0
                },
                view["background_compaction_at"].as_u64().unwrap_or(0) / 1000
            ));
        }
        let turns = s.budget.report("turn", Some(session), None, 5).await?;
        if !turns.is_empty() {
            t.push_str("\n**Requêtes les plus chères de la session**\n\n");
            for r in &turns {
                t.push_str(&row_line(r));
            }
        }
        let today_day = s.clock.now_rfc3339().chars().take(10).collect::<String>();
        let models = s.budget.report("model", None, Some(&today_day), 5).await?;
        if !models.is_empty() {
            t.push_str("\n**Par modèle, aujourd'hui**\n\n");
            for r in &models {
                t.push_str(&format!(
                    "- {} · `{}` · {} appel(s)\n",
                    usd(r.cost_usd),
                    r.key,
                    r.calls
                ));
            }
        }
        t.push_str("\nDétail : `/budget sessions`, `/budget requêtes`, `/budget modèles`");
        Ok(t)
    }

    // ================================================================ boutons

    async fn callback(
        &self,
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
            return self.form_clicked(action, chat_id).await;
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
                .workflow_choice_clicked(callback_id, action, chat_id, message_id)
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
        let topic_id = self.topic_of(&approval).await;
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
                self.finalize_decision(
                    &approval_id,
                    &Decision::deny("telegram", None),
                    chat_id,
                    topic_id,
                )
                .await?;
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
            k::DENY_REASON => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.daemon
                    .kv_set(&format!("tg.await_reason.{chat_id}"), &approval_id)
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
    async fn finalize_decision(
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
        let note = match (first, a.state) {
            (false, st) => format!(
                "ℹ️ Déjà tranché : {} via {}.",
                st.as_str(),
                a.decided_via.clone().unwrap_or_default()
            ),
            (true, ApprovalState::Approved) if checkpoint => "▶️ Je continue.".to_string(),
            (true, _) if checkpoint => "⏹ Tour arrêté.".to_string(),
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

    async fn topic_of(&self, a: &ApprovalRequest) -> Option<i64> {
        let sid = a.session_id.as_deref()?;
        self.daemon
            .services
            .sessions
            .get(sid)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.tg_topic_id)
    }

    // ================================================================ cartes

    pub async fn send_approval_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        if a.kind == penelope_hitl::ApprovalKind::MemoryProposal {
            return self.send_memory_card(chat_id, topic_id, a).await;
        }
        let s = &self.daemon.services;
        // Point de contrôle de coût d'un tour (issue #19) : continuer ou arrêter.
        if a.payload["checkpoint"].as_bool() == Some(true) {
            let ttl = 24 * 3_600_000;
            let mut row = Vec::new();
            for (label, action) in [("▶️ Continuer", k::APPROVE), ("⏹ Arrêter", k::DENY)] {
                let t = s
                    .actions
                    .create(action, a.id.as_str(), json!({}), ttl, true)
                    .await?;
                row.push(ButtonSpec::callback(label, &t.token, ""));
            }
            let text = format!(
                "💸 {}",
                a.payload["reason"]
                    .as_str()
                    .unwrap_or("Ce tour coûte cher, je continue ?")
            );
            return self
                .outbox_push(
                    chat_id,
                    topic_id,
                    "sendMessage",
                    json!({
                        "chat_id": chat_id,
                        "text": markdown_to_html(&text),
                        "parse_mode": "HTML",
                        "reply_markup": inline_keyboard(&[row]),
                        "message_thread_id": topic_id,
                    }),
                )
                .await;
        }
        let args = serde_json::to_string_pretty(&a.payload["arguments"])
            .unwrap_or_default()
            .replace("```", "ʼʼʼ");
        let args: String = if args.chars().count() > 1_500 {
            format!("{}…", args.chars().take(1_500).collect::<String>())
        } else {
            args
        };
        let server = crate::agent::server_of(&a.subject).unwrap_or_else(|| "natif".into());
        let double = a.payload["double"].as_bool().unwrap_or(false);
        let mut vars = BTreeMap::new();
        vars.insert("outil".into(), a.subject.clone());
        vars.insert("serveur".into(), server);
        vars.insert("risque".into(), a.risk.as_str().to_string());
        vars.insert("arguments".into(), args);
        vars.insert(
            "raison".into(),
            a.payload["reason"].as_str().unwrap_or("").to_string(),
        );
        vars.insert(
            "alerte".into(),
            if double {
                "⚠️ Action destructive : une seconde confirmation sera demandée.".into()
            } else {
                String::new()
            },
        );

        let ttl = 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [
            k::APPROVE,
            k::APPROVE_RUN,
            k::APPROVE_ALWAYS,
            k::DENY,
            k::DENY_REASON,
        ] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("tool_approval")
            .ok_or_else(|| anyhow::anyhow!("gabarit tool_approval absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // Le libellé « Pour ce run » du gabarit vaut « pour cette session » en conversation.
        let buttons: Vec<Vec<ButtonSpec>> = rendered
            .buttons
            .iter()
            .map(|row| {
                row.iter()
                    .map(|b| {
                        let mut b = b.clone();
                        if b.label.contains("Pour ce run") {
                            b.label = "✅ Pour cette session".into();
                        }
                        b
                    })
                    .collect()
            })
            .collect();
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Menu `/sessions` : un bouton par session (bascule), un « ⋯ » par session (forker,
    /// renommer, fermer), pagination, sessions fermées masquées sauf `all` (issue #14).
    /// `edit` : message à remplacer plutôt qu'un nouvel envoi.
    async fn send_sessions_menu(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        page: usize,
        all: bool,
        edit: Option<i64>,
    ) -> anyhow::Result<()> {
        const PER_PAGE: usize = 12;
        let d = &self.daemon;
        let s = &d.services;
        let current = s
            .sessions
            .find_by_topic(chat_id, topic_id)
            .await?
            .map(|x| x.id.to_string());
        let sessions: Vec<_> = s
            .sessions
            .list(Some(penelope_kernel::session::SessionKind::Chat), 1_000)
            .await?
            .into_iter()
            .filter(|x| all || x.state != "closed")
            .collect();
        let busy: std::collections::BTreeMap<String, i64> = s
            .store
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT session_id, COUNT(*) FROM turn_queue
                     WHERE state IN ('pending', 'leased') GROUP BY session_id",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        let pages = sessions.len().div_ceil(PER_PAGE).max(1);
        let page = page.min(pages - 1);
        let ttl = 24 * 3_600_000;
        let make = |label: String, action: &'static str, target: String, args: Value| async move {
            s.actions
                .create(action, &target, args, ttl, true)
                .await
                .map(|t| ButtonSpec::callback(&label, &t.token, ""))
        };
        let nav = json!({"page": page, "all": all});
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for (i, sess) in sessions
            .iter()
            .enumerate()
            .skip(page * PER_PAGE)
            .take(PER_PAGE)
        {
            let id = sess.id.to_string();
            let mut label = String::new();
            if current.as_deref() == Some(id.as_str()) {
                label.push_str("▶️ ");
            } else if sess.state == "closed" {
                label.push_str("🔒 ");
            }
            if busy.get(&id).is_some_and(|n| *n > 0) {
                label.push_str("⏳ ");
            }
            let title = crate::titles::label(sess);
            label.push_str(&title.chars().take(48).collect::<String>());
            // La plus récente porte aussi l'heure de sa dernière activité.
            if i == 0
                && let Some(hm) = sess.last_activity.as_deref().and_then(|t| t.get(11..16))
            {
                label.push_str(&format!(" {hm}"));
            }
            rows.push(vec![
                make(label, k::SESSION_SWITCH, id.clone(), nav.clone()).await?,
                make("⋯".into(), k::SESSION_MENU, id, nav.clone()).await?,
            ]);
        }
        let mut footer = Vec::new();
        if page > 0 {
            footer.push(
                make(
                    "« Plus récentes".into(),
                    k::SESSIONS_PAGE,
                    String::new(),
                    json!({"page": page - 1, "all": all}),
                )
                .await?,
            );
        }
        if page + 1 < pages {
            footer.push(
                make(
                    "Plus anciennes »".into(),
                    k::SESSIONS_PAGE,
                    String::new(),
                    json!({"page": page + 1, "all": all}),
                )
                .await?,
            );
        }
        footer.push(
            make(
                if all {
                    "Masquer les fermées".into()
                } else {
                    "Voir les fermées".into()
                },
                k::SESSIONS_PAGE,
                String::new(),
                json!({"page": 0, "all": !all}),
            )
            .await?,
        );
        rows.push(footer);
        let text = if sessions.is_empty() {
            "**Sessions** : aucune.".to_string()
        } else {
            format!(
                "**Sessions** ({} · page {}/{pages})\nUn clic bascule ce chat sur la session ; \
                 « ⋯ » pour forker, renommer ou fermer. ▶️ session de ce chat, ⏳ tour en cours \
                 ou en attente.",
                sessions.len(),
                page + 1
            )
        };
        let html = markdown_to_html(&text);
        let keyboard = inline_keyboard(&rows);
        if let Some(message_id) = edit {
            match self
                .bot
                .edit_text(chat_id, message_id, &html, Some(keyboard.clone()))
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if e.to_string().contains("not modified") => return Ok(()),
                Err(_) => {}
            }
        }
        self.bot
            .send_text(chat_id, topic_id, &html, Some(keyboard), None)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Boutons du menu `/sessions`.
    async fn session_menu_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let page = action.args["page"].as_u64().unwrap_or(0) as usize;
        let all = action.args["all"].as_bool().unwrap_or(false);
        let target = action.target.clone();
        let toast = match action.action.as_str() {
            k::SESSION_SWITCH => {
                let Some(sess) = s.sessions.get(&target).await? else {
                    let _ = self
                        .bot
                        .answer_callback(callback_id, Some("Session introuvable."), false)
                        .await;
                    return self
                        .send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
                        .await;
                };
                if sess.state != "active" {
                    s.sessions.set_state(&target, "active").await?;
                }
                let cancelled = self.bind_chat(&target, chat_id, topic_id).await?;
                s.sessions.touch(&target).await?;
                let mut t = format!("Session « {} »", crate::titles::label(&sess));
                if cancelled > 0 {
                    t.push_str(&format!(" ({cancelled} en attente abandonné(s) ailleurs)"));
                }
                // Bouton d'une notification « réponses en attente » : pas de menu à redessiner.
                if action.args["notice"].as_bool() == Some(true) {
                    let _ = self.bot.answer_callback(callback_id, Some(&t), false).await;
                    let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                    return Ok(());
                }
                Some(t)
            }
            k::SESSION_FORK => match crate::session_ops::fork(d, &target, None).await {
                Ok(v) => {
                    let fork = v["session"].as_str().unwrap_or_default().to_string();
                    self.bind_chat(&fork, chat_id, topic_id).await?;
                    s.sessions.touch(&fork).await?;
                    Some("Session dupliquée : la suite se passe dans le fork.".to_string())
                }
                Err(e) => Some(format!("Fork impossible : {e}")),
            },
            k::SESSION_CLOSE => match crate::session_ops::close(d, &target).await {
                Ok(v) => Some(format!(
                    "Session fermée{}",
                    match v["cancelled"].as_u64().unwrap_or(0) {
                        0 => String::new(),
                        n => format!(", {n} en attente annulé(s)"),
                    }
                )),
                Err(e) => Some(format!("Fermeture impossible : {e}")),
            },
            k::SESSION_RENAME => {
                d.kv_set(
                    &format!("tg.await_title.{chat_id}"),
                    &format!("{target} {}", s.clock.now_ms()),
                )
                .await?;
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        "✏️ Envoie le nouveau titre de la session en un message (dans les \
                         5 minutes).",
                    )
                    .await;
            }
            k::SESSION_MENU => {
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                return self
                    .send_session_actions(chat_id, topic_id, message_id, &target, page, all)
                    .await;
            }
            _ => None,
        };
        let _ = self
            .bot
            .answer_callback(callback_id, toast.as_deref(), false)
            .await;
        self.send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
            .await
    }

    /// Sous-menu d'une session : basculer, forker, renommer, fermer, retour.
    async fn send_session_actions(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        session_id: &str,
        page: usize,
        all: bool,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let Some(sess) = s.sessions.get(session_id).await? else {
            return self
                .send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
                .await;
        };
        let ttl = 24 * 3_600_000;
        let nav = json!({"page": page, "all": all});
        let mut rows = Vec::new();
        for pair in [
            [
                ("↪️ Basculer", k::SESSION_SWITCH),
                ("🍴 Forker", k::SESSION_FORK),
            ],
            [
                ("✏️ Renommer", k::SESSION_RENAME),
                ("🔒 Fermer", k::SESSION_CLOSE),
            ],
        ] {
            let mut row = Vec::new();
            for (label, action) in pair {
                let t = s
                    .actions
                    .create(action, session_id, nav.clone(), ttl, true)
                    .await?;
                row.push(ButtonSpec::callback(label, &t.token, ""));
            }
            rows.push(row);
        }
        let back = s
            .actions
            .create(k::SESSIONS_PAGE, "", nav, ttl, true)
            .await?;
        rows.push(vec![ButtonSpec::callback("↩️ Retour", &back.token, "")]);
        let text = format!(
            "**{}**\n`{}` · {}",
            crate::titles::label(&sess),
            sess.id,
            if sess.state == "closed" {
                "fermée"
            } else {
                "active"
            }
        );
        self.bot
            .edit_text(
                chat_id,
                message_id,
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Lie une session au chat : elle a le focus. Celles qui le perdent finissent leur tour
    /// en cours, dont la sortie est mise de côté, et leur file est annulée ; la session liée
    /// reçoit ce qui l'attendait (issue #10). Renvoie le nombre de tours annulés.
    async fn bind_chat(
        &self,
        session_id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<usize> {
        let d = &self.daemon;
        let mut cancelled = 0;
        for other in d
            .services
            .sessions
            .bind_telegram(session_id, chat_id, topic_id)
            .await?
        {
            cancelled += d
                .services
                .turns
                .cancel_pending(&other, "session détachée du chat")
                .await?;
        }
        self.flush_held(session_id).await?;
        Ok(cancelled)
    }

    /// Vrai quand une autre session a le focus du chat : la sortie de `session_id` est
    /// alors mise de côté. Seules les sessions de conversation actives sont concernées, et un
    /// chat sans session liée reçoit tout.
    async fn out_of_focus(&self, session_id: &str, chat_id: i64, topic_id: Option<i64>) -> bool {
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
    async fn hold(
        &self,
        session_id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
        item: Held,
    ) -> anyhow::Result<()> {
        use penelope_telegram::render::escape_html;
        let _guard = self.held_lock.lock().await;
        let d = &self.daemon;
        let key = held_key(session_id);
        let mut held: Value = d
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
        let title = d
            .services
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
        let token = d
            .services
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
        d.kv_set(&key, &held.to_string()).await?;
        Ok(())
    }

    /// Retour au focus : les sorties mises de côté partent dans l'ordre, approbations encore
    /// ouvertes comprises. Renvoie le nombre de sorties envoyées.
    async fn flush_held(&self, session_id: &str) -> anyhow::Result<usize> {
        let _guard = self.held_lock.lock().await;
        let d = &self.daemon;
        let key = held_key(session_id);
        let Some(held) = d
            .kv_get(&key)
            .await?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            return Ok(0);
        };
        d.kv_delete(&key).await?;
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
                    if let Some(a) = d.services.approvals.get(id).await?
                        && a.state == ApprovalState::Pending
                    {
                        self.send_approval_card(chat_id, topic_id, &a).await?;
                    }
                }
                Held::Failure { error, reply_to } => {
                    self.send_failure(chat_id, topic_id, *reply_to, session_id, error)
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
            }
        }
        Ok(items.len())
    }

    /// Écran courant d'un formulaire : le champ à remplir, ou le récapitulatif à envoyer.
    async fn send_form_step(&self, chat_id: i64, pending: &Value) -> anyhow::Result<()> {
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
                None,
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Une réponse au champ courant (message ou bouton ; `None` : passer le champ).
    async fn form_input(
        &self,
        chat_id: i64,
        raw: &str,
        answer: Option<&str>,
    ) -> anyhow::Result<()> {
        use penelope_telegram::forms::FormState;
        let mut pending: Value = serde_json::from_str(raw)?;
        let mut state: FormState = serde_json::from_value(pending["state"].clone())?;
        let applied = match answer {
            Some(a) => state.answer(a),
            None => state.skip(),
        };
        if let Err(e) = applied {
            self.reply(chat_id, None, None, &format!("⚠️ {e}")).await?;
            return self.send_form_step(chat_id, &pending).await;
        }
        pending["state"] = serde_json::to_value(&state)?;
        self.daemon
            .kv_set(&form_key(chat_id), &pending.to_string())
            .await?;
        self.send_form_step(chat_id, &pending).await
    }

    /// Boutons d'un formulaire : passer, revenir, envoyer, abandonner.
    async fn form_clicked(&self, action: &Action, chat_id: i64) -> anyhow::Result<()> {
        use penelope_telegram::forms::FormState;
        let d = &self.daemon;
        let Some(raw) = d
            .kv_get(&form_key(chat_id))
            .await?
            .filter(|r| !r.is_empty())
        else {
            return self
                .reply(chat_id, None, None, "ℹ️ aucun formulaire en cours")
                .await;
        };
        let mut pending: Value = serde_json::from_str(&raw)?;
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
                d.kv_set(&form_key(chat_id), &pending.to_string()).await?;
                self.send_form_step(chat_id, &pending).await
            }
            k::FORM_DECLINE => {
                d.kv_set(&form_key(chat_id), "").await?;
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
                        )
                        .await;
                }
                self.reply(
                    chat_id,
                    None,
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
                        self.reply(chat_id, None, None, &format!("⚠️ {e}")).await?;
                        return self.send_form_step(chat_id, &pending).await;
                    }
                };
                d.kv_set(&form_key(chat_id), "").await?;
                // Paramètres d'un workflow lancé depuis `/wf` ou `/run` (issue #30).
                if let Some(workflow) = pending["workflow"].as_str() {
                    let origin = Origin::Telegram {
                        chat_id,
                        topic_id: None,
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
                    return self.reply(chat_id, None, None, &note).await;
                }
                // Arguments d'un prompt MCP (`/p`).
                if let (Some(server), Some(prompt)) = (
                    pending["prompt"]["server"].as_str(),
                    pending["prompt"]["name"].as_str(),
                ) {
                    let note = match self
                        .run_mcp_prompt(chat_id, None, server, prompt, values)
                        .await
                    {
                        Ok(n) => {
                            format!("💬 Prompt `{prompt}` : {n} message(s) envoyé(s) au modèle.")
                        }
                        Err(e) => format!("❌ {e}"),
                    };
                    return self.reply(chat_id, None, None, &note).await;
                }
                if let Some(id) = pending["elicitation"].as_str() {
                    return self
                        .finish_elicitation(
                            chat_id,
                            id,
                            crate::elicitation::Action::Accept(Some(values)),
                            "✔️ Formulaire envoyé à `{server}`.",
                            true,
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
                self.reply(chat_id, None, None, &note).await
            }
        }
    }

    // ============================================================ accueil

    async fn propose_onboarding(&self, chat_id: i64, topic_id: Option<i64>) -> anyhow::Result<()> {
        let t = self
            .daemon
            .services
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
    async fn onboarding_next(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        part: Option<crate::onboarding::Part>,
    ) -> anyhow::Result<()> {
        let sitting = crate::onboarding::start(&self.daemon, part).await?;
        self.onboarding_ask(chat_id, topic_id, &sitting).await
    }

    /// Pose la question suivante, ou montre le récapitulatif à valider.
    async fn onboarding_ask(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        sitting: &crate::onboarding::Sitting,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let key = format!("tg.onboard.{chat_id}");
        let ttl = 7 * 24 * 3_600_000;
        let button = |label: String, action: &'static str, args: Value| {
            let rel = sitting.rel.clone();
            async move {
                s.actions
                    .create(action, &rel, args, ttl, true)
                    .await
                    .map(|t| ButtonSpec::callback(&label, &t.token, ""))
            }
        };
        let Some(q) = sitting.next() else {
            d.kv_set(&key, "").await?;
            let plan = crate::onboarding::plan(d, sitting).await?;
            if plan.is_empty() && plan.keep.is_empty() {
                crate::onboarding::cancel(d).await?;
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
                    &markdown_to_html(&crate::onboarding::plan_text(&plan)),
                    Some(inline_keyboard(&rows)),
                    None,
                )
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!(e.to_string()));
        };
        d.kv_set(&key, &json!({"rel": sitting.rel, "n": q.n}).to_string())
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
    async fn onboarding_clicked(
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
                match crate::onboarding::answer(d, rel, n, action.args["answer"].as_str()).await {
                    Ok(sitting) => self.onboarding_ask(chat_id, topic_id, &sitting).await,
                    Err(e) => {
                        self.reply(chat_id, topic_id, None, &format!("⚠️ {e}"))
                            .await
                    }
                }
            }
            k::ONBOARD_PAUSE => {
                d.kv_set(&format!("tg.onboard.{chat_id}"), "").await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    "⏸ Accueil en pause : `/accueil` reprend à la première question sans réponse.",
                )
                .await
            }
            k::ONBOARD_WRITE => {
                let Some(sitting) = crate::onboarding::load(d, rel) else {
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
                let (added, replaced) = crate::onboarding::write(d, &sitting, &session).await?;
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
                crate::onboarding::cancel(d).await?;
                d.kv_set(&format!("tg.onboard.{chat_id}"), "").await?;
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

    // ============================================================ élicitation MCP

    /// Texte d'une carte d'élicitation : le serveur est nommé, son message cité et échappé,
    /// un lien montré en entier avec son domaine (§8.4, issue #12).
    fn elicitation_html(r: &crate::elicitation::Request) -> String {
        use crate::elicitation::Kind;
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
        r: &crate::elicitation::Request,
    ) -> anyhow::Result<ButtonSpec> {
        let ttl = r.timeout.as_millis() as i64 + 3_600_000;
        let t = self
            .daemon
            .services
            .actions
            .create(action, &r.id, json!({}), ttl, true)
            .await?;
        Ok(ButtonSpec::callback(label, &t.token, ""))
    }

    /// Remplace la carte (texte d'origine, puis l'issue) ; à défaut, un nouveau message.
    async fn elicitation_update(
        &self,
        r: &crate::elicitation::Request,
        card: Option<i64>,
        note: &str,
        keyboard: Option<Value>,
    ) -> anyhow::Result<()> {
        let html = format!(
            "{}\n\n{}",
            Self::elicitation_html(r),
            markdown_to_html(note)
        );
        if let Some(id) = card
            && self
                .bot
                .edit_text(self.owner_id, id, &html, keyboard.clone())
                .await
                .is_ok()
        {
            return Ok(());
        }
        self.bot
            .send_text(self.owner_id, None, &html, keyboard, None)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Répond au serveur, met la carte à jour et le dit dans le chat. `note` : `{server}`
    /// est remplacé par le nom du serveur.
    async fn finish_elicitation(
        &self,
        chat_id: i64,
        id: &str,
        answer: crate::elicitation::Action,
        note: &str,
        echo: bool,
    ) -> anyhow::Result<()> {
        let broker = &self.daemon.services.elicitations;
        let card = broker.request(id).and_then(|(_, c)| c);
        match broker.resolve(id, answer) {
            Ok(request) => {
                if let Some(raw) = self.daemon.kv_get(&form_key(chat_id)).await?
                    && serde_json::from_str::<Value>(&raw)
                        .is_ok_and(|p| p["elicitation"].as_str() == Some(id))
                {
                    self.daemon.kv_set(&form_key(chat_id), "").await?;
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
    async fn elicitation_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        message_id: i64,
    ) -> anyhow::Result<()> {
        use crate::elicitation::{Action as Answer, Kind};
        let broker = self.daemon.services.elicitations.clone();
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
                    let pending = json!({
                        "elicitation": request.id,
                        "choice": format!("{} · {title}", request.server),
                        "state": state,
                    });
                    self.daemon
                        .kv_set(&form_key(chat_id), &pending.to_string())
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
        self.finish_elicitation(chat_id, &request.id, answer, note, elsewhere)
            .await
    }

    /// Échec d'un tour, avec un bouton « Réessayer » qui relance la réponse sur le même
    /// transcript.
    async fn send_failure(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        session_id: &str,
        error: &str,
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
        let mut payload = json!({
            "chat_id": chat_id,
            "text": markdown_to_html(&format!("❌ {error}")),
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
            "reply_markup": inline_keyboard(&[vec![ButtonSpec::callback(
                "🔁 Réessayer",
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

    /// Carte `mcp_oauth_required` : bouton d'autorisation, collage, relance (§8.5).
    async fn send_oauth_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        server: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let Some(cfg) = (match d.hooks.mcp_supervisor() {
            Some(sup) => sup.config_of(server).await,
            None => None,
        }) else {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!("Serveur MCP `{server}` inconnu (`/mcp`)."),
                )
                .await;
        };
        let start = match crate::mcp_auth::start(d, &cfg, None).await {
            Ok(st) => st,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        &format!("🔐 Autorisation impossible : {e}"),
                    )
                    .await;
            }
        };
        let ttl = crate::mcp_auth::REQUEST_TTL_MS;
        let mut tokens = BTreeMap::new();
        for action in [k::OAUTH_PASTED, k::OAUTH_RETRY] {
            let t = s
                .actions
                .create(action, server, json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let mut vars = BTreeMap::new();
        vars.insert("serveur".into(), server.to_string());
        vars.insert(
            "scopes".into(),
            if start.scopes.is_empty() {
                "par défaut".into()
            } else {
                start.scopes.join(" ")
            },
        );
        vars.insert(
            "mode".into(),
            if start.mode == "paste_back" {
                "si la page finit sur une erreur 127.0.0.1, colle son adresse ici (10 min)".into()
            } else {
                "retour automatique".into()
            },
        );
        vars.insert("url".into(), start.url.clone());
        let tpl = s
            .templates
            .get("mcp_oauth_required")
            .ok_or_else(|| anyhow::anyhow!("gabarit mcp_oauth_required absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rendered.buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Carte `memory_proposal` : les faits proposés, « Tout » ou « Rien ».
    async fn send_memory_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let items: Vec<String> = a.payload["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|i| i.as_str().map(|t| format!("- {t}")))
            .collect();
        let mut vars = BTreeMap::new();
        vars.insert(
            "items".into(),
            format!(
                "{}\n\nSource : `{}`",
                items.join("\n"),
                a.payload["source"].as_str().unwrap_or("?")
            ),
        );
        let ttl = 7 * 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [k::MEMORY_ACCEPT, k::MEMORY_AS_EXCEPTION, k::MEMORY_REJECT] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("memory_proposal")
            .ok_or_else(|| anyhow::anyhow!("gabarit memory_proposal absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // Un fait tiré d'un document n'a pas de contexte d'exception : « Tout » ou « Rien ».
        let buttons: Vec<Vec<ButtonSpec>> = rendered
            .buttons
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|b| !b.label.contains("exception"))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    async fn send_destructive_confirm(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let confirm = s
            .actions
            .create(
                k::CONFIRM_DESTRUCTIVE,
                a.id.as_str(),
                json!({}),
                600_000,
                true,
            )
            .await?;
        let deny = s
            .actions
            .create(k::DENY, a.id.as_str(), json!({}), 600_000, true)
            .await?;
        let buttons = vec![vec![
            ButtonSpec::callback("⚠️ Confirmer", &confirm.token, "danger"),
            ButtonSpec::callback("Annuler", &deny.token, ""),
        ]];
        let text = format!(
            "⚠️ **Seconde confirmation**\n\n`{}` est une action destructive. Confirmer ?",
            a.subject
        );
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(&text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
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
        for (i, fragment) in penelope_telegram::split_message(&body, FRAGMENT_CHARS)
            .iter()
            .enumerate()
        {
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

    async fn outbox_push(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        method: &str,
        mut payload: Value,
    ) -> anyhow::Result<()> {
        if let Some(o) = payload.as_object_mut() {
            o.retain(|_, v| !v.is_null());
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

    async fn outbox_loop(self: Arc<Self>) {
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
                let mut st = c.prepare(
                    "SELECT id, chat_id, method, payload, attempts FROM tg_outbox
                     WHERE state = 'pending' AND (not_before IS NULL OR not_before <= ?1)
                     ORDER BY created_at, rowid LIMIT 25",
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
        for (id, chat_id, method, payload, attempts) in rows {
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
                    s.store
                        .write(move |tx| {
                            tx.execute(
                                "UPDATE tg_outbox SET attempts=?2, error=?3, not_before=?4,
                                    state = CASE WHEN ?5 = 1 THEN 'failed' ELSE state END
                                 WHERE id=?1",
                                params![id, attempts, err, not_before, give_up as i64],
                            )?;
                            Ok(())
                        })
                        .await?;
                }
            }
        }
        Ok(n)
    }

    /// Réaction d'état sur le message du propriétaire, sans jamais bloquer.
    fn react(&self, chat_id: i64, message_id: i64, emoji: &'static str) {
        let bot = self.bot.clone();
        tokio::spawn(async move {
            let _ = bot.set_reaction(chat_id, message_id, emoji).await;
        });
    }

    // ================================================================ brouillons

    async fn draft_loop(self: Arc<Self>) {
        struct Draft {
            chat_id: i64,
            topic_id: Option<i64>,
            draft_id: i64,
            text: String,
            last: Instant,
            /// Dernière vérification du focus : une session quittée cesse d'écrire.
            checked: Instant,
        }
        const FOCUS_EVERY: Duration = Duration::from_secs(2);
        let mut rx = self.daemon.bus.subscribe();
        let mut drafts: HashMap<String, Draft> = HashMap::new();
        while !self.shutting_down() {
            let ev = match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Err(_) => continue,
                Ok(Ok(ev)) => ev,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
            };
            let Some((chat_id, topic_id)) = ev.origin.telegram_chat() else {
                continue;
            };
            // `sendMessageDraft` ne vaut que pour une conversation privée.
            if chat_id <= 0 {
                continue;
            }
            // Session en arrière-plan : ni brouillon ni « écrit… » (issue #10).
            if let Some(d) = drafts.get_mut(&ev.turn_id)
                && d.checked.elapsed() >= FOCUS_EVERY
            {
                d.checked = Instant::now();
                if self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                    drafts.remove(&ev.turn_id);
                    continue;
                }
            }
            match &ev.kind {
                BusKind::Started => {
                    if self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                        continue;
                    }
                    drafts.insert(
                        ev.turn_id.clone(),
                        Draft {
                            chat_id,
                            topic_id,
                            draft_id: crate::bus::draft_id_for(&ev.turn_id),
                            text: String::new(),
                            last: Instant::now() - self.draft_interval,
                            checked: Instant::now(),
                        },
                    );
                    let bot = self.bot.clone();
                    tokio::spawn(async move {
                        let _ = bot
                            .call(
                                penelope_telegram::api::method::SEND_CHAT_ACTION,
                                None,
                                json!({"chat_id": chat_id, "action": "typing"}),
                            )
                            .await;
                    });
                }
                BusKind::Event(TurnEvent::Delta(t)) => {
                    if let Some(d) = drafts.get_mut(&ev.turn_id) {
                        d.text.push_str(t);
                        if d.last.elapsed() >= self.draft_interval {
                            d.last = Instant::now();
                            self.spawn_draft(d.chat_id, d.topic_id, d.draft_id, &d.text);
                        }
                    }
                }
                BusKind::Event(TurnEvent::ToolCall { name, .. }) => {
                    if let Some(d) = drafts.get_mut(&ev.turn_id) {
                        d.last = Instant::now();
                        let preview = format!("{}\n\n⚙️ {name}…", d.text.trim_end());
                        self.spawn_draft(d.chat_id, d.topic_id, d.draft_id, preview.trim());
                    }
                }
                BusKind::Finished(_) => {
                    drafts.remove(&ev.turn_id);
                }
                _ => {}
            }
        }
    }

    fn spawn_draft(&self, chat_id: i64, topic_id: Option<i64>, draft_id: i64, text: &str) {
        let text: String = text
            .chars()
            .rev()
            .take(4_000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if text.trim().is_empty() {
            return;
        }
        let bot = self.bot.clone();
        tokio::spawn(async move {
            let _ = bot.send_draft(chat_id, topic_id, draft_id, &text).await;
        });
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for TelegramGateway {
    async fn session_titled(&self, session_id: &str, title: &str) {
        let key = format!("tg.new_session.{session_id}");
        let Some((chat_id, message_id)) =
            self.daemon.kv_get(&key).await.ok().flatten().and_then(|v| {
                let (c, m) = v.split_once(':')?;
                Some((c.parse::<i64>().ok()?, m.parse::<i64>().ok()?))
            })
        else {
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
        let _ = self.daemon.kv_delete(&key).await;
    }

    async fn deliver(
        &self,
        _turn_id: &str,
        session_id: &str,
        origin: &Origin,
        outcome: &TurnOutcome,
    ) {
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
                    text: crate::agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
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
        let result: anyhow::Result<()> = async {
            match outcome {
                TurnOutcome::Answered { text, .. } => {
                    self.reply(chat_id, topic_id, message_id, text).await?;
                    if let Some(mid) = message_id {
                        self.react(chat_id, mid, reaction::DONE);
                    }
                }
                TurnOutcome::AwaitingApproval { approval_id } => {
                    if let Some(a) = self.daemon.services.approvals.get(approval_id).await? {
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
                    self.reply(
                        chat_id,
                        topic_id,
                        message_id,
                        &crate::agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
                    )
                    .await?;
                }
                TurnOutcome::Failed { error } => {
                    self.send_failure(chat_id, topic_id, message_id, session_id, error)
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
#[async_trait::async_trait]
impl crate::elicitation::OwnerChannel for TelegramGateway {
    async fn show(&self, r: &crate::elicitation::Request) -> Result<Option<i64>, String> {
        use crate::elicitation::Kind;
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
            crate::elicitation::human(r.timeout)
        );
        let sent = self
            .bot
            .send_text(
                self.owner_id,
                None,
                &html,
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(sent["message_id"].as_i64())
    }

    async fn close(&self, r: &crate::elicitation::Request, card: Option<i64>, markdown: &str) {
        if let Err(e) = self.elicitation_update(r, card, markdown, None).await {
            tracing::warn!(error = %e, "carte d'élicitation non mise à jour");
        }
    }
}

#[async_trait::async_trait]
impl Messenger for TelegramGateway {
    async fn send_text(&self, origin: &Origin, markdown: &str) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or((self.owner_id, None));
        self.reply(chat_id, topic_id, None, markdown)
            .await
            .map_err(|e| e.to_string())
    }

    async fn send_file(
        &self,
        origin: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or((self.owner_id, None));
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
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or((self.owner_id, None));
        let s = &self.daemon.services;
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
            let t = s
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
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or((self.owner_id, None));
        let html = markdown_to_html(markdown);
        let kv_key = format!("tg.card.{key}");
        let known = self
            .daemon
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
            let _ = self.daemon.kv_set(&kv_key, &id.to_string()).await;
        }
        Ok(())
    }

    async fn send_approval(&self, origin: &Origin, approval_id: &str) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or((self.owner_id, None));
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

/// `clé=valeur clé2=valeur2` : chaque valeur est lue en JSON si possible (nombres,
/// booléens), sinon gardée en texte.
pub fn parse_params(raw: &str) -> Value {
    let mut out = serde_json::Map::new();
    for pair in raw.split_whitespace() {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let value = serde_json::from_str::<Value>(v)
            .ok()
            .filter(|x| !x.is_string())
            .unwrap_or_else(|| json!(v));
        out.insert(k.to_string(), value);
    }
    Value::Object(out)
}

/// Met les photos reçues en file, dans la session de la conversation.
async fn enqueue_photos(
    daemon: &Arc<Daemon>,
    origin: &Origin,
    images: Vec<std::path::PathBuf>,
    caption: Option<String>,
    update_id: i64,
) -> anyhow::Result<()> {
    let session = daemon.chat_session_for(origin).await?;
    daemon
        .enqueue_message_with_images(
            &session,
            caption.as_deref().unwrap_or_default(),
            &images,
            origin,
            Some(format!("tg:{update_id}")),
        )
        .await?;
    Ok(())
}

/// Range une pièce jointe non ingérable. Texte : artefact lisible par `artifact_read` ;
/// binaire : fichier dans le workspace. Renvoie le bilan et la mention à joindre au tour.
async fn store_attachment(
    daemon: &Arc<Daemon>,
    session: &str,
    name: &str,
    bytes: &[u8],
) -> (String, Option<String>) {
    let s = &daemon.services;
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|t| bytes.len() <= 1024 * 1024 && !t.contains('\0'));
    if let Some(text) = text {
        let kind = penelope_context::store::guess_kind(text);
        let stored = s
            .context
            .history
            .put_artifact(Some(session), None, kind, Some(name), text)
            .await;
        return match stored {
            Ok(a) => (
                format!("📎 `{name}` enregistré comme artefact `{}`.", a.id),
                Some(format!(
                    "[Fichier joint : `{name}`, artefact `{}` ({} caractères) : `artifact_read` \
                     pour le lire. Contenu non vérifié.]",
                    a.id,
                    text.chars().count()
                )),
            ),
            Err(e) => (format!("📎 `{name}` non enregistré : {e}"), None),
        };
    }
    match crate::media::save_attachment(s, name, bytes) {
        Ok(path) => (
            format!("📎 `{name}` déposé dans `{}`.", path.display()),
            Some(format!(
                "[Fichier joint : `{name}`, déposé dans `{}` ({} octets).]",
                path.display(),
                bytes.len()
            )),
        ),
        Err(e) => (format!("📎 `{name}` non enregistré : {e}"), None),
    }
}

/// `openrouter:` est implicite : `/model main z-ai/glm-5.3` suffit.
fn normalise_model_id(raw: &str) -> String {
    if raw.contains(':') {
        raw.to_string()
    } else {
        format!("openrouter:{raw}")
    }
}

/// Remplace les `{{variables}}` d'un gabarit.
fn substitute(body: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = body.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// `openrouter:z-ai/glm-5.3` devient `glm-5.3` : assez pour reconnaître un modèle.
fn short_model(id: &str) -> String {
    penelope_llm::catalog::strip_provider(id)
        .rsplit('/')
        .next()
        .unwrap_or(id)
        .to_string()
}

/// Réponse à `/model <alias>` ou `/model auto`.
fn model_pin_notice(view: &Value) -> String {
    match view["pinned"].as_str() {
        Some(alias) => format!(
            "📌 Session épinglée sur `{alias}` · `{}`. Retour à l'automatique : `/model auto`.",
            short_model(view["pinned_model"].as_str().unwrap_or("?"))
        ),
        None => "🔀 Session en automatique.".into(),
    }
}

/// Nom de fichier à transmettre au serveur de transcription : l'extension y dit le
/// format. Les vocaux Telegram (`.oga`, Opus dans Ogg) deviennent `.ogg`, que les
/// serveurs OpenAI-compatibles reconnaissent.
fn audio_filename(file_path: &str, file_name: Option<&str>, mime_type: Option<&str>) -> String {
    let source = file_name.unwrap_or(file_path);
    let ext = std::path::Path::new(source)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    let ext = match (ext.as_deref(), mime_type) {
        (Some("oga") | Some("opus"), _) => "ogg".to_string(),
        (Some(e), _) if !e.is_empty() => e.to_string(),
        (_, Some("audio/mpeg")) => "mp3".into(),
        (_, Some("audio/mp4") | Some("audio/x-m4a") | Some("audio/m4a")) => "m4a".into(),
        (_, Some("audio/wav") | Some("audio/x-wav")) => "wav".into(),
        (_, Some("audio/flac")) => "flac".into(),
        _ => "ogg".into(),
    };
    format!("audio.{ext}")
}

/// `/schedules` : un déclencheur par ligne, prochain passage et cible.
fn schedules_text(v: &Value) -> String {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "Aucun déclencheur planifié.".into();
    }
    let mut t = String::from("**Déclencheurs**\n\n");
    for sc in &list {
        let state = sc["state"].as_str().unwrap_or("?");
        let icon = match state {
            "active" => "🟢",
            "paused" => "⏸",
            "done" => "✅",
            _ => "⚪",
        };
        let spec = &sc["spec"];
        let when = match sc["kind"].as_str().unwrap_or("?") {
            "cron" => format!(
                "cron `{}`{}",
                spec["expr"].as_str().unwrap_or("?"),
                if spec["once"].as_bool() == Some(true) {
                    " (une fois)"
                } else {
                    ""
                }
            ),
            "interval" | "mcp_poll" => format!(
                "{} toutes les {} min",
                sc["kind"].as_str().unwrap_or("?"),
                spec["every_ms"].as_u64().unwrap_or(0) / 60_000
            ),
            "watch_file" => format!("fichier `{}`", spec["path"].as_str().unwrap_or("?")),
            other => format!("{other} `{}`", spec["event"].as_str().unwrap_or("?")),
        };
        let target = &sc["target"];
        let what = match target["type"].as_str().unwrap_or("?") {
            "notify" => target["template"].as_str().unwrap_or("").to_string(),
            "prompt" => target["prompt"].as_str().unwrap_or("").to_string(),
            "workflow" => format!("workflow {}", target["workflowId"].as_str().unwrap_or("?")),
            other => other.to_string(),
        };
        t.push_str(&format!(
            "{icon} `{}` · {when} · {}\n",
            sc["id"].as_str().unwrap_or("?"),
            what.chars().take(80).collect::<String>()
        ));
        if let Some(next) = sc["next_run"].as_str().filter(|_| state == "active") {
            t.push_str(&format!("   ↳ prochain : {next}\n"));
        }
        if let Some(e) = sc["last_error"].as_str() {
            t.push_str(&format!(
                "   ↳ erreur : {}\n",
                e.chars().take(160).collect::<String>()
            ));
        }
    }
    t.push_str("\n`/schedules pause|resume|rm|run <id>`");
    t
}

/// Dernières lignes du journal JSON du jour (le plus récent à défaut), filtrées par
/// composant (cible `tracing` ou texte), rendues `HH:MM:SS NIVEAU cible : message`.
fn recent_log_lines(dir: &Path, component: &str, n: usize) -> Vec<String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|f| f.to_string_lossy())
                .is_some_and(|f| f.starts_with("penelope-") && f.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    let Some(latest) = files.last() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(latest) else {
        return Vec::new();
    };
    let wanted = component.to_lowercase();
    let mut out: Vec<String> = raw
        .lines()
        .rev()
        .filter_map(|line| {
            let v: Value = serde_json::from_str(line).ok()?;
            let target = v["target"].as_str().unwrap_or_default();
            let message = v["fields"]["message"].as_str().unwrap_or_default();
            if !wanted.is_empty()
                && !target.to_lowercase().contains(&wanted)
                && !message.to_lowercase().contains(&wanted)
            {
                return None;
            }
            let time = v["timestamp"]
                .as_str()
                .and_then(|t| t.get(11..19))
                .unwrap_or("");
            let short_target = target.rsplit("::").next().unwrap_or(target);
            Some(format!(
                "{time} {} {short_target} : {}",
                v["level"].as_str().unwrap_or("?"),
                message.chars().take(200).collect::<String>()
            ))
        })
        .take(n)
        .collect();
    out.reverse();
    out
}

fn mcp_state_icon(state: &str) -> &'static str {
    match state {
        "ready" => "🟢",
        "degraded" => "🟡",
        "connecting" => "🔄",
        "failed" => "🔴",
        "disabled" => "⏸",
        "auth_required" => "🔐",
        _ => "⚪",
    }
}

/// `/mcp` : un serveur par ligne, puis les déclarations invalides.
fn mcp_list_text(v: &Value) -> String {
    let servers = v["servers"].as_array().cloned().unwrap_or_default();
    let mut t = if servers.is_empty() {
        "Aucun serveur MCP déclaré.".to_string()
    } else {
        let mut t = String::from("**Serveurs MCP**\n\n");
        for srv in &servers {
            let state = srv["state"].as_str().unwrap_or("?");
            let label = match state {
                "configured" => "démarre au premier appel",
                "ready" => "prêt",
                "degraded" => "dégradé",
                "connecting" => "connexion",
                "failed" => "en panne",
                "disabled" => "désactivé",
                "auth_required" => "autorisation requise",
                other => other,
            };
            t.push_str(&format!(
                "{} `{}` · {} outil(s) · {label}\n",
                mcp_state_icon(state),
                srv["name"].as_str().unwrap_or("?"),
                srv["tools"]
            ));
            if let Some(e) = srv["last_error"].as_str().filter(|_| state != "ready") {
                t.push_str(&format!(
                    "   ↳ {}\n",
                    e.chars().take(200).collect::<String>()
                ));
            }
        }
        t
    };
    for bad in v["invalid"].as_array().cloned().unwrap_or_default() {
        t.push_str(&format!(
            "\n⚠️ `{}` : {}",
            bad["file"].as_str().unwrap_or("?"),
            bad["error"].as_str().unwrap_or("?")
        ));
    }
    t.push_str("\n\nDétail : `/mcp <serveur>` ; `/mcp restart|logs|test <serveur>`");
    t
}

/// `/mcp <serveur>` : état, outils, dernière erreur.
fn mcp_show_text(v: &Value) -> String {
    let st = &v["status"];
    let name = st["name"].as_str().unwrap_or("?");
    let state = st["state"].as_str().unwrap_or("?");
    let mut t = format!(
        "{} **{name}** · {} · {} outil(s) · {} appel(s), {} erreur(s)\n",
        mcp_state_icon(state),
        state,
        st["tool_count"],
        st["calls"],
        st["errors"]
    );
    if let Some(p) = st["protocol"].as_str() {
        t.push_str(&format!(
            "Protocole {p} · transport {}\n",
            st["transport"].as_str().unwrap_or("?")
        ));
    }
    if let Some(e) = st["last_error"].as_str() {
        t.push_str(&format!(
            "Dernière erreur : {}\n",
            e.chars().take(300).collect::<String>()
        ));
    }
    let tools = v["tools"].as_array().cloned().unwrap_or_default();
    if !tools.is_empty() {
        t.push('\n');
        for tool in tools.iter().take(25) {
            t.push_str(&format!(
                "- `{}` ({})\n",
                tool["title"].as_str().unwrap_or("?"),
                tool["risk"].as_str().unwrap_or("?")
            ));
        }
        if tools.len() > 25 {
            t.push_str(&format!("… et {} autres\n", tools.len() - 25));
        }
    }
    t
}

/// Alias et routage, tels que `model.list` les décrit./// Alias et routage, tels que `model.list` les décrit.
fn routing_text(v: &Value) -> String {
    let mut t = String::from("**Alias**\n\n");
    for a in v["aliases"].as_array().cloned().unwrap_or_default() {
        t.push_str(&format!(
            "- `{}` → `{}`\n",
            a["alias"].as_str().unwrap_or("?"),
            a["model"].as_str().unwrap_or("?")
        ));
    }
    let r = &v["routing"];
    if r.is_object() {
        let step = |k: &str| {
            format!(
                "`{}` (`{}`)",
                r[k]["alias"].as_str().unwrap_or("?"),
                r[k]["model"].as_str().unwrap_or("?")
            )
        };
        t.push_str("\n**Routage**\n\n");
        if r["classifier"].as_bool().unwrap_or(false) {
            t.push_str(&format!(
                "Adaptatif (classifieur `{}`) :\n- simple → {}\n- ordinaire → {}\n- difficile → {}\n",
                r["classifier_model"].as_str().unwrap_or("?"),
                step("low"),
                step("medium"),
                step("high"),
            ));
            t.push_str("Tout sur `main` : `/model auto off`\n");
        } else {
            t.push_str(&format!(
                "Fixe : tout passe par {} (adaptatif : `/model auto on`)\n",
                step("default")
            ));
        }
        if let Some(fb) = r["fallback"].as_object().filter(|f| !f.is_empty()) {
            let chains: Vec<String> = fb
                .iter()
                .map(|(from, to)| {
                    let to: Vec<&str> = to
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                        .unwrap_or_default();
                    format!("`{from}` → `{}`", to.join("`, `"))
                })
                .collect();
            t.push_str(&format!("Replis sur panne : {}\n", chains.join(" ; ")));
        }
    }
    t
}

/// Rend une valeur RPC en Markdown lisible dans une conversation.
pub fn render_value(v: &Value) -> String {
    fn scalar(v: &Value) -> String {
        match v {
            Value::String(s) => {
                let s: String = s.chars().take(120).collect();
                s
            }
            Value::Null => "—".into(),
            other => {
                let s = other.to_string();
                s.chars().take(120).collect()
            }
        }
    }
    let out = match v {
        Value::Array(items) if items.is_empty() => "(vide)".to_string(),
        Value::Array(items) => {
            let mut s = String::new();
            for it in items.iter().take(40) {
                match it {
                    Value::Object(o) => {
                        let line = o
                            .iter()
                            .filter(|(_, v)| !v.is_null() && !v.is_object() && !v.is_array())
                            .take(4)
                            .map(|(k, v)| format!("{k} : {}", scalar(v)))
                            .collect::<Vec<_>>()
                            .join(" · ");
                        s.push_str(&format!("- {line}\n"));
                    }
                    other => s.push_str(&format!("- {}\n", scalar(other))),
                }
            }
            if items.len() > 40 {
                s.push_str(&format!("… et {} de plus\n", items.len() - 40));
            }
            s
        }
        Value::Object(o) => {
            let mut s = String::new();
            for (k, v) in o {
                match v {
                    Value::Object(_) | Value::Array(_) => {
                        let compact = v.to_string();
                        let compact: String = compact.chars().take(200).collect();
                        s.push_str(&format!("**{k}** : `{compact}`\n"));
                    }
                    other => s.push_str(&format!("**{k}** : {}\n", scalar(other))),
                }
            }
            s
        }
        other => scalar(other),
    };
    out.chars().take(3_500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::{MockProvider, Scripted};
    use penelope_llm::types::ToolCall;
    use penelope_telegram::api::method as tg;
    use penelope_telegram::mock::{MockTransport, updates};

    const OWNER: i64 = 42;

    async fn gateway() -> (
        tempfile::TempDir,
        Arc<TelegramGateway>,
        Arc<MockTransport>,
        Arc<MockProvider>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        // Le limiteur réel dort une seconde par message : inutile en test.
        d.publish_config("test", |c| {
            c.telegram.rate_per_chat_per_s = 1_000.0;
            Ok(vec!["telegram.rate_per_chat_per_s".into()])
        })
        .unwrap();
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let t = MockTransport::new();
        let g = TelegramGateway::with_transport(d, t.clone());
        g.register();
        (dir, g, t, p)
    }

    /// Exécute les tours en file, comme le ferait le pool de runners.
    async fn drain(g: &TelegramGateway) {
        while let Some(turn) = g.daemon.services.turns.claim("test").await.unwrap() {
            crate::runner::process(&g.daemon, turn, Duration::from_secs(30)).await;
        }
        g.flush_outbox().await.unwrap();
    }

    /// Boutons d'un message envoyé : (libellé, `callback_data` ou URL).
    fn inline_buttons(call: &Value) -> Vec<(String, String)> {
        call["reply_markup"]["inline_keyboard"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|row| row.as_array().cloned().unwrap_or_default())
            .map(|b| {
                let target = b["callback_data"].as_str().or(b["url"].as_str());
                (
                    b["text"].as_str().unwrap_or_default().to_string(),
                    target.unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    fn texts(calls: &[Value]) -> Vec<String> {
        calls
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
            .collect()
    }

    #[tokio::test]
    async fn a_text_message_gets_an_html_answer_as_a_reply() {
        let (_d, g, t, p) = gateway().await;
        // Accueil déjà proposé : seul l'échange compte ici.
        g.daemon
            .kv_set("tg.onboard.proposed", "test")
            .await
            .unwrap();
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("Bonjour, **Edouard**.");
        g.process_update(&updates::text_message(1, OWNER, OWNER, "salut"))
            .await
            .unwrap();
        drain(&g).await;

        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["text"], "Bonjour, <b>Edouard</b>.");
        assert_eq!(sent[0]["parse_mode"], "HTML");
        assert!(sent[0]["reply_parameters"]["message_id"].is_i64());
        // Réactions : reçu, puis terminé.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let reactions = t.calls_to(tg::SET_MESSAGE_REACTION).await;
        assert!(reactions.len() >= 2, "{reactions:?}");
    }

    /// Issue #5 : un tour échoué porte un bouton « Réessayer » qui relance la réponse sur
    /// le même transcript, sans dupliquer le message.
    #[tokio::test]
    async fn a_failed_turn_offers_a_retry_button() {
        let (_d, g, t, p) = gateway().await;
        p.reply(r#"{"complexity":"low"}"#);
        p.push(Scripted::Error(
            penelope_llm::types::LlmErrorKind::Other,
            "panne du fournisseur".into(),
        ));
        g.process_update(&updates::text_message(1, OWNER, OWNER, "salut"))
            .await
            .unwrap();
        drain(&g).await;
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        let failure = sent.last().unwrap();
        assert!(
            failure["text"]
                .as_str()
                .unwrap()
                .contains("panne du fournisseur"),
            "{failure}"
        );
        let token = failure["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap()
            .to_string();

        p.reply("Deuxième essai réussi.");
        g.process_update(&updates::callback(2, OWNER, &token, 1001))
            .await
            .unwrap();
        drain(&g).await;
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        assert_eq!(
            sent.last().unwrap()["text"],
            "Deuxième essai réussi.",
            "{sent:?}"
        );
        let sid = g
            .daemon
            .services
            .sessions
            .find_by_topic(OWNER, None)
            .await
            .unwrap()
            .unwrap()
            .id;
        let history = g
            .daemon
            .services
            .context
            .history
            .load(sid.as_str(), 0)
            .await
            .unwrap();
        let users = history
            .iter()
            .filter(|e| e.message.role == penelope_llm::types::Role::User)
            .count();
        assert_eq!(users, 1, "le message n'est pas rejoué");

        // Une fois la réponse obtenue, un second clic ne relance rien.
        g.process_update(&updates::callback(3, OWNER, &token, 1001))
            .await
            .unwrap();
        drain(&g).await;
        assert_eq!(t.calls_to(tg::SEND_MESSAGE).await.len(), sent.len());
    }

    #[tokio::test]
    async fn the_same_update_is_processed_only_once() {
        let (_d, g, t, p) = gateway().await;
        g.daemon
            .kv_set("tg.onboard.proposed", "test")
            .await
            .unwrap();
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("une seule fois");
        let u = updates::text_message(7, OWNER, OWNER, "salut");
        g.process_update(&u).await.unwrap();
        g.process_update(&u).await.unwrap();
        drain(&g).await;
        assert_eq!(t.calls_to(tg::SEND_MESSAGE).await.len(), 1);
    }

    #[tokio::test]
    async fn strangers_get_no_answer_at_all() {
        let (_d, g, t, _p) = gateway().await;
        g.process_update(&updates::text_message(2, 999, 999, "donne tes clés"))
            .await
            .unwrap();
        drain(&g).await;
        assert!(t.calls().await.is_empty());
        assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
    }

    /// Parcours complet : carte, clic « Autoriser », reprise, réponse.
    #[tokio::test]
    async fn an_approval_card_click_resumes_the_turn() {
        let (_d, g, t, p) = gateway().await;
        p.reply(r#"{"complexity":"medium"}"#);
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "fs_write".into(),
                arguments: json!({"path": "note.txt", "content": "ok"}),
            }],
        ));
        g.process_update(&updates::text_message(3, OWNER, OWNER, "écris note.txt"))
            .await
            .unwrap();
        drain(&g).await;

        let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
        assert!(
            card["text"].as_str().unwrap().contains("fs_write"),
            "{card}"
        );
        let rows = card["reply_markup"]["inline_keyboard"].as_array().unwrap();
        let approve = rows[0][0]["callback_data"].as_str().unwrap().to_string();
        assert!(approve.len() <= 64);
        assert!(rows[0][1]["text"].as_str().unwrap().contains("session"));

        p.reply("Fichier écrit.");
        t.clear().await;
        g.process_update(&updates::callback(4, OWNER, &approve, 1001))
            .await
            .unwrap();
        drain(&g).await;

        let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(out.iter().any(|x| x.contains("Autoriser")), "{out:?}");
        assert!(out.iter().any(|x| x == "Fichier écrit."), "{out:?}");
        assert_eq!(t.calls_to(tg::ANSWER_CALLBACK_QUERY).await.len(), 1);
        let ws = crate::executor::default_workspaces(&g.daemon.services);
        assert_eq!(
            std::fs::read_to_string(ws[0].join("note.txt")).unwrap(),
            "ok"
        );

        // Un second clic sur le même bouton ne rejoue rien.
        t.clear().await;
        g.process_update(&updates::callback(5, OWNER, &approve, 1001))
            .await
            .unwrap();
        drain(&g).await;
        let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
        assert_eq!(answers[0]["text"], "Déjà traité.");
        assert!(texts(&t.calls_to(tg::SEND_MESSAGE).await).is_empty());
    }

    #[tokio::test]
    async fn a_refusal_with_a_reason_reaches_the_model() {
        let (_d, g, t, p) = gateway().await;
        p.reply(r#"{"complexity":"medium"}"#);
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command": "rm -rf build"}),
            }],
        ));
        g.process_update(&updates::text_message(10, OWNER, OWNER, "nettoie"))
            .await
            .unwrap();
        drain(&g).await;
        let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
        let rows = card["reply_markup"]["inline_keyboard"].as_array().unwrap();
        let deny_reason = rows
            .iter()
            .flat_map(|r| r.as_array().unwrap().iter())
            .find(|b| b["text"].as_str().unwrap().contains("raison"))
            .unwrap()["callback_data"]
            .as_str()
            .unwrap()
            .to_string();

        g.process_update(&updates::callback(11, OWNER, &deny_reason, 1001))
            .await
            .unwrap();
        p.reply("Compris, je garde le dossier build.");
        g.process_update(&updates::text_message(
            12,
            OWNER,
            OWNER,
            "on en a besoin pour la démo",
        ))
        .await
        .unwrap();
        drain(&g).await;

        let last = p.requests().last().unwrap().clone();
        let seen: Vec<String> = last.messages.iter().map(|m| m.text()).collect();
        assert!(
            seen.iter()
                .any(|t| t.contains("on en a besoin pour la démo")),
            "la raison doit arriver au modèle : {seen:?}"
        );
    }

    #[tokio::test]
    async fn commands_answer_without_calling_the_model() {
        let (_d, g, t, p) = gateway().await;
        g.process_update(&updates::text_message(
            20,
            OWNER,
            OWNER,
            "/model main z-ai/glm-5.3",
        ))
        .await
        .unwrap();
        g.process_update(&updates::text_message(21, OWNER, OWNER, "/status"))
            .await
            .unwrap();
        g.process_update(&updates::text_message(
            22,
            OWNER,
            OWNER,
            "/secret set openrouter_api_key sk-xxx",
        ))
        .await
        .unwrap();
        drain(&g).await;
        assert_eq!(p.call_count(), 0);
        let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(out[0].contains("openrouter:z-ai/glm-5.3"), "{out:?}");
        assert!(out[1].contains("version"), "{out:?}");
        assert!(out[2].contains("jamais"), "{out:?}");
        let cfg = g.daemon.services.config.config();
        assert_eq!(cfg.alias_model("main"), Some("openrouter:z-ai/glm-5.3"));
    }

    #[tokio::test]
    async fn routing_and_costs_are_readable_from_telegram() {
        let (_d, g, t, p) = gateway().await;
        g.process_update(&updates::text_message(40, OWNER, OWNER, "/models"))
            .await
            .unwrap();
        g.process_update(&updates::text_message(41, OWNER, OWNER, "/model auto off"))
            .await
            .unwrap();
        g.process_update(&updates::text_message(42, OWNER, OWNER, "/model"))
            .await
            .unwrap();

        let origin = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let sid = g.daemon.chat_session_for(&origin).await.unwrap();
        g.daemon
            .services
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(sid.clone()),
                turn_id: Some("t_x".into()),
                role: Some("chat".into()),
                model: "z-ai/glm-5.3".into(),
                provider: "openrouter".into(),
                cost_usd: 0.0123,
                ..Default::default()
            })
            .await
            .unwrap();
        g.process_update(&updates::text_message(43, OWNER, OWNER, "/budget"))
            .await
            .unwrap();
        g.process_update(&updates::text_message(44, OWNER, OWNER, "/budget sessions"))
            .await
            .unwrap();
        drain(&g).await;
        assert_eq!(p.call_count(), 0);

        let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(out[0].contains("Adaptatif"), "{out:?}");
        assert!(out[0].contains("simple"), "{out:?}");
        assert!(out[1].contains("Routage fixe"), "{out:?}");
        assert!(out[2].contains("Modèle de cette session"), "{out:?}");
        assert!(out[2].contains("tout passe par"), "{out:?}");
        assert!(!g.daemon.services.config.config().models.routing.classifier);
        assert!(out[3].contains("0,0123 $"), "{out:?}");
        assert!(out[3].contains("t_x"), "{out:?}");
        assert!(out[4].contains(&sid), "{out:?}");
    }

    /// Boutons du dernier menu `/model` envoyé : (libellé, jeton).
    async fn model_buttons(t: &MockTransport) -> Vec<(String, String)> {
        let menu = t
            .calls_to(tg::SEND_MESSAGE)
            .await
            .into_iter()
            .rev()
            .find(|c| {
                c["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Modèle de cette session")
            })
            .expect("menu /model envoyé");
        menu["reply_markup"]["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row[0]["text"].as_str().unwrap().to_string(),
                    row[0]["callback_data"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn model_buttons_pin_the_session_then_give_it_back_to_the_router() {
        let (_d, g, t, p) = gateway().await;
        let origin = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let sid = g.daemon.chat_session_for(&origin).await.unwrap();
        let cfg = g.daemon.services.config.config();
        let main = cfg.alias_model("main").unwrap().to_string();
        let reasoning = cfg.alias_model("reasoning").unwrap().to_string();

        g.process_update(&updates::text_message(70, OWNER, OWNER, "/model"))
            .await
            .unwrap();
        drain(&g).await;
        let buttons = model_buttons(&t).await;
        let labels: Vec<&str> = buttons.iter().map(|(l, _)| l.as_str()).collect();
        assert!(labels[0].starts_with("fast · "), "{labels:?}");
        assert!(labels[1].starts_with("main · "), "{labels:?}");
        assert!(labels[2].starts_with("reasoning · "), "{labels:?}");
        assert_eq!(labels.last(), Some(&"✅ 🔀 Automatique"));
        assert!(
            !labels
                .iter()
                .any(|l| l.contains("embedding") || l.contains("stt")),
            "{labels:?}"
        );

        // Clic sur `main` : la session est épinglée, le menu se met à jour.
        let main_token = buttons[1].1.clone();
        g.process_update(&updates::callback(71, OWNER, &main_token, 700))
            .await
            .unwrap();
        let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
        assert_eq!(answers.last().unwrap()["text"], "Session épinglée sur main");
        let edited = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let edited = edited.last().unwrap();
        assert!(
            edited["text"].as_str().unwrap().contains("Épinglé sur"),
            "{edited}"
        );
        assert!(
            edited["reply_markup"]["inline_keyboard"][1][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("✅ main"),
            "{edited}"
        );

        // Le message suivant part sur `main`, sans appel au classifieur.
        p.reply("Réponse de main.");
        g.process_update(&updates::text_message(72, OWNER, OWNER, "ok"))
            .await
            .unwrap();
        drain(&g).await;
        assert_eq!(
            p.call_count(),
            1,
            "pas de classifieur quand la session est épinglée"
        );
        assert_eq!(p.requests()[0].model, main);

        // Le même bouton sert encore : un jeton de menu n'est pas à usage unique.
        g.process_update(&updates::callback(73, OWNER, &main_token, 700))
            .await
            .unwrap();
        assert_eq!(
            t.calls_to(tg::ANSWER_CALLBACK_QUERY).await.last().unwrap()["text"],
            "Session épinglée sur main"
        );

        // En texte : `/model reasoning` épingle, `/model auto` rend la main au routeur.
        g.process_update(&updates::text_message(74, OWNER, OWNER, "/model reasoning"))
            .await
            .unwrap();
        p.reply("Réponse de reasoning.");
        g.process_update(&updates::text_message(75, OWNER, OWNER, "et là ?"))
            .await
            .unwrap();
        drain(&g).await;
        assert_eq!(p.requests().last().unwrap().model, reasoning);

        let auto_token = model_buttons(&t).await.last().unwrap().1.clone();
        g.process_update(&updates::callback(76, OWNER, &auto_token, 701))
            .await
            .unwrap();
        assert_eq!(
            t.calls_to(tg::ANSWER_CALLBACK_QUERY).await.last().unwrap()["text"],
            "Session en automatique"
        );
        assert!(g.daemon.pinned_model(&sid).await.is_none());

        g.process_update(&updates::text_message(77, OWNER, OWNER, "/model inconnu"))
            .await
            .unwrap();
        drain(&g).await;
        let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            out.iter()
                .any(|m| m.contains("reasoning") && m.contains("📌")),
            "{out:?}"
        );
        assert!(out.last().unwrap().contains("alias inconnu"), "{out:?}");
    }

    /// Octets d'un JPEG : l'en-tête suffit à la reconnaissance.
    const JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
    ];

    fn photo_update(
        update_id: i64,
        file_id: &str,
        group: Option<&str>,
        caption: Option<&str>,
    ) -> Value {
        let mut u = updates::photo(update_id, OWNER, OWNER, group);
        u["message"]["photo"][0]["file_id"] = json!(file_id);
        if let Some(c) = caption {
            u["message"]["caption"] = json!(c);
        }
        u
    }

    fn document_update(update_id: i64, file_id: &str, name: &str, caption: Option<&str>) -> Value {
        let mut u = updates::document(update_id, OWNER, OWNER, name);
        u["message"]["document"]["file_id"] = json!(file_id);
        if let Some(c) = caption {
            u["message"]["caption"] = json!(c);
        }
        u
    }

    /// Attend qu'une condition asynchrone devienne vraie (tâches lancées en fond).
    async fn eventually<F, Fut>(mut f: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..150 {
            if f().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_photo_is_described_for_a_model_that_cannot_see() {
        let (_d, g, t, p) = gateway().await;
        t.set_file("ph1", JPEG).await;
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Un reçu de pharmacie : total 23,40 €, daté du 12 septembre.");
        p.reply("Tu as dépensé 23,40 € à la pharmacie.");
        g.process_update(&photo_update(70, "ph1", None, Some("combien ?")))
            .await
            .unwrap();
        drain(&g).await;

        let requests = p.requests();
        let vision = requests
            .iter()
            .find(|r| r.messages[0].text().contains("Tu décris des images"))
            .expect("appel au modèle de vision");
        let cfg = g.daemon.services.config.config();
        assert_eq!(vision.model, cfg.alias_model("vision").unwrap());
        assert!(vision.messages[1].content.iter().any(|c| matches!(
            c,
            penelope_llm::types::Content::ImageUrl { url, .. } if url.starts_with("data:image/jpeg;base64,")
        )));
        let chat = requests.last().unwrap();
        assert!(
            chat.messages.iter().all(|m| m
                .content
                .iter()
                .all(|c| !matches!(c, penelope_llm::types::Content::ImageUrl { .. }))),
            "le modèle de la conversation ne reçoit que du texte"
        );
        assert!(chat.messages.iter().any(|m| {
            let t = m.text();
            // Le contexte volatil (T4) précède le texte du dernier message utilisateur.
            t.contains("combien ?") && t.contains("23,40 €") && t.contains("modèle de vision")
        }));
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            sent.iter().any(|m| m.contains("23,40 € à la pharmacie")),
            "{sent:?}"
        );
        let roles = g
            .daemon
            .services
            .budget
            .report("role", None, None, 10)
            .await
            .unwrap();
        assert!(roles.iter().any(|r| r.key == "image_describe"), "{roles:?}");
    }

    #[tokio::test]
    async fn a_multimodal_model_sees_the_album_in_one_turn() {
        let (_d, g, t, p) = gateway().await;
        let cfg = g.daemon.services.config.config();
        let main =
            penelope_llm::catalog::strip_provider(cfg.alias_model("main").unwrap()).to_string();
        let mut info = penelope_llm::catalog::ModelInfo::minimal(&main, "deepseek", 128_000);
        info.input_modalities = vec!["text".into(), "image".into()];
        g.daemon.services.catalog.upsert(vec![info]);
        t.set_file("a1", JPEG).await;
        t.set_file("a2", JPEG).await;
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Deux vues du même salon.");

        g.process_update(&photo_update(80, "a1", Some("album-1"), Some("compare")))
            .await
            .unwrap();
        g.process_update(&photo_update(81, "a2", Some("album-1"), None))
            .await
            .unwrap();
        assert!(
            g.daemon
                .services
                .turns
                .claim("test")
                .await
                .unwrap()
                .is_none(),
            "l'album attend ses photos avant de partir"
        );
        tokio::time::sleep(ALBUM_WINDOW + Duration::from_millis(300)).await;
        drain(&g).await;

        let requests = p.requests();
        assert_eq!(
            requests.len(),
            2,
            "un classifieur et une réponse, pas de vision"
        );
        let images: usize = requests[1]
            .messages
            .iter()
            .map(|m| {
                m.content
                    .iter()
                    .filter(|c| matches!(c, penelope_llm::types::Content::ImageUrl { .. }))
                    .count()
            })
            .sum();
        assert_eq!(images, 2, "les deux photos dans le même tour");
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(sent.iter().any(|m| m.contains("même salon")), "{sent:?}");
    }

    #[tokio::test]
    async fn a_document_is_ingested_proposed_and_answered() {
        let (_d, g, t, p) = gateway().await;
        let body = "Contrat de maintenance ACME\n\nLe contrat court jusqu'au 31 mars 2027.\n\n\
                    Carte de test : 4111 1111 1111 1111\n\nContact : Paul Martin.";
        t.set_file("doc1", body.as_bytes()).await;
        p.reply(
            r#"{"resume": "Contrat de maintenance avec ACME jusqu'en mars 2027.",
                "faits": ["Le contrat de maintenance ACME court jusqu'au 31 mars 2027."]}"#,
        );
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Il expire le 31 mars 2027.");
        g.process_update(&document_update(
            90,
            "doc1",
            "Contrat ACME.txt",
            Some("quand expire-t-il ?"),
        ))
        .await
        .unwrap();

        let vault = crate::conversation::vault_dir(&g.daemon.services);
        let source = vault.join("sources/contrat-acme.md");
        assert!(
            eventually(|| async { source.exists() }).await,
            "fiche écrite"
        );
        assert!(
            eventually(|| async {
                g.flush_outbox().await.unwrap();
                texts(&t.calls_to(tg::SEND_MESSAGE).await)
                    .iter()
                    .any(|m| m.contains("Propositions de mémoire"))
            })
            .await
        );
        drain(&g).await;

        let raw = std::fs::read_to_string(&source).unwrap();
        let parsed = penelope_memory::ingest::parse_source(&raw).unwrap();
        assert_eq!(parsed.origine, penelope_memory::Origin::Untrusted);
        assert!(
            !raw.contains("4111"),
            "un numéro de carte n'entre pas dans le vault"
        );
        assert!(raw.contains("## Résumé"));

        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            sent.iter().any(|m| m.contains("sources/contrat-acme.md")),
            "{sent:?}"
        );
        assert!(sent.iter().any(|m| m.contains("31 mars 2027")), "{sent:?}");
        let chat = p.requests().last().unwrap().clone();
        let asked = chat
            .messages
            .iter()
            .map(|m| m.text())
            .find(|t| t.contains("quand expire-t-il ?"))
            .expect("la légende part en tour");
        assert!(asked.contains("contenu non fiable"), "{asked}");
        assert!(asked.contains("Paul Martin"));

        // Les passages se retrouvent par recherche explicite, jamais par rappel automatique.
        let s = &g.daemon.services;
        let explicit = penelope_memory::SearchFilter::explicit();
        let hits = s.memory.search("ACME", None, &explicit, &[]).await.unwrap();
        assert!(hits.iter().any(|h| h.entry.etype == "source"));
        let auto = s
            .memory
            .search("ACME", None, &penelope_memory::SearchFilter::default(), &[])
            .await
            .unwrap();
        assert!(auto.iter().all(|h| h.entry.etype != "source"));

        // « Tout » : le fait rejoint notes.md, avec le document en provenance.
        let pending = s.approvals.pending(10).await.unwrap();
        let proposal = pending
            .iter()
            .find(|a| a.kind == penelope_hitl::ApprovalKind::MemoryProposal)
            .expect("proposition en attente");
        let token = s
            .actions
            .create(
                k::MEMORY_ACCEPT,
                proposal.id.as_str(),
                json!({}),
                60_000,
                true,
            )
            .await
            .unwrap()
            .token;
        g.process_update(&updates::callback(91, OWNER, &token, 900))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let notes = std::fs::read_to_string(vault.join("notes.md")).unwrap();
        assert!(notes.contains("31 mars 2027"), "{notes}");
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            sent.iter().any(|m| m.contains("1 fait(s) ajouté(s)")),
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn mien_marks_a_document_as_written_by_the_owner() {
        let (_d, g, t, p) = gateway().await;
        t.set_file(
            "doc2",
            b"# Mes principes\n\nToujours relire avant d'envoyer.",
        )
        .await;
        p.reply(r#"{"resume": "Principes de travail.", "faits": []}"#);
        g.process_update(&document_update(95, "doc2", "principes.md", Some("/mien")))
            .await
            .unwrap();
        let source =
            crate::conversation::vault_dir(&g.daemon.services).join("sources/principes.md");
        assert!(eventually(|| async { source.exists() }).await);
        let raw = std::fs::read_to_string(&source).unwrap();
        assert_eq!(
            penelope_memory::ingest::parse_source(&raw).unwrap().origine,
            penelope_memory::Origin::Owner
        );
        assert!(
            g.daemon
                .services
                .turns
                .claim("test")
                .await
                .unwrap()
                .is_none(),
            "`/mien` seul n'est pas une demande"
        );
    }

    #[tokio::test]
    async fn other_files_become_attachments_the_agent_can_reach() {
        let (_d, g, t, _p) = gateway().await;
        t.set_file("csv1", b"date,montant\n2026-09-01,12\n").await;
        t.set_file("bin1", &[0u8, 159, 146, 150, 0, 1]).await;
        g.process_update(&document_update(96, "csv1", "depenses.csv", None))
            .await
            .unwrap();
        g.process_update(&document_update(97, "bin1", "../archive.bin", None))
            .await
            .unwrap();
        assert!(
            eventually(|| async {
                g.flush_outbox().await.unwrap();
                let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
                sent.iter().any(|m| m.contains("artefact"))
                    && sent.iter().any(|m| m.contains("archive.bin"))
            })
            .await
        );
        let workspace = crate::executor::default_workspaces(&g.daemon.services)[0].clone();
        assert!(workspace.join("telegram/archive.bin").exists());
    }

    #[tokio::test]
    async fn a_voice_note_is_transcribed_quoted_then_answered() {
        let (_d, g, t, p) = gateway().await;
        t.set_file("v1", b"OggS\x00fake-opus").await;
        p.set_transcript(Some("Rappelle-moi d'appeler Paul demain"));
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("C'est noté.");
        g.process_update(&updates::voice(60, OWNER, OWNER))
            .await
            .unwrap();
        drain(&g).await;

        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            sent[0].contains("Rappelle-moi d'appeler Paul demain"),
            "{sent:?}"
        );
        assert!(sent[0].contains("blockquote"), "citation : {sent:?}");
        assert!(sent.iter().any(|m| m.contains("C'est noté.")), "{sent:?}");

        let (name, size, lang) = p.transcribed.lock().unwrap()[0].clone();
        assert_eq!(name, "audio.ogg");
        assert_eq!(size, 14);
        assert_eq!(lang.as_deref(), Some("fr"));
        let chat = p.requests().last().unwrap().clone();
        assert!(
            chat.messages
                .iter()
                .any(|m| m.text().contains("(message vocal transcrit) Rappelle-moi")),
            "le modèle reçoit le texte transcrit"
        );
        let stt = g
            .daemon
            .services
            .budget
            .report("role", None, None, 10)
            .await
            .unwrap();
        assert!(stt.iter().any(|r| r.key == "stt"), "{stt:?}");
    }

    #[tokio::test]
    async fn a_voice_note_without_local_stt_explains_what_to_configure() {
        let (_d, g, t, _p) = gateway().await;
        // Sans provider imposé : la configuration par défaut vise un serveur local éteint.
        let g = TelegramGateway::with_transport(
            Arc::new(Daemon::from_services(g.daemon.services.clone())),
            t.clone(),
        );
        t.set_file("v1", b"OggS").await;
        g.process_update(&updates::voice(61, OWNER, OWNER))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(sent[0].contains("providers.local"), "{sent:?}");
        assert_eq!(g.daemon.services.turns.pending_count().await.unwrap(), 0);
    }

    #[test]
    fn audio_filenames_carry_a_format_servers_understand() {
        assert_eq!(
            audio_filename("voice/file_12.oga", None, Some("audio/ogg")),
            "audio.ogg"
        );
        assert_eq!(
            audio_filename("music/file_3", Some("note.m4a"), None),
            "audio.m4a"
        );
        assert_eq!(
            audio_filename("music/file_4", None, Some("audio/mpeg")),
            "audio.mp3"
        );
        assert_eq!(audio_filename("x", None, None), "audio.ogg");
    }

    #[tokio::test]
    async fn mcp_servers_are_visible_and_restartable_from_telegram() {
        use crate::mcp::testing::{FakeConnector, declare, server, tool};
        let (_d, g, t, _p) = gateway().await;
        let fake = Arc::new(FakeConnector::default());
        fake.serve(
            "redmine",
            server(Arc::new(std::sync::Mutex::new(vec![tool(
                "list_issues",
                json!({"readOnlyHint": true}),
            )]))),
        );
        let sup = crate::mcp::McpSupervisor::new(g.daemon.services.clone(), fake.clone());
        declare(&sup, "redmine", "");
        sup.reload().await;
        g.daemon.hooks.set_mcp(sup.clone());
        std::fs::write(sup.dir().join("casse.toml"), "transport = \"stdio\"\n").unwrap();
        sup.reload().await;

        for (i, text) in [
            "/mcp",
            "/mcp redmine",
            "/mcp restart redmine",
            "/mcp logs redmine",
        ]
        .iter()
        .enumerate()
        {
            g.process_update(&updates::text_message(80 + i as i64, OWNER, OWNER, text))
                .await
                .unwrap();
        }
        drain(&g).await;
        let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            out[0].contains("redmine") && out[0].contains("prêt"),
            "{out:?}"
        );
        assert!(
            out[0].contains("casse"),
            "déclaration invalide signalée : {out:?}"
        );
        assert!(out[1].contains("list_issues"), "{out:?}");
        assert!(out[2].contains("redémarré"), "{out:?}");
        assert!(out[3].contains("Aucune ligne"), "{out:?}");
        assert_eq!(fake.opened("redmine"), 2);
    }

    /// Dernier message envoyé ou édité portant des boutons, et le jeton du bouton dont le
    /// libellé contient `label`.
    async fn button(t: &MockTransport, label: &str) -> String {
        let mut calls = t.calls_to(tg::SEND_MESSAGE).await;
        calls.extend(t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
        calls
            .iter()
            .rev()
            .flat_map(inline_buttons)
            .find(|(l, _)| l.contains(label))
            .unwrap_or_else(|| panic!("pas de bouton « {label} »"))
            .1
    }

    /// Issue #30 : `/wf`, ▶️ sur un workflow à paramètres, le formulaire s'ouvre, la saisie
    /// est validée, et le run démarre avec ces paramètres.
    #[tokio::test]
    async fn a_workflow_is_launched_from_its_menu_with_a_parameter_form() {
        let (_d, g, t, _p) = gateway().await;
        let s = &g.daemon.services;
        g.process_update(&updates::text_message(300, OWNER, OWNER, "/wf"))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let menu = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
        assert!(
            menu.contains("build-verify") && !menu.contains("Usage"),
            "{menu}"
        );
        let launch = button(&t, "▶️ Construire puis vérifier").await;
        g.process_update(&updates::callback(301, OWNER, &launch, 900))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let form = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
        assert!(form.contains("Objectif"), "formulaire ouvert : {form}");
        assert!(
            s.runs.list(None, 5).await.unwrap().is_empty(),
            "rien lancé avant l'envoi"
        );

        g.process_update(&updates::text_message(
            302,
            OWNER,
            OWNER,
            "réparer le build",
        ))
        .await
        .unwrap();
        g.flush_outbox().await.unwrap();
        let submit = button(&t, "Envoyer").await;
        g.process_update(&updates::callback(303, OWNER, &submit, 901))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let run = s
            .runs
            .list(None, 5)
            .await
            .unwrap()
            .pop()
            .expect("run lancé");
        assert_eq!(run.workflow_id, "build-verify");
        assert_eq!(run.params["objectif"], "réparer le build");
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
        assert!(sent.contains(&run.id), "{sent}");
    }

    /// Issue #30 : `/schedules`, 🗑, Confirmer : la planification est supprimée et le
    /// message édité sur place.
    #[tokio::test]
    async fn a_schedule_is_deleted_after_confirmation() {
        let (_d, g, t, _p) = gateway().await;
        let s = &g.daemon.services;
        let sched = s
            .schedules
            .create(
                penelope_workflow::TriggerKind::Cron,
                json!({"expr": "0 9 * * 1"}),
                json!({"type": "notify", "template": "⏰ Revue hebdo"}),
                json!({}),
            )
            .await
            .unwrap();
        g.process_update(&updates::text_message(310, OWNER, OWNER, "/schedules"))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let delete = button(&t, "🗑").await;
        g.process_update(&updates::callback(311, OWNER, &delete, 910))
            .await
            .unwrap();
        let confirm_screen = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        assert!(
            texts(&confirm_screen)
                .join("\n")
                .contains("Supprimer le déclencheur"),
            "écran de confirmation : {confirm_screen:?}"
        );
        assert_eq!(
            s.schedules.list().await.unwrap().len(),
            1,
            "rien de supprimé avant la confirmation"
        );
        let confirm = button(&t, "Confirmer").await;
        g.process_update(&updates::callback(312, OWNER, &confirm, 910))
            .await
            .unwrap();
        assert!(
            !s.schedules
                .list()
                .await
                .unwrap()
                .iter()
                .any(|x| x.id == sched.id),
            "déclencheur supprimé"
        );
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let last = edits.last().unwrap();
        assert_eq!(last["message_id"], 910, "même message redessiné");
        assert!(
            last["text"].as_str().unwrap().contains("Aucun déclencheur"),
            "{last}"
        );
    }

    /// Issue #30 : `/mcp`, 🔄 sur un serveur : le superviseur le redémarre et le message
    /// affiche son état à jour.
    #[tokio::test]
    async fn an_mcp_server_is_restarted_from_its_menu() {
        use crate::mcp::testing::{FakeConnector, declare, server, tool};
        let (_d, g, t, _p) = gateway().await;
        let fake = Arc::new(FakeConnector::default());
        fake.serve(
            "redmine",
            server(Arc::new(std::sync::Mutex::new(vec![tool(
                "list_issues",
                json!({"readOnlyHint": true}),
            )]))),
        );
        let sup = crate::mcp::McpSupervisor::new(g.daemon.services.clone(), fake.clone());
        declare(&sup, "redmine", "");
        sup.reload().await;
        g.daemon.hooks.set_mcp(sup.clone());
        let opened = fake.opened("redmine");

        g.process_update(&updates::text_message(320, OWNER, OWNER, "/mcp"))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let restart = button(&t, "🔄").await;
        g.process_update(&updates::callback(321, OWNER, &restart, 920))
            .await
            .unwrap();
        assert_eq!(fake.opened("redmine"), opened + 1, "redémarrage demandé");
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let last = edits.last().expect("message redessiné");
        assert_eq!(last["message_id"], 920);
        assert!(
            last["text"].as_str().unwrap().contains("redmine")
                && last["text"].as_str().unwrap().contains("prêt"),
            "{last}"
        );
        let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
        assert!(
            answers
                .iter()
                .any(|a| a["text"].as_str().is_some_and(|x| x.contains("redmine"))),
            "{answers:?}"
        );
    }

    /// Issue #31 : la réponse d'une boucle arrêtée arrive avec ses suites en boutons, sans
    /// rapport technique, et un clic arrive dans la session comme un message du propriétaire.
    #[tokio::test]
    async fn a_stopped_loop_answer_offers_choices_that_become_messages() {
        use crate::bus::ChannelDelivery;
        let (_d, g, t, _p) = gateway().await;
        let d = &g.daemon;
        let chat = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: Some(640),
        };
        let sid = d.chat_session_for(&chat).await.unwrap();
        g.deliver(
            "t1",
            &sid,
            &chat,
            &TurnOutcome::LoopAborted {
                report: "l'outil `tool_call` a été appelé 4 fois\n\nAppels du tour :".into(),
                answer: "La messagerie répond « 401 Unauthorized » à chaque lecture.".into(),
                choices: vec!["Chercher autrement".into(), "Laisser tomber".into()],
            },
        )
        .await;
        g.flush_outbox().await.unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        let card = sent.last().unwrap();
        let text = card["text"].as_str().unwrap();
        assert!(text.contains("401 Unauthorized"), "{text}");
        assert!(
            !text.contains("Appels du tour"),
            "rapport technique absent : {text}"
        );
        let token = inline_buttons(card)
            .into_iter()
            .find(|(l, _)| l == "Chercher autrement")
            .expect("suite en bouton")
            .1;
        g.process_update(&updates::callback(641, OWNER, &token, 642))
            .await
            .unwrap();
        let turn = d
            .services
            .turns
            .claim("test")
            .await
            .unwrap()
            .expect("message mis en file");
        assert_eq!(turn.session_id, sid);
        assert!(
            turn.payload.to_string().contains("Chercher autrement"),
            "{}",
            turn.payload
        );
    }

    /// Issue #30 : le digest renvoie vers un écran précis par lien profond, une fois le bot
    /// connu.
    #[tokio::test]
    async fn the_digest_links_to_screens_once_the_bot_is_known() {
        let (_d, g, _t, _p) = gateway().await;
        let d = &g.daemon;
        assert!(deep_link(d, "approvals").await.is_none());
        d.kv_set(BOT_USERNAME_KEY, "penelope_test_bot")
            .await
            .unwrap();
        assert_eq!(
            deep_link(d, "approvals").await.as_deref(),
            Some("https://t.me/penelope_test_bot?start=approvals")
        );
        d.services
            .approvals
            .create(
                penelope_hitl::ApprovalKind::ToolCall,
                "native__shell",
                penelope_kernel::risk::RiskClass::Write,
                json!({"arguments": {}}),
                Vec::new(),
                None,
                None,
                false,
            )
            .await
            .unwrap();
        let digest = crate::dream::digest_text(d).await.unwrap();
        assert!(
            digest.contains("[ouvrir](https://t.me/penelope_test_bot?start=approvals)"),
            "{digest}"
        );
        assert!(
            markdown_to_html(&digest)
                .contains("<a href=\"https://t.me/penelope_test_bot?start=approvals\">")
        );
    }

    /// Issue #30 : chaque commande du catalogue appelée sans argument rend un clavier ou un
    /// texte sans « Usage : », et aucune ne tombe dans un affichage générique.
    #[tokio::test]
    async fn every_catalog_command_without_arguments_opens_a_screen() {
        let (_d, g, t, _p) = gateway().await;
        g.daemon
            .publish_config("test", |c| {
                c.upgrade.base_url = "http://127.0.0.1:9".into();
                Ok(vec!["upgrade.base_url".into()])
            })
            .unwrap();
        // Réponses différées (résumé, rêve) ou fichier : pas de message immédiat attendu.
        let deferred = ["compact", "dream", "export"];
        let mut update = 400;
        for c in penelope_telegram::commands::all() {
            let before = t.calls_to(tg::SEND_MESSAGE).await.len();
            let edits_before = t.calls_to(tg::EDIT_MESSAGE_TEXT).await.len();
            update += 1;
            g.process_update(&updates::text_message(
                update,
                OWNER,
                OWNER,
                &format!("/{}", c.name),
            ))
            .await
            .unwrap_or_else(|e| panic!("/{} : {e}", c.name));
            g.flush_outbox().await.unwrap();
            let mut new = t.calls_to(tg::SEND_MESSAGE).await.split_off(before);
            new.extend(
                t.calls_to(tg::EDIT_MESSAGE_TEXT)
                    .await
                    .split_off(edits_before),
            );
            if new.is_empty() {
                assert!(deferred.contains(&c.name), "/{} n'a rien répondu", c.name);
                continue;
            }
            for call in &new {
                let text = call["text"].as_str().unwrap_or_default();
                assert!(!text.contains("Usage"), "/{} : {text}", c.name);
                assert!(
                    !text.contains("Commande inconnue") && !text.contains("pas encore branchée"),
                    "/{} : {text}",
                    c.name
                );
            }
        }
        // Lien profond : `/start <charge>` ouvre l'écran visé.
        let before = t.calls_to(tg::SEND_MESSAGE).await.len();
        g.process_update(&updates::text_message(
            499,
            OWNER,
            OWNER,
            "/start runs_stuck",
        ))
        .await
        .unwrap();
        g.flush_outbox().await.unwrap();
        let opened = texts(&t.calls_to(tg::SEND_MESSAGE).await.split_off(before)).join("\n");
        assert!(opened.contains("Runs en pause ou bloqués"), "{opened}");
    }

    #[tokio::test]
    async fn schedules_are_listed_paused_and_run_from_telegram() {
        let (_d, g, t, _p) = gateway().await;
        let sched = g
            .daemon
            .services
            .schedules
            .create(
                penelope_workflow::TriggerKind::Cron,
                json!({"expr": "0 9 * * 1"}),
                json!({"type": "notify", "template": "⏰ Revue hebdo"}),
                json!({}),
            )
            .await
            .unwrap();
        let id = sched.id.clone();
        for (i, text) in [
            "/schedules".to_string(),
            format!("/schedules pause {id}"),
            format!("/schedules run {id}"),
            "/schedules".to_string(),
        ]
        .iter()
        .enumerate()
        {
            g.process_update(&updates::text_message(90 + i as i64, OWNER, OWNER, text))
                .await
                .unwrap();
        }
        drain(&g).await;
        let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            out[0].contains("0 9 * * 1") && out[0].contains("Revue hebdo"),
            "{out:?}"
        );
        assert!(out[1].contains("en pause"), "{out:?}");
        // Le tir immédiat envoie le rappel lui-même, puis la confirmation.
        assert!(
            out.iter()
                .any(|m| m.contains("⏰ Revue hebdo") && !m.contains("cron")),
            "{out:?}"
        );
        assert!(out.iter().any(|m| m.contains("déclenché")), "{out:?}");
        assert!(out.last().unwrap().contains("⏸"), "{out:?}");
    }

    #[tokio::test]
    async fn compact_summarises_the_chat_session_and_reports_back() {
        let (_d, g, t, p) = gateway().await;
        let origin = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let sid = g.daemon.chat_session_for(&origin).await.unwrap();
        let h = &g.daemon.services.context.history;
        for i in 0..40 {
            let m = if i % 2 == 0 {
                penelope_llm::types::ChatMessage::user(format!("q{i} {}", "mot ".repeat(500)))
            } else {
                penelope_llm::types::ChatMessage::assistant(format!("r{i} {}", "mot ".repeat(500)))
            };
            h.append(&sid, &m, 600, 0, false, None).await.unwrap();
        }
        p.reply(r#"{"objectif": "tester /compact", "fait": "tout"}"#);

        g.process_update(&updates::text_message(95, OWNER, OWNER, "/compact"))
            .await
            .unwrap();
        // Le bilan arrive quand le résumé est publié, sans bloquer la file des updates.
        let mut out = Vec::new();
        for _ in 0..100 {
            g.flush_outbox().await.unwrap();
            out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
            if !out.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            out.iter().any(|m| m.contains("messages résumés")),
            "{out:?}"
        );
        assert_eq!(
            g.daemon
                .services
                .context
                .lcm
                .active_nodes(&sid)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_workflow_runs_from_telegram_with_buttons_and_typed_input() {
        let (_d, g, t, _p) = gateway().await;
        let s = &g.daemon.services;
        let raw = json!({
            "metadata": {"id": "validation", "name": "Validation", "parameters": [
                {"id": "sujet", "label": "Sujet", "type": "string", "required": true}
            ]},
            "entryStep": "decider",
            "settings": {"budget": {"maxUsd": 1.0, "maxTokens": 1000, "maxWallMs": 60000}},
            "steps": [
                {"id": "decider", "name": "Valider {{sujet}} ?", "type": "user",
                 "template": "question", "choices": ["Valider", "Réviser"], "input": "text",
                 "transitions": [
                    {"goto": "$done", "condition": {"type": "step_result", "result": "Valider"}},
                    {"goto": "$blocked", "condition": {"type": "step_result", "result": "Réviser"}}
                 ]}
            ]
        });
        let wf = penelope_workflow::Workflow::from_json(&raw.to_string()).unwrap();
        let known = crate::runtime::workflow_known(&s.config.config(), &s.mcp_tools).await;
        let dir = s.platform.dirs.workflows();
        std::fs::create_dir_all(&dir).unwrap();
        s.workflows.write(&dir, &wf, &known).unwrap();
        s.workflows
            .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);

        // Sans paramètres saisis, le formulaire les demande un par un (issue #30).
        g.process_update(&updates::text_message(120, OWNER, OWNER, "/run validation"))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(
            sent.iter().any(|m| m.contains("Sujet")),
            "paramètre demandé : {sent:?}"
        );
        g.process_update(&updates::text_message(121, OWNER, OWNER, "devis-42"))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let recap = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
        let send = inline_buttons(&recap)
            .into_iter()
            .find(|(l, _)| l.contains("Envoyer"))
            .expect("récapitulatif")
            .1;
        g.process_update(&updates::callback(1210, OWNER, &send, 690))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let after = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        let run = s
            .runs
            .list(None, 5)
            .await
            .unwrap()
            .pop()
            .unwrap_or_else(|| panic!("run lancé : {after:?}"));
        assert_eq!(run.params["sujet"], "devis-42");

        crate::workflow::drive(&g.daemon, &run.id).await.unwrap();
        let calls = t.calls_to(tg::SEND_MESSAGE).await;
        let question = calls
            .iter()
            .find(|c| {
                c["text"]
                    .as_str()
                    .is_some_and(|x| x.contains("Valider devis-42"))
            })
            .expect("question avec boutons");
        let token = question["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap()
            .to_string();

        // « Valider » demande une précision : le message suivant la donne.
        g.process_update(&updates::callback(122, OWNER, &token, 700))
            .await
            .unwrap();
        g.process_update(&updates::text_message(
            123,
            OWNER,
            OWNER,
            "ok pour la version 2",
        ))
        .await
        .unwrap();
        g.flush_outbox().await.unwrap();
        assert!(
            s.turns.claim("test").await.unwrap().is_none(),
            "la saisie n'est pas un message de conversation"
        );
        assert_eq!(
            crate::workflow::drive(&g.daemon, &run.id).await.unwrap(),
            penelope_workflow::RunState::Done
        );
        let done = s.runs.get(&run.id).await.unwrap().unwrap();
        assert_eq!(done.step_outputs["decider"]["choice"], "Valider");
        assert_eq!(
            done.step_outputs["decider"]["input"],
            "ok pour la version 2"
        );

        // La carte du run a été éditée sur place, pas renvoyée à chaque étape.
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        assert!(
            edits
                .iter()
                .any(|e| e["text"].as_str().is_some_and(|x| x.contains("terminé"))),
            "{edits:?}"
        );
    }

    #[tokio::test]
    async fn new_starts_a_fresh_session_for_the_chat() {
        let (_d, g, _t, _p) = gateway().await;
        let origin = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let first = g.daemon.chat_session_for(&origin).await.unwrap();
        g.process_update(&updates::text_message(30, OWNER, OWNER, "/new refonte"))
            .await
            .unwrap();
        let second = g.daemon.chat_session_for(&origin).await.unwrap();
        assert_ne!(first, second);
    }

    /// Issue #21 : une séance d'accueil complète sur Telegram écrit `profil.md` et
    /// `memoire.md`, chaque directive reliée à sa réponse ; rejouer une partie montre ce
    /// qui change et remplace l'ancienne réponse.
    #[tokio::test]
    async fn onboarding_writes_the_profile_from_the_answers() {
        let (_d, g, t, _p) = gateway().await;
        let d = g.daemon.clone();
        let vault = crate::conversation::vault_dir(&d.services);
        let last = || {
            let t = t.clone();
            async move { t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone() }
        };
        let click = |label: &'static str, update: i64| {
            let (g, last) = (g.clone(), last);
            async move {
                let msg = last().await;
                let token = inline_buttons(&msg)
                    .into_iter()
                    .find(|(l, _)| l.contains(label))
                    .unwrap_or_else(|| panic!("pas de bouton « {label} » : {msg}"))
                    .1;
                g.process_update(&updates::callback(update, OWNER, &token, 4000))
                    .await
                    .unwrap();
            }
        };
        let say = |text: &'static str, update: i64| {
            let g = g.clone();
            async move {
                g.process_update(&updates::text_message(update, OWNER, OWNER, text))
                    .await
                    .unwrap();
            }
        };

        say("/accueil", 400).await;
        assert!(
            last().await["text"]
                .as_str()
                .unwrap()
                .contains("Accueil · 1/9")
        );
        say("développeur indépendant", 401).await;
        let file = std::fs::read_dir(vault.join("accueil"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let raw = std::fs::read_to_string(&file).unwrap();
        assert!(
            raw.contains("## 1. Quel est ton rôle ou ton métier ?\n\ndéveloppeur indépendant"),
            "{raw}"
        );
        assert!(
            raw.contains("## 9. Et ce que je dois toujours faire"),
            "questions écrites d'avance"
        );
        say("Squirrel et ses clients", 402).await;
        say("Pénélope\nSite vitrine", 403).await;
        click("Passer", 404).await;
        click("Tutoiement", 405).await;
        click("Courtes", 406).await;
        say("français", 407).await;
        say("écrire en mon nom\nsupprimer sans demander", 408).await;
        say("éviter le jargon", 409).await;
        let recap = last().await["text"].as_str().unwrap().to_string();
        assert!(
            recap.contains("+ Toujours tutoyer le propriétaire"),
            "{recap}"
        );
        assert!(
            recap.contains("+ Jamais supprimer sans demander"),
            "{recap}"
        );
        assert!(
            recap.contains("+ Projet en cours du propriétaire : Site vitrine"),
            "{recap}"
        );
        assert!(!recap.contains("Outils"), "question passée : {recap}");
        click("Écrire", 410).await;

        let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
        for directive in [
            "Toujours tutoyer le propriétaire",
            "Préférer des réponses courtes",
            "Toujours répondre en français",
            "Jamais écrire en mon nom",
            "Éviter le jargon",
        ] {
            assert!(profil.contains(directive), "{directive} absent : {profil}");
        }
        assert!(
            std::fs::read_to_string(vault.join("memoire.md"))
                .unwrap()
                .contains("Rôle du propriétaire : développeur indépendant")
        );
        let rel = format!("accueil/{}", file.file_name().unwrap().to_string_lossy());
        let source: String = d
            .services
            .store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT p.source_ref FROM mem_entries e JOIN mem_provenance p ON p.uid = e.uid
                     WHERE e.text = 'Toujours tutoyer le propriétaire'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(source, format!("{rel}#q5"));

        // Rejouer le style : l'ancienne réponse est remplacée, le reste gardé.
        say("/accueil style", 420).await;
        click("Vouvoiement", 421).await;
        click("Courtes", 422).await;
        click("Français", 423).await;
        let recap = last().await["text"].as_str().unwrap().to_string();
        assert!(
            recap.contains(
                "- Toujours tutoyer le propriétaire\n+ Toujours vouvoyer le propriétaire"
            ),
            "{recap}"
        );
        assert!(
            recap.contains("= Préférer des réponses courtes (déjà retenu)"),
            "{recap}"
        );
        click("Écrire", 424).await;
        let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
        assert!(profil.contains("Toujours vouvoyer le propriétaire"));
        assert!(
            !profil.contains("Toujours tutoyer le propriétaire"),
            "{profil}"
        );
    }

    /// Issue #21 : un premier message sur un profil vide propose l'accueil, une fois.
    #[tokio::test]
    async fn an_empty_profile_proposes_onboarding_once() {
        let (_d, g, t, p) = gateway().await;
        for i in 0..2 {
            p.reply(r#"{"complexity":"low"}"#);
            p.reply("Bonjour !");
            g.process_update(&updates::text_message(430 + i, OWNER, OWNER, "salut"))
                .await
                .unwrap();
            drain(&g).await;
        }
        let proposals = texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .into_iter()
            .filter(|x| x.contains("Ton profil est encore vide"))
            .count();
        assert_eq!(proposals, 1);
    }

    /// Issue #12 : une demande `elicitation/create` devient une carte Telegram ; le clic du
    /// propriétaire part au serveur (confirmation, formulaire, refus, lien), un délai
    /// dépassé annule et le dit.
    #[tokio::test]
    async fn mcp_elicitation_is_answered_from_telegram() {
        use crate::mcp::testing::{FakeConnector, declare, server, tool};
        use penelope_mcp::transport::LoopbackTransport;
        let (_d, g, t, _p) = gateway().await;
        let fake = Arc::new(FakeConnector::default());
        for name in ["redmine", "lent"] {
            fake.serve(
                name,
                server(Arc::new(std::sync::Mutex::new(vec![tool(
                    "update_issue",
                    json!({}),
                )]))),
            );
        }
        let sup = crate::mcp::McpSupervisor::new(g.daemon.services.clone(), fake.clone());
        declare(&sup, "redmine", "");
        declare(&sup, "lent", "elicitation_timeout = \"300ms\"\n");
        sup.reload().await;
        g.daemon.hooks.set_mcp(sup.clone());
        let broker = g.daemon.services.elicitations.clone();

        // Un propriétaire est joignable : l'élicitation est annoncée, pas le sampling.
        let tr = fake.last_transport("redmine");
        let log = tr.call_log().await;
        let (_, init) = log.iter().find(|(m, _)| m == "initialize").unwrap();
        assert!(init["capabilities"]["elicitation"].is_object(), "{init}");
        assert!(init["capabilities"].get("sampling").is_none(), "{init}");

        let answer = |tr: Arc<LoopbackTransport>, id: u64| async move {
            for _ in 0..300 {
                if let Some((_, r)) = tr
                    .responses
                    .lock()
                    .await
                    .iter()
                    .find(|(i, _)| *i == json!(id))
                {
                    return r.clone().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("aucune réponse à la requête {id}");
        };
        let card = |needle: &'static str| {
            let (t, broker) = (t.clone(), broker.clone());
            async move {
                for _ in 0..300 {
                    let open = broker.open();
                    if let Some((req, id)) = open.iter().find(|(r, _)| r.message.contains(needle))
                        && let Some(sent) = t
                            .calls_to(tg::SEND_MESSAGE)
                            .await
                            .into_iter()
                            .rev()
                            .find(|c| c["text"].as_str().is_some_and(|x| x.contains(needle)))
                    {
                        return (req.clone(), id.unwrap(), sent);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("aucune carte « {needle} »");
            }
        };

        // 1. Confirmation : Accepter.
        tr.push_server_request(
            8,
            "elicitation/create",
            json!({"message": "Modifier le ticket 42 ?",
                   "requestedSchema": {"type": "object", "properties": {}}}),
        );
        let (_, card_id, sent) = card("Modifier le ticket 42").await;
        let text = sent["text"].as_str().unwrap();
        assert!(text.contains("<code>redmine</code>"), "{text}");
        assert!(text.contains("Sans réponse d'ici 10 min"), "{text}");
        let b = inline_buttons(&sent);
        let labels: Vec<&str> = b.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["✅ Accepter", "🚫 Refuser", "✖️ Annuler"]);
        g.process_update(&updates::callback(200, OWNER, &b[0].1, card_id))
            .await
            .unwrap();
        assert_eq!(
            answer(tr.clone(), 8).await,
            json!({"action": "accept", "content": {}})
        );
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        assert!(
            edits.last().unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("Accepté"),
            "{edits:?}"
        );

        // 2. Formulaire : Remplir, un enum titré au bouton, un texte en message, Envoyer.
        tr.push_server_request(
            9,
            "elicitation/create",
            json!({"message": "Précise la priorité",
                   "requestedSchema": {"type": "object", "properties": {
                       "priorite": {"type": "string", "title": "Priorité", "oneOf": [
                           {"const": "high", "title": "Haute"},
                           {"const": "low", "title": "Basse"}]},
                       "note": {"type": "string", "title": "Note"}},
                   "required": ["priorite"]}}),
        );
        let (_, card_id, sent) = card("Précise la priorité").await;
        assert!(sent["text"].as_str().unwrap().contains("Priorité *, Note"));
        let fill = inline_buttons(&sent)[0].clone();
        assert_eq!(fill.0, "📝 Remplir");
        g.process_update(&updates::callback(201, OWNER, &fill.1, card_id))
            .await
            .unwrap();
        let step = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
        let high = inline_buttons(&step)
            .into_iter()
            .find(|(l, _)| l == "Haute")
            .unwrap();
        g.process_update(&updates::callback(202, OWNER, &high.1, 3000))
            .await
            .unwrap();
        g.process_update(&updates::text_message(
            203,
            OWNER,
            OWNER,
            "client en attente",
        ))
        .await
        .unwrap();
        let summary = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
        let buttons = inline_buttons(&summary);
        assert!(
            buttons.iter().any(|(l, _)| l == "🚫 Refuser"),
            "{buttons:?}"
        );
        let send = buttons.iter().find(|(l, _)| l == "✅ Envoyer").unwrap();
        g.process_update(&updates::callback(204, OWNER, &send.1, 3001))
            .await
            .unwrap();
        assert_eq!(
            answer(tr.clone(), 9).await,
            json!({"action": "accept",
                   "content": {"priorite": "high", "note": "client en attente"}})
        );
        assert!(
            g.daemon
                .services
                .turns
                .claim("test")
                .await
                .unwrap()
                .is_none(),
            "la saisie du formulaire n'ouvre pas de tour"
        );

        // 3. Refus.
        tr.push_server_request(
            10,
            "elicitation/create",
            json!({"message": "Supprimer le ticket 7 ?"}),
        );
        let (_, card_id, sent) = card("Supprimer le ticket 7").await;
        let decline = inline_buttons(&sent)[1].clone();
        g.process_update(&updates::callback(205, OWNER, &decline.1, card_id))
            .await
            .unwrap();
        assert_eq!(answer(tr.clone(), 10).await, json!({"action": "decline"}));

        // 4. Sans réponse : annulation, carte mise à jour.
        let slow = fake.last_transport("lent");
        slow.push_server_request(
            11,
            "elicitation/create",
            json!({"message": "Toujours là ?"}),
        );
        assert_eq!(answer(slow.clone(), 11).await, json!({"action": "cancel"}));
        let edits = texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
        assert!(
            edits
                .iter()
                .any(|e| e.contains("Toujours là") && e.contains("Sans réponse")),
            "{edits:?}"
        );
    }

    /// Issue #12, suite : en 2026-07-28 (MRTR) l'appel est relancé avec les réponses et
    /// l'état du serveur, le modèle lit qui a répondu ; un lien (2025-11-25) montre son
    /// domaine, s'ouvre après accord et sa fin signalée met la carte à jour.
    #[tokio::test]
    async fn mcp_links_and_mrtr_elicitations_from_telegram() {
        use crate::executor::McpGateway;
        use crate::mcp::testing::{FakeConnector, declare, server, tool};
        let (_d, g, t, _p) = gateway().await;
        let fake = Arc::new(FakeConnector::default());
        fake.serve(
            "tracker",
            Arc::new(|m, p| match m {
                "server/discover" => Ok(json!({
                    "protocolVersion": "2026-07-28",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "tracker"}
                })),
                "tools/list" => Ok(json!({"tools": [tool("close_ticket", json!({}))]})),
                "tools/call" => Ok(
                    match (p.get("inputResponses"), p["arguments"]["lien"].as_bool()) {
                        (Some(r), _) => {
                            json!({"content": [{"type": "text", "text": format!("reçu {r}")}]})
                        }
                        (None, Some(true)) => json!({
                            "resultType": "input_required",
                            "inputRequests": {"compte": {"method": "elicitation/create", "params": {
                                "mode": "url", "url": "https://auth.example.com/connect?t=1",
                                "message": "Relie ton compte."}}},
                            "requestState": "etat-lien"
                        }),
                        (None, _) => json!({
                            "resultType": "input_required",
                            "inputRequests": {"confirm": {"method": "elicitation/create",
                                "params": {"message": "Fermer le ticket 9 ?"}}},
                            "requestState": "etat-1"
                        }),
                    },
                ),
                _ => Ok(json!({})),
            }),
        );
        let legacy = server(Arc::new(std::sync::Mutex::new(vec![tool(
            "sync",
            json!({}),
        )])));
        fake.serve(
            "drive",
            Arc::new(move |m, p| match m {
                "initialize" => Ok(json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "drive"}
                })),
                _ => legacy(m, p),
            }),
        );
        let sup = crate::mcp::McpSupervisor::new(g.daemon.services.clone(), fake.clone());
        declare(&sup, "tracker", "");
        declare(&sup, "drive", "");
        sup.reload().await;
        g.daemon.hooks.set_mcp(sup.clone());
        let broker = g.daemon.services.elicitations.clone();
        let card = |needle: &'static str| {
            let (t, broker) = (t.clone(), broker.clone());
            async move {
                for _ in 0..300 {
                    if let Some((req, Some(id))) = broker
                        .open()
                        .into_iter()
                        .find(|(r, _)| r.message.contains(needle))
                        && let Some(sent) = t
                            .calls_to(tg::SEND_MESSAGE)
                            .await
                            .into_iter()
                            .rev()
                            .find(|c| c["text"].as_str().is_some_and(|x| x.contains(needle)))
                    {
                        return (req, id, sent);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("aucune carte « {needle} »");
            }
        };

        // MRTR, formulaire : refus, relance avec la réponse et l'état, note pour le modèle.
        let call = {
            let sup = sup.clone();
            tokio::spawn(async move {
                sup.call_tool("mcp__tracker__close_ticket", &json!({}))
                    .await
            })
        };
        let (_, card_id, sent) = card("Fermer le ticket 9").await;
        g.process_update(&updates::callback(
            300,
            OWNER,
            &inline_buttons(&sent)[1].1,
            card_id,
        ))
        .await
        .unwrap();
        let result = call.await.unwrap().unwrap();
        let text = result.to_string();
        assert!(
            text.contains(r#"reçu {\"confirm\":{\"action\":\"decline\"}}"#),
            "{text}"
        );
        assert!(
            text.contains("le propriétaire a refusé sur Telegram (decline)"),
            "{text}"
        );
        let tr = fake.last_transport("tracker");
        let log = tr.call_log().await;
        let retry = &log
            .iter()
            .filter(|(m, _)| m == "tools/call")
            .nth(1)
            .unwrap()
            .1;
        assert_eq!(retry["requestState"], "etat-1");
        assert_eq!(
            retry["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"],
            json!({"form": {}, "url": {}})
        );

        // MRTR, lien : domaine et URL entière, accord, puis « J'ai terminé ».
        let call = {
            let sup = sup.clone();
            tokio::spawn(async move {
                sup.call_tool("mcp__tracker__close_ticket", &json!({"lien": true}))
                    .await
            })
        };
        let (_, card_id, sent) = card("Relie ton compte").await;
        let html = sent["text"].as_str().unwrap();
        assert!(html.contains("Domaine : <b>auth.example.com</b>"), "{html}");
        assert!(
            html.contains("<code>https://auth.example.com/connect?t=1</code>"),
            "{html}"
        );
        g.process_update(&updates::callback(
            301,
            OWNER,
            &inline_buttons(&sent)[0].1,
            card_id,
        ))
        .await
        .unwrap();
        let edited = t
            .calls_to(tg::EDIT_MESSAGE_TEXT)
            .await
            .last()
            .unwrap()
            .clone();
        let buttons = inline_buttons(&edited);
        assert_eq!(
            buttons[0],
            (
                "🌐 Ouvrir auth.example.com".to_string(),
                "https://auth.example.com/connect?t=1".to_string()
            )
        );
        let done = buttons
            .iter()
            .find(|(l, _)| l == "✅ J'ai terminé")
            .unwrap();
        g.process_update(&updates::callback(302, OWNER, &done.1, card_id))
            .await
            .unwrap();
        let text = call.await.unwrap().unwrap().to_string();
        assert!(
            text.contains(r#"{\"compte\":{\"action\":\"accept\"}}"#),
            "{text}"
        );

        // 2025-11-25, requête du serveur en mode URL : accord, puis fin signalée.
        let drive = fake.last_transport("drive");
        drive.push_server_request(
            21,
            "elicitation/create",
            json!({"mode": "url", "elicitationId": "e-9",
                   "url": "https://drive.example.org/oauth", "message": "Autorise Drive."}),
        );
        let (_, card_id, sent) = card("Autorise Drive").await;
        g.process_update(&updates::callback(
            303,
            OWNER,
            &inline_buttons(&sent)[0].1,
            card_id,
        ))
        .await
        .unwrap();
        for _ in 0..300 {
            if !drive.responses.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            drive.responses.lock().await[0].1.clone().unwrap(),
            json!({"action": "accept"})
        );
        drive.push_notification(
            "notifications/elicitation/complete",
            json!({"elicitationId": "e-9"}),
        );
        let mut finished = false;
        for _ in 0..300 {
            if texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await)
                .iter()
                .any(|e| e.contains("Autorise Drive") && e.contains("terminée"))
            {
                finished = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(finished, "carte mise à jour à la fin du lien");
    }

    /// Issue #14 : `/sessions` rend un bouton par session ; un clic lie le chat à la session
    /// et édite le message, les sessions fermées n'apparaissent qu'à la demande.
    #[tokio::test]
    async fn sessions_menu_switches_with_a_click() {
        let (_d, g, t, _p) = gateway().await;
        let d = g.daemon.clone();
        let chat = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let first = d.chat_session_for(&chat).await.unwrap();
        d.services
            .sessions
            .set_title(&first, "Refonte du site", false)
            .await
            .unwrap();
        d.enqueue_message(&first, "en attente", &chat, None)
            .await
            .unwrap();
        let other = d
            .services
            .sessions
            .create(
                penelope_kernel::session::SessionKind::Chat,
                Some("Budget 2027".into()),
            )
            .await
            .unwrap()
            .id
            .to_string();
        let closed = d
            .services
            .sessions
            .create(
                penelope_kernel::session::SessionKind::Chat,
                Some("Vieux sujet".into()),
            )
            .await
            .unwrap()
            .id
            .to_string();
        d.services
            .sessions
            .set_state(&closed, "closed")
            .await
            .unwrap();

        let buttons = inline_buttons;
        g.process_update(&updates::text_message(90, OWNER, OWNER, "/sessions"))
            .await
            .unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        let menu = buttons(sent.last().unwrap());
        let labels: Vec<&str> = menu.iter().map(|(l, _)| l.as_str()).collect();
        assert!(
            labels
                .iter()
                .any(|l| l.starts_with("▶️ ⏳ Refonte du site")),
            "{labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.contains("Vieux sujet")),
            "{labels:?}"
        );
        let switch = menu
            .iter()
            .find(|(l, _)| l.contains("Budget 2027"))
            .unwrap()
            .1
            .clone();

        g.process_update(&updates::callback(91, OWNER, &switch, 1001))
            .await
            .unwrap();
        let bound = d
            .services
            .sessions
            .find_by_topic(OWNER, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bound.id.to_string(), other);
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let edited = edits.last().expect("message édité");
        assert_eq!(edited["message_id"], 1001);
        let labels: Vec<String> = buttons(edited).into_iter().map(|(l, _)| l).collect();
        assert!(
            labels.iter().any(|l| l.starts_with("▶️ Budget 2027")),
            "{labels:?}"
        );
        // La session qui a perdu le chat n'a plus rien en attente.
        assert!(
            labels.iter().any(|l| l.starts_with("Refonte du site")),
            "{labels:?}"
        );

        // Fermées à la demande, puis « ⋯ » > Fermer sur la session d'origine.
        let show_closed = buttons(edited)
            .into_iter()
            .find(|(l, _)| l == "Voir les fermées")
            .unwrap()
            .1;
        g.process_update(&updates::callback(92, OWNER, &show_closed, 1001))
            .await
            .unwrap();
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let all = buttons(edits.last().unwrap());
        assert!(
            all.iter().any(|(l, _)| l.starts_with("🔒 Vieux sujet")),
            "{all:?}"
        );
        let refonte = all
            .iter()
            .position(|(l, _)| l.starts_with("Refonte du site"))
            .unwrap();
        let more = all[refonte + 1].1.clone();
        g.process_update(&updates::callback(93, OWNER, &more, 1001))
            .await
            .unwrap();
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let close = buttons(edits.last().unwrap())
            .into_iter()
            .find(|(l, _)| l.contains("Fermer"))
            .unwrap()
            .1;
        g.process_update(&updates::callback(94, OWNER, &close, 1001))
            .await
            .unwrap();
        assert_eq!(
            d.services
                .sessions
                .get(&first)
                .await
                .unwrap()
                .unwrap()
                .state,
            "closed"
        );
        let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
        assert!(
            answers.last().unwrap()["text"]
                .as_str()
                .unwrap()
                .starts_with("Session fermée"),
            "{answers:?}"
        );

        // Renommer : le message suivant devient le titre, sans ouvrir de tour.
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let rows = buttons(edits.last().unwrap());
        let budget = rows
            .iter()
            .position(|(l, _)| l.contains("Budget 2027"))
            .unwrap();
        g.process_update(&updates::callback(95, OWNER, &rows[budget + 1].1, 1001))
            .await
            .unwrap();
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        let rename = buttons(edits.last().unwrap())
            .into_iter()
            .find(|(l, _)| l.contains("Renommer"))
            .unwrap()
            .1;
        g.process_update(&updates::callback(96, OWNER, &rename, 1001))
            .await
            .unwrap();
        g.process_update(&updates::text_message(
            97,
            OWNER,
            OWNER,
            "Budget prévisionnel",
        ))
        .await
        .unwrap();
        assert_eq!(
            d.services
                .sessions
                .get(&other)
                .await
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("Budget prévisionnel")
        );
        assert!(d.services.turns.claim("test").await.unwrap().is_none());
    }

    /// Issue #10, règle précisée : une session quittée finit son tour en cours, mais sa
    /// réponse, ses messages et ses approbations sont mis de côté derrière une seule
    /// notification ; les nouveaux messages vont au focus ; le bouton « Basculer » envoie
    /// tout dans l'ordre.
    #[tokio::test]
    async fn background_sessions_hold_their_replies_until_switched_back() {
        let (_d, g, t, p) = gateway().await;
        let d = g.daemon.clone();
        let chat = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let first = d.chat_session_for(&chat).await.unwrap();
        d.services
            .sessions
            .set_title(&first, "Refonte", false)
            .await
            .unwrap();
        let asked = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: Some(77),
        };
        d.enqueue_message(&first, "longue question", &asked, None)
            .await
            .unwrap();
        // Tour déjà pris par un runner quand l'utilisateur change de session.
        let running = d.services.turns.claim("test").await.unwrap().unwrap();
        g.process_update(&updates::text_message(100, OWNER, OWNER, "/fork"))
            .await
            .unwrap();
        let fork = d.chat_session_for(&chat).await.unwrap();
        assert_ne!(fork, first);

        p.reply(r#"{"complexity":"low"}"#);
        p.reply("réponse de fond");
        crate::runner::process(&d, running, Duration::from_secs(30)).await;
        let m: Arc<dyn Messenger> = g.clone();
        m.send_session_text(&first, &chat, "question de fond ?")
            .await
            .unwrap();
        let approval = d
            .services
            .approvals
            .create(
                penelope_hitl::ApprovalKind::ToolCall,
                "shell_exec",
                penelope_kernel::risk::RiskClass::Write,
                json!({"arguments": {"cmd": "make deploy"}}),
                vec!["Autoriser".into(), "Refuser".into()],
                Some(&first),
                None,
                false,
            )
            .await
            .unwrap();
        g.deliver(
            "t-approbation",
            &first,
            &chat,
            &TurnOutcome::AwaitingApproval {
                approval_id: approval.id.0.clone(),
            },
        )
        .await;
        g.flush_outbox().await.unwrap();

        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        let out = texts(&sent);
        assert!(
            !out.iter()
                .any(|x| x.contains("réponse de fond") || x.contains("question de fond")),
            "{out:?}"
        );
        let notices: Vec<&Value> = sent
            .iter()
            .filter(|c| c["text"].as_str().is_some_and(|x| x.contains("📬")))
            .collect();
        assert_eq!(notices.len(), 1, "une seule notification : {out:?}");
        assert_eq!(notices[0]["disable_notification"], true);
        let edits = texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await);
        assert!(
            edits
                .iter()
                .any(|x| x.contains("2 réponses et 1 approbation en attente dans « Refonte »")),
            "{edits:?}"
        );

        // Un nouveau message va au focus, qui répond aussitôt.
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("réponse du fork");
        g.process_update(&updates::text_message(101, OWNER, OWNER, "et maintenant ?"))
            .await
            .unwrap();
        drain(&g).await;
        g.flush_outbox().await.unwrap();
        assert!(
            texts(&t.calls_to(tg::SEND_MESSAGE).await).contains(&"réponse du fork".to_string())
        );

        // Basculer : tout part dans l'ordre, la notification perd son bouton.
        let switch = inline_buttons(notices[0])[0].clone();
        assert_eq!(switch.0, "↪️ Basculer");
        g.process_update(&updates::callback(102, OWNER, &switch.1, 5000))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        assert_eq!(
            d.services
                .sessions
                .find_by_topic(OWNER, None)
                .await
                .unwrap()
                .unwrap()
                .id
                .to_string(),
            first
        );
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        let out = texts(&sent);
        let at = |needle: &str| out.iter().position(|x| x.contains(needle)).unwrap();
        assert!(at("réponse de fond") < at("question de fond ?"), "{out:?}");
        assert!(at("question de fond ?") < at("make deploy"), "{out:?}");
        let answer = sent
            .iter()
            .find(|c| {
                c["text"]
                    .as_str()
                    .is_some_and(|x| x.contains("réponse de fond"))
            })
            .unwrap();
        assert_eq!(answer["reply_parameters"]["message_id"], 77);
        assert!(
            texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await)
                .iter()
                .any(|x| x.contains("envoyées ci-dessous"))
        );
        assert!(d.kv_get(&held_key(&first)).await.unwrap().is_none());
    }

    /// Issue #10 : après `/fork`, seul le fork répond ; les messages en attente de la session
    /// d'origine sont annulés, et `/close` arrête une session et vide sa file.
    #[tokio::test]
    async fn after_a_fork_only_the_fork_answers() {
        let (_d, g, t, p) = gateway().await;
        let d = g.daemon.clone();
        let chat = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let first = d.chat_session_for(&chat).await.unwrap();
        for text in ["vieux message 1", "vieux message 2"] {
            d.enqueue_message(&first, text, &chat, None).await.unwrap();
        }
        g.process_update(&updates::text_message(80, OWNER, OWNER, "/fork"))
            .await
            .unwrap();
        let fork = d.chat_session_for(&chat).await.unwrap();
        assert_ne!(fork, first);
        g.flush_outbox().await.unwrap();
        let notice = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
        assert!(notice.contains("2 messages en attente"), "{notice}");

        for i in 0..3 {
            p.reply(r#"{"complexity":"low"}"#);
            p.reply(&format!("réponse {i}"));
            g.process_update(&updates::text_message(
                81 + i,
                OWNER,
                OWNER,
                &format!("question {i}"),
            ))
            .await
            .unwrap();
        }
        drain(&g).await;

        let states = |sid: String| {
            let store = d.services.store.clone();
            async move {
                store
                    .read(move |c| {
                        let mut st = c.prepare(
                            "SELECT state FROM turn_queue WHERE session_id = ?1 ORDER BY enqueued_at",
                        )?;
                        let rows = st.query_map([sid], |r| r.get::<_, String>(0))?;
                        Ok(rows.collect::<Result<Vec<String>, _>>()?)
                    })
                    .await
                    .unwrap()
            }
        };
        assert_eq!(states(first.clone()).await, vec!["cancelled", "cancelled"]);
        assert_eq!(states(fork.clone()).await, vec!["done", "done", "done"]);
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        for i in 0..3 {
            assert!(
                sent.iter().any(|x| x == &format!("réponse {i}")),
                "{sent:?}"
            );
        }
        assert_eq!(
            d.services
                .sessions
                .find_by_topic(OWNER, None)
                .await
                .unwrap()
                .unwrap()
                .id
                .as_str(),
            fork.as_str(),
            "une seule session liée au chat"
        );

        // `/close` : la file du fork est vidée, le chat n'a plus de session liée.
        d.enqueue_message(&fork, "encore un", &chat, None)
            .await
            .unwrap();
        g.process_update(&updates::text_message(90, OWNER, OWNER, "/close"))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        assert_eq!(
            states(fork.clone()).await.last().unwrap(),
            "pending",
            "fermer demande confirmation (issue #30)"
        );
        let confirm = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
        let token = inline_buttons(&confirm)
            .into_iter()
            .find(|(l, _)| l.contains("Confirmer"))
            .expect("bouton Confirmer")
            .1;
        g.process_update(&updates::callback(91, OWNER, &token, 5000))
            .await
            .unwrap();
        assert_eq!(states(fork.clone()).await.last().unwrap(), "cancelled");
        assert!(
            d.services
                .sessions
                .find_by_topic(OWNER, None)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            d.services.sessions.get(&fork).await.unwrap().unwrap().state,
            "closed"
        );
    }

    /// Étape `user` avec `input: "form:<id>"` : le choix ouvre le formulaire, un champ par
    /// écran (boutons ou message), récapitulatif, puis la saisie validée part au workflow.
    #[tokio::test]
    async fn a_workflow_form_is_filled_field_by_field() {
        let (_d, g, t, _p) = gateway().await;
        let d = g.daemon.clone();
        let raw = json!({
            "metadata": {"id": "deploiement-form", "name": "Déploiement", "parameters": []},
            "entryStep": "parametres",
            "settings": {
                "maxIterations": 5,
                "budget": {"maxUsd": 1.0, "maxTokens": 1000, "maxWallMs": 60000},
                "forms": {"deploy": {
                    "type": "object",
                    "required": ["environnement", "version"],
                    "properties": {
                        "environnement": {"type": "string", "title": "Environnement",
                            "enum": ["prod", "staging"], "enumNames": ["Production", "Pré-production"]},
                        "version": {"type": "string", "title": "Version", "minLength": 1},
                        "notifier": {"type": "boolean", "title": "Prévenir l'équipe"}
                    }
                }}
            },
            "steps": [
                {"id": "parametres", "type": "user", "template": "question",
                 "choices": ["Déployer", "Annuler"], "input": "form:deploy",
                 "transitions": [{"goto": "$done"}]}
            ]
        });
        let s = &d.services;
        let wf = penelope_workflow::model::Workflow::from_json(&raw.to_string()).unwrap();
        let known =
            crate::runtime::workflow_known_with(&s.config.config(), &s.mcp_tools, &s.workflows)
                .await;
        let dir = s.platform.dirs.workflows();
        std::fs::create_dir_all(&dir).unwrap();
        s.workflows
            .write(&dir, &wf, &known)
            .expect("workflow valide");
        s.workflows
            .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);

        let origin = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let run = crate::workflow::start_run(&d, "deploiement-form", json!({}), &origin, None, 0)
            .await
            .unwrap();
        crate::workflow::drive(&d, &run.id).await.unwrap();

        let button = |calls: &[Value], label: &str| -> String {
            calls
                .iter()
                .rev()
                .find_map(|c| {
                    c["reply_markup"]["inline_keyboard"]
                        .as_array()?
                        .iter()
                        .find_map(|row| {
                            row.as_array()?.iter().find_map(|b| {
                                (b["text"].as_str()? == label)
                                    .then(|| b["callback_data"].as_str().map(String::from))
                                    .flatten()
                            })
                        })
                })
                .unwrap_or_else(|| panic!("bouton « {label} » absent"))
        };
        let mut update = 500;
        let mut click = |label: &str, calls: Vec<Value>| {
            update += 1;
            (update, button(&calls, label))
        };

        let (u, token) = click("Déployer", t.calls_to(tg::SEND_MESSAGE).await);
        g.process_update(&updates::callback(u, OWNER, &token, 900))
            .await
            .unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        assert!(
            sent.last().unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("Environnement"),
            "{sent:?}"
        );

        let (u, token) = click("Production", sent);
        g.process_update(&updates::callback(u, OWNER, &token, 901))
            .await
            .unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        assert!(
            sent.last().unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("Version")
        );

        g.process_update(&updates::text_message(600, OWNER, OWNER, "1.4.2"))
            .await
            .unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        assert!(
            sent.last().unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("Prévenir")
        );

        let (u, token) = click("Oui", sent);
        g.process_update(&updates::callback(u, OWNER, &token, 902))
            .await
            .unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        let summary = sent.last().unwrap()["text"].as_str().unwrap().to_string();
        assert!(
            summary.contains("Récapitulatif") && summary.contains("1.4.2"),
            "{summary}"
        );

        let (u, token) = click("✅ Envoyer", sent);
        g.process_update(&updates::callback(u, OWNER, &token, 903))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        assert_eq!(
            crate::workflow::drive(&d, &run.id).await.unwrap(),
            penelope_workflow::runs::RunState::Done
        );
        let done = d.services.runs.get(&run.id).await.unwrap().unwrap();
        assert_eq!(
            done.step_outputs["parametres"]["input"],
            json!({"environnement": "prod", "version": "1.4.2", "notifier": true})
        );
        assert_eq!(done.step_outputs["parametres"]["choice"], "Déployer");
    }

    /// Issue #2 : une session reçoit un titre lisible après son premier échange ; il
    /// complète le message « Nouvelle session », se change par `/title` et s'affiche
    /// dans `/sessions`.
    #[tokio::test]
    async fn sessions_get_a_readable_title() {
        let (_d, g, t, p) = gateway().await;
        g.daemon
            .publish_config("test", |c| {
                c.context.auto_title = true;
                Ok(vec!["context.auto_title".into()])
            })
            .unwrap();
        g.process_update(&updates::text_message(40, OWNER, OWNER, "/new"))
            .await
            .unwrap();
        let origin = Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        };
        let sid = g.daemon.chat_session_for(&origin).await.unwrap();

        p.reply(r#"{"complexity":"low"}"#);
        p.reply("On garde la maquette verte pour Zéphyr.");
        p.reply("« Refonte du site Zéphyr. »");
        g.process_update(&updates::text_message(
            41,
            OWNER,
            OWNER,
            "Quelle maquette pour la refonte du site Zéphyr ?",
        ))
        .await
        .unwrap();
        drain(&g).await;
        let mut title = None;
        for _ in 0..100 {
            title = g
                .daemon
                .services
                .sessions
                .get(&sid)
                .await
                .unwrap()
                .unwrap()
                .title;
            if title.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(title.as_deref(), Some("Refonte du site Zéphyr"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
        assert!(
            edits.iter().any(|e| e["text"]
                .as_str()
                .unwrap()
                .contains("Refonte du site Zéphyr")),
            "{edits:?}"
        );

        g.process_update(&updates::text_message(
            42,
            OWNER,
            OWNER,
            "/title Maquette Zéphyr",
        ))
        .await
        .unwrap();
        g.process_update(&updates::text_message(43, OWNER, OWNER, "/sessions"))
            .await
            .unwrap();
        g.flush_outbox().await.unwrap();
        let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
        assert!(sent.iter().any(|x| x.contains("renommée")), "{sent:?}");
        assert!(sent.last().unwrap().contains("Maquette Zéphyr"), "{sent:?}");
        // Un titre posé à la main n'est jamais remplacé par un titre automatique.
        assert!(
            !g.daemon
                .services
                .sessions
                .set_title(&sid, "Autre", true)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn html_rejected_by_telegram_falls_back_to_plain_text() {
        let (_d, g, t, _p) = gateway().await;
        t.fail_once(400, "Bad Request: can't parse entities", None)
            .await;
        g.reply(OWNER, None, None, "a **b** c").await.unwrap();
        g.flush_outbox().await.unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["text"], "a b c");
        assert!(sent[1].get("parse_mode").is_none());
    }

    #[tokio::test]
    async fn long_answers_are_split_in_order() {
        let (_d, g, t, _p) = gateway().await;
        let long = "paragraphe assez long pour déborder\n\n".repeat(300);
        g.reply(OWNER, None, Some(9), &long).await.unwrap();
        g.flush_outbox().await.unwrap();
        let sent = t.calls_to(tg::SEND_MESSAGE).await;
        assert!(sent.len() >= 3, "{}", sent.len());
        assert!(
            sent.iter()
                .all(|s| s["text"].as_str().unwrap().chars().count() <= 4096)
        );
        assert!(sent[0].get("reply_parameters").is_some());
        assert!(sent[1].get("reply_parameters").is_none());
    }

    #[test]
    fn values_render_compactly() {
        let v = json!([{"id": "s1", "title": "refonte", "state": "active"}]);
        assert_eq!(
            render_value(&v),
            "- id : s1 · title : refonte · state : active\n"
        );
        assert_eq!(render_value(&json!([])), "(vide)");
        assert!(render_value(&json!({"version": "0.1.0"})).contains("**version** : 0.1.0"));
    }

    #[test]
    fn model_ids_default_to_openrouter() {
        assert_eq!(
            normalise_model_id("z-ai/glm-5.3"),
            "openrouter:z-ai/glm-5.3"
        );
        assert_eq!(normalise_model_id("local:llama"), "local:llama");
    }
}
