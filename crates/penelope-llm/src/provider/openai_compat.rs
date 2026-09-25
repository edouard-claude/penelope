//! Provider OpenAI-compatible générique (§10.1).

use super::*;

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
pub(super) async fn embed_openai(
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
