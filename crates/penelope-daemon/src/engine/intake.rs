//! Port `TurnIntake` : l'entrée des tours dans la file ; la session d'un canal reste
//! dans engine.rs (`chat_session`).

use super::*;

#[async_trait::async_trait]
impl TurnIntake for Core {
    async fn enqueue_message(
        &self,
        session_id: &str,
        text: &str,
        origin: &Origin,
        dedup: Option<String>,
    ) -> anyhow::Result<Option<TurnId>> {
        let payload = json!({"text": text, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(session_id, TurnKind::Message, payload, dedup, 0)
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    async fn enqueue_message_with_images(
        &self,
        session_id: &str,
        text: &str,
        images: &[std::path::PathBuf],
        origin: &Origin,
        dedup: Option<String>,
    ) -> anyhow::Result<Option<TurnId>> {
        let images: Vec<String> = images
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let payload = json!({"text": text, "images": images, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(session_id, TurnKind::Message, payload, dedup, 0)
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    async fn enqueue_retry(
        &self,
        session_id: &str,
        origin: &Origin,
        token: &str,
    ) -> anyhow::Result<Option<TurnId>> {
        let payload = json!({"retry": true, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(
                session_id,
                TurnKind::Resume,
                payload,
                Some(format!("retry:{token}")),
                10,
            )
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    async fn enqueue_resume(
        &self,
        session_id: &str,
        approval_id: &str,
        origin: &Origin,
    ) -> anyhow::Result<Option<TurnId>> {
        // Un run de workflow reprend par son pilote, pas par un tour de conversation.
        if let Ok(Some(sess)) = self.services.sessions.get(session_id).await
            && sess.kind == penelope_kernel::session::SessionKind::WorkflowRun
        {
            self.workflows.wake();
            return Ok(None);
        }
        let payload = json!({"approval_id": approval_id, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(
                session_id,
                TurnKind::Resume,
                payload,
                Some(format!("resume:{approval_id}")),
                10,
            )
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    async fn chat_session_for(&self, origin: &Origin) -> anyhow::Result<String> {
        chat_session(&self.services, origin).await
    }
}
