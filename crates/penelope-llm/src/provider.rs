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
    /// Silence toléré pendant un flux (issue #51).
    stream_idle: std::time::Duration,
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
        // Le délai global est large : c'est l'inactivité du flux qui borne un tour, pas
        // sa durée (issue #51). Une longue réponse de raisonnement n'est plus coupée.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
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
            stream_idle: DEFAULT_STREAM_IDLE,
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

    /// Silence toléré pendant un flux ; zéro : aucun (issue #51).
    pub fn with_stream_idle(mut self, idle: std::time::Duration) -> Self {
        self.stream_idle = idle;
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

    async fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let req = self
            .http
            .post(format!("{}/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", &self.referer)
            .header("X-OpenRouter-Title", &self.title)
            .timeout(std::time::Duration::from_secs(60));
        embed_openai(req, model, inputs).await
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

        stream_from_response(
            resp,
            cancel,
            self.name().to_string(),
            self.stream_idle,
            Box::new(StreamAccumulator::new()),
        )
        .await
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
    /// Silence toléré pendant un flux (issue #51).
    stream_idle: std::time::Duration,
    /// Fenêtre annoncée pour les modèles dont `GET /models` ne dit rien (issue #53).
    window: u64,
}

impl OpenAiCompatProvider {
    /// Fenêtre annoncée pour un modèle dont l'endpoint ne dit pas la sienne (issue #53).
    pub fn with_window(mut self, window: u64) -> Self {
        if window > 0 {
            self.window = window;
        }
        self
    }

    /// Silence toléré pendant un flux ; zéro : aucun (issue #51).
    pub fn with_stream_idle(mut self, idle: std::time::Duration) -> Self {
        self.stream_idle = idle;
        self
    }

    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        catalog: Catalog,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        Ok(OpenAiCompatProvider {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            label: "openai_compat".into(),
            catalog,
            stream_idle: DEFAULT_STREAM_IDLE,
            window: DEFAULT_LOCAL_WINDOW,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// `POST {base}/embeddings` au format OpenAI, commun à OpenRouter et aux serveurs
/// compatibles.
async fn embed_openai(
    request: reqwest::RequestBuilder,
    model: &str,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>> {
    let resp = request
        .json(&json!({"model": strip_provider(model), "input": inputs}))
        .send()
        .await
        .map_err(map_reqwest_error)?;
    let status = resp.status().as_u16();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
    if status >= 400 || body.get("error").is_some() {
        return Err(LlmError::from_status(status.max(400), &body.to_string()));
    }
    let vectors = parse_embeddings(&body)?;
    if vectors.len() != inputs.len() {
        return Err(LlmError::new(
            LlmErrorKind::Other,
            format!("{} embeddings pour {} textes", vectors.len(), inputs.len()),
        ));
    }
    Ok(vectors)
}

/// Vecteurs d'une réponse d'embeddings, remis dans l'ordre de `index`.
pub fn parse_embeddings(body: &Value) -> Result<Vec<Vec<f32>>> {
    let data = body
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| LlmError::new(LlmErrorKind::Other, "réponse d'embeddings sans `data`"))?;
    let mut indexed: Vec<(usize, Vec<f32>)> = data
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let v = e.get("embedding")?.as_array()?;
            let index = e
                .get("index")
                .and_then(|x| x.as_u64())
                .map_or(i, |x| x as usize);
            Some((
                index,
                v.iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect(),
            ))
        })
        .collect();
    indexed.sort_by_key(|(i, _)| *i);
    Ok(indexed.into_iter().map(|(_, v)| v).collect())
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        &self.label
    }

    async fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut req = self.http.post(format!("{}/embeddings", self.base_url));
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        embed_openai(req, model, inputs).await
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

    async fn speak(&self, model: &str, input: &str, voice: &str, format: &str) -> Result<Vec<u8>> {
        let mut request = self.http.post(format!("{}/audio/speech", self.base_url));
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        speak_openai(request, model, input, voice, format).await
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
        stream_from_response(
            resp,
            cancel,
            self.label.clone(),
            self.stream_idle,
            Box::new(StreamAccumulator::new()),
        )
        .await
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
                    .filter(|m| m.get("id").and_then(|i| i.as_str()).is_some())
                    .map(|m| {
                        let id = m["id"].as_str().unwrap_or_default();
                        ModelInfo::minimal(id, "openai_compat", local_window(m, self.window))
                    })
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
        // Sans cela, un serveur local (vLLM, llama.cpp, LM Studio, mlx_lm) ne renvoie
        // jamais `usage` en streaming : plus de comptage de tokens, ni de compaction sur
        // la taille réelle (issue #53).
        "stream_options": {"include_usage": true},
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
    // Raisonnement : soit éteint, soit budgété (issue #152). `max_tokens` borne la
    // sortie raisonnement compris chez OpenRouter ; un budget de raisonnement à part est
    // ce qui empêche la réflexion de manger la réponse.
    match (&req.reasoning_effort, req.reasoning_max_tokens) {
        // `none` n'est pas un niveau d'effort : c'est l'extinction. OpenRouter l'entend
        // par `enabled: false` ; envoyé comme `effort: "none"`, plusieurs modèles
        // l'ignorent et réfléchissent quand même, jusqu'à dépenser tout `max_tokens`
        // avant d'écrire la moindre réponse.
        (Some(r), _) if r == "none" => {
            obj.insert(
                "reasoning".into(),
                json!({"enabled": false, "exclude": true}),
            );
        }
        // `max_tokens` et `effort` s'excluent dans le paramètre unifié : le budget, plus
        // précis, gagne.
        (_, Some(budget)) => {
            obj.insert("reasoning".into(), json!({"max_tokens": budget}));
        }
        (Some(r), None) => {
            obj.insert("reasoning".into(), json!({ "effort": r }));
        }
        (None, None) => {}
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

/// Silence toléré par défaut pendant un flux (issue #51).
pub const DEFAULT_STREAM_IDLE: std::time::Duration = std::time::Duration::from_secs(120);

/// Fenêtre par défaut d'un modèle servi par un endpoint OpenAI-compatible.
pub const DEFAULT_LOCAL_WINDOW: u64 = 32_768;

/// Fenêtre d'un modèle local : ce que `GET /models` en dit (vLLM, llama.cpp, LM Studio),
/// sinon la valeur configurée (issue #53).
fn local_window(m: &Value, configured: u64) -> u64 {
    for path in [
        "context_length",
        "max_model_len",
        "max_context_length",
        "n_ctx",
    ] {
        for value in [m.get(path), m.get("meta").and_then(|x| x.get(path))] {
            if let Some(n) = value.and_then(|v| v.as_u64()).filter(|n| *n > 0) {
                return n;
            }
        }
    }
    configured
}

/// Transforme une réponse HTTP en flux de fragments.
///
/// Les en-têtes sont déjà reçus à ce stade : l'appel passe en `response_started` (§4.3).
pub(crate) async fn stream_from_response(
    resp: reqwest::Response,
    cancel: CancelToken,
    provider: String,
    idle: std::time::Duration,
    mut acc: Box<dyn EventAccumulator>,
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
        let mut bytes = resp.bytes_stream();
        let mut finished = false;
        let mut last_data = std::time::Instant::now();

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
                    // Fournisseur muet après ses en-têtes : on coupe et on laisse la
                    // relance jouer, plutôt que d'attendre le délai global (issue #51).
                    if !idle.is_zero() && last_data.elapsed() >= idle {
                        let _ = tx
                            .send(StreamChunk::Error {
                                message: format!(
                                    "flux muet ({provider}) : aucune donnée depuis {} s",
                                    idle.as_secs()
                                ),
                                retryable: true,
                                error_type: None,
                            })
                            .await;
                        return;
                    }
                    continue;
                }
                Ok(None) => break,
                Ok(Some(next)) => next,
            };
            // Tout octet reçu, commentaire SSE compris, prouve que le flux vit.
            last_data = std::time::Instant::now();
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
            // Flux coupé avant sa fin : à l'accumulateur de dire ce que ça vaut.
            for out in acc.on_eof() {
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
