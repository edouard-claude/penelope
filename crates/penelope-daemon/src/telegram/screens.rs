//! Écrans de commandes cliquables (issue #30).
//!
//! ```text
//! /commande sans argument ─► écran : texte + un bouton par élément actionnable
//!   bouton « écran » ──────► redessine le message en place (pagination, détail, retour)
//!   bouton « opération » ──► effectue, toast, redessine l'écran d'origine
//!   bouton « risqué » ─────► écran de confirmation ─► Confirmer ─► opération
//!                                                  └► Annuler ──► écran d'origine
//!   paramètres déclarés ───► moteur de formulaires (un champ par écran, récapitulatif)
//! ```
//!
//! Les jetons de navigation sont réutilisables une semaine ; ceux des opérations servent
//! une fois, l'écran redessiné en porte de nouveaux.

use super::{
    TelegramGateway, cancelled_note, context_line, form_key, mcp_list_text, mcp_show_text,
    mcp_state_icon, recent_log_lines, routing_text, schedules_text, short_model, shown,
};
use crate::bus::Origin;
use penelope_kernel::api::method as m;
use penelope_telegram::actions::{Action, kind as k};
use penelope_telegram::api::inline_keyboard;
use penelope_telegram::markdown_to_html;
use penelope_telegram::render::ButtonSpec;
use serde_json::{Value, json};

mod memory;
mod ops;
mod perform;
mod sessions;
mod workflows;

/// Éléments par page avant pagination.
pub(super) const PER_PAGE: usize = 10;
const WEEK_MS: i64 = 7 * 24 * 3_600_000;
const DAY_MS: i64 = 24 * 3_600_000;
/// Au-delà, le texte d'un écran est coupé : un message Telegram tient en 4 096 caractères.
const SCREEN_CHARS: usize = 3_500;

/// Écran : texte Markdown et lignes de boutons.
#[derive(Default)]
pub(super) struct Screen {
    pub text: String,
    pub rows: Vec<Vec<ButtonSpec>>,
}

impl Screen {
    fn new(text: impl Into<String>) -> Screen {
        Screen {
            text: text.into(),
            rows: Vec::new(),
        }
    }
}

/// Résultat d'une opération d'écran.
struct Done {
    toast: String,
    /// Message envoyé en plus (journaux, bilan).
    note: Option<String>,
    /// Redessiner l'écran d'origine.
    redraw: bool,
}

impl Done {
    fn toast(t: impl Into<String>) -> Done {
        Done {
            toast: t.into(),
            note: None,
            redraw: true,
        }
    }
    fn note(t: impl Into<String>, note: impl Into<String>) -> Done {
        Done {
            toast: t.into(),
            note: Some(note.into()),
            redraw: true,
        }
    }
    fn quiet(t: impl Into<String>) -> Done {
        Done {
            toast: t.into(),
            note: None,
            redraw: false,
        }
    }
}

fn trunc(s: &str, n: usize) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    if one_line.chars().count() > n {
        format!("{}…", one_line.chars().take(n).collect::<String>())
    } else {
        one_line
    }
}

fn page_of(args: &Value) -> usize {
    args["page"].as_u64().unwrap_or(0) as usize
}

fn back_of(screen: &str, args: &Value) -> Value {
    json!({"screen": screen, "args": args})
}

fn run_icon(state: &str) -> &'static str {
    match state {
        "running" => "🏃",
        "paused" => "⏸",
        "blocked" => "⛔",
        "done" => "✅",
        "failed" => "❌",
        "cancelled" => "⏹",
        _ => "⚪",
    }
}

/// Schéma JSON des arguments d'un prompt MCP.
fn prompt_schema(prompt: &Value) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for a in prompt["arguments"].as_array().cloned().unwrap_or_default() {
        let Some(name) = a["name"].as_str() else {
            continue;
        };
        properties.insert(
            name.to_string(),
            json!({
                "type": "string",
                "title": a["title"].as_str().unwrap_or(name),
                "description": a["description"].as_str().unwrap_or_default(),
            }),
        );
        if a["required"].as_bool() == Some(true) {
            required.push(json!(name));
        }
    }
    json!({"type": "object", "properties": properties, "required": required})
}

impl TelegramGateway {
    // ------------------------------------------------------------ boutons

    async fn nav(&self, label: &str, screen: &str, args: Value) -> anyhow::Result<ButtonSpec> {
        let t = self
            .daemon
            .services
            .actions
            .create(k::SCREEN, screen, args, WEEK_MS, false)
            .await?;
        Ok(ButtonSpec::callback(label, &t.token, ""))
    }

    pub(super) async fn op(
        &self,
        label: &str,
        op: &str,
        params: Value,
        back: Value,
    ) -> anyhow::Result<ButtonSpec> {
        let t = self
            .daemon
            .services
            .actions
            .create(
                k::SCREEN_DO,
                op,
                json!({"params": params, "back": back}),
                DAY_MS,
                true,
            )
            .await?;
        Ok(ButtonSpec::callback(label, &t.token, ""))
    }

    /// Bouton d'une opération risquée : il ouvre d'abord l'écran de confirmation.
    async fn guarded(
        &self,
        label: &str,
        op: &str,
        params: Value,
        question: &str,
        back: Value,
    ) -> anyhow::Result<ButtonSpec> {
        self.nav(
            label,
            "confirm",
            json!({"op": op, "params": params, "question": question, "back": back}),
        )
        .await
    }

    pub(super) async fn command_button(
        &self,
        label: &str,
        command: &str,
    ) -> anyhow::Result<ButtonSpec> {
        self.command_button_with(label, command, "").await
    }

    /// Bouton qui lance une commande avec ses arguments (`mode`, `auto`).
    pub(super) async fn command_button_with(
        &self,
        label: &str,
        command: &str,
        args: &str,
    ) -> anyhow::Result<ButtonSpec> {
        let t = self
            .daemon
            .services
            .actions
            .create(
                k::RUN_COMMAND,
                command,
                json!({"args": args}),
                WEEK_MS,
                false,
            )
            .await?;
        Ok(ButtonSpec::callback(label, &t.token, ""))
    }

    /// « Précédent / Suivant » en conservant les autres paramètres de l'écran.
    async fn pager(
        &self,
        screen: &mut Screen,
        name: &str,
        args: &Value,
        total: usize,
    ) -> anyhow::Result<()> {
        let pages = total.div_ceil(PER_PAGE).max(1);
        if pages <= 1 {
            return Ok(());
        }
        let page = page_of(args).min(pages - 1);
        let with_page = |p: usize| {
            let mut a = args.clone();
            if !a.is_object() {
                a = json!({});
            }
            a["page"] = json!(p);
            a
        };
        let mut row = Vec::new();
        if page > 0 {
            row.push(self.nav("« Précédent", name, with_page(page - 1)).await?);
        }
        if page + 1 < pages {
            row.push(self.nav("Suivant »", name, with_page(page + 1)).await?);
        }
        screen
            .text
            .push_str(&format!("\n_Page {}/{pages}_", page + 1));
        screen.rows.push(row);
        Ok(())
    }

    // ------------------------------------------------------------ affichage

    /// Envoie un écran, ou le redessine à la place de `edit`.
    pub(super) async fn show_screen(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        name: &str,
        args: &Value,
        edit: Option<i64>,
    ) -> anyhow::Result<()> {
        // Écran construit dans une allocation à part : son futur est gros, et la commande qui
        // l'affiche l'embarquerait sinon sur la pile.
        let screen = match Box::pin(self.build_screen(chat_id, topic_id, name, args)).await {
            Ok(s) => s,
            Err(e) => Screen::new(format!("❌ {e}")),
        };
        self.send_screen(chat_id, topic_id, reply_to, screen, edit)
            .await
    }

    pub(super) async fn send_screen(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        screen: Screen,
        edit: Option<i64>,
    ) -> anyhow::Result<()> {
        let text = if screen.text.chars().count() > SCREEN_CHARS {
            format!(
                "{}\n…",
                screen.text.chars().take(SCREEN_CHARS).collect::<String>()
            )
        } else {
            screen.text
        };
        let html = markdown_to_html(&text);
        let rows: Vec<Vec<ButtonSpec>> =
            screen.rows.into_iter().filter(|r| !r.is_empty()).collect();
        let keyboard = (!rows.is_empty()).then(|| inline_keyboard(&rows));
        if let Some(message_id) = edit {
            match self
                .bot
                .edit_text(chat_id, message_id, &html, keyboard.clone())
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if e.to_string().contains("not modified") => return Ok(()),
                Err(_) => {}
            }
        }
        let mut payload = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
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

    /// Texte suivi d'un bouton qui copie une commande à compléter.
    pub(super) fn typed_screen(text: &str, label: &str, command: &str) -> Screen {
        Screen {
            text: text.to_string(),
            rows: vec![vec![ButtonSpec::copy_text(label, command)]],
        }
    }

    // ------------------------------------------------------------ clics

    pub(super) async fn screen_clicked(
        self: &std::sync::Arc<Self>,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        match action.action.as_str() {
            k::RUN_COMMAND => {
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                let args = action.args["args"].as_str().unwrap_or_default().to_string();
                Box::pin(self.command(chat_id, topic_id, message_id, &action.target, &args)).await
            }
            k::SCREEN => {
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                self.show_screen(
                    chat_id,
                    topic_id,
                    None,
                    &action.target,
                    &action.args,
                    Some(message_id),
                )
                .await
            }
            _ => {
                // Telegram invalide une `callback_query` en une dizaine de secondes : on
                // répond d'abord, on travaille ensuite, et l'issue arrive dans le chat
                // (issue #73).
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                let (me, target, params, back) = (
                    self.clone(),
                    action.target.clone(),
                    action.args["params"].clone(),
                    action.args["back"].clone(),
                );
                tokio::spawn(async move {
                    let done = match Box::pin(me.perform(chat_id, topic_id, &target, &params)).await
                    {
                        Ok(d) => d,
                        Err(e) => Done::quiet(format!("❌ {e}")),
                    };
                    // Le toast ne sert plus que d'accusé : ce qui compte est dit dans la
                    // conversation.
                    if let Some(note) = &done.note
                        && let Err(e) = me.reply(chat_id, topic_id, None, note).await
                    {
                        tracing::warn!(error = %e, "résultat d'un bouton non livré");
                    }
                    if done.redraw
                        && let Some(screen) = back["screen"].as_str()
                    {
                        if let Err(e) = me
                            .show_screen(
                                chat_id,
                                topic_id,
                                None,
                                screen,
                                &back["args"],
                                Some(message_id),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "écran non redessiné");
                        }
                    } else if !done.redraw {
                        let _ = me.bot.edit_markup(chat_id, message_id, None).await;
                        // Sans écran à redessiner, le toast seul se perdrait : il est
                        // aussi dit dans la conversation.
                        if done.note.is_none() && !done.toast.trim().is_empty() {
                            let _ = me.reply(chat_id, topic_id, None, &done.toast).await;
                        }
                    }
                });
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------ écrans

    /// Aiguillage des écrans : un bras, une fonction de même signature, rangée par famille
    /// dans screens/*.rs (lot G).
    pub(super) async fn build_screen(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        match name {
            "help" => self.screen_help(chat_id, topic_id, name, args).await,
            "confirm" => self.screen_confirm(chat_id, topic_id, name, args).await,
            "wf" => self.screen_wf(chat_id, topic_id, name, args).await,
            "wf.detail" => self.screen_wf_detail(chat_id, topic_id, name, args).await,
            "runs" => self.screen_runs(chat_id, topic_id, name, args).await,
            "run.detail" => self.screen_run_detail(chat_id, topic_id, name, args).await,
            "schedules" => self.screen_schedules(chat_id, topic_id, name, args).await,
            "mcp" => self.screen_mcp(chat_id, topic_id, name, args).await,
            "mcp.server" => self.screen_mcp_server(chat_id, topic_id, name, args).await,
            "models" => self.screen_models(chat_id, topic_id, name, args).await,
            "model.assign" => {
                self.screen_model_assign(chat_id, topic_id, name, args)
                    .await
            }
            "skills" => self.screen_skills(chat_id, topic_id, name, args).await,
            "skill" => self.screen_skill(chat_id, topic_id, name, args).await,
            "forget" => self.screen_forget(chat_id, topic_id, name, args).await,
            "learned" => self.screen_learned(chat_id, topic_id, name, args).await,
            "entry" => self.screen_entry(chat_id, topic_id, name, args).await,
            "practices" => self.screen_practices(chat_id, topic_id, name, args).await,
            "practice" => self.screen_practice(chat_id, topic_id, name, args).await,
            "intentions" => self.screen_intentions(chat_id, topic_id, name, args).await,
            "policies" => self.screen_policies(chat_id, topic_id, name, args).await,
            "status" => self.screen_status(chat_id, topic_id, name, args).await,
            "doctor" => self.screen_doctor(chat_id, topic_id, name, args).await,
            "config" => self.screen_config(chat_id, topic_id, name, args).await,
            "logs" => self.screen_logs(chat_id, topic_id, name, args).await,
            "quiet" => self.screen_quiet(chat_id, topic_id, name, args).await,
            "secrets" => self.screen_secrets(chat_id, topic_id, name, args).await,
            "upgrade" => self.screen_upgrade(chat_id, topic_id, name, args).await,
            "upgrade.switch" => {
                self.screen_upgrade_switch(chat_id, topic_id, name, args)
                    .await
            }
            "rewind" => self.screen_rewind(chat_id, topic_id, name, args).await,
            "forget.sessions" => {
                self.screen_forget_sessions(chat_id, topic_id, name, args)
                    .await
            }
            "prompts" => self.screen_prompts(chat_id, topic_id, name, args).await,
            other => {
                let screen: Screen = {
                    let _ = (chat_id, topic_id);
                    Screen::new(format!("Écran inconnu : `{other}`."))
                };
                Ok(screen)
            }
        }
    }

    /// `/start <charge>` d'un lien profond : une commande du catalogue, sinon un écran
    /// (`runs_stuck` : les runs en pause ou bloqués).
    pub(super) async fn open_deep_link(
        self: &std::sync::Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        payload: &str,
    ) -> anyhow::Result<()> {
        let payload = payload.trim();
        if payload != "start"
            && payload != "help"
            && penelope_telegram::commands::find(payload).is_some()
        {
            return Box::pin(self.command(chat_id, topic_id, message_id, payload, "")).await;
        }
        let (screen, args) = match payload {
            "runs_stuck" => ("runs", json!({"filter": "stuck"})),
            other => (other, json!({})),
        };
        self.show_screen(chat_id, topic_id, Some(message_id), screen, &args, None)
            .await
    }

    /// Rend un prompt MCP et l'envoie au modèle de la session, comme un message du
    /// propriétaire ; rend le nombre de messages du prompt.
    pub(super) async fn run_mcp_prompt(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        server: &str,
        prompt: &str,
        args: Value,
    ) -> anyhow::Result<usize> {
        let d = &self.daemon;
        let sup = d
            .hooks
            .mcp_supervisor()
            .ok_or_else(|| anyhow::anyhow!("aucun serveur MCP chargé"))?;
        let rendered = sup
            .get_prompt(server, prompt, args)
            .await
            .map_err(anyhow::Error::msg)?;
        let messages = rendered["messages"].as_array().cloned().unwrap_or_default();
        let text: Vec<String> = messages
            .iter()
            .filter_map(|m| {
                m["content"]["text"]
                    .as_str()
                    .or_else(|| m["content"].as_str())
                    .map(String::from)
            })
            .collect();
        if text.is_empty() {
            anyhow::bail!("le prompt `{prompt}` n'a rendu aucun texte");
        }
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        let session = d.chat_session_for(&origin).await?;
        d.enqueue_message(
            &session,
            &format!("[prompt MCP {server} · {prompt}]\n{}", text.join("\n\n")),
            &origin,
            None,
        )
        .await?;
        Ok(messages.len())
    }
}
