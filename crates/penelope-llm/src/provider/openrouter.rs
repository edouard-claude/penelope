//! Provider OpenRouter (§10.1).

use super::*;

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
    pub(super) fn body(&self, req: &ChatRequest, pinned_slug: Option<&str>) -> Value {
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
