//! Canal de messages simulé (`messenger = true`) : ce que Pénélope envoie au propriétaire
//! pendant un tour (`send_message`, `send_file`, `send_voice`, `ask_user`, cartes de plan
//! et d'approbation) est enregistré, puis relevé dans le monde en lignes `sent`.
//!
//! Distinct de la passerelle Telegram simulée : aucun `tg_outbox`, aucun rendu propre à un
//! canal, seulement ce qui a été confié au port `Messenger`.

use super::{Shared, lock};
use penelope_app::bus::Origin;
use penelope_workflow::plan::PlanDraft;
use serde_json::{Value, json};
use std::path::Path;

pub(super) struct Recorder {
    pub(super) sent: Shared<Vec<Value>>,
}

impl Recorder {
    fn push(&self, line: Value) {
        lock(&self.sent).push(line);
    }
}

fn origin(o: &Origin) -> Value {
    o.to_value()
}

#[async_trait::async_trait]
impl penelope_executor::executor::Messenger for Recorder {
    async fn send_text(&self, o: &Origin, markdown: &str) -> Result<(), String> {
        self.push(json!({"type": "sent", "kind": "text", "origin": origin(o), "text": markdown}));
        Ok(())
    }

    async fn send_file(
        &self,
        o: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        self.push(json!({
            "type": "sent", "kind": "file", "origin": origin(o),
            "path": path.to_string_lossy(), "caption": caption,
        }));
        Ok(())
    }

    async fn send_approval(&self, o: &Origin, approval_id: &str) -> Result<(), String> {
        self.push(json!({
            "type": "sent", "kind": "approval", "origin": origin(o), "approval": approval_id,
        }));
        Ok(())
    }

    async fn send_plan_card(
        &self,
        o: &Origin,
        session: &str,
        draft: &PlanDraft,
    ) -> Result<(), String> {
        self.push(json!({
            "type": "sent", "kind": "plan", "origin": origin(o), "session": session,
            "workflow": draft.workflow_id, "version": draft.plan.version(),
            "goal": draft.plan.goal(),
        }));
        Ok(())
    }

    async fn send_session_text(
        &self,
        session_id: &str,
        o: &Origin,
        markdown: &str,
    ) -> Result<(), String> {
        self.push(json!({
            "type": "sent", "kind": "text", "origin": origin(o), "session": session_id,
            "text": markdown,
        }));
        Ok(())
    }

    async fn send_session_file(
        &self,
        session_id: &str,
        o: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        self.push(json!({
            "type": "sent", "kind": "file", "origin": origin(o), "session": session_id,
            "path": path.to_string_lossy(), "caption": caption,
        }));
        Ok(())
    }

    async fn send_session_voice(
        &self,
        session_id: &str,
        o: &Origin,
        path: &Path,
        duration_s: u32,
        caption: Option<&str>,
    ) -> Result<(), String> {
        self.push(json!({
            "type": "sent", "kind": "voice", "origin": origin(o), "session": session_id,
            "path": path.to_string_lossy(), "duration_s": duration_s, "caption": caption,
        }));
        Ok(())
    }
}
