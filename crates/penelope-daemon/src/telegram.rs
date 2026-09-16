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
use penelope_telegram::actions::{ClickOutcome, kind as k};
use penelope_telegram::api::{BotTransport, HttpTransport, inline_keyboard, reaction};
use penelope_telegram::render::ButtonSpec;
use penelope_telegram::{Bot, Incoming, TgError, classify, html_to_plain, markdown_to_html};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Longueur d'un fragment Markdown avant conversion HTML : marge pour les balises.
const FRAGMENT_CHARS: usize = 3_500;
const MAX_ATTEMPTS: i64 = 6;

pub struct TelegramGateway {
    pub daemon: Arc<Daemon>,
    pub bot: Arc<Bot>,
    owner_id: i64,
    allow_groups: bool,
    draft_interval: Duration,
    poll_timeout_s: u64,
    outbox_wake: Notify,
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
    }

    /// Vérifie le jeton, publie les commandes, lance les boucles.
    pub async fn start(self: &Arc<Self>) -> Result<Vec<tokio::task::JoinHandle<()>>, String> {
        let me =
            self.bot.get_me().await.map_err(|e| {
                format!("jeton Telegram refusé ({e}) : vérifier `telegram_bot_token`")
            })?;
        tracing::info!(
            bot = me.get("username").and_then(|u| u.as_str()).unwrap_or("?"),
            "Telegram connecté"
        );
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
                // Une raison de refus était attendue : ce message la donne.
                let reason_key = format!("tg.await_reason.{chat_id}");
                if let Some(approval_id) = self.daemon.kv_get(&reason_key).await? {
                    if !approval_id.is_empty() {
                        self.daemon.kv_set(&reason_key, "").await?;
                        let d = Decision::deny("telegram", Some(text.clone()));
                        self.finalize_decision(&approval_id, &d, chat_id, topic_id)
                            .await?;
                        return Ok(());
                    }
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
                self.command(chat_id, topic_id, message_id, &command, &args)
                    .await?;
            }
            Incoming::Callback {
                callback_id,
                data,
                message_id,
                chat_id,
                from_id,
                ..
            } => {
                self.callback(&callback_id, &data, chat_id, message_id, from_id)
                    .await?;
            }
            Incoming::StoppedGeneration { draft_id, .. } => {
                if let Some(session) = self.daemon.bus.session_for_draft(draft_id) {
                    self.daemon.bus.cancel_session(&session);
                }
            }
            Incoming::Photo {
                chat_id,
                message_id,
                ..
            }
            | Incoming::Document {
                chat_id,
                message_id,
                ..
            }
            | Incoming::Voice {
                chat_id,
                message_id,
                ..
            } => {
                self.reply(
                    chat_id,
                    None,
                    Some(message_id),
                    "Les pièces jointes (photo, document, vocal) ne sont pas encore prises en \
                     charge. Envoie le contenu en texte, ou dépose le fichier dans le workspace.",
                )
                .await?;
            }
            Incoming::OAuthCallback { chat_id, .. } => {
                self.reply(
                    chat_id,
                    None,
                    None,
                    "URL d'autorisation reçue, mais le flux OAuth MCP n'est pas encore branché \
                     dans ce daemon.",
                )
                .await?;
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
            "start" | "help" => penelope_telegram::commands::help_text(),
            "new" => {
                if let Some(old) = s.sessions.find_by_topic(chat_id, topic_id).await? {
                    s.sessions.set_state(old.id.as_str(), "closed").await?;
                }
                let title = (!args.is_empty()).then(|| args.to_string());
                let sess = s
                    .sessions
                    .create(penelope_kernel::session::SessionKind::Chat, title)
                    .await?;
                s.sessions
                    .bind_telegram(sess.id.as_str(), chat_id, topic_id)
                    .await?;
                format!("🆕 Nouvelle session `{}`.", sess.id)
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
                    "Usage : `/switch <identifiant de session>`".into()
                } else if s.sessions.get(args).await?.is_none() {
                    format!("Session `{args}` introuvable.")
                } else {
                    s.sessions.bind_telegram(args, chat_id, topic_id).await?;
                    s.sessions.touch(args).await?;
                    format!("↪️ Session `{args}` reprise.")
                }
            }
            "model" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if let ["auto", switch] = parts.as_slice() {
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
                                "📌 Routage fixe : tous les messages passent par `main`.".into()
                            }
                        }
                    }
                } else if parts.len() < 2 {
                    let v = rpc
                        .call(m::MODEL_LIST, json!({}))
                        .await
                        .unwrap_or(json!({}));
                    let mut t = routing_text(&v);
                    t.push_str("\nChanger : `/model main openrouter:<identifiant>`");
                    t
                } else {
                    let model = normalise_model_id(parts[1]);
                    match rpc
                        .call(m::MODEL_SET, json!({"alias": parts[0], "model": model}))
                        .await
                    {
                        Ok(v) => format!(
                            "✅ `{}` → `{model}` (génération {}).",
                            parts[0], v["generation"]
                        ),
                        Err(e) => format!("❌ {e}"),
                    }
                }
            }
            "models" => {
                let v = rpc
                    .call(m::MODEL_LIST, json!({"filter": args}))
                    .await
                    .unwrap_or(json!({}));
                let mut t = routing_text(&v);
                let models = v["models"].as_array().cloned().unwrap_or_default();
                if !models.is_empty() {
                    t.push_str(&format!(
                        "\n**Catalogue** ({} résultat(s))\n\n",
                        models.len()
                    ));
                    for x in models.iter().take(30) {
                        t.push_str(&format!(
                            "- `{}` · {} $/M en entrée\n",
                            x["id"].as_str().unwrap_or("?"),
                            x["usd_per_m_in"]
                        ));
                    }
                } else {
                    t.push_str(&format!(
                        "\n{} modèle(s) au catalogue. Chercher : `/models glm`",
                        v["catalog_size"]
                    ));
                }
                t
            }
            "budget" => {
                let session = d.chat_session_for(&origin).await?;
                self.budget_text(&session, args).await?
            }
            "retiens" => {
                if args.is_empty() {
                    "Usage : `/retiens <ce qu'il faut retenir>`".into()
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
            "oublie" | "forget" => {
                if args.is_empty() {
                    "Usage : `/oublie <uid ou mots-clés>`".into()
                } else if s.memory.get(args).await?.is_some() {
                    let vault = crate::conversation::vault_dir(s);
                    match crate::vault_ops::forget(s, &vault, args).await {
                        Ok(true) => "🗑 Oublié.".into(),
                        Ok(false) => "Rien à oublier.".into(),
                        Err(e) => format!("❌ {e}"),
                    }
                } else {
                    let v = rpc.call(m::MEM_SEARCH, json!({"query": args})).await?;
                    let mut t = String::from("Entrées trouvées (répondre `/oublie <uid>`) :\n\n");
                    for h in v.as_array().cloned().unwrap_or_default().iter().take(10) {
                        t.push_str(&format!(
                            "- `{}` {}\n",
                            h["uid"].as_str().unwrap_or("?"),
                            h["text"].as_str().unwrap_or("")
                        ));
                    }
                    t
                }
            }
            "secret" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                match parts.first().copied() {
                    None | Some("list") => {
                        render_value(&rpc.call(m::SECRET_LIST, json!({})).await?)
                    }
                    Some("rm") if parts.len() > 1 => {
                        rpc.call(m::SECRET_RM, json!({"name": parts[1]})).await?;
                        format!("🗑 Secret `{}` supprimé.", parts[1])
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
            "recall" => render_value(&rpc.call(m::MEM_SEARCH, json!({"query": args})).await?),
            "resume" => render_value(
                &rpc.call(m::WF_CONTROL, json!({"run": args, "op": "resume"}))
                    .await?,
            ),
            other => {
                // Commandes du catalogue sans traitement dédié : appel RPC générique.
                let cmd = penelope_telegram::commands::all()
                    .into_iter()
                    .find(|c| c.name == other);
                match cmd {
                    None => format!("Commande inconnue : `/{other}`. Voir `/help`."),
                    Some(c) => {
                        let params = generic_params(c.rpc, args);
                        match rpc.call(c.rpc, params).await {
                            Ok(v) => render_value(&v),
                            Err(e) if e.to_string().contains("méthode inconnue") => {
                                format!("`/{other}` n'est pas encore branchée dans ce daemon.")
                            }
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/budget` : dépense du jour et de la session, requêtes les plus chères.
    /// `/budget sessions|requêtes|modèles|jours` : un regroupement précis.
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
        message_id: i64,
        from_id: i64,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let outcome = s.actions.click(data, from_id).await?;
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
        let note = match (first, a.state) {
            (false, st) => format!(
                "ℹ️ Déjà tranché : {} via {}.",
                st.as_str(),
                a.decided_via.clone().unwrap_or_default()
            ),
            (true, ApprovalState::Approved) => format!("✅ {} : `{}`.", decision.choice, a.subject),
            (true, _) => format!("❌ Refusé : `{}`.", a.subject),
        };
        self.reply(chat_id, topic_id, None, &note).await?;

        if first {
            if let Some(session) = &a.session_id {
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: None,
                };
                self.daemon
                    .enqueue_resume(session, approval_id, &origin)
                    .await?;
            }
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
        let s = &self.daemon.services;
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
        }
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
            match &ev.kind {
                BusKind::Started => {
                    drafts.insert(
                        ev.turn_id.clone(),
                        Draft {
                            chat_id,
                            topic_id,
                            draft_id: crate::bus::draft_id_for(&ev.turn_id),
                            text: String::new(),
                            last: Instant::now() - self.draft_interval,
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
    async fn deliver(
        &self,
        _turn_id: &str,
        _session_id: &str,
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
                TurnOutcome::LoopAborted { report } => {
                    let r: String = report.chars().take(2_500).collect();
                    self.reply(
                        chat_id,
                        topic_id,
                        message_id,
                        &format!("⛔ **Boucle détectée**, tour arrêté.\n\n```\n{r}\n```"),
                    )
                    .await?;
                }
                TurnOutcome::Cancelled => {
                    self.reply(chat_id, topic_id, None, "⏹ Génération arrêtée.")
                        .await?;
                }
                TurnOutcome::BudgetExceeded { scope } => {
                    self.reply(
                        chat_id,
                        topic_id,
                        message_id,
                        &format!(
                            "💸 Budget `{scope}` atteint : tour suspendu. Relever le plafond, \
                             par exemple `penelope config set budget.daily_usd 30`."
                        ),
                    )
                    .await?;
                }
                TurnOutcome::Failed { error } => {
                    self.reply(chat_id, topic_id, message_id, &format!("❌ {error}"))
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
}

/// `openrouter:` est implicite : `/model main z-ai/glm-5.3` suffit.
fn normalise_model_id(raw: &str) -> String {
    if raw.contains(':') {
        raw.to_string()
    } else {
        format!("openrouter:{raw}")
    }
}

/// Paramètres RPC d'une commande générique : l'argument libre va dans le champ usuel.
fn generic_params(method: &str, args: &str) -> Value {
    if args.is_empty() {
        return json!({});
    }
    match method {
        m::MEM_SEARCH => json!({"query": args}),
        m::SESSION_EXPORT => json!({"session": args}),
        m::WF_SHOW => json!({"id": args}),
        m::WF_TRACE => json!({"run": args}),
        m::SKILL_SHOW | m::SKILL_ROLLBACK => json!({"name": args}),
        m::INTENT_CANCEL | m::POLICY_REVOKE => json!({"id": args}),
        m::MODEL_LIST => json!({"filter": args}),
        m::USAGE => json!({"by": args}),
        _ => json!({"arg": args}),
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

/// Alias et routage, tels que `model.list` les décrit.
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

    fn texts(calls: &[Value]) -> Vec<String> {
        calls
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
            .collect()
    }

    #[tokio::test]
    async fn a_text_message_gets_an_html_answer_as_a_reply() {
        let (_d, g, t, p) = gateway().await;
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

    #[tokio::test]
    async fn the_same_update_is_processed_only_once() {
        let (_d, g, t, p) = gateway().await;
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
        assert!(out[2].contains("Fixe : tout passe par"), "{out:?}");
        assert!(!g.daemon.services.config.config().models.routing.classifier);
        assert!(out[3].contains("0,0123 $"), "{out:?}");
        assert!(out[3].contains("t_x"), "{out:?}");
        assert!(out[4].contains(&sid), "{out:?}");
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
