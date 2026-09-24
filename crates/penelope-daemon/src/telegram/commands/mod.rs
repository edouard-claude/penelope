//! Commandes `/…` du propriétaire.

use super::*;

impl TelegramGateway {
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
