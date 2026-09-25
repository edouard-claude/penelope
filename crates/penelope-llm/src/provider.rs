//! Providers : OpenRouter et endpoint OpenAI-compatible générique (§10.1).
//!
//! Le streaming SSE est **obligatoire** : le PRD exige un premier retour visible en
//! moins de 1,5 s (§1), et la machine d'état des appels (§4.3) s'appuie sur l'arrivée des
//! en-têtes pour distinguer `dispatching` de `response_started`.

use crate::catalog::{Catalog, ModelInfo, parse_openrouter_models, strip_provider};
use crate::sse::{EventAccumulator, SseDecoder, StreamAccumulator};
use crate::types::*;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Flux de fragments renvoyé par un provider.
pub type ChunkStream = mpsc::Receiver<StreamChunk>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Lance un appel en streaming. Le `Sender` est fermé à la fin du flux.
    async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: tokio_util_lite::CancelToken,
    ) -> Result<ChunkStream>;

    /// Rafraîchit le catalogue depuis le provider.
    async fn fetch_models(&self) -> Result<Vec<ModelInfo>>;

    /// Embeddings (§6.11) : un vecteur par texte, dans l'ordre.
    async fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let _ = (model, inputs);
        Err(LlmError::new(
            LlmErrorKind::BadRequest,
            format!("le provider `{}` ne calcule pas d'embeddings", self.name()),
        ))
    }

    /// Transcrit un fichier audio (rôle `stt`). Le nom de fichier porte le format
    /// (`.ogg`, `.mp3`, `.wav`…), que les serveurs lisent à l'extension.
    async fn transcribe(
        &self,
        model: &str,
        audio: Vec<u8>,
        filename: &str,
        language: Option<&str>,
    ) -> Result<Transcription> {
        let _ = (model, audio, filename, language);
        Err(LlmError::new(
            LlmErrorKind::BadRequest,
            format!("le provider `{}` ne sait pas transcrire", self.name()),
        ))
    }

    /// Synthèse vocale (rôle `tts`, issue #41) : `POST /audio/speech`, renvoie les octets
    /// audio au format demandé (`wav`).
    async fn speak(&self, model: &str, input: &str, voice: &str, format: &str) -> Result<Vec<u8>> {
        let _ = (model, input, voice, format);
        Err(LlmError::new(
            LlmErrorKind::BadRequest,
            format!(
                "le provider `{}` ne sait pas synthétiser la voix",
                self.name()
            ),
        ))
    }
}

/// Texte au plus par appel de synthèse vocale.
pub const SPEECH_MAX_CHARS: usize = 4_000;

/// Synthèse sur un endpoint `/audio/speech` OpenAI-compatible (mlx-audio, Kokoro-FastAPI…).
async fn speak_openai(
    request: reqwest::RequestBuilder,
    model: &str,
    input: &str,
    voice: &str,
    format: &str,
) -> Result<Vec<u8>> {
    if input.trim().is_empty() {
        return Err(LlmError::new(LlmErrorKind::BadRequest, "texte vide"));
    }
    if input.chars().count() > SPEECH_MAX_CHARS {
        return Err(LlmError::new(
            LlmErrorKind::BadRequest,
            format!("texte trop long pour une synthèse ({SPEECH_MAX_CHARS} caractères au plus)"),
        ));
    }
    let resp = request
        .json(&json!({
            "model": strip_provider(model),
            "input": input,
            "voice": voice,
            "response_format": format,
        }))
        .send()
        .await
        .map_err(map_reqwest_error)?;
    let status = resp.status().as_u16();
    let is_json = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("json"));
    let bytes = resp.bytes().await.map_err(map_reqwest_error)?.to_vec();
    if status >= 400 || is_json {
        return Err(LlmError::from_status(
            status.max(500),
            &String::from_utf8_lossy(&bytes),
        ));
    }
    if bytes.is_empty() {
        return Err(LlmError::new(LlmErrorKind::Other, "synthèse vide"));
    }
    Ok(bytes)
}

/// Taille maximale d'un envoi multipart de transcription (limite d'OpenAI et d'OpenRouter).
pub const TRANSCRIPTION_MAX_BYTES: usize = 25 * 1024 * 1024;

/// Envoie un audio en `multipart/form-data` à un endpoint `/audio/transcriptions`
/// OpenAI-compatible (OpenRouter, whisper.cpp, faster-whisper-server…).
async fn transcribe_multipart(
    request: reqwest::RequestBuilder,
    model: &str,
    audio: Vec<u8>,
    filename: &str,
    language: Option<&str>,
) -> Result<Transcription> {
    if audio.is_empty() {
        return Err(LlmError::new(LlmErrorKind::BadRequest, "audio vide"));
    }
    if audio.len() > TRANSCRIPTION_MAX_BYTES {
        return Err(LlmError::new(
            LlmErrorKind::BadRequest,
            format!(
                "audio trop gros pour une transcription ({} Mo, 25 au plus)",
                audio.len() / (1024 * 1024)
            ),
        ));
    }
    let mut form = reqwest::multipart::Form::new()
        .text("model", strip_provider(model).to_string())
        .text("response_format", "json")
        .part(
            "file",
            reqwest::multipart::Part::bytes(audio).file_name(filename.to_string()),
        );
    if let Some(l) = language.filter(|l| !l.is_empty()) {
        form = form.text("language", l.to_string());
    }
    let resp = request
        .multipart(form)
        .send()
        .await
        .map_err(map_reqwest_error)?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if status >= 400 {
        return Err(LlmError::from_status(status, &body));
    }
    let v: Value = serde_json::from_str(&body).map_err(|_| {
        LlmError::new(
            LlmErrorKind::Other,
            format!(
                "réponse de transcription illisible : {}",
                body.chars().take(200).collect::<String>()
            ),
        )
    })?;
    if v.get("error").is_some() {
        return Err(LlmError::from_status(status.max(500), &body));
    }
    let usage = v.get("usage");
    Ok(Transcription {
        text: v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .trim()
            .to_string(),
        seconds: usage
            .and_then(|u| u.get("seconds"))
            .and_then(|x| x.as_f64())
            .or_else(|| v.get("duration").and_then(|x| x.as_f64())),
        cost_usd: usage.and_then(|u| u.get("cost")).and_then(|x| x.as_f64()),
    })
}

/// Jeton d'annulation minimal (§3.3), sans dépendre de `tokio-util`.
pub mod tokio_util_lite {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Propagé à chaque appel LLM, outil et processus.
    ///
    /// Un jeton enfant suit son parent sans l'entraîner : annuler le run annule l'étape en
    /// cours, annuler une étape (délai dépassé) ne touche pas le run ni ses sœurs
    /// (issue #56).
    #[derive(Clone, Default)]
    pub struct CancelToken {
        flag: Arc<AtomicBool>,
        parent: Option<Arc<CancelToken>>,
    }

    impl CancelToken {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn cancel(&self) {
            self.flag.store(true, Ordering::SeqCst);
        }
        pub fn is_cancelled(&self) -> bool {
            self.flag.load(Ordering::SeqCst)
                || self.parent.as_ref().is_some_and(|p| p.is_cancelled())
        }
        /// Jeton enfant lié au parent : annuler le parent annule l'enfant, jamais
        /// l'inverse.
        pub fn child(&self) -> CancelToken {
            CancelToken {
                flag: Arc::new(AtomicBool::new(false)),
                parent: Some(Arc::new(self.clone())),
            }
        }
    }
}

pub use tokio_util_lite::CancelToken;

mod body;
mod openai_compat;
mod openrouter;
mod stream;
#[cfg(test)]
use body::message_to_json;
pub use body::*;
use openai_compat::embed_openai;
pub use openai_compat::*;
pub use openrouter::*;
pub use stream::*;
use stream::{local_window, map_reqwest_error};

/// Fabrique de providers à partir de la configuration.
pub struct ProviderSet {
    pub openrouter: Option<Arc<OpenRouterProvider>>,
    pub compat: Option<Arc<OpenAiCompatProvider>>,
    /// Backend Codex d'un abonnement ChatGPT, quand un compte est connecté (issue #142).
    pub codex: Option<Arc<crate::codex::CodexProvider>>,
    pub catalog: Catalog,
}

impl ProviderSet {
    pub fn get(&self, model_id: &str) -> Option<Arc<dyn Provider>> {
        match crate::catalog::provider_of(model_id) {
            // Un modèle `codex:` ne part **jamais** ailleurs : aucun autre fournisseur ne
            // le sert, et un repli silencieux enverrait `codex:gpt-6-astra` comme nom de
            // modèle à OpenRouter (issue #142). Sans compte connecté, pas de provider :
            // le routeur se replie sur l'alias suivant, en le disant.
            "codex" => self.codex.clone().map(|p| p as Arc<dyn Provider>),
            "openrouter" => self
                .openrouter
                .clone()
                .map(|p| p as Arc<dyn Provider>)
                .or_else(|| self.compat.clone().map(|p| p as Arc<dyn Provider>)),
            _ => self
                .compat
                .clone()
                .map(|p| p as Arc<dyn Provider>)
                .or_else(|| self.openrouter.clone().map(|p| p as Arc<dyn Provider>)),
        }
    }
}

#[cfg(test)]
mod tests;
