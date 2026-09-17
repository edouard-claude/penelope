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
    /// Texte puis images (`data:` URI), pour tester la génération d'image.
    Images(String, Vec<String>),
    /// Flux ouvert (HTTP 200) puis erreur réessayable, après un texte éventuel.
    MidStreamError(String, String),
}

#[derive(Clone, Default)]
pub struct MockProvider {
    script: Arc<Mutex<Vec<Scripted>>>,
    pub seen: Arc<Mutex<Vec<ChatRequest>>>,
    usage: Arc<Mutex<Usage>>,
    models: Arc<Mutex<Vec<ModelInfo>>>,
    /// Transcription renvoyée par `transcribe`, et fichiers reçus.
    transcript: Arc<Mutex<Option<String>>>,
    pub transcribed: Arc<Mutex<Vec<TranscribedFile>>>,
    /// Calcul d'embedding simulé ; `None` : le provider refuse.
    embedder: Arc<Mutex<Option<Embedder>>>,
    /// Synthèse vocale : `Some(erreur)` fait échouer `speak` ; textes reçus.
    speech_error: Arc<Mutex<Option<String>>>,
    pub spoken: Arc<Mutex<Vec<(String, String)>>>,
    /// Nom rendu par `name()` : les chemins qui dépendent du fournisseur (repli côté
    /// serveur d'OpenRouter, issue #50) se testent avec `named("openrouter")`.
    name: Arc<std::sync::OnceLock<String>>,
}

/// WAV PCM mono 16 bits à 16 kHz, de silence : ce que renvoie la synthèse simulée
/// (0,1 s par tranche de 10 caractères).
pub fn silent_wav(seconds: f64) -> Vec<u8> {
    let rate: u32 = 16_000;
    let samples = (seconds * rate as f64) as u32;
    let data_len = samples * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.resize(44 + data_len as usize, 0);
    out
}

/// Fonction d'embedding d'un faux provider.
pub type Embedder = Arc<dyn Fn(&str) -> Vec<f32> + Send + Sync>;

/// Fichier reçu par `transcribe` : nom, taille en octets, langue demandée.
pub type TranscribedFile = (String, usize, Option<String>);

impl MockProvider {
    pub fn new() -> Self {
        MockProvider {
            script: Arc::new(Mutex::new(Vec::new())),
            seen: Arc::new(Mutex::new(Vec::new())),
            usage: Arc::new(Mutex::new(Usage {
                prompt: 1000,
                completion: 50,
                ..Default::default()
            })),
            models: Arc::new(Mutex::new(vec![ModelInfo::minimal(
                "mock/model",
                "mock",
                128_000,
            )])),
            transcript: Arc::new(Mutex::new(None)),
            transcribed: Arc::new(Mutex::new(Vec::new())),
            embedder: Arc::new(Mutex::new(None)),
            speech_error: Arc::new(Mutex::new(None)),
            spoken: Arc::new(Mutex::new(Vec::new())),
            name: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Fait échouer la synthèse vocale (`Some(raison)`), ou la rétablit (`None`).
    pub fn set_speech_error(&self, error: Option<&str>) -> &Self {
        *self.speech_error.lock().unwrap_or_else(|p| p.into_inner()) = error.map(String::from);
        self
    }

    /// Embeddings simulés ; `None` : le provider refuse.
    pub fn set_embedder(&self, f: Option<Embedder>) -> &Self {
        *self.embedder.lock().unwrap_or_else(|p| p.into_inner()) = f;
        self
    }

    /// Texte que rendra la prochaine transcription ; `None` : le provider refuse.
    pub fn set_transcript(&self, text: Option<&str>) -> &Self {
        *self.transcript.lock().unwrap_or_else(|p| p.into_inner()) = text.map(String::from);
        self
    }

    pub fn push(&self, s: Scripted) -> &Self {
        self.script
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(s);
        self
    }

    /// Se fait passer pour un fournisseur donné (`openrouter`, `openai_compat`) : les
    /// chemins qui en dépendent se testent alors pour de vrai (issue #50).
    pub fn named(&self, name: &str) -> &Self {
        let _ = self.name.set(name.to_string());
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
        self.name.get().map(String::as_str).unwrap_or("mock")
    }

    async fn embed(&self, _model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let f = self
            .embedder
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| LlmError::new(LlmErrorKind::BadRequest, "embeddings non simulés"))?;
        Ok(inputs.iter().map(|t| f(t)).collect())
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
                Scripted::Images(t, urls) => {
                    if !t.is_empty() {
                        let _ = tx.send(StreamChunk::Delta { text: t }).await;
                    }
                    for url in urls {
                        let _ = tx.send(StreamChunk::Image { url }).await;
                    }
                    FinishReason::Stop
                }
                Scripted::MidStreamError(t, message) => {
                    if !t.is_empty() {
                        let _ = tx.send(StreamChunk::Delta { text: t }).await;
                    }
                    let _ = tx
                        .send(StreamChunk::Error {
                            message,
                            retryable: true,
                            error_type: None,
                        })
                        .await;
                    return;
                }
                _ => FinishReason::Stop,
            };
            let _ = tx.send(StreamChunk::Usage(usage)).await;
            let _ = tx.send(StreamChunk::Done { finish }).await;
        });
        Ok(rx)
    }

    async fn speak(
        &self,
        _model: &str,
        input: &str,
        voice: &str,
        _format: &str,
    ) -> Result<Vec<u8>> {
        if let Some(e) = self
            .speech_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return Err(LlmError::new(LlmErrorKind::Other, e));
        }
        self.spoken
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((input.to_string(), voice.to_string()));
        Ok(silent_wav(input.chars().count() as f64 / 100.0))
    }

    async fn transcribe(
        &self,
        _model: &str,
        audio: Vec<u8>,
        filename: &str,
        language: Option<&str>,
    ) -> Result<Transcription> {
        self.transcribed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((
                filename.to_string(),
                audio.len(),
                language.map(String::from),
            ));
        match self
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            Some(text) => Ok(Transcription {
                text,
                seconds: Some(3.0),
                cost_usd: Some(0.0001),
            }),
            None => Err(LlmError::new(
                LlmErrorKind::BadRequest,
                "le provider `mock` ne sait pas transcrire",
            )),
        }
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
