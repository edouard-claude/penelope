//! Commandes de workflows et de compétences : `/run`, `/resume`, `/skill`.

use super::*;

impl TelegramGateway {
    /// `/run`.
    pub(super) async fn cmd_run(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        let text: String = {
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
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/resume`.
    pub(super) async fn cmd_resume(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let reply_to = Some(message_id);
        let text: String = {
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
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/wf`, `/runs`, `/skills`, `/intentions`, `/policies`, `/status`, `/doctor`, `/config` :
    /// l'écran du même nom.
    pub(super) async fn cmd_screen(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
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

    /// `/skill`.
    pub(super) async fn cmd_skill(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
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
}
