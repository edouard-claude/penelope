//! Providers : OpenRouter et endpoint OpenAI-compatible générique (§10.1).
//!
//! Le streaming SSE est **obligatoire** : le PRD exige un premier retour visible en
//! moins de 1,5 s (§1), et la machine d'état des appels (§4.3) s'appuie sur l'arrivée des
//! en-têtes pour distinguer `dispatching` de `response_started`.

use crate::catalog::{Catalog, ModelInfo, parse_openrouter_models, strip_provider};
use crate::sse::{SseDecoder, StreamAccumulator};
use crate::types::*;
use futures::StreamExt;
use serde_json::{Value, json};
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
}

/// Jeton d'annulation minimal (§3.3), sans dépendre de `tokio-util`.
pub mod tokio_util_lite {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Propagé à chaque appel LLM, outil et processus.
    #[derive(Clone, Default)]
    pub struct CancelToken(Arc<AtomicBool>);

    impl CancelToken {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn cancel(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
        pub fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        /// Jeton enfant lié au parent : annuler le parent annule l'enfant.
        pub fn child(&self) -> CancelToken {
            self.clone()
        }
    }
}

pub use tokio_util_lite::CancelToken;

// ---------------------------------------------------------------- OpenRouter

#[derive(Clone)]
pub struct OpenRouterProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    referer: String,
    title: String,
    routing: Value,
    catalog: Catalog,
}

impl OpenRouterProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        catalog: Catalog,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        Ok(OpenRouterProvider {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            referer: "https://github.com/penelope-agent/penelope".into(),
            title: "Penelope".into(),
            routing: Value::Null,
            catalog,
        })
    }

    pub fn with_routing(mut self, routing: Value) -> Self {
        self.routing = routing;
        self
    }

    pub fn with_identity(mut self, referer: impl Into<String>, title: impl Into<String>) -> Self {
        self.referer = referer.into();
        self.title = title.into();
        self
    }

    fn body(&self, req: &ChatRequest) -> Value {
        let mut b = to_openai_body(req);
        if let Some(obj) = b.as_object_mut() {
            obj.insert("usage".into(), json!({"include": true}));
            if !self.routing.is_null() {
                obj.insert("provider".into(), self.routing.clone());
            }
        }
        b
    }
}

#[async_trait::async_trait]
impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

    async fn chat_stream(&self, req: ChatRequest, cancel: CancelToken) -> Result<ChunkStream> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.body(&req);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", &self.referer)
            .header("X-Title", &self.title)
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        stream_from_response(resp, cancel, self.name().to_string()).await
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let status = resp.status().as_u16();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        if status >= 400 {
            return Err(LlmError::from_status(status, &body.to_string()));
        }
        let models = parse_openrouter_models(&body);
        self.catalog.replace(
            models.clone(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        );
        Ok(models)
    }
}

// ------------------------------------------------- OpenAI-compatible générique

#[derive(Clone)]
pub struct OpenAiCompatProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    label: String,
    catalog: Catalog,
}

impl OpenAiCompatProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        catalog: Catalog,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        Ok(OpenAiCompatProvider {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            label: "openai_compat".into(),
            catalog,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Embeddings (§6.11) : dimension lue à la première réponse et figée par index.
    pub async fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.base_url);
        let mut req = self.http.post(&url).json(&json!({
            "model": strip_provider(model),
            "input": inputs,
        }));
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req.send().await.map_err(map_reqwest_error)?;
        let status = resp.status().as_u16();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        if status >= 400 {
            return Err(LlmError::from_status(status, &body.to_string()));
        }
        let data = body.get("data").and_then(|d| d.as_array()).ok_or_else(|| {
            LlmError::new(LlmErrorKind::Other, "réponse d'embeddings sans `data`")
        })?;
        Ok(data
            .iter()
            .filter_map(|e| {
                e.get("embedding").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_f64().map(|f| f as f32))
                        .collect()
                })
            })
            .collect())
    }

    /// Transcription audio (§14.4, rôle `stt`).
    pub async fn transcribe(&self, model: &str, audio: Vec<u8>, filename: &str) -> Result<String> {
        let url = format!("{}/audio/transcriptions", self.base_url);
        let part = reqwest::multipart::Part::bytes(audio).file_name(filename.to_string());
        let form = reqwest::multipart::Form::new()
            .text("model", strip_provider(model).to_string())
            .part("file", part);
        let mut req = self.http.post(&url).multipart(form);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req.send().await.map_err(map_reqwest_error)?;
        let status = resp.status().as_u16();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        if status >= 400 {
            return Err(LlmError::from_status(status, &body.to_string()));
        }
        Ok(body
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string())
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        &self.label
    }

    async fn chat_stream(&self, req: ChatRequest, cancel: CancelToken) -> Result<ChunkStream> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut r = self
            .http
            .post(&url)
            .header("Accept", "text/event-stream")
            .json(&to_openai_body(&req));
        if !self.api_key.is_empty() {
            r = r.bearer_auth(&self.api_key);
        }
        let resp = r.send().await.map_err(map_reqwest_error)?;
        stream_from_response(resp, cancel, self.label.clone()).await
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.base_url);
        let mut r = self.http.get(&url);
        if !self.api_key.is_empty() {
            r = r.bearer_auth(&self.api_key);
        }
        let resp = r.send().await.map_err(map_reqwest_error)?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        let models: Vec<ModelInfo> = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
                    .map(|id| ModelInfo::minimal(id, "openai_compat", 32_768))
                    .collect()
            })
            .unwrap_or_default();
        self.catalog.upsert(models.clone());
        Ok(models)
    }
}

// ---------------------------------------------------------------- commun

/// Convertit une requête interne en corps « chat completions ».
pub fn to_openai_body(req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(message_to_json).collect();
    let mut b = json!({
        "model": strip_provider(&req.model),
        "messages": messages,
        "stream": true,
    });
    let obj = b.as_object_mut().expect("objet");
    if !req.tools.is_empty() {
        obj.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            ),
        );
        if let Some(tc) = req.tool_choice {
            obj.insert(
                "tool_choice".into(),
                Value::String(
                    match tc {
                        ToolChoice::Auto => "auto",
                        ToolChoice::None => "none",
                        ToolChoice::Required => "required",
                    }
                    .into(),
                ),
            );
        }
    }
    if let Some(t) = req.temperature {
        obj.insert("temperature".into(), json!(t));
    }
    if let Some(m) = req.max_tokens {
        obj.insert("max_tokens".into(), json!(m));
    }
    if let Some(r) = &req.reasoning_effort {
        obj.insert("reasoning".into(), json!({ "effort": r }));
    }
    if let Some(f) = &req.response_format {
        obj.insert("response_format".into(), f.clone());
    }
    b
}

fn message_to_json(m: &ChatMessage) -> Value {
    let content: Value = if m.content.len() == 1 && matches!(m.content[0], Content::Text { .. }) {
        let text = m.content[0].as_text().unwrap_or("").to_string();
        if m.cache_marker {
            // Forme « parties » avec `cache_control`, comprise par Anthropic via
            // OpenRouter ; les autres providers ignorent le champ supplémentaire.
            json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"}
            }])
        } else {
            Value::String(text)
        }
    } else {
        Value::Array(
            m.content
                .iter()
                .map(|c| match c {
                    Content::Text { text } => json!({"type":"text","text":text}),
                    Content::ImageUrl { url, detail } => json!({
                        "type":"image_url",
                        "image_url": {"url": url, "detail": detail.clone().unwrap_or_else(|| "auto".into())}
                    }),
                    Content::InputAudio { data, format } => json!({
                        "type":"input_audio",
                        "input_audio": {"data": data, "format": format}
                    }),
                })
                .collect(),
        )
    };

    let mut o = json!({"role": m.role.as_str(), "content": content});
    let obj = o.as_object_mut().expect("objet");
    if !m.tool_calls.is_empty() {
        obj.insert(
            "tool_calls".into(),
            Value::Array(
                m.tool_calls
                    .iter()
                    .map(|t| {
                        json!({
                            "id": t.id,
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "arguments": t.arguments.to_string(),
                            }
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(id) = &m.tool_call_id {
        obj.insert("tool_call_id".into(), json!(id));
    }
    if let Some(n) = &m.name {
        if m.role == Role::Tool {
            obj.insert("name".into(), json!(n));
        }
    }
    o
}

fn map_reqwest_error(e: reqwest::Error) -> LlmError {
    // Délai dépassé et connexion refusée sont tous deux transitoires : on réessaie.
    let kind = if e.is_timeout() || e.is_connect() {
        LlmErrorKind::Transient
    } else {
        LlmErrorKind::Other
    };
    LlmError::new(kind, e.to_string())
}

/// Transforme une réponse HTTP en flux de fragments.
///
/// Les en-têtes sont déjà reçus à ce stade : l'appel passe en `response_started` (§4.3).
async fn stream_from_response(
    resp: reqwest::Response,
    cancel: CancelToken,
    provider: String,
) -> Result<ChunkStream> {
    let status = resp.status().as_u16();
    if status >= 400 {
        let body = resp.text().await.unwrap_or_default();
        let mut e = LlmError::from_status(status, &body);
        // Un 5xx après envoi complet peut avoir été facturé.
        if status >= 500 {
            e = e.billed();
        }
        return Err(e);
    }

    let (tx, rx) = mpsc::channel::<StreamChunk>(64);
    tokio::spawn(async move {
        let mut decoder = SseDecoder::new();
        let mut acc = StreamAccumulator::new();
        let mut bytes = resp.bytes_stream();
        let mut finished = false;

        while let Some(next) = bytes.next().await {
            if cancel.is_cancelled() {
                let _ = tx
                    .send(StreamChunk::Done {
                        finish: FinishReason::Cancelled,
                    })
                    .await;
                return;
            }
            let chunk = match next {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx
                        .send(StreamChunk::Error {
                            message: format!("flux interrompu ({provider}) : {e}"),
                            retryable: true,
                        })
                        .await;
                    return;
                }
            };
            for payload in decoder.push(&chunk) {
                for out in acc.push_payload(&payload) {
                    if matches!(out, StreamChunk::Done { .. }) {
                        finished = true;
                    }
                    if tx.send(out).await.is_err() {
                        return; // le consommateur est parti
                    }
                }
            }
        }

        if !finished {
            // Flux coupé sans `[DONE]` : on clôt proprement avec ce qui a été reçu.
            for out in acc.push_payload("[DONE]") {
                if tx.send(out).await.is_err() {
                    return;
                }
            }
        }
    });

    Ok(rx)
}

/// Consomme un flux jusqu'au bout et reconstruit une réponse complète.
///
/// Utilisé par les appels non interactifs (classifieur, résumé, sub-agents).
pub async fn collect_stream(
    mut rx: ChunkStream,
    model: &str,
    provider: &str,
    catalog: &Catalog,
) -> Result<ChatResponse> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    let mut usage = Usage::default();
    let mut finish = FinishReason::Stop;
    let mut id = String::new();
    let mut actual_model = model.to_string();
    let mut usage_seen = false;

    while let Some(c) = rx.recv().await {
        match c {
            StreamChunk::Started { id: i, model: m } => {
                id = i;
                if !m.is_empty() {
                    actual_model = m;
                }
            }
            StreamChunk::Delta { text: t } => text.push_str(&t),
            StreamChunk::Reasoning { text: t } => reasoning.push_str(&t),
            StreamChunk::ToolCall(tc) => calls.push(tc),
            StreamChunk::Usage(u) => {
                usage = u;
                usage_seen = true;
            }
            StreamChunk::Done { finish: f } => finish = f,
            StreamChunk::Error { message, retryable } => {
                let kind = if retryable {
                    LlmErrorKind::Transient
                } else {
                    LlmErrorKind::Other
                };
                return Err(LlmError::new(kind, message).billed());
            }
        }
    }

    let info = catalog.get(&actual_model);
    let cost = info.as_ref().map(|i| i.cost(&usage)).unwrap_or(0.0);
    let message = ChatMessage {
        role: Role::Assistant,
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![Content::text(text)]
        },
        tool_calls: calls,
        tool_call_id: None,
        name: None,
        cache_marker: false,
    };

    Ok(ChatResponse {
        id,
        model: actual_model,
        provider: provider.to_string(),
        message,
        finish,
        usage,
        cost_usd: cost,
        cost_estimated: !usage_seen || info.is_none(),
        reasoning,
    })
}

/// Fabrique de providers à partir de la configuration.
pub struct ProviderSet {
    pub openrouter: Option<Arc<OpenRouterProvider>>,
    pub compat: Option<Arc<OpenAiCompatProvider>>,
    pub catalog: Catalog,
}

impl ProviderSet {
    pub fn get(&self, model_id: &str) -> Option<Arc<dyn Provider>> {
        match crate::catalog::provider_of(model_id) {
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
mod tests {
    use super::*;

    #[test]
    fn body_uses_string_content_for_plain_text() {
        let req = ChatRequest {
            model: "openrouter:a/b".into(),
            messages: vec![ChatMessage::user("salut")],
            ..Default::default()
        };
        let b = to_openai_body(&req);
        assert_eq!(b["model"], "a/b", "le préfixe de provider est retiré");
        assert_eq!(b["messages"][0]["content"], "salut");
        assert_eq!(b["stream"], true);
    }

    #[test]
    fn cache_marker_emits_cache_control() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![ChatMessage::system("préfixe stable").cached()],
            ..Default::default()
        };
        let b = to_openai_body(&req);
        assert_eq!(
            b["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn tool_calls_are_serialised_with_string_arguments() {
        let m = ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
            id: "c1".into(),
            name: "fs_read".into(),
            arguments: json!({"path":"a.rs"}),
        }]);
        let v = message_to_json(&m);
        assert_eq!(v["tool_calls"][0]["function"]["name"], "fs_read");
        assert_eq!(
            v["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
            "{\"path\":\"a.rs\"}"
        );
    }

    #[test]
    fn tool_result_message_carries_id_and_name() {
        let v = message_to_json(&ChatMessage::tool_result("c1", "fs_read", "contenu"));
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "c1");
        assert_eq!(v["name"], "fs_read");
    }

    #[test]
    fn images_use_the_parts_form() {
        let m = ChatMessage {
            role: Role::User,
            content: vec![
                Content::text("que vois-tu ?"),
                Content::ImageUrl {
                    url: "data:image/png;base64,AA".into(),
                    detail: None,
                },
            ],
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
            cache_marker: false,
        };
        let v = message_to_json(&m);
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][1]["type"], "image_url");
        assert_eq!(v["content"][1]["image_url"]["detail"], "auto");
    }

    #[test]
    fn reasoning_effort_is_forwarded() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![],
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        assert_eq!(to_openai_body(&req)["reasoning"]["effort"], "high");
    }

    #[test]
    fn cancel_token_propagates() {
        let t = CancelToken::new();
        let c = t.child();
        assert!(!c.is_cancelled());
        t.cancel();
        assert!(c.is_cancelled());
    }

    #[tokio::test]
    async fn collect_stream_builds_a_response() {
        let (tx, rx) = mpsc::channel(8);
        let catalog = Catalog::new();
        catalog.upsert(vec![{
            let mut m = ModelInfo::minimal("a/b", "openrouter", 128_000);
            m.price_prompt = 1e-6;
            m.price_completion = 2e-6;
            m
        }]);
        tokio::spawn(async move {
            tx.send(StreamChunk::Started {
                id: "id1".into(),
                model: "a/b".into(),
            })
            .await
            .unwrap();
            tx.send(StreamChunk::Delta {
                text: "bonjour".into(),
            })
            .await
            .unwrap();
            tx.send(StreamChunk::Usage(Usage {
                prompt: 1000,
                completion: 10,
                cached: 0,
                reasoning: 0,
            }))
            .await
            .unwrap();
            tx.send(StreamChunk::Done {
                finish: FinishReason::Stop,
            })
            .await
            .unwrap();
        });
        let r = collect_stream(rx, "a/b", "openrouter", &catalog)
            .await
            .unwrap();
        assert_eq!(r.message.text(), "bonjour");
        assert_eq!(r.usage.prompt, 1000);
        assert!((r.cost_usd - (1000.0 * 1e-6 + 10.0 * 2e-6)).abs() < 1e-12);
        assert!(!r.cost_estimated);
    }

    #[tokio::test]
    async fn collect_stream_propagates_errors() {
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            tx.send(StreamChunk::Error {
                message: "surcharge".into(),
                retryable: true,
            })
            .await
            .unwrap();
        });
        let e = collect_stream(rx, "m", "p", &Catalog::new())
            .await
            .unwrap_err();
        assert_eq!(e.kind, LlmErrorKind::Transient);
    }
}
