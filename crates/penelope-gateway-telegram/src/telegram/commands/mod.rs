//! Commandes `/…` du propriétaire.

use super::*;

mod memory;
mod models;
mod ops;
mod project;
mod session;
mod workflows;

impl TelegramGateway {
    /// Aiguillage des commandes `/…` : un bras, une fonction de même signature, rangée
    /// par famille dans commands/*.rs (lot G). Les fonctions reçoivent `args` rogné.
    pub(super) async fn command(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let args = args.trim();
        match command {
            "start" | "help" => {
                self.cmd_start(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "new" => {
                self.cmd_new(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "title" => {
                self.cmd_title(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "sessions" => {
                self.cmd_sessions(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "compact" => {
                self.cmd_compact(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "fork" => {
                self.cmd_fork(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "rewind" if args.is_empty() => {
                self.cmd_rewind_screen(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "rewind" => {
                self.cmd_rewind(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "export" => {
                self.cmd_export(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "upgrade" => {
                self.cmd_upgrade(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "stop" => {
                self.cmd_stop(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "switch" => {
                self.cmd_switch(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "close" => {
                self.cmd_close(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "purge" => {
                self.cmd_purge(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "model" => {
                self.cmd_model(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "home" | "foyer" => {
                self.cmd_home(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "models" => {
                self.cmd_models(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "projet" => {
                self.cmd_projet(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "mode" => {
                self.cmd_mode(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "schedules" => {
                self.cmd_schedules(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "mcp" => {
                self.cmd_mcp(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "budget" => {
                self.cmd_budget(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "usage" => {
                self.cmd_usage(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "audit" => {
                self.cmd_audit(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "accueil" => {
                self.cmd_accueil(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "dream" => {
                self.cmd_dream(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "appris" => {
                self.cmd_appris(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "pratique" => {
                self.cmd_pratique(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "retiens" => {
                self.cmd_retiens(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "oublie" => {
                self.cmd_oublie(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "forget" => {
                self.cmd_forget(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "secret" => {
                self.cmd_secret(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "approvals" => {
                self.cmd_approvals(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "recall" => {
                self.cmd_recall(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "run" => {
                self.cmd_run(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "resume" => {
                self.cmd_resume(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "wf" | "runs" | "skills" | "intentions" | "policies" | "status" | "doctor"
            | "config" => {
                self.cmd_screen(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "skill" => {
                self.cmd_skill(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "logs" => {
                self.cmd_logs(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "restart" => {
                self.cmd_restart(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "note" => {
                self.cmd_note(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "mien" => {
                self.cmd_mien(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "p" => {
                self.cmd_p(chat_id, topic_id, message_id, command, args)
                    .await
            }
            "quiet" => {
                self.cmd_quiet(chat_id, topic_id, message_id, command, args)
                    .await
            }
            other => {
                let reply_to = Some(message_id);
                let text: String = format!("Commande inconnue : `/{other}`. Voir `/help`.");
                self.reply(chat_id, topic_id, reply_to, &text).await
            }
        }
    }

    /// Réponse suivie d'un bouton par suite proposée ; un clic envoie la suite comme message
    /// du propriétaire dans la session (issue #31).
    /// `/run <workflow>` sans paramètres : la demande part au modèle, qui complète les
    /// paramètres avec ses outils et propose le plan avant le gate (issues #35, #186).
    pub(super) async fn run_by_conversation(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        w: &penelope_workflow::Workflow,
        provided: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let id = &w.metadata.id;
        let title = if w.metadata.name.is_empty() {
            id.clone()
        } else {
            w.metadata.name.clone()
        };
        let describe = |required: bool| {
            w.metadata
                .parameters
                .iter()
                .filter(|p| p.required == required)
                .map(|p| format!("{} ({})", p.id, p.label))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (required, optional) = (describe(true), describe(false));
        let mut ask = format!(
            "/run {id}\n\n[Je veux lancer le workflow `{id}` (« {title} ») sans avoir donné ses \
             paramètres."
        );
        if !required.is_empty() {
            ask.push_str(&format!(" Requis : {required}."));
        }
        if !optional.is_empty() {
            ask.push_str(&format!(" Facultatifs : {optional}."));
        }
        if !provided.is_empty() {
            ask.push_str(&format!(
                " Paramètres fournis par le propriétaire : {provided}."
            ));
        }
        ask.push_str(
            " Complète-les avec tes outils, demande-moi seulement ce qui manque, puis propose \
             un plan structuré avec `workflow_plan` (`id`, `goal`, `steps`, `params`, `brief`) ; \
             découvre cet outil avec `tool_search` si nécessaire. \
             Montre-moi le plan pour correction ; ne lance aucun run avant « vas-y ».]",
        );
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = d.chat_session_for(&origin).await?;
        d.enqueue_message(&session, &ask, &origin, None).await?;
        self.send_screen(
            chat_id,
            topic_id,
            Some(message_id),
            screens::Screen {
                text: format!(
                    "💬 Je prépare le plan de « {title} » avec toi. Tu pourras le corriger avant « vas-y »."
                ),
                rows: vec![],
            },
            None,
        )
        .await
    }
}
