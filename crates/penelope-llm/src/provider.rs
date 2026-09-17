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
    categories: String,
    routing: Value,
    catalog: Catalog,
    /// Identifiants de fournisseurs amont par modèle : nom affiché → slug de `provider.order`.
    slugs: SlugCache,
}

/// Par modèle : date de lecture et slugs de ses fournisseurs.
type SlugCache =
    Arc<tokio::sync::Mutex<HashMap<String, (std::time::Instant, HashMap<String, String>)>>>;

/// Durée de validité des identifiants de fournisseurs amont d'un modèle.
const SLUGS_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// Slugs des fournisseurs d'un modèle, lus dans `GET /models/{id}/endpoints` :
/// `provider_name` (« Z.AI ») → base du `tag` (« z-ai »), en minuscules.
pub fn endpoint_slugs(v: &Value) -> HashMap<String, String> {
    v["data"]["endpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let name = e["provider_name"].as_str()?;
            let tag = e["tag"].as_str()?;
            let base = tag.split('/').next().filter(|b| !b.is_empty())?;
            Some((name.to_lowercase(), base.to_string()))
        })
        .collect()
}

/// Longueur maximale d'un `session_id` accepté par OpenRouter.
const SESSION_ID_MAX: usize = 256;
/// Nombre de modèles de repli transmis dans `models`.
const MAX_FALLBACK_MODELS: usize = 3;

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
            referer: "https://github.com/edouard-claude/penelope".into(),
            title: "Penelope".into(),
            categories: "personal-agent".into(),
            routing: Value::Null,
            catalog,
            slugs: Default::default(),
        })
    }

    /// Slug du fournisseur amont `upstream` (nom affiché dans la réponse) pour `model`.
    async fn upstream_slug(&self, model: &str, upstream: &str) -> Option<String> {
        let model = strip_provider(model).to_string();
        let mut cache = self.slugs.lock().await;
        let fresh = cache
            .get(&model)
            .is_some_and(|(at, _)| at.elapsed() < SLUGS_TTL);
        if !fresh {
            let url = format!("{}/models/{model}/endpoints", self.base_url);
            let fetched = async {
                let resp = self
                    .http
                    .get(&url)
                    .bearer_auth(&self.api_key)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()?;
                resp.json::<Value>().await.ok()
            }
            .await;
            // Un échec est retenu aussi : pas de nouvelle requête avant l'expiration.
            let slugs = fetched.map(|v| endpoint_slugs(&v)).unwrap_or_default();
            cache.insert(model.clone(), (std::time::Instant::now(), slugs));
        }
        cache
            .get(&model)
            .and_then(|(_, s)| s.get(&upstream.to_lowercase()).cloned())
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

    pub fn with_categories(mut self, categories: impl Into<String>) -> Self {
        self.categories = categories.into();
        self
    }

    /// Corps OpenRouter : le corps commun, plus les champs propres à OpenRouter.
    ///
    /// L'usage (coût compris) arrive toujours dans le dernier fragment : rien à demander.
    fn body(&self, req: &ChatRequest, pinned_slug: Option<&str>) -> Value {
        let mut b = to_openai_body(req);
        let Some(obj) = b.as_object_mut() else {
            return b;
        };
        let mut routing = self.routing.clone();
        // Fournisseur amont collant (issue #17) : le cache de préfixe y est chaud. Un ordre
        // ou une liste imposés par la configuration gardent la main ; les replis restent
        // permis, et le fournisseur qui reprend devient le nouveau collant.
        if let Some(slug) = pinned_slug
            && routing.get("order").is_none()
            && routing.get("only").is_none()
        {
            if !routing.is_object() {
                routing = json!({});
            }
            routing["order"] = json!([slug]);
        }
        if !routing.is_null() {
            obj.insert("provider".into(), routing);
        }
        if let Some(sid) = req.session_id.as_deref().filter(|s| !s.is_empty()) {
            obj.insert(
                "session_id".into(),
                json!(sid.chars().take(SESSION_ID_MAX).collect::<String>()),
            );
        }
        // Replis tentés par OpenRouter avant le premier jeton, dans l'ordre.
        let primary = strip_provider(&req.model).to_string();
        let mut models: Vec<String> = Vec::new();
        for m in &req.fallback_models {
            if crate::catalog::provider_of(m) != "openrouter" {
                continue;
            }
            let id = strip_provider(m).to_string();
            if id != primary && !models.contains(&id) {
                models.push(id);
            }
        }
        models.truncate(MAX_FALLBACK_MODELS);
        if !models.is_empty() {
            obj.insert("models".into(), json!(models));
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
        let slug = match req.pinned_upstream.as_deref() {
            Some(upstream) => self.upstream_slug(&req.model, upstream).await,
            None => None,
        };
        let body = self.body(&req, slug.as_deref());
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", &self.referer)
            .header("X-OpenRouter-Title", &self.title)
            .header("X-OpenRouter-Categories", &self.categories)
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        stream_from_response(resp, cancel, self.name().to_string()).await
    }

    async fn transcribe(
        &self,
        model: &str,
        audio: Vec<u8>,
        filename: &str,
        language: Option<&str>,
    ) -> Result<Transcription> {
        let request = self
            .http
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", &self.referer)
            .header("X-OpenRouter-Title", &self.title)
            .header("X-OpenRouter-Categories", &self.categories);
        transcribe_multipart(request, model, audio, filename, language).await
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
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        &self.label
    }

    async fn transcribe(
        &self,
        model: &str,
        audio: Vec<u8>,
        filename: &str,
        language: Option<&str>,
    ) -> Result<Transcription> {
        let mut request = self
            .http
            .post(format!("{}/audio/transcriptions", self.base_url));
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        transcribe_multipart(request, model, audio, filename, language).await
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
    // Le raisonnement accompagne les messages qui appellent un outil, et eux seuls : c'est
    // là qu'il sert à enchaîner l'appel et sa suite. La règle ne dépend pas du tour en
    // cours, sinon le tour suivant réécrirait ces messages et casserait le cache de
    // préfixe (issue #17) ; une réponse finale ne le renvoie jamais.
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let mut v = message_to_json(m);
            if m.role == Role::Assistant
                && !m.tool_calls.is_empty()
                && let Some(o) = v.as_object_mut()
            {
                if let Some(d) = &m.reasoning_details {
                    o.insert("reasoning_details".into(), d.clone());
                } else if let Some(r) = &m.reasoning {
                    o.insert("reasoning".into(), json!(r));
                }
            }
            v
        })
        .collect();
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
    if !req.modalities.is_empty() {
        obj.insert("modalities".into(), json!(req.modalities));
    }
    b
}

fn message_to_json(m: &ChatMessage) -> Value {
    let content: Value = if m.content.is_empty() {
        // `content: []` est refusé ou mal lu par plusieurs providers : un message
        // d'appel d'outil sans texte porte `null`, les autres une chaîne vide.
        if m.tool_calls.is_empty() {
            Value::String(String::new())
        } else {
            Value::Null
        }
    } else if m.content.len() == 1 && matches!(m.content[0], Content::Text { .. }) {
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
    if let Some(n) = &m.name
        && m.role == Role::Tool
    {
        obj.insert("name".into(), json!(n));
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

/// Intervalle de vérification de l'annulation pendant un flux silencieux.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

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
        // 429 et 503 peuvent porter `Retry-After` (secondes).
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = resp.text().await.unwrap_or_default();
        let mut e = LlmError::from_status(status, &body);
        e.retry_after = retry_after;
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

        loop {
            // L'annulation est vérifiée même quand rien n'arrive (modèle qui réfléchit) :
            // couper la connexion arrête la génération, et sa facturation, chez les
            // providers qui le supportent.
            let next = match tokio::time::timeout(CANCEL_POLL, bytes.next()).await {
                Err(_) => {
                    if cancel.is_cancelled() {
                        let _ = tx
                            .send(StreamChunk::Done {
                                finish: FinishReason::Cancelled,
                            })
                            .await;
                        return;
                    }
                    continue;
                }
                Ok(None) => break,
                Ok(Some(next)) => next,
            };
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
                            error_type: None,
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
    rx: ChunkStream,
    model: &str,
    provider: &str,
    catalog: &Catalog,
) -> Result<ChatResponse> {
    collect_stream_observed(rx, model, provider, catalog, &|_| {}).await
}

/// Comme [`collect_stream`], mais chaque fragment est aussi présenté à `observe` au
/// moment où il arrive : c'est ce qui permet d'afficher la réponse pendant qu'elle
/// s'écrit (brouillons Telegram, `penelope chat`) sans lire le flux deux fois.
pub async fn collect_stream_observed(
    mut rx: ChunkStream,
    model: &str,
    provider: &str,
    catalog: &Catalog,
    observe: &(dyn Fn(&StreamChunk) + Send + Sync),
) -> Result<ChatResponse> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut reasoning_parts: Vec<Value> = Vec::new();
    let mut refusal = String::new();
    let mut images: Vec<String> = Vec::new();
    let mut calls = Vec::new();
    let mut usage = Usage::default();
    let mut finish = FinishReason::Stop;
    let mut id = String::new();
    let mut actual_model = model.to_string();
    let mut upstream = None;
    let mut native_finish = None;

    while let Some(c) = rx.recv().await {
        observe(&c);
        match c {
            StreamChunk::Started { id: i, model: m } => {
                id = i;
                if !m.is_empty() {
                    actual_model = m;
                }
            }
            StreamChunk::Delta { text: t } => text.push_str(&t),
            StreamChunk::Reasoning { text: t } => reasoning.push_str(&t),
            StreamChunk::ReasoningDetails(v) => reasoning_parts.push(v),
            StreamChunk::Refusal { text: t } => refusal.push_str(&t),
            StreamChunk::Image { url } => images.push(url),
            StreamChunk::Meta {
                upstream: u,
                native_finish: n,
            } => {
                upstream = u.or(upstream);
                native_finish = n.or(native_finish);
            }
            StreamChunk::ToolCall(tc) => calls.push(tc),
            StreamChunk::Usage(u) => usage = u,
            StreamChunk::Done { finish: f } => finish = f,
            StreamChunk::Error {
                message,
                retryable,
                error_type,
            } => {
                return Err(LlmError::mid_stream(message, retryable, error_type));
            }
        }
    }

    // Le coût facturé par OpenRouter fait foi ; à défaut, estimation du catalogue.
    let (cost, cost_estimated) = match usage.cost_usd {
        Some(c) => (c, false),
        None => (
            catalog
                .get(&actual_model)
                .map(|i| i.cost(&usage))
                .unwrap_or(0.0),
            true,
        ),
    };
    let mut content = if text.is_empty() {
        Vec::new()
    } else {
        vec![Content::text(text)]
    };
    content.extend(
        images
            .into_iter()
            .map(|url| Content::ImageUrl { url, detail: None }),
    );
    let message = ChatMessage {
        role: Role::Assistant,
        content,
        tool_calls: calls,
        tool_call_id: None,
        name: None,
        cache_marker: false,
        reasoning: (!reasoning.is_empty()).then(|| reasoning.clone()),
        reasoning_details: crate::sse::merge_reasoning_details(&reasoning_parts),
    };

    Ok(ChatResponse {
        id,
        model: actual_model,
        provider: provider.to_string(),
        message,
        finish,
        usage,
        cost_usd: cost,
        cost_estimated,
        reasoning,
        upstream,
        native_finish,
        refusal: (!refusal.is_empty()).then_some(refusal),
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
    fn reasoning_goes_back_only_with_tool_calls() {
        let with_reasoning = |text: &str| ChatMessage {
            reasoning: Some(format!("pensée {text}")),
            reasoning_details: Some(json!([{"type":"reasoning.text","text":text,"index":0}])),
            ..ChatMessage::assistant(text)
        };
        let req = ChatRequest {
            model: "openrouter:a/b".into(),
            messages: vec![
                ChatMessage::user("ancien"),
                ChatMessage {
                    tool_calls: vec![ToolCall {
                        id: "c0".into(),
                        name: "fs_list".into(),
                        arguments: json!({}),
                    }],
                    content: vec![],
                    ..with_reasoning("appel ancien")
                },
                ChatMessage::tool_result("c0", "fs_list", "liste"),
                with_reasoning("ancienne réponse"),
                ChatMessage::user("nouveau"),
                ChatMessage {
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "fs_read".into(),
                        arguments: json!({}),
                    }],
                    content: vec![],
                    ..with_reasoning("appel")
                },
                ChatMessage::tool_result("c1", "fs_read", "contenu"),
            ],
            ..Default::default()
        };
        let b = to_openai_body(&req);
        assert_eq!(
            b["messages"][1]["reasoning_details"][0]["text"], "appel ancien",
            "un appel d'outil d'un tour précédent garde le sien : le préfixe ne bouge pas"
        );
        assert!(
            b["messages"][3].get("reasoning_details").is_none(),
            "réponse finale : rien"
        );
        assert_eq!(b["messages"][5]["reasoning_details"][0]["text"], "appel");
        assert!(
            b["messages"][5].get("reasoning").is_none(),
            "les blocs priment sur le texte"
        );
    }

    #[test]
    fn empty_content_is_never_an_empty_array() {
        let call = ChatMessage {
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({}),
            }],
            content: vec![],
            ..ChatMessage::assistant("")
        };
        let req = ChatRequest {
            model: "openrouter:a/b".into(),
            messages: vec![
                call,
                ChatMessage {
                    content: vec![],
                    ..ChatMessage::user("")
                },
            ],
            ..Default::default()
        };
        let b = to_openai_body(&req);
        assert!(b["messages"][0]["content"].is_null(), "{b}");
        assert_eq!(b["messages"][1]["content"], "");
    }

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
            reasoning: None,
            reasoning_details: None,
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
                ..Default::default()
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
        assert!(
            r.cost_estimated,
            "sans `usage.cost`, le coût vient du catalogue"
        );
    }

    #[tokio::test]
    async fn billed_cost_wins_over_the_catalog() {
        let (tx, rx) = mpsc::channel(8);
        let catalog = Catalog::new();
        catalog.upsert(vec![{
            let mut m = ModelInfo::minimal("a/b", "openrouter", 128_000);
            m.price_prompt = 1e-6;
            m
        }]);
        tokio::spawn(async move {
            for c in [
                StreamChunk::Started {
                    id: "gen-1".into(),
                    model: "a/b".into(),
                },
                StreamChunk::Meta {
                    upstream: Some("Z.AI".into()),
                    native_finish: None,
                },
                StreamChunk::Refusal { text: "non".into() },
                StreamChunk::Meta {
                    upstream: None,
                    native_finish: Some("refusal".into()),
                },
                StreamChunk::Usage(Usage {
                    prompt: 1000,
                    completion: 10,
                    cost_usd: Some(0.5),
                    ..Default::default()
                }),
                StreamChunk::Done {
                    finish: FinishReason::ContentFilter,
                },
            ] {
                tx.send(c).await.unwrap();
            }
        });
        let r = collect_stream(rx, "a/b", "openrouter", &catalog)
            .await
            .unwrap();
        assert_eq!(r.cost_usd, 0.5);
        assert!(!r.cost_estimated);
        assert_eq!(r.upstream.as_deref(), Some("Z.AI"));
        assert_eq!(r.native_finish.as_deref(), Some("refusal"));
        assert_eq!(r.refusal.as_deref(), Some("non"));
    }

    fn openrouter() -> OpenRouterProvider {
        OpenRouterProvider::new("http://127.0.0.1:9/api/v1", "sk-or-v1-test", Catalog::new())
            .unwrap()
    }

    #[test]
    fn openrouter_body_carries_session_and_fallbacks() {
        let req = ChatRequest {
            model: "openrouter:z-ai/glm-5.3".into(),
            messages: vec![ChatMessage::user("salut")],
            session_id: Some("01J9SESSION".into()),
            fallback_models: vec![
                "openrouter:z-ai/glm-5.3".into(),
                "openrouter:deepseek/deepseek-v4-flash".into(),
                "openai_compat:local".into(),
                "openrouter:deepseek/deepseek-v4-flash".into(),
            ],
            ..Default::default()
        };
        let b = openrouter().body(&req, None);
        assert_eq!(b["model"], "z-ai/glm-5.3");
        assert_eq!(b["session_id"], "01J9SESSION");
        assert_eq!(b["models"], json!(["deepseek/deepseek-v4-flash"]));
        assert!(
            b.get("usage").is_none(),
            "l'usage arrive toujours, rien à demander"
        );
        assert!(b.get("provider").is_none(), "pas de préférences par défaut");

        let long = ChatRequest {
            model: "a/b".into(),
            session_id: Some("x".repeat(400)),
            ..Default::default()
        };
        let b = openrouter().body(&long, None);
        assert_eq!(b["session_id"].as_str().unwrap().len(), 256);
        assert!(b.get("models").is_none());
    }

    /// Issue #17 : le fournisseur amont de l'appel précédent passe en tête de
    /// `provider.order`, sauf ordre imposé par la configuration.
    #[test]
    fn the_previous_upstream_is_pinned_unless_routing_is_configured() {
        let slugs = endpoint_slugs(&json!({"data": {"endpoints": [
            {"provider_name": "Z.AI", "tag": "z-ai/fp8"},
            {"provider_name": "Sail Research", "tag": "sail-research"},
            {"provider_name": "sans tag"}
        ]}}));
        assert_eq!(slugs.get("z.ai").map(String::as_str), Some("z-ai"));
        assert_eq!(
            slugs.get("sail research").map(String::as_str),
            Some("sail-research")
        );
        assert_eq!(slugs.len(), 2);

        let req = ChatRequest {
            model: "openrouter:z-ai/glm-5.3".into(),
            messages: vec![ChatMessage::user("salut")],
            ..Default::default()
        };
        let b = openrouter().body(&req, Some("z-ai"));
        assert_eq!(b["provider"], json!({"order": ["z-ai"]}));
        let configured = openrouter().with_routing(json!({"order": ["together"], "sort": "price"}));
        assert_eq!(
            configured.body(&req, Some("z-ai"))["provider"]["order"],
            json!(["together"])
        );
        let sorted = openrouter().with_routing(json!({"sort": "price"}));
        assert_eq!(
            sorted.body(&req, Some("z-ai"))["provider"],
            json!({"sort": "price", "order": ["z-ai"]})
        );
    }

    /// Faux serveur HTTP : répond une fois avec les octets fournis, puis garde la
    /// connexion ouverte le temps indiqué.
    async fn one_shot_server(response: String, hold: std::time::Duration) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).await;
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            tokio::time::sleep(hold).await;
        });
        format!("http://{addr}/api/v1")
    }

    #[tokio::test]
    async fn rate_limits_keep_the_retry_after_hint() {
        let body = r#"{"error":{"code":429,"message":"Rate limit exceeded","metadata":{"error_type":"rate_limit_exceeded"}}}"#;
        let resp = format!(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let url = one_shot_server(resp, std::time::Duration::from_millis(10)).await;
        let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
        let err = p
            .chat_stream(
                ChatRequest {
                    model: "a/b".into(),
                    messages: vec![ChatMessage::user("x")],
                    ..Default::default()
                },
                CancelToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, LlmErrorKind::RateLimited);
        assert_eq!(err.retry_after, Some(7));
    }

    #[tokio::test]
    async fn a_documented_stream_is_decoded_end_to_end() {
        let events = [
            ": OPENROUTER PROCESSING",
            r#"data: {"id":"gen-1","object":"chat.completion.chunk","created":1,"model":"z-ai/glm-5.3","provider":"Z.AI","choices":[{"index":0,"delta":{"role":"assistant","content":"Bon"},"finish_reason":null}]}"#,
            r#"data: {"id":"gen-1","object":"chat.completion.chunk","created":1,"model":"z-ai/glm-5.3","provider":"Z.AI","choices":[{"index":0,"delta":{"content":"jour"},"finish_reason":"stop","native_finish_reason":"stop"}]}"#,
            r#"data: {"id":"gen-1","object":"chat.completion.chunk","created":1,"model":"z-ai/glm-5.3","provider":"Z.AI","choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop","native_finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15,"cost":0.00042}}"#,
            "data: [DONE]",
        ];
        let sse: String = events.iter().map(|e| format!("{e}\n\n")).collect();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{sse}"
        );
        let url = one_shot_server(resp, std::time::Duration::from_millis(10)).await;
        let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
        let rx = p
            .chat_stream(
                ChatRequest {
                    model: "openrouter:z-ai/glm-5.3".into(),
                    messages: vec![ChatMessage::user("x")],
                    ..Default::default()
                },
                CancelToken::new(),
            )
            .await
            .unwrap();
        let r = collect_stream(rx, "z-ai/glm-5.3", "openrouter", &Catalog::new())
            .await
            .unwrap();
        assert_eq!(r.message.text(), "Bonjour");
        assert_eq!(r.id, "gen-1");
        assert_eq!(r.cost_usd, 0.00042);
        assert!(!r.cost_estimated);
        assert_eq!(r.upstream.as_deref(), Some("Z.AI"));
    }

    /// Faux serveur qui capture la requête reçue avant de répondre.
    async fn capturing_server(
        response: String,
    ) -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut got = Vec::new();
            let mut buf = vec![0u8; 65536];
            // Lit jusqu'à la fin du corps multipart (délimiteur final `--\r\n`).
            loop {
                let n = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    sock.read(&mut buf),
                )
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                if got.ends_with(b"--\r\n") {
                    break;
                }
            }
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            let _ = tx.send(got);
        });
        (format!("http://{addr}/v1"), rx)
    }

    #[tokio::test]
    async fn local_whisper_transcription_uses_the_openai_multipart_form() {
        let body = r#"{"text":" Bonjour Pénélope, rappelle-moi demain. "}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, seen) = capturing_server(resp).await;
        let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
        let t = p
            .transcribe(
                "openai_compat:whisper",
                b"OggS-fake-audio".to_vec(),
                "voice.ogg",
                Some("fr"),
            )
            .await
            .unwrap();
        assert_eq!(t.text, "Bonjour Pénélope, rappelle-moi demain.");
        assert_eq!(t.cost_usd, None);

        let raw = String::from_utf8_lossy(&seen.await.unwrap()).to_string();
        assert!(raw.starts_with("POST /v1/audio/transcriptions"), "{raw}");
        assert!(raw.contains("multipart/form-data"), "{raw}");
        assert!(
            raw.contains("name=\"file\"; filename=\"voice.ogg\""),
            "{raw}"
        );
        assert!(raw.contains("name=\"language\"\r\n\r\nfr"), "{raw}");
        assert!(raw.contains("name=\"model\"\r\n\r\nwhisper"), "{raw}");
        assert!(
            !raw.to_lowercase().contains("authorization"),
            "pas de clé : pas d'en-tête"
        );
    }

    #[tokio::test]
    async fn openrouter_transcription_reports_its_cost() {
        let body = r#"{"text":"Salut","usage":{"seconds":9.2,"total_tokens":113,"cost":0.000508}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, seen) = capturing_server(resp).await;
        let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
        let t = p
            .transcribe(
                "openrouter:openai/whisper-large-v3",
                b"ID3".to_vec(),
                "a.mp3",
                None,
            )
            .await
            .unwrap();
        assert_eq!(t.text, "Salut");
        assert_eq!(t.cost_usd, Some(0.000508));
        assert_eq!(t.seconds, Some(9.2));
        let raw = String::from_utf8_lossy(&seen.await.unwrap()).to_lowercase();
        assert!(raw.contains("authorization: bearer sk-or-v1-test"), "{raw}");
        assert!(raw.contains("x-openrouter-title"), "{raw}");
        assert!(raw.contains("openai/whisper-large-v3"), "{raw}");

        let empty = p
            .transcribe("m", Vec::new(), "a.ogg", None)
            .await
            .unwrap_err();
        assert_eq!(empty.kind, LlmErrorKind::BadRequest);
    }

    #[tokio::test]
    async fn cancelling_a_silent_stream_does_not_wait_for_the_next_byte() {
        // Le serveur n'envoie que les en-têtes puis se tait (modèle qui réfléchit).
        let resp =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n: OPENROUTER PROCESSING\n\n"
                .to_string();
        let url = one_shot_server(resp, std::time::Duration::from_secs(30)).await;
        let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
        let cancel = CancelToken::new();
        let mut rx = p
            .chat_stream(
                ChatRequest {
                    model: "a/b".into(),
                    messages: vec![ChatMessage::user("x")],
                    ..Default::default()
                },
                cancel.clone(),
            )
            .await
            .unwrap();
        let started = std::time::Instant::now();
        cancel.cancel();
        let last = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("l'annulation doit clore le flux sans attendre le serveur");
        assert!(matches!(
            last,
            Some(StreamChunk::Done {
                finish: FinishReason::Cancelled
            })
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn collect_stream_propagates_errors() {
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            tx.send(StreamChunk::Error {
                message: "surcharge".into(),
                retryable: true,
                error_type: None,
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
