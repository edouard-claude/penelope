//! L'orchestrateur sous son ancienne forme, `WorkflowOrchestrator { daemon }`, que
//! construisent encore l'exécution des jobs, deux tests du daemon et la passerelle :
//! chaque méthode passe à celui de `penelope-orchestrator`, sur le contexte du daemon. À retirer en T30.

use super::{Context, context_of};
use crate::bus::Origin;
use crate::ports::Orchestrator;
use crate::runtime::Daemon;
use penelope_llm::provider::CancelToken;
use serde_json::Value;
use std::sync::Arc;

/// Workflows, sous-agents et images, offerts aux outils, sur le daemon.
pub struct WorkflowOrchestrator {
    pub daemon: Arc<Daemon>,
}

impl WorkflowOrchestrator {
    fn engine(&self) -> penelope_orchestrator::WorkflowOrchestrator {
        penelope_orchestrator::WorkflowOrchestrator {
            context: context_of(&self.daemon),
        }
    }

    /// Le contexte de l'orchestrateur sur ce daemon.
    pub fn context(&self) -> Context {
        context_of(&self.daemon)
    }
}

#[async_trait::async_trait]
impl Orchestrator for WorkflowOrchestrator {
    async fn start_workflow(
        &self,
        id: &str,
        params: Value,
        brief: Option<&str>,
        origin: &Origin,
    ) -> Result<Value, String> {
        self.engine()
            .start_workflow(id, params, brief, origin)
            .await
    }

    async fn spawn_sub_agent(
        &self,
        session_id: &str,
        prompt: &str,
        model: Option<&str>,
        tools: Vec<String>,
        origin: &Origin,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        self.engine()
            .spawn_sub_agent(session_id, prompt, model, tools, origin, cancel)
            .await
    }

    async fn generate_image(&self, prompt: &str, size: Option<&str>) -> Result<Value, String> {
        self.engine().generate_image(prompt, size).await
    }

    async fn inspect_image(
        &self,
        session_id: &str,
        path: &std::path::Path,
        task: crate::vision::Task,
        question: &str,
    ) -> Result<Value, String> {
        self.engine()
            .inspect_image(session_id, path, task, question)
            .await
    }

    async fn control_run(&self, run_id: &str, op: &str) -> Result<Value, String> {
        self.engine().control_run(run_id, op).await
    }

    async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        self.engine().embed_query(text).await
    }

    async fn schedule_create(
        &self,
        kind: penelope_workflow::TriggerKind,
        spec: Value,
        target: Value,
        dedup: Value,
    ) -> Result<Value, String> {
        self.engine()
            .schedule_create(kind, spec, target, dedup)
            .await
    }

    async fn schedule_list(&self) -> Result<Vec<Value>, String> {
        self.engine().schedule_list().await
    }

    async fn schedule_move(
        &self,
        id: &str,
        chat: i64,
        topic: Option<i64>,
    ) -> Result<String, String> {
        self.engine().schedule_move(id, chat, topic).await
    }

    async fn schedule_delete(&self, id: &str) -> Result<(), String> {
        self.engine().schedule_delete(id).await
    }
}
