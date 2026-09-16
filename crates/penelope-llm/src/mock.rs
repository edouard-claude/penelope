//! Provider simulé, pour les suites déterministes (§20.1, toutes les suites « sans réseau »).
//!
//! Il rejoue des scénarios scriptés : texte, appels d'outils, erreurs, usage. Il vérifie
//! aussi ce qu'on lui envoie, ce qui permet de tester le préfixe stable (CA 5) et le
//! routage (CA 10) sans toucher au réseau.

use crate::catalog::ModelInfo;
use crate::provider::{CancelToken, ChunkStream, Provider};
use crate::types::*;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Réponse scriptée.
#[derive(Debug, Clone)]
pub enum Scripted {
    Text(String),
    /// Texte puis appels d'outils.
    ToolCalls(String, Vec<ToolCall>),
    Error(LlmErrorKind, String),
    /// Erreur de dépassement de contexte, pour tester la compaction d'urgence.
    ContextOverflow,
}

#[derive(Clone, Default)]
pub struct MockProvider {
    script: Arc<Mutex<Vec<Scripted>>>,
    pub seen: Arc<Mutex<Vec<ChatRequest>>>,
    usage: Arc<Mutex<Usage>>,
    models: Arc<Mutex<Vec<ModelInfo>>>,
}

impl MockProvider {
    pub fn new() -> Self {
        MockProvider {
            script: Arc::new(Mutex::new(Vec::new())),
            seen: Arc::new(Mutex::new(Vec::new())),
            usage: Arc::new(Mutex::new(Usage {
                prompt: 1000,
                completion: 50,
                cached: 0,
                reasoning: 0,
            })),
            models: Arc::new(Mutex::new(vec![ModelInfo::minimal(
                "mock/model",
                "mock",
                128_000,
            )])),
        }
    }

    pub fn push(&self, s: Scripted) -> &Self {
        self.script
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(s);
        self
    }

    pub fn reply(&self, text: &str) -> &Self {
        self.push(Scripted::Text(text.into()))
    }

    pub fn set_usage(&self, u: Usage) -> &Self {
        *self.usage.lock().unwrap_or_else(|p| p.into_inner()) = u;
        self
    }

    pub fn set_models(&self, m: Vec<ModelInfo>) -> &Self {
        *self.models.lock().unwrap_or_else(|p| p.into_inner()) = m;
        self
    }

    /// Requêtes reçues, pour les assertions de test.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn call_count(&self) -> usize {
        self.requests().len()
    }

    fn next(&self) -> Scripted {
        let mut g = self.script.lock().unwrap_or_else(|p| p.into_inner());
        if g.is_empty() {
            Scripted::Text("réponse simulée".into())
        } else {
            g.remove(0)
        }
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn chat_stream(&self, req: ChatRequest, cancel: CancelToken) -> Result<ChunkStream> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(req.clone());

        let scripted = self.next();
        if let Scripted::Error(kind, msg) = &scripted {
            return Err(LlmError::new(kind.clone(), msg.clone()));
        }
        if matches!(scripted, Scripted::ContextOverflow) {
            return Err(LlmError::context_length(
                "This model's maximum context length is 8192 tokens",
            ));
        }

        let usage = *self.usage.lock().unwrap_or_else(|p| p.into_inner());
        let model = req.model.clone();
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            if cancel.is_cancelled() {
                let _ = tx
                    .send(StreamChunk::Done {
                        finish: FinishReason::Cancelled,
                    })
                    .await;
                return;
            }
            let _ = tx
                .send(StreamChunk::Started {
                    id: "mock-1".into(),
                    model,
                })
                .await;
            let finish = match scripted {
                Scripted::Text(t) => {
                    // Découpé en fragments, comme un vrai flux.
                    for part in split_into_chunks(&t, 8) {
                        if cancel.is_cancelled() {
                            break;
                        }
                        let _ = tx.send(StreamChunk::Delta { text: part }).await;
                    }
                    FinishReason::Stop
                }
                Scripted::ToolCalls(t, calls) => {
                    if !t.is_empty() {
                        let _ = tx.send(StreamChunk::Delta { text: t }).await;
                    }
                    for c in calls {
                        let _ = tx.send(StreamChunk::ToolCall(c)).await;
                    }
                    FinishReason::ToolCalls
                }
                _ => FinishReason::Stop,
            };
            let _ = tx.send(StreamChunk::Usage(usage)).await;
            let _ = tx.send(StreamChunk::Done { finish }).await;
        });
        Ok(rx)
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(self
            .models
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone())
    }
}

fn split_into_chunks(s: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(n.max(1)).map(|c| c.iter().collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::provider::collect_stream;
    use serde_json::json;

    #[tokio::test]
    async fn mock_streams_text_in_fragments() {
        let p = MockProvider::new();
        p.reply("bonjour tout le monde");
        let rx = p
            .chat_stream(
                ChatRequest {
                    model: "mock/model".into(),
                    messages: vec![ChatMessage::user("salut")],
                    ..Default::default()
                },
                CancelToken::new(),
            )
            .await
            .unwrap();
        let r = collect_stream(rx, "mock/model", "mock", &Catalog::new())
            .await
            .unwrap();
        assert_eq!(r.message.text(), "bonjour tout le monde");
        assert_eq!(p.call_count(), 1);
    }

    #[tokio::test]
    async fn mock_emits_tool_calls() {
        let p = MockProvider::new();
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({"path":"a.rs"}),
            }],
        ));
        let rx = p
            .chat_stream(
                ChatRequest {
                    model: "mock/model".into(),
                    ..Default::default()
                },
                CancelToken::new(),
            )
            .await
            .unwrap();
        let r = collect_stream(rx, "mock/model", "mock", &Catalog::new())
            .await
            .unwrap();
        assert_eq!(r.finish, FinishReason::ToolCalls);
        assert_eq!(r.message.tool_calls[0].name, "fs_read");
    }

    #[tokio::test]
    async fn mock_reports_context_overflow() {
        let p = MockProvider::new();
        p.push(Scripted::ContextOverflow);
        let e = p
            .chat_stream(ChatRequest::default(), CancelToken::new())
            .await
            .unwrap_err();
        assert_eq!(e.kind, LlmErrorKind::ContextLength);
    }

    #[tokio::test]
    async fn cancellation_stops_the_stream() {
        let p = MockProvider::new();
        p.reply(&"a".repeat(400));
        let token = CancelToken::new();
        token.cancel();
        let rx = p.chat_stream(ChatRequest::default(), token).await.unwrap();
        let r = collect_stream(rx, "m", "mock", &Catalog::new())
            .await
            .unwrap();
        assert_eq!(r.finish, FinishReason::Cancelled);
    }
}
