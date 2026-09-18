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
    TelegramGateway, form_key, mcp_list_text, mcp_show_text, routing_text, schedules_text,
    short_model,
};
use crate::bus::Origin;
use penelope_kernel::api::method as m;
use penelope_telegram::actions::{Action, kind as k};
use penelope_telegram::api::inline_keyboard;
use penelope_telegram::markdown_to_html;
use penelope_telegram::render::ButtonSpec;
use serde_json::{Value, json};

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

/// Schéma JSON des paramètres d'un workflow, pour le moteur de formulaires.
pub(super) fn parameters_schema(params: &[penelope_workflow::model::Parameter]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for p in params {
        let kind = match p.kind.as_str() {
            "number" | "float" => "number",
            "integer" | "int" => "integer",
            "boolean" | "bool" => "boolean",
            _ => "string",
        };
        let mut field = json!({
            "type": kind,
            "title": if p.label.is_empty() { p.id.clone() } else { p.label.clone() },
            "description": p.description,
        });
        if let Some(d) = &p.default {
            field["default"] = d.clone();
        }
        properties.insert(p.id.clone(), field);
        if p.required {
            required.push(json!(p.id));
        }
    }
    json!({"type": "object", "properties": properties, "required": required})
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

    async fn command_button(&self, label: &str, command: &str) -> anyhow::Result<ButtonSpec> {
        let t = self
            .daemon
            .services
            .actions
            .create(k::RUN_COMMAND, command, json!({}), WEEK_MS, false)
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
                Box::pin(self.command(chat_id, topic_id, message_id, &action.target, "")).await
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
                let params = action.args["params"].clone();
                let done = match Box::pin(self.perform(chat_id, topic_id, &action.target, &params))
                    .await
                {
                    Ok(d) => d,
                    Err(e) => Done::quiet(format!("❌ {e}")),
                };
                let toast: String = done.toast.chars().take(190).collect();
                let _ = self
                    .bot
                    .answer_callback(callback_id, Some(&toast), false)
                    .await;
                if let Some(note) = &done.note {
                    self.reply(chat_id, topic_id, None, note).await?;
                }
                let back = &action.args["back"];
                if done.redraw
                    && let Some(screen) = back["screen"].as_str()
                {
                    self.show_screen(
                        chat_id,
                        topic_id,
                        None,
                        screen,
                        &back["args"],
                        Some(message_id),
                    )
                    .await?;
                } else if !done.redraw {
                    let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                }
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------ écrans

    pub(super) async fn build_screen(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let rpc = crate::rpc::Rpc::new(d.clone());
        let here = back_of(name, args);
        Ok(match name {
            "help" => {
                let all = penelope_telegram::commands::all();
                match args["cat"].as_str() {
                    None => {
                        let mut sc = Screen::new(
                            "**Commandes**\nChoisis une famille : chaque bouton exécute la \
                             commande, ou ouvre son menu quand elle attend un argument.",
                        );
                        let mut row = Vec::new();
                        for cat in penelope_telegram::commands::categories() {
                            let n = all.iter().filter(|c| c.category == cat).count();
                            row.push(
                                self.nav(&format!("📂 {cat} ({n})"), "help", json!({"cat": cat}))
                                    .await?,
                            );
                            if row.len() == 2 {
                                sc.rows.push(std::mem::take(&mut row));
                            }
                        }
                        sc.rows.push(row);
                        sc
                    }
                    Some(cat) => {
                        let mut sc = Screen::new(format!("**{cat}**\n"));
                        let mut row = Vec::new();
                        for c in all.iter().filter(|c| c.category == cat) {
                            sc.text
                                .push_str(&format!("\n- /{} : {}", c.name, c.description));
                            row.push(self.command_button(&format!("/{}", c.name), c.name).await?);
                            if row.len() == 3 {
                                sc.rows.push(std::mem::take(&mut row));
                            }
                        }
                        sc.rows.push(row);
                        sc.rows
                            .push(vec![self.nav("« Familles", "help", json!({})).await?]);
                        sc
                    }
                }
            }

            "confirm" => {
                let back = args["back"].clone();
                let mut sc = Screen::new(format!(
                    "⚠️ {}",
                    args["question"].as_str().unwrap_or("Confirmer ?")
                ));
                let op = args["op"].as_str().unwrap_or_default();
                let cancel = match back["screen"].as_str() {
                    Some(screen) => self.nav("✖️ Annuler", screen, back["args"].clone()).await?,
                    // Sans écran d'origine : « Annuler » retire simplement les boutons.
                    None => {
                        self.op("✖️ Annuler", "noop", json!({}), Value::Null)
                            .await?
                    }
                };
                sc.rows.push(vec![
                    self.op("✅ Confirmer", op, args["params"].clone(), back.clone())
                        .await?,
                    cancel,
                ]);
                sc
            }

            "wf" => {
                let list = rpc.call(m::WF_LIST, json!({})).await?;
                let list = list.as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return Ok(Screen::new("Aucun workflow enregistré."));
                }
                let page = page_of(args);
                let mut sc = Screen::new(format!(
                    "**Workflows** ({})\n▶️ lance (les paramètres sont demandés un par un), ℹ️ \
                     détaille les étapes.\n",
                    list.len()
                ));
                for w in list.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    let id = w["id"].as_str().unwrap_or("?");
                    let name = w["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
                    sc.text.push_str(&format!(
                        "\n- **{name}** (`{id}`) · {} étape(s){}",
                        w["steps"],
                        if w["runsHere"].as_bool() == Some(false) {
                            " · pas sur cette machine"
                        } else {
                            ""
                        }
                    ));
                    sc.rows.push(vec![
                        self.op(
                            &format!("▶️ {}", trunc(name, 32)),
                            "wf.run",
                            json!({"id": id}),
                            here.clone(),
                        )
                        .await?,
                        self.nav("ℹ️", "wf.detail", json!({"id": id})).await?,
                    ]);
                }
                self.pager(&mut sc, name, args, list.len()).await?;
                sc
            }

            "wf.detail" => {
                let id = args["id"].as_str().unwrap_or_default();
                let entry = s
                    .workflows
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("workflow `{id}` introuvable"))?;
                let w = &entry;
                let mut t = format!(
                    "**{}** (`{id}`)\n{}\n",
                    if w.metadata.name.is_empty() {
                        id
                    } else {
                        &w.metadata.name
                    },
                    w.metadata.description
                );
                if !w.metadata.parameters.is_empty() {
                    t.push_str("\n**Paramètres**\n");
                    for p in &w.metadata.parameters {
                        t.push_str(&format!(
                            "- {} (`{}`){}{}\n",
                            if p.label.is_empty() { &p.id } else { &p.label },
                            p.id,
                            if p.required { " *" } else { "" },
                            if p.description.is_empty() {
                                String::new()
                            } else {
                                format!(" : {}", trunc(&p.description, 120))
                            }
                        ));
                    }
                }
                let steps: Vec<&str> = w.steps.iter().map(|st| st.id.as_str()).collect();
                t.push_str(&format!(
                    "\n**Étapes** ({}) : {}",
                    steps.len(),
                    trunc(&steps.join(" → "), 600)
                ));
                let mut sc = Screen::new(t);
                sc.rows.push(vec![
                    self.op("▶️ Lancer", "wf.run", json!({"id": id}), here.clone())
                        .await?,
                    self.nav("« Workflows", "wf", json!({})).await?,
                ]);
                sc
            }

            "runs" => {
                let stuck = args["filter"].as_str() == Some("stuck");
                let runs: Vec<_> = s
                    .runs
                    .list(None, 50)
                    .await?
                    .into_iter()
                    .filter(|r| {
                        !stuck
                            || matches!(
                                r.state,
                                penelope_workflow::RunState::Paused
                                    | penelope_workflow::RunState::Blocked
                            )
                    })
                    .collect();
                let mut sc = Screen::new(if stuck {
                    format!("**Runs en pause ou bloqués** ({})\n", runs.len())
                } else {
                    format!("**Runs** ({})\n", runs.len())
                });
                if runs.is_empty() {
                    sc.text.push_str(if stuck {
                        "\nAucun run à reprendre."
                    } else {
                        "\nAucun run."
                    });
                }
                let page = page_of(args);
                for r in runs.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    let state = r.state.as_str();
                    sc.text.push_str(&format!(
                        "\n{} **{}** · {state}{} · `{}`",
                        run_icon(state),
                        r.workflow_id,
                        r.current_step
                            .as_deref()
                            .map(|st| format!(" · étape `{st}`"))
                            .unwrap_or_default(),
                        r.id
                    ));
                    let mut row = vec![
                        self.nav(
                            &format!("🔎 {}", trunc(&r.workflow_id, 24)),
                            "run.detail",
                            json!({"run": r.id}),
                        )
                        .await?,
                    ];
                    let question = format!("Arrêter le run `{}` ({}) ?", r.id, r.workflow_id);
                    let stop = |label: &'static str| {
                        self.guarded(
                            label,
                            "run.cancel",
                            json!({"run": r.id}),
                            &question,
                            here.clone(),
                        )
                    };
                    match r.state {
                        penelope_workflow::RunState::Running => {
                            row.push(
                                self.op("⏸", "run.pause", json!({"run": r.id}), here.clone())
                                    .await?,
                            );
                            row.push(stop("⏹").await?);
                        }
                        penelope_workflow::RunState::Paused
                        | penelope_workflow::RunState::Blocked => {
                            row.push(
                                self.op("▶️", "run.resume", json!({"run": r.id}), here.clone())
                                    .await?,
                            );
                            row.push(stop("⏹").await?);
                        }
                        _ => {}
                    }
                    sc.rows.push(row);
                }
                self.pager(&mut sc, name, args, runs.len()).await?;
                if stuck {
                    sc.rows
                        .push(vec![self.nav("Tous les runs", "runs", json!({})).await?]);
                }
                sc
            }

            "run.detail" => {
                let id = args["run"].as_str().unwrap_or_default();
                let r = s
                    .runs
                    .get(id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("run `{id}` introuvable"))?;
                let state = r.state.as_str();
                let mut t = format!(
                    "{} **{}** · {state}\n`{}`\nÉtape : `{}` · itérations {}/{} · {:.2} $\nDémarré : {}",
                    run_icon(state),
                    r.workflow_id,
                    r.id,
                    r.current_step.as_deref().unwrap_or("-"),
                    r.iterations,
                    r.max_iterations,
                    r.spent_usd,
                    r.started_at.get(..16).unwrap_or(&r.started_at)
                );
                if r.params.as_object().is_some_and(|o| !o.is_empty()) {
                    t.push_str(&format!(
                        "\nParamètres : `{}`",
                        trunc(&r.params.to_string(), 300)
                    ));
                }
                if let Some(e) = &r.error {
                    t.push_str(&format!("\nErreur : {}", trunc(e, 400)));
                }
                if let Some(res) = &r.result {
                    t.push_str(&format!("\nRésultat : {}", trunc(res, 400)));
                }
                let mut sc = Screen::new(t);
                let mut row = Vec::new();
                match r.state {
                    penelope_workflow::RunState::Running => {
                        row.push(
                            self.op("⏸ Pause", "run.pause", json!({"run": id}), here.clone())
                                .await?,
                        );
                    }
                    penelope_workflow::RunState::Paused | penelope_workflow::RunState::Blocked => {
                        row.push(
                            self.op(
                                "▶️ Reprendre",
                                "run.resume",
                                json!({"run": id}),
                                here.clone(),
                            )
                            .await?,
                        );
                    }
                    _ => {}
                }
                if matches!(
                    r.state,
                    penelope_workflow::RunState::Running
                        | penelope_workflow::RunState::Paused
                        | penelope_workflow::RunState::Blocked
                ) {
                    row.push(
                        self.guarded(
                            "⏹ Arrêter",
                            "run.cancel",
                            json!({"run": id}),
                            &format!("Arrêter le run `{id}` ?"),
                            here.clone(),
                        )
                        .await?,
                    );
                }
                sc.rows.push(row);
                sc.rows
                    .push(vec![self.nav("« Runs", "runs", json!({})).await?]);
                sc
            }

            "schedules" => {
                let v = rpc.call(m::SCHEDULE_LIST, json!({})).await?;
                let list = v.as_array().cloned().unwrap_or_default();
                let mut sc = Screen::new(schedules_text(&v));
                if !list.is_empty() {
                    sc.text.push_str(
                        "\n⚡ déclenche maintenant, ⏸/▶️ suspend ou reprend, 🗑 supprime.",
                    );
                }
                let page = page_of(args);
                for sched in list.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    let id = sched["id"].as_str().unwrap_or("?");
                    let label = format!(
                        "{} {}",
                        sched["kind"].as_str().unwrap_or("?"),
                        &id[id.len().saturating_sub(6)..]
                    );
                    let paused = sched["state"].as_str() == Some("paused");
                    sc.rows.push(vec![
                        self.op(
                            &format!("⚡ {label}"),
                            "schedule.run",
                            json!({"id": id}),
                            here.clone(),
                        )
                        .await?,
                        if paused {
                            self.op("▶️", "schedule.resume", json!({"id": id}), here.clone())
                                .await?
                        } else {
                            self.op("⏸", "schedule.pause", json!({"id": id}), here.clone())
                                .await?
                        },
                        self.guarded(
                            "🗑",
                            "schedule.rm",
                            json!({"id": id}),
                            &format!("Supprimer le déclencheur `{id}` ?"),
                            here.clone(),
                        )
                        .await?,
                    ]);
                }
                self.pager(&mut sc, name, args, list.len()).await?;
                sc
            }

            "mcp" => {
                let v = rpc.call(m::MCP_LIST, json!({})).await?;
                let mut sc = Screen::new(mcp_list_text(&v));
                for srv in v["servers"].as_array().cloned().unwrap_or_default() {
                    let server = srv["name"].as_str().unwrap_or("?");
                    let state = srv["state"].as_str().unwrap_or("?");
                    sc.rows.push(vec![
                        self.nav(
                            &format!("{} {}", super::mcp_state_icon(state), trunc(server, 24)),
                            "mcp.server",
                            json!({"name": server}),
                        )
                        .await?,
                        self.op("🔄", "mcp.restart", json!({"name": server}), here.clone())
                            .await?,
                        self.op("🧪", "mcp.test", json!({"name": server}), here.clone())
                            .await?,
                    ]);
                }
                sc
            }

            "mcp.server" => {
                let server = args["name"].as_str().unwrap_or_default();
                let v = rpc.call(m::MCP_SHOW, json!({"name": server})).await?;
                let mut sc = Screen::new(mcp_show_text(&v));
                let p = json!({"name": server});
                sc.rows.push(vec![
                    self.op("🔄 Redémarrer", "mcp.restart", p.clone(), here.clone())
                        .await?,
                    self.op("🧪 Tester", "mcp.test", p.clone(), here.clone())
                        .await?,
                ]);
                let enabled = v["config"]["enabled"].as_bool().unwrap_or(true);
                sc.rows.push(vec![
                    self.op("📜 Journal", "mcp.logs", p.clone(), here.clone())
                        .await?,
                    if enabled {
                        self.guarded(
                            "⏻ Désactiver",
                            "mcp.disable",
                            p.clone(),
                            &format!("Désactiver le serveur `{server}` ?"),
                            here.clone(),
                        )
                        .await?
                    } else {
                        self.op("⏻ Activer", "mcp.enable", p.clone(), here.clone())
                            .await?
                    },
                ]);
                if v["status"]["state"].as_str() == Some("auth_required") {
                    sc.rows.push(vec![
                        self.op("🔐 Autoriser", "mcp.auth", p.clone(), here.clone())
                            .await?,
                    ]);
                }
                sc.rows
                    .push(vec![self.nav("« Serveurs", "mcp", json!({})).await?]);
                sc
            }

            "models" => {
                let filter = args["filter"].as_str().unwrap_or_default();
                let v = rpc.call(m::MODEL_LIST, json!({"filter": filter})).await?;
                let models = v["models"].as_array().cloned().unwrap_or_default();
                let mut t = routing_text(&v);
                t.push_str(&if models.is_empty() && filter.is_empty() {
                    format!("\n{} modèle(s) au catalogue.", v["catalog_size"])
                } else if filter.is_empty() {
                    format!(
                        "\n**Catalogue** ({}) : un modèle pour l'affecter à un alias.",
                        models.len()
                    )
                } else {
                    format!(
                        "\n**Catalogue « {filter} »** ({}) : un modèle pour l'affecter à un alias.",
                        models.len()
                    )
                });
                let mut sc = Screen::new(t);
                let page = page_of(args);
                for x in models.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    let id = x["id"].as_str().unwrap_or("?");
                    let mut back_args = args.clone();
                    if !back_args.is_object() {
                        back_args = json!({});
                    }
                    sc.rows.push(vec![
                        self.nav(
                            &format!(
                                "{} · {} $/M",
                                trunc(&short_model(id), 36),
                                x["usd_per_m_in"]
                            ),
                            "model.assign",
                            json!({"model": id, "back": back_of("models", &back_args)}),
                        )
                        .await?,
                    ]);
                }
                self.pager(&mut sc, name, args, models.len()).await?;
                let mut last = vec![ButtonSpec::copy_text("🔎 Chercher", "/models ")];
                if !filter.is_empty() {
                    last.push(self.nav("Tout le catalogue", "models", json!({})).await?);
                }
                sc.rows.push(last);
                sc
            }

            "model.assign" => {
                let model = args["model"].as_str().unwrap_or_default();
                let back = if args["back"].is_object() {
                    args["back"].clone()
                } else {
                    back_of("models", &json!({}))
                };
                let v = rpc.call(m::MODEL_LIST, json!({"filter": "\u{0}"})).await?;
                let mut sc = Screen::new(format!(
                    "Affecter `{model}` à quel alias ?\nL'alias change pour toutes les sessions \
                     qui l'utilisent."
                ));
                let mut row = Vec::new();
                for a in v["aliases"].as_array().cloned().unwrap_or_default() {
                    let alias = a["alias"].as_str().unwrap_or("?");
                    row.push(
                        self.op(
                            alias,
                            "model.assign",
                            json!({"alias": alias, "model": model}),
                            back.clone(),
                        )
                        .await?,
                    );
                    if row.len() == 3 {
                        sc.rows.push(std::mem::take(&mut row));
                    }
                }
                sc.rows.push(row);
                sc.rows.push(vec![
                    self.nav(
                        "« Retour",
                        back["screen"].as_str().unwrap_or("models"),
                        back["args"].clone(),
                    )
                    .await?,
                ]);
                sc
            }

            "skills" => {
                let all = s.skills.all();
                if all.is_empty() {
                    return Ok(Screen::new("Aucune skill chargée."));
                }
                let mut sc = Screen::new(format!(
                    "**Skills** ({})\n📖 affiche, ⏪ revient à la version précédente.\n",
                    all.len()
                ));
                let page = page_of(args);
                for sk in all.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    sc.text.push_str(&format!(
                        "\n- **{}** · {} · v{} : {}",
                        sk.name,
                        sk.scope.as_str(),
                        sk.version,
                        trunc(&sk.description, 90)
                    ));
                    let mut row = vec![
                        self.nav(
                            &format!("📖 {}", trunc(&sk.name, 28)),
                            "skill",
                            json!({"name": sk.name}),
                        )
                        .await?,
                    ];
                    if sk.scope == penelope_skills::Scope::User {
                        row.push(
                            self.guarded(
                                "⏪",
                                "skill.rollback",
                                json!({"name": sk.name}),
                                &format!("Revenir à la version précédente de `{}` ?", sk.name),
                                here.clone(),
                            )
                            .await?,
                        );
                    }
                    sc.rows.push(row);
                }
                self.pager(&mut sc, name, args, all.len()).await?;
                sc
            }

            "skill" => {
                let skill_name = args["name"].as_str().unwrap_or_default();
                let sk = s
                    .skills
                    .get(skill_name)
                    .ok_or_else(|| anyhow::anyhow!("skill `{skill_name}` introuvable"))?;
                let mut sc = Screen::new(format!(
                    "📖 **{}** · {} · v{}\n{}\n{}\n\n```\n{}\n```",
                    sk.name,
                    sk.scope.as_str(),
                    sk.version,
                    sk.description,
                    if sk.activation.is_empty() {
                        String::new()
                    } else {
                        format!("Déclencheurs : {}", sk.activation.join(", "))
                    },
                    trunc(&sk.body, 900).replace("```", "ʼʼʼ")
                ));
                let mut row = Vec::new();
                if sk.scope == penelope_skills::Scope::User {
                    row.push(
                        self.guarded(
                            "⏪ Version précédente",
                            "skill.rollback",
                            json!({"name": sk.name}),
                            &format!("Revenir à la version précédente de `{}` ?", sk.name),
                            here.clone(),
                        )
                        .await?,
                    );
                }
                row.push(self.nav("« Skills", "skills", json!({})).await?);
                sc.rows.push(row);
                sc
            }

            "forget" => {
                let query = args["query"].as_str().unwrap_or_default();
                let items: Vec<(String, String)> = if query.is_empty() {
                    let mut seen = std::collections::BTreeSet::new();
                    crate::dream::learned(s, 30)
                        .await?
                        .into_iter()
                        .filter_map(|i| {
                            let uid = i["uid"].as_str()?.to_string();
                            let text = i["text"].as_str()?.to_string();
                            seen.insert(uid.clone()).then_some((uid, text))
                        })
                        .take(PER_PAGE)
                        .collect()
                } else {
                    rpc.call(m::MEM_SEARCH, json!({"query": query}))
                        .await?
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|h| {
                            Some((
                                h["uid"].as_str()?.to_string(),
                                h["text"].as_str().unwrap_or_default().to_string(),
                            ))
                        })
                        .take(PER_PAGE)
                        .collect()
                };
                let mut sc = Screen::new(match (query.is_empty(), items.is_empty()) {
                    (true, true) => "Rien de récent à oublier : cherche une entrée.".to_string(),
                    (true, false) => {
                        "**Oublier** : entrées récentes, ou cherche une entrée.\n".to_string()
                    }
                    (false, true) => format!("Aucune entrée pour « {query} »."),
                    (false, false) => format!("**Oublier** : entrées pour « {query} ».\n"),
                });
                for (i, (uid, text)) in items.iter().enumerate() {
                    sc.text
                        .push_str(&format!("\n{}. {}", i + 1, trunc(text, 160)));
                    sc.rows.push(vec![
                        self.guarded(
                            &format!("🗑 {}. {}", i + 1, trunc(text, 36)),
                            "mem.forget",
                            json!({"uid": uid}),
                            &format!("Oublier « {} » ?", trunc(text, 200)),
                            here.clone(),
                        )
                        .await?,
                    ]);
                }
                sc.rows
                    .push(vec![ButtonSpec::copy_text("🔎 Chercher", "/oublie ")]);
                sc
            }

            "learned" => {
                let days = args["days"].as_i64().unwrap_or(7);
                let items = crate::dream::learned(s, days).await?;
                let mut sc = Screen::new(if items.is_empty() {
                    format!("Rien appris sur les {days} derniers jours.")
                } else {
                    format!("📚 **Appris sur {days} jours** ({})\n", items.len())
                });
                let page = page_of(args);
                for i in items.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    let text = i["text"].as_str().unwrap_or("(entrée retirée depuis)");
                    sc.text.push_str(&format!(
                        "\n- {} _({}, {})_",
                        trunc(text, 160),
                        i["file"].as_str().unwrap_or("?"),
                        i["ts"].as_str().and_then(|t| t.get(..10)).unwrap_or("")
                    ));
                    if let Some(uid) = i["uid"].as_str().filter(|_| i["text"].is_string()) {
                        sc.rows.push(vec![
                            self.nav(
                                &format!("🔎 {}", trunc(text, 40)),
                                "entry",
                                json!({"uid": uid, "back": here.clone()}),
                            )
                            .await?,
                        ]);
                    }
                }
                self.pager(&mut sc, name, args, items.len()).await?;
                let mut row = Vec::new();
                for n in [7, 30, 90] {
                    if n != days {
                        row.push(
                            self.nav(&format!("{n} jours"), "learned", json!({"days": n}))
                                .await?,
                        );
                    }
                }
                sc.rows.push(row);
                sc
            }

            "entry" => {
                let uid = args["uid"].as_str().unwrap_or_default();
                let back = if args["back"].is_object() {
                    args["back"].clone()
                } else {
                    back_of("learned", &json!({}))
                };
                let e = s
                    .memory
                    .get(uid)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("entrée `{uid}` introuvable"))?;
                let u = uid.to_string();
                let prov: Option<(String, Option<String>, String)> = s
                    .store
                    .read(move |c| {
                        let mut st = c.prepare(
                            "SELECT origin, source_ref, observed_at FROM mem_provenance WHERE uid = ?1",
                        )?;
                        let mut rows = st.query([&u])?;
                        Ok(match rows.next()? {
                            Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?)),
                            None => None,
                        })
                    })
                    .await?;
                let mut t = format!(
                    "**Entrée** `{uid}`\n{}\n\nFichier `{}` · niveau {} · statut {}",
                    e.text,
                    e.file,
                    e.level.as_str(),
                    e.statut
                );
                if let Some((origin, source, at)) = prov {
                    t.push_str(&format!(
                        "\nOrigine {origin}{} · {}",
                        source.map(|x| format!(" · `{x}`")).unwrap_or_default(),
                        at.get(..10).unwrap_or(&at)
                    ));
                }
                let mut sc = Screen::new(t);
                sc.rows.push(vec![
                    self.op(
                        "✅ Valider",
                        "mem.validate",
                        json!({"uid": uid}),
                        here.clone(),
                    )
                    .await?,
                    self.guarded(
                        "🚫 Rejeter",
                        "mem.forget",
                        json!({"uid": uid}),
                        &format!("Retirer « {} » de la mémoire ?", trunc(&e.text, 200)),
                        back.clone(),
                    )
                    .await?,
                ]);
                sc.rows.push(vec![
                    self.nav(
                        "« Retour",
                        back["screen"].as_str().unwrap_or("learned"),
                        back["args"].clone(),
                    )
                    .await?,
                ]);
                sc
            }

            "practices" => {
                let dir = crate::conversation::vault_dir(s).join("pratiques");
                let mut found: Vec<(String, penelope_memory::vault::Practice)> =
                    std::fs::read_dir(&dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter_map(|e| {
                            let name = e.file_name().to_string_lossy().to_string();
                            let slug = name.strip_suffix(".md")?.to_string();
                            let raw = std::fs::read_to_string(e.path()).ok()?;
                            let p = penelope_memory::vault::Practice::parse(&raw, &slug).ok()?;
                            Some((slug, p))
                        })
                        .collect();
                found.sort_by(|a, b| a.0.cmp(&b.0));
                let mut sc = Screen::new(if found.is_empty() {
                    "Aucune pratique dans le vault (`pratiques/`).".to_string()
                } else {
                    format!("📐 **Pratiques** ({})\n", found.len())
                });
                let page = page_of(args);
                for (slug, p) in found.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    sc.rows.push(vec![
                        self.nav(
                            &format!("📐 {} · {}", trunc(&p.title, 32), p.statut.as_str()),
                            "practice",
                            json!({"slug": slug}),
                        )
                        .await?,
                    ]);
                }
                self.pager(&mut sc, name, args, found.len()).await?;
                sc
            }

            "practice" => {
                let slug = args["slug"].as_str().unwrap_or_default();
                let path = crate::conversation::vault_dir(s).join(format!("pratiques/{slug}.md"));
                let p = std::fs::read_to_string(&path)
                    .map_err(|_| anyhow::anyhow!("pratique `{slug}` introuvable"))
                    .and_then(|raw| {
                        penelope_memory::vault::Practice::parse(&raw, slug)
                            .map_err(anyhow::Error::msg)
                    })?;
                let mut t = format!(
                    "📐 **{}** · confiance {:.1} · {}\n\nDéfaut : {}",
                    p.title,
                    p.confiance,
                    p.statut.as_str(),
                    p.default_entry
                        .as_ref()
                        .map(|e| e.text.as_str())
                        .unwrap_or("(aucun)")
                );
                for e in &p.exceptions {
                    t.push_str(&format!(
                        "\n- Exception : {} ({})",
                        e.text,
                        e.annotations
                            .quand
                            .as_ref()
                            .map(|q| q.render())
                            .unwrap_or_default()
                    ));
                }
                if !p.ecarts.is_empty() {
                    t.push_str(&format!("\n{} écart(s) observé(s).", p.ecarts.len()));
                }
                let mut sc = Screen::new(t);
                sc.rows.push(vec![
                    self.op(
                        "✅ Valider",
                        "practice.status",
                        json!({"slug": slug, "statut": "active"}),
                        here.clone(),
                    )
                    .await?,
                    self.op(
                        "🚫 Rejeter",
                        "practice.status",
                        json!({"slug": slug, "statut": "contestee"}),
                        here.clone(),
                    )
                    .await?,
                ]);
                sc.rows
                    .push(vec![self.nav("« Pratiques", "practices", json!({})).await?]);
                sc
            }

            "intentions" => {
                let armed: Vec<_> = s
                    .intents
                    .all()
                    .await?
                    .into_iter()
                    .filter(|i| i.etat == penelope_memory::intents::IntentState::Armee)
                    .collect();
                let mut sc = Screen::new(if armed.is_empty() {
                    "Aucune intention armée.".to_string()
                } else {
                    format!("🎯 **Intentions armées** ({})\n", armed.len())
                });
                for i in &armed {
                    sc.text.push_str(&format!(
                        "\n- {} · déclencheurs : {} · {}/{} tir(s)",
                        trunc(&i.texte, 140),
                        i.declencheurs.join(", "),
                        i.tirs,
                        i.budget_tirs
                    ));
                    sc.rows.push(vec![
                        self.op(
                            &format!("❌ {}", trunc(&i.texte, 40)),
                            "intent.cancel",
                            json!({"id": i.id}),
                            here.clone(),
                        )
                        .await?,
                    ]);
                }
                sc
            }

            "policies" => {
                let rules = s.policies.active_rules().await?;
                let mut sc = Screen::new(if rules.is_empty() {
                    "Aucune règle d'autorisation active.".to_string()
                } else {
                    format!("🛡 **Règles d'autorisation** ({})\n", rules.len())
                });
                for r in &rules {
                    let label = r
                        .tool
                        .clone()
                        .or_else(|| r.server.clone().map(|x| format!("serveur {x}")))
                        .unwrap_or_else(|| "toutes les actions".into());
                    // Portée réelle de la règle : « command commence par cargo test »
                    // (issue #67).
                    let scope = r
                        .arg_match
                        .as_ref()
                        .map(|p| format!(" · {}", penelope_hitl::policy::describe_pattern(p)))
                        .unwrap_or_default();
                    sc.text.push_str(&format!(
                        "\n- `{label}`{scope} · {:?} · {:?} · {} usage(s)",
                        r.decision, r.window, r.hits
                    ));
                    sc.rows.push(vec![
                        self.guarded(
                            &format!("🗑 {}", trunc(&label, 40)),
                            "policy.revoke",
                            json!({"id": r.id}),
                            &format!("Retirer la règle « {label} » ?"),
                            here.clone(),
                        )
                        .await?,
                    ]);
                }
                sc
            }

            "status" => {
                let st = d.status().await?;
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: None,
                };
                let session_line = match d.chat_session_for(&origin).await {
                    Ok(sid) => {
                        let cfg = s.config.config();
                        let (_, limit, _) = s.budget.limits(&cfg.budget, Some(&sid), None).await?;
                        let view = crate::compaction::context_view(s, &sid, None).await?;
                        format!(
                            "\n- Session : {:.2} $ sur {:.2} $\n- {}",
                            s.budget.spent_session(&sid).await?,
                            limit,
                            super::context_line(&view)
                        )
                    }
                    Err(_) => String::new(),
                };
                let up = st.uptime_s;
                let mut sc = Screen::new(format!(
                    "**Pénélope** · version {} · en route depuis {} h {:02} min\n\
                     - Sessions actives : {}\n- Tours en file : {}\n- Runs actifs : {}\n\
                     - Demandes en attente : {}\n- Serveurs MCP prêts : {}/{}\n\
                     - Dépense du jour : {:.2} $\n- Mémoire : {:.0} Mo\n- Telegram : {}",
                    st.version,
                    up / 3600,
                    (up % 3600) / 60,
                    st.sessions_active,
                    st.turns_queued,
                    st.runs_active,
                    st.approvals_pending,
                    st.mcp_ready,
                    st.mcp_total,
                    st.spent_today_usd,
                    st.rss_mb,
                    st.telegram
                ));
                sc.text.push_str(&session_line);
                let mut row = Vec::new();
                if st.approvals_pending > 0 {
                    row.push(
                        self.command_button(
                            &format!("📋 Demandes ({})", st.approvals_pending),
                            "approvals",
                        )
                        .await?,
                    );
                }
                if st.mcp_ready < st.mcp_total {
                    row.push(
                        self.nav(
                            &format!("🔌 MCP ({}/{})", st.mcp_ready, st.mcp_total),
                            "mcp",
                            json!({}),
                        )
                        .await?,
                    );
                }
                if st.runs_active > 0 {
                    row.push(self.nav("🏃 Runs", "runs", json!({})).await?);
                }
                sc.rows.push(row);
                sc.rows.push(vec![
                    self.command_button("💶 Dépenses", "usage").await?,
                    self.nav("🩺 Diagnostic", "doctor", json!({})).await?,
                ]);
                sc
            }

            "doctor" => {
                let checks = rpc.call(m::DOCTOR, json!({})).await?;
                let checks = checks.as_array().cloned().unwrap_or_default();
                let failed: Vec<&Value> = checks
                    .iter()
                    .filter(|c| c["ok"].as_bool() == Some(false))
                    .collect();
                let mut t = if failed.is_empty() {
                    format!(
                        "🩺 **Diagnostic** : {} contrôle(s), tout va bien.",
                        checks.len()
                    )
                } else {
                    format!(
                        "🩺 **Diagnostic** : {} alerte(s) sur {} contrôle(s)\n",
                        failed.len(),
                        checks.len()
                    )
                };
                let mut targets: Vec<(&str, &str)> = Vec::new();
                for c in &failed {
                    let id = c["id"].as_str().unwrap_or_default();
                    t.push_str(&format!(
                        "\n⚠️ **{}** : {}",
                        c["label"].as_str().unwrap_or(id),
                        trunc(c["detail"].as_str().unwrap_or_default(), 300)
                    ));
                    if let Some(fix) = c["fix"].as_str() {
                        t.push_str(&format!("\n  ↳ `{}`", trunc(fix, 160)));
                    }
                    let target = if id.starts_with("mcp") {
                        Some(("🔌 MCP", "mcp"))
                    } else if id.contains("budget") {
                        Some(("💶 Dépenses", "usage"))
                    } else if id.contains("embedding") || id.contains("model") {
                        Some(("🧠 Modèles", "models"))
                    } else {
                        None
                    };
                    if let Some(tg) = target
                        && !targets.contains(&tg)
                    {
                        targets.push(tg);
                    }
                }
                let ok: Vec<&str> = checks
                    .iter()
                    .filter(|c| c["ok"].as_bool() == Some(true))
                    .filter_map(|c| c["label"].as_str())
                    .collect();
                if !failed.is_empty() && !ok.is_empty() {
                    t.push_str(&format!("\n\n✅ {}", trunc(&ok.join(", "), 600)));
                }
                let mut sc = Screen::new(t);
                let mut row = Vec::new();
                for (label, target) in targets {
                    row.push(if target == "usage" {
                        self.command_button(label, target).await?
                    } else {
                        self.nav(label, target, json!({})).await?
                    });
                }
                sc.rows.push(row);
                sc.rows
                    .push(vec![self.nav("🔁 Relancer", "doctor", json!({})).await?]);
                sc
            }

            "config" => {
                let v = rpc.call(m::CONFIG_STATUS, json!({})).await?;
                let mut t = format!(
                    "⚙️ **Configuration** · génération {}\n`{}`\n\n**Sous-systèmes**",
                    v["generation"],
                    v["path"].as_str().unwrap_or("?")
                );
                match v["subsystems"].as_object() {
                    Some(subs) if !subs.is_empty() => {
                        for (sub, r) in subs {
                            let ok = r["ok"]
                                .as_bool()
                                .or_else(|| r["error"].is_null().then_some(true));
                            let detail = r["message"]
                                .as_str()
                                .or(r["error"].as_str())
                                .unwrap_or_default();
                            t.push_str(&format!(
                                "\n{} {sub}{}",
                                if ok == Some(false) { "❌" } else { "✅" },
                                if detail.is_empty() {
                                    String::new()
                                } else {
                                    format!(" : {}", trunc(detail, 160))
                                }
                            ));
                        }
                    }
                    _ => t.push_str("\nAucune génération appliquée depuis le démarrage."),
                }
                let mut sc = Screen::new(t);
                sc.rows
                    .push(vec![self.nav("🩺 Diagnostic", "doctor", json!({})).await?]);
                sc
            }

            "logs" => {
                let component = args["component"].as_str().unwrap_or_default();
                let n = args["n"].as_u64().unwrap_or(20).clamp(5, 200) as usize;
                let lines = super::recent_log_lines(&s.platform.dirs.logs(), component, n);
                let mut sc = Screen::new(if lines.is_empty() {
                    format!(
                        "📜 Aucune ligne de journal{}.",
                        if component.is_empty() {
                            String::new()
                        } else {
                            format!(" pour `{component}`")
                        }
                    )
                } else {
                    format!(
                        "📜 **Journal**{} · {} ligne(s)\n```\n{}\n```",
                        if component.is_empty() {
                            String::new()
                        } else {
                            format!(" `{component}`")
                        },
                        lines.len(),
                        lines.join("\n").replace("```", "ʼʼʼ")
                    )
                });
                let mut row = Vec::new();
                for c in ["", "telegram", "mcp", "llm", "workflow", "memory"] {
                    if c == component {
                        continue;
                    }
                    row.push(
                        self.nav(
                            if c.is_empty() { "Tout" } else { c },
                            "logs",
                            json!({"component": c}),
                        )
                        .await?,
                    );
                    if row.len() == 3 {
                        sc.rows.push(std::mem::take(&mut row));
                    }
                }
                sc.rows.push(row);
                sc.rows.push(vec![
                    self.nav("Plus", "logs", json!({"component": component, "n": n * 2}))
                        .await?,
                ]);
                sc
            }

            "quiet" => {
                let v = rpc.call(m::QUIET, json!({})).await?;
                let range = v["quiet_hours"].as_str().unwrap_or_default();
                let mut sc = Screen::new(if range.is_empty() {
                    "🌙 Heures silencieuses : **désactivées**.".to_string()
                } else {
                    format!(
                        "🌙 Heures silencieuses : **{range}**. Les messages non urgents attendent \
                         la fin de la plage."
                    )
                });
                let mut row = Vec::new();
                for preset in ["22:00-07:00", "23:00-08:00"] {
                    if preset != range {
                        row.push(
                            self.op(preset, "quiet.set", json!({"range": preset}), here.clone())
                                .await?,
                        );
                    }
                }
                if !range.is_empty() {
                    row.push(
                        self.op(
                            "Désactiver",
                            "quiet.set",
                            json!({"range": ""}),
                            here.clone(),
                        )
                        .await?,
                    );
                }
                sc.rows.push(row);
                sc.rows
                    .push(vec![ButtonSpec::copy_text("✏️ Autre plage", "/quiet ")]);
                sc
            }

            "secrets" => {
                let names = rpc.call(m::SECRET_LIST, json!({})).await?;
                let names: Vec<String> = names
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|x| {
                        x.as_str()
                            .map(String::from)
                            .or_else(|| x["name"].as_str().map(String::from))
                    })
                    .collect();
                let mut sc = Screen::new(format!(
                    "🔑 **Secrets** ({})\nUn secret ne se saisit **jamais** dans une conversation : \
                     en SSH, `penelope secret set <nom>` puis coller la valeur à l'invite.",
                    names.len()
                ));
                for n in &names {
                    sc.text.push_str(&format!("\n- `{n}`"));
                    sc.rows.push(vec![
                        self.guarded(
                            &format!("🗑 {}", trunc(n, 40)),
                            "secret.rm",
                            json!({"name": n}),
                            &format!("Supprimer le secret `{n}` ?"),
                            here.clone(),
                        )
                        .await?,
                    ]);
                }
                sc
            }

            "upgrade" => {
                // Le dernier résultat de « Vérifier » vaut tant que l'écran n'en porte pas.
                let cached: Value = d
                    .kv_get("tg.upgrade.last_check")
                    .await?
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or(json!({}));
                let args = if args["latest"].is_string() || args["error"].is_string() {
                    args
                } else {
                    &cached
                };
                let mut t = format!("⬆️ **Mise à jour** · version installée {}", crate::VERSION);
                match (args["latest"].as_str(), args["up_to_date"].as_bool()) {
                    (Some(latest), Some(true)) => {
                        t.push_str(&format!("\n✅ À jour (dernière publiée : {latest})."))
                    }
                    (Some(latest), _) => t.push_str(&format!("\n🆕 **{latest}** est disponible.")),
                    _ => t.push_str("\n🔎 « Vérifier » interroge les releases publiées."),
                }
                if let Some(e) = args["error"].as_str() {
                    t.push_str(&format!("\n❌ {e}"));
                }
                // Installation depuis les sources : l'installation passe par la bascule vers
                // les releases (issue #33).
                let from_sources = crate::upgrade::running_binary()
                    .is_ok_and(|b| crate::upgrade::is_source_build(&b));
                if from_sources {
                    t.push_str(
                        "\n📦 Installation depuis les sources : « Installer » propose de basculer \
                         vers les releases.",
                    );
                }
                let mut sc = Screen::new(t);
                sc.rows.push(vec![
                    self.op("🔎 Vérifier", "upgrade.check", json!({}), here.clone())
                        .await?,
                ]);
                if from_sources {
                    sc.rows.push(vec![
                        self.nav("⬆️ Installer", "upgrade.switch", json!({}))
                            .await?,
                    ]);
                } else {
                    sc.rows.push(vec![
                        self.guarded(
                            "⬆️ Installer",
                            "upgrade.install",
                            json!({}),
                            "Installer la dernière version publiée puis redémarrer ?",
                            here.clone(),
                        )
                        .await?,
                        self.guarded(
                            "⏪ Revenir",
                            "upgrade.rollback",
                            json!({}),
                            "Revenir au binaire précédent puis redémarrer ?",
                            here.clone(),
                        )
                        .await?,
                    ]);
                }
                sc
            }

            "upgrade.switch" => {
                let cfg = s.config.config();
                let current = crate::upgrade::running_binary().map_err(anyhow::Error::msg)?;
                let install_dir = s.platform.dirs.expand(&cfg.upgrade.install_dir);
                let source = crate::upgrade::Source::from_config(&cfg);
                let latest: Option<String> = d
                    .kv_get("tg.upgrade.last_check")
                    .await?
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .and_then(|v| v["latest"].as_str().map(String::from));
                let preflight = crate::upgrade::switch_preflight(&crate::upgrade::Switch {
                    source: &source,
                    tag: None,
                    current: &current,
                    install_dir: &install_dir,
                    state_dir: &s.platform.dirs.state(),
                    now: s.clock.now_rfc3339(),
                    codesign: crate::upgrade::codesign_of(&cfg),
                    host: &crate::upgrade::SystemHost,
                });
                let mut t = format!(
                    "📦 **Installation depuis les sources** (`{}`).\nBasculer vers les releases ? \
                     Le service lancera `{}/penelope`, un chemin stable que les mises à jour \
                     remplacent ; `make deploy` sur la machine y installe une compilation.\n",
                    current.display(),
                    install_dir.display()
                );
                match &preflight {
                    Ok(p) => t.push_str(&format!(
                        "\n✅ Signature utilisable, `{}` inscriptible, service `{}` modifiable.",
                        install_dir.display(),
                        p.service_file.display()
                    )),
                    Err(e) => t.push_str(&format!("\n❌ {e}")),
                }
                let mut sc = Screen::new(t);
                if preflight.is_ok() {
                    let label = match &latest {
                        Some(v) => format!("📦 Basculer et installer {v}"),
                        None => "📦 Basculer et installer la dernière version".to_string(),
                    };
                    sc.rows.push(vec![
                        self.op(
                            &label,
                            "upgrade.switch",
                            json!({}),
                            back_of("upgrade", &json!({})),
                        )
                        .await?,
                    ]);
                }
                sc.rows.push(vec![
                    self.op(
                        "Garder les sources",
                        "upgrade.keep_sources",
                        json!({}),
                        Value::Null,
                    )
                    .await?,
                    self.nav("Annuler", "upgrade", json!({})).await?,
                ]);
                sc
            }

            "rewind" => {
                let mut sc = Screen::new(
                    "⏪ **Revenir en arrière** : combien d'échanges défaire ? Les messages \
                     retirés sont mis de côté, pas effacés.",
                );
                let mut row = Vec::new();
                for n in [1, 2, 3, 5] {
                    row.push(
                        self.guarded(
                            &n.to_string(),
                            "session.rewind",
                            json!({"turns": n}),
                            &format!("Défaire les {n} dernier(s) échange(s) ?"),
                            here.clone(),
                        )
                        .await?,
                    );
                }
                sc.rows.push(row);
                sc
            }

            "forget.sessions" => {
                let sessions = s
                    .sessions
                    .list(Some(penelope_kernel::session::SessionKind::Chat), 50)
                    .await?;
                let mut sc = Screen::new(
                    "🧹 **Oublier une session** : tout ce que la mémoire a retenu d'elle est retiré \
                     (entrées et candidats). La conversation elle-même reste.",
                );
                let page = page_of(args);
                for sess in sessions.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                    let label = crate::titles::label(sess);
                    sc.rows.push(vec![
                        self.guarded(
                            &format!("🗑 {}", trunc(&label, 40)),
                            "session.forget",
                            json!({"session": sess.id.to_string()}),
                            &format!("Oublier tout ce qui vient de « {label} » ?"),
                            here.clone(),
                        )
                        .await?,
                    ]);
                }
                self.pager(&mut sc, name, args, sessions.len()).await?;
                sc
            }

            "prompts" => {
                let Some(sup) = d.hooks.mcp_supervisor() else {
                    return Ok(Screen::new("Aucun serveur MCP chargé."));
                };
                match args["server"].as_str() {
                    None => {
                        let statuses = sup.statuses().await;
                        let mut sc = Screen::new(if statuses.is_empty() {
                            "Aucun serveur MCP déclaré.".to_string()
                        } else {
                            "💬 **Prompts MCP** : choisis un serveur.".to_string()
                        });
                        for st in statuses {
                            sc.rows.push(vec![
                                self.nav(
                                    &format!(
                                        "{} {}",
                                        super::mcp_state_icon(st.state.as_str()),
                                        st.name
                                    ),
                                    "prompts",
                                    json!({"server": st.name}),
                                )
                                .await?,
                            ]);
                        }
                        sc
                    }
                    Some(server) => {
                        let prompts = sup.prompts(server).await.map_err(anyhow::Error::msg)?;
                        let mut sc = Screen::new(if prompts.is_empty() {
                            format!("`{server}` ne propose aucun prompt.")
                        } else {
                            format!("💬 **Prompts de `{server}`** ({})\n", prompts.len())
                        });
                        for p in prompts.iter().take(30) {
                            let pname = p["name"].as_str().unwrap_or("?");
                            sc.text.push_str(&format!(
                                "\n- `{pname}` : {}",
                                trunc(p["description"].as_str().unwrap_or_default(), 120)
                            ));
                            sc.rows.push(vec![
                                self.op(
                                    &format!("▶️ {}", trunc(pname, 36)),
                                    "prompt.run",
                                    json!({"server": server, "prompt": pname}),
                                    here.clone(),
                                )
                                .await?,
                            ]);
                        }
                        sc.rows
                            .push(vec![self.nav("« Serveurs", "prompts", json!({})).await?]);
                        sc
                    }
                }
            }

            other => {
                let _ = (chat_id, topic_id);
                Screen::new(format!("Écran inconnu : `{other}`."))
            }
        })
    }

    // ------------------------------------------------------------ opérations

    async fn perform(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        op: &str,
        p: &Value,
    ) -> anyhow::Result<Done> {
        let d = &self.daemon;
        let s = &d.services;
        let rpc = crate::rpc::Rpc::new(d.clone());
        let str_of = |key: &str| p[key].as_str().unwrap_or_default().to_string();
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        Ok(match op {
            "wf.run" => {
                let id = str_of("id");
                let entry = s
                    .workflows
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("workflow `{id}` introuvable"))?;
                let title = if entry.metadata.name.is_empty() {
                    id.clone()
                } else {
                    entry.metadata.name.clone()
                };
                if entry.metadata.parameters.is_empty() {
                    let run = crate::workflow::start_run(d, &id, json!({}), &origin, None, 0)
                        .await
                        .map_err(anyhow::Error::msg)?;
                    Done::note(
                        "Run lancé",
                        format!("▶️ Run `{}` lancé (« {title} »).", run.id),
                    )
                } else {
                    self.start_workflow_form(chat_id, topic_id, &id, &title)
                        .await?;
                    Done::quiet(format!("Paramètres de « {title} »"))
                }
            }
            // Rafale de messages : le propriétaire choisit ce qu'on en fait (issue #49).
            "burst.one" | "burst.ingest" | "burst.each" | "burst.drop" => {
                let id = str_of("id");
                let key = format!("tg.burst.{id}");
                let raw = d.kv_get(&key).await?.unwrap_or_default();
                if raw.is_empty() {
                    return Ok(Done::quiet("Rafale déjà traitée."));
                }
                d.kv_set(&key, "").await?;
                let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let session = v["session"].as_str().unwrap_or_default().to_string();
                let joined = v["joined"].as_str().unwrap_or_default().to_string();
                let parts: Vec<String> = v["parts"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: v["message_id"].as_i64(),
                };
                match op {
                    "burst.one" => {
                        d.enqueue_message(&session, &joined, &origin, None).await?;
                        Done::quiet("Traité comme un seul document")
                    }
                    "burst.each" => {
                        for part in &parts {
                            d.enqueue_message(&session, part, &origin, None).await?;
                        }
                        Done::quiet(format!("{} messages remis en file", parts.len()))
                    }
                    "burst.ingest" => {
                        let (daemon, sess) = (d.clone(), session.clone());
                        let name = format!(
                            "collage-{}.md",
                            s.clock.now_rfc3339().chars().take(19).collect::<String>()
                        );
                        let chan = origin.clone();
                        tokio::spawn(async move {
                            let note = match crate::ingest::ingest(
                                &daemon,
                                &name,
                                joined.into_bytes(),
                                "telegram",
                                // Texte écrit par le propriétaire lui-même.
                                penelope_memory::Origin::Owner,
                                Some(&sess),
                            )
                            .await
                            {
                                Ok(i) => i.report(),
                                Err(e) => format!("📄 Ingestion impossible : {e}"),
                            };
                            if let Some(m) = daemon.hooks.messenger() {
                                let _ = m.send_text(&chan, &note).await;
                            }
                        });
                        Done::quiet("Ingestion lancée")
                    }
                    _ => Done::quiet("Rien n'a été traité"),
                }
            }
            "run.pause" | "run.resume" | "run.cancel" => {
                let run = str_of("run");
                let control = match op {
                    "run.pause" => penelope_workflow::Control::Pause,
                    "run.resume" => penelope_workflow::Control::Resume,
                    _ => penelope_workflow::Control::Cancel,
                };
                let state = crate::workflow::control(d, &run, &control).await?;
                Done::toast(format!("Run {} : {}", run, state.as_str()))
            }
            "schedule.run" | "schedule.pause" | "schedule.resume" | "schedule.rm" => {
                let id = str_of("id");
                let method = match op {
                    "schedule.run" => m::SCHEDULE_RUN_NOW,
                    "schedule.pause" => m::SCHEDULE_PAUSE,
                    "schedule.resume" => m::SCHEDULE_RESUME,
                    _ => m::SCHEDULE_RM,
                };
                rpc.call(method, json!({"id": id})).await?;
                Done::toast(match op {
                    "schedule.run" => "⚡ Déclenché",
                    "schedule.pause" => "⏸ En pause",
                    "schedule.resume" => "▶️ Repris",
                    _ => "🗑 Supprimé",
                })
            }
            "mcp.restart" => {
                let name = str_of("name");
                let v = rpc.call(m::MCP_RESTART, json!({"name": name})).await?;
                Done::toast(format!(
                    "🔄 {name} : {} outil(s), {}",
                    v["tool_count"],
                    v["state"].as_str().unwrap_or("?")
                ))
            }
            "mcp.test" => {
                let name = str_of("name");
                let v = rpc.call(m::MCP_TEST, json!({"name": name})).await?;
                if v["ok"].as_bool() == Some(true) {
                    Done::toast(format!("✅ {name} répond ({} ms)", v["ms"]))
                } else {
                    Done::toast(format!(
                        "❌ {name} : {}",
                        v["error"].as_str().unwrap_or("échec")
                    ))
                }
            }
            "mcp.enable" | "mcp.disable" => {
                let name = str_of("name");
                let method = if op == "mcp.enable" {
                    m::MCP_ENABLE
                } else {
                    m::MCP_DISABLE
                };
                rpc.call(method, json!({"name": name})).await?;
                Done::toast(if op == "mcp.enable" {
                    format!("▶️ {name} activé")
                } else {
                    format!("⏸ {name} désactivé")
                })
            }
            "mcp.logs" => {
                let name = str_of("name");
                let v = rpc.call(m::MCP_LOGS, json!({"name": name})).await?;
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
                let note = if lines.is_empty() {
                    format!("Aucune ligne de journal pour `{name}`.")
                } else {
                    format!("```\n{}\n```", lines.join("\n").replace("```", "ʼʼʼ"))
                };
                Done {
                    toast: "📜 Journal".into(),
                    note: Some(note),
                    redraw: true,
                }
            }
            "mcp.auth" => {
                self.send_oauth_card(chat_id, topic_id, &str_of("name"))
                    .await?;
                Done {
                    toast: "🔐 Lien d'autorisation envoyé".into(),
                    note: None,
                    redraw: true,
                }
            }
            "model.assign" => {
                let (alias, model) = (str_of("alias"), str_of("model"));
                let v = rpc
                    .call(m::MODEL_SET, json!({"alias": alias, "model": model}))
                    .await?;
                Done::note(
                    format!("✅ {alias} → {}", short_model(&model)),
                    format!("✅ `{alias}` → `{model}` (génération {}).", v["generation"]),
                )
            }
            "skill.rollback" => {
                let name = str_of("name");
                rpc.call(m::SKILL_ROLLBACK, json!({"name": name})).await?;
                Done::toast(format!("⏪ {name} : version précédente"))
            }
            "mem.forget" => {
                let vault = crate::conversation::vault_dir(s);
                match crate::vault_ops::forget(s, &vault, &str_of("uid"))
                    .await
                    .map_err(anyhow::Error::msg)?
                {
                    true => Done::toast("🗑 Oublié"),
                    false => Done::toast("Rien à oublier"),
                }
            }
            "mem.validate" => {
                let uid = str_of("uid");
                let e = s
                    .memory
                    .get(&uid)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("entrée `{uid}` introuvable"))?;
                let vault = crate::conversation::vault_dir(s);
                let day = crate::vault_ops::day(s);
                crate::vault_ops::update_note(&vault, &e.file, Some(&uid), &day, |raw| {
                    penelope_memory::edit::update_annotations(raw, &uid, |a| {
                        a.revue = Some(day.clone())
                    })
                    .ok_or_else(|| format!("entrée `{uid}` absente de {}", e.file))
                })
                .map_err(anyhow::Error::msg)?;
                Done::toast("✅ Entrée validée")
            }
            "practice.status" => {
                let (slug, statut) = (str_of("slug"), str_of("statut"));
                let vault = crate::conversation::vault_dir(s);
                let rel = format!("pratiques/{}", penelope_platform::slugify(&slug)) + ".md";
                crate::vault_ops::update_note(
                    &vault,
                    &rel,
                    None,
                    &crate::vault_ops::day(s),
                    |raw| {
                        let mut practice = penelope_memory::vault::Practice::parse(raw, &slug)?;
                        practice.statut = penelope_memory::vault::PracticeStatus::parse(&statut);
                        Ok(practice.render())
                    },
                )
                .map_err(anyhow::Error::msg)?;
                Done::toast(if statut == "active" {
                    "✅ Pratique validée"
                } else {
                    "🚫 Pratique contestée"
                })
            }
            "intent.cancel" => {
                rpc.call(m::INTENT_CANCEL, json!({"id": str_of("id")}))
                    .await?;
                Done::toast("❌ Intention annulée")
            }
            "policy.revoke" => {
                rpc.call(m::POLICY_REVOKE, json!({"id": str_of("id")}))
                    .await?;
                Done::toast("🗑 Règle retirée")
            }
            "secret.rm" => {
                let name = str_of("name");
                rpc.call(m::SECRET_RM, json!({"name": name})).await?;
                Done::toast(format!("🗑 Secret {name} supprimé"))
            }
            "quiet.set" => {
                let range = str_of("range");
                rpc.call(m::QUIET, json!({"range": range})).await?;
                Done::toast(if range.is_empty() {
                    "🔔 Heures silencieuses désactivées".to_string()
                } else {
                    format!("🌙 Silence de {range}")
                })
            }
            "restart" => {
                rpc.call(m::RESTART, json!({})).await?;
                Done::quiet("🔁 Redémarrage demandé")
            }
            "session.close" => {
                let id = str_of("session");
                let v = crate::session_ops::close(d, &id)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Done {
                    toast: "🔒 Session fermée".into(),
                    note: Some(format!(
                        "🔒 Session {} fermée.{}",
                        v["title"]
                            .as_str()
                            .map(|t| format!("« {t} »"))
                            .unwrap_or_else(|| format!("`{id}`")),
                        super::cancelled_note(v["cancelled"].as_u64().unwrap_or(0) as usize)
                    )),
                    redraw: false,
                }
            }
            "session.rewind" => {
                let turns = p["turns"].as_u64().unwrap_or(1) as usize;
                let session = d.chat_session_for(&origin).await?;
                let v = crate::session_ops::rewind(d, &session, turns)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Done {
                    toast: format!("⏪ {turns} échange(s) défait(s)"),
                    note: Some(format!(
                        "⏪ {turns} échange(s) défait(s) ({} messages mis de côté dans `{}`).",
                        v["removed"],
                        v["archive"].as_str().unwrap_or("?")
                    )),
                    redraw: false,
                }
            }
            "upgrade.check" => {
                let v = rpc.call(m::UPGRADE, json!({"check": true})).await;
                let args = match v {
                    Ok(v) => json!({"latest": v["latest"], "up_to_date": v["up_to_date"]}),
                    Err(e) => json!({"error": e.to_string()}),
                };
                // L'écran porte le résultat : il est redessiné ici plutôt que par le retour.
                let _ = d.kv_set("tg.upgrade.last_check", &args.to_string()).await;
                Done::toast("🔎 Vérifié")
            }
            "upgrade.keep_sources" => {
                Done::quiet("Les sources restent : `make deploy` sur la machine")
            }
            "upgrade.install" | "upgrade.rollback" | "upgrade.switch" => {
                let params = match (op, p["tag"].as_str()) {
                    ("upgrade.rollback", _) => json!({"rollback": true}),
                    ("upgrade.switch", _) => json!({"switch": true}),
                    (_, Some(tag)) => json!({"tag": tag}),
                    _ => json!({}),
                };
                let (daemon, messenger) = (d.clone(), d.hooks.messenger());
                tokio::spawn(async move {
                    let rpc = crate::rpc::Rpc::new(daemon);
                    let text = match rpc.call(m::UPGRADE, params).await {
                        Ok(v) => crate::upgrade::render(&v),
                        Err(e) => format!("❌ {e}"),
                    };
                    if let Some(m) = messenger {
                        let _ = m.send_text(&origin, &text).await;
                    }
                });
                Done::quiet(match op {
                    "upgrade.install" => "⬆️ Installation lancée",
                    "upgrade.switch" => "📦 Bascule vers les releases lancée",
                    _ => "⏪ Retour arrière lancé",
                })
            }
            "prompt.run" => {
                let (server, prompt) = (str_of("server"), str_of("prompt"));
                let sup = d
                    .hooks
                    .mcp_supervisor()
                    .ok_or_else(|| anyhow::anyhow!("aucun serveur MCP chargé"))?;
                let listed = sup
                    .prompts(&server)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .into_iter()
                    .find(|x| x["name"].as_str() == Some(prompt.as_str()))
                    .ok_or_else(|| {
                        anyhow::anyhow!("prompt `{prompt}` introuvable sur `{server}`")
                    })?;
                let has_args = listed["arguments"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty());
                if has_args {
                    let state =
                        penelope_telegram::forms::FormState::new(&prompt, prompt_schema(&listed))
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    let pending = json!({
                        "prompt": {"server": server, "name": prompt},
                        "choice": format!("{server} · {prompt}"),
                        "state": state,
                    });
                    d.kv_set(&form_key(chat_id), &pending.to_string()).await?;
                    self.send_form_step(chat_id, &pending).await?;
                    Done::quiet(format!("Arguments de {prompt}"))
                } else {
                    let n = self
                        .run_mcp_prompt(chat_id, topic_id, &server, &prompt, json!({}))
                        .await?;
                    Done::quiet(format!("💬 {n} message(s) envoyés au modèle"))
                }
            }
            "session.forget" => {
                let id = str_of("session");
                let vault = crate::conversation::vault_dir(s);
                let uids = s.memory.forget_session(&id).await?;
                for uid in &uids {
                    crate::vault_ops::forget(s, &vault, uid)
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
                Done::toast(format!("🧹 {} entrée(s) oubliée(s)", uids.len()))
            }
            "notes.adopt" => {
                let copied = crate::session_notes::copy(s, &str_of("from"), &str_of("to")).await?;
                Done::quiet(if copied {
                    "📓 Notes reprises"
                } else {
                    "Aucune note à reprendre"
                })
            }
            "noop" => Done::quiet("Annulé"),
            other => Done::quiet(format!("Opération inconnue : {other}")),
        })
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

    /// Formulaire des paramètres d'un workflow ; l'envoi démarre le run.
    pub(super) async fn start_workflow_form(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        workflow: &str,
        title: &str,
    ) -> anyhow::Result<()> {
        let entry = self
            .daemon
            .services
            .workflows
            .get(workflow)
            .ok_or_else(|| anyhow::anyhow!("workflow `{workflow}` introuvable"))?;
        let schema = parameters_schema(&entry.metadata.parameters);
        let state = penelope_telegram::forms::FormState::new(workflow, schema)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // Le sujet d'origine suit le formulaire : le run y parlera (issue #35).
        let pending =
            json!({"workflow": workflow, "choice": title, "state": state, "topic": topic_id});
        self.daemon
            .kv_set(&form_key(chat_id), &pending.to_string())
            .await?;
        self.send_form_step(chat_id, &pending).await
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
