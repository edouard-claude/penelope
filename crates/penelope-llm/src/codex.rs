//! Fournisseur `codex` : les modèles d'un abonnement ChatGPT, par le backend Codex
//! (issue #142).
//!
//! Le backend ne parle pas `chat/completions` mais l'**API Responses** en flux : une
//! liste d'items typés en entrée (`message`, `function_call`, `function_call_output`,
//! `reasoning`), des événements `response.*` en sortie, une fin sur `response.completed`
//! et **pas de `[DONE]`**. Il est sans état : `store: false`, et le raisonnement chiffré
//! doit être réinjecté au tour suivant, sinon il est perdu.
//!
//! Ce module ne connaît **pas** le magasin de secrets : le jeton d'accès lui vient d'un
//! [`TokenSource`] que le daemon fournit (il détient la connexion, la rotation et le
//! verrou). `penelope-llm` ne dépend donc pas de `penelope-mcp` ni de la plateforme.
//!
//! Identité : le backend filtre l'en-tête `originator` et sert un catalogue qui en
//! dépend. Pénélope envoie celle de Codex CLI, **la même sur toutes les requêtes**
//! (`/responses`, `/models`) : une identité incohérente vaut des heures de « servers
//! overloaded ». C'est une usurpation assumée, tolérée par OpenAI, jamais garantie.

use crate::catalog::{Catalog, ModelInfo, strip_provider};
use crate::provider::{CancelToken, ChunkStream, Provider, stream_from_response};
use crate::sse::EventAccumulator;
use crate::types::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Jeton d'accès courant du compte ChatGPT connecté.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexToken {
    pub access_token: String,
    /// `chatgpt_account_id` du jeton d'identité, envoyé en `ChatGPT-Account-ID`.
    pub account_id: String,
    /// Plan du compte (`plus`, `pro`, `business`), pour l'affichage et les quotas.
    pub plan_type: String,
    /// Compte FedRAMP : en-tête `X-OpenAI-Fedramp: true`.
    pub fedramp: bool,
}

/// Source du jeton d'accès, tenue par le daemon.
///
/// Le fournisseur ne rafraîchit jamais lui-même : il demande, et sur 401 il demande un
/// jeton neuf **une fois**. La rotation du `refresh_token` est à usage unique — deux
/// rafraîchissements concurrents déconnectent le compte pour de bon — donc elle est
/// sérialisée là où vit le magasin de secrets.
#[async_trait::async_trait]
pub trait TokenSource: Send + Sync {
    /// Jeton valide pour un appel.
    async fn token(&self) -> Result<CodexToken>;
    /// Jeton neuf après un 401 : force un rafraîchissement.
    async fn refreshed(&self) -> Result<CodexToken>;
}

/// Ce que le fournisseur doit savoir de la configuration (`[providers.codex]`).
#[derive(Debug, Clone)]
pub struct CodexOptions {
    pub base_url: String,
    pub originator: String,
    pub client_version: String,
    pub reasoning_summary: String,
    pub verbosity: String,
    pub stream_idle: std::time::Duration,
    /// Modèles servis, en repli quand `GET /models` ne répond pas.
    pub models: Vec<String>,
    /// Part de la fenêtre de quota au-delà de laquelle le fournisseur se met en retrait.
    pub quota_stop_ratio: f64,
}

impl Default for CodexOptions {
    fn default() -> Self {
        CodexOptions {
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            originator: "codex_cli_rs".into(),
            client_version: "0.104.0".into(),
            reasoning_summary: "auto".into(),
            verbosity: "medium".into(),
            stream_idle: crate::provider::DEFAULT_STREAM_IDLE,
            models: Vec::new(),
            quota_stop_ratio: 0.95,
        }
    }
}

/// Une fenêtre de quota du plan, telle que le backend la renvoie.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct QuotaWindow {
    /// Part consommée, en pourcents.
    pub used_percent: f64,
    /// Largeur de la fenêtre, en minutes.
    pub window_minutes: u64,
    /// Remise à zéro, en secondes depuis l'époque.
    pub reset_at: i64,
}

/// Instantané des deux fenêtres de quota (5 h et hebdomadaire).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Quota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<QuotaWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<QuotaWindow>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plan_type: String,
    /// Fenêtre qui borne réellement (`x-codex-active-limit`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_limit: String,
    /// Lecture, en millisecondes depuis l'époque.
    #[serde(default)]
    pub read_at_ms: i64,
}

impl Quota {
    /// Part consommée la plus élevée des deux fenêtres, en fraction de 1.
    pub fn worst_ratio(&self) -> f64 {
        [self.primary, self.secondary]
            .into_iter()
            .flatten()
            .map(|w| w.used_percent / 100.0)
            .fold(0.0, f64::max)
    }
    /// Vrai si l'instantané ne dit rien.
    pub fn is_empty(&self) -> bool {
        self.primary.is_none() && self.secondary.is_none()
    }
}

/// Lit les jauges `x-codex-*` des en-têtes d'une réponse.
pub fn quota_from_headers(headers: &reqwest::header::HeaderMap, now_ms: i64) -> Quota {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let window = |prefix: &str| {
        let used = get(&format!("x-codex-{prefix}-used-percent"))?
            .trim()
            .parse::<f64>()
            .ok()?;
        Some(QuotaWindow {
            used_percent: used,
            window_minutes: get(&format!("x-codex-{prefix}-window-minutes"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            reset_at: get(&format!("x-codex-{prefix}-reset-at"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
        })
    };
    Quota {
        primary: window("primary"),
        secondary: window("secondary"),
        plan_type: get("x-codex-plan-type").unwrap_or_default().to_string(),
        active_limit: get("x-codex-active-limit").unwrap_or_default().to_string(),
        read_at_ms: now_ms,
    }
}

/// Lit l'événement SSE `codex.rate_limits`.
pub fn quota_from_event(v: &Value, now_ms: i64) -> Quota {
    let window = |k: &str| {
        let w = v.get("rate_limits")?.get(k)?;
        let used = w.get("used_percent").and_then(|x| x.as_f64())?;
        Some(QuotaWindow {
            used_percent: used,
            window_minutes: w
                .get("window_minutes")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            reset_at: w.get("reset_at").and_then(|x| x.as_i64()).unwrap_or(0),
        })
    };
    Quota {
        primary: window("primary"),
        secondary: window("secondary"),
        plan_type: v
            .get("plan_type")
            .and_then(|p| p.as_str())
            .unwrap_or_default()
            .to_string(),
        active_limit: String::new(),
        read_at_ms: now_ms,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Où vont les jauges du plan, lues à chaque réponse : le daemon les range et alerte.
/// Appelé depuis le flux, donc **sans attente** : à l'implémentation de faire vite.
pub trait QuotaSink: Send + Sync {
    fn record(&self, quota: Quota);
}

#[derive(Clone)]
pub struct CodexProvider {
    http: reqwest::Client,
    opts: CodexOptions,
    tokens: Arc<dyn TokenSource>,
    catalog: Catalog,
    /// Identifiant d'installation, stable, envoyé sur toutes les requêtes.
    installation_id: String,
    /// Dernier instantané de quota lu, en en-tête ou en événement.
    quota: Arc<Mutex<Quota>>,
    /// Où le publier, quand quelqu'un l'écoute.
    sink: Option<Arc<dyn QuotaSink>>,
}

impl CodexProvider {
    pub fn new(
        opts: CodexOptions,
        tokens: Arc<dyn TokenSource>,
        catalog: Catalog,
        installation_id: impl Into<String>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
        Ok(CodexProvider {
            http,
            opts: CodexOptions {
                base_url: opts.base_url.trim_end_matches('/').to_string(),
                ..opts
            },
            tokens,
            catalog,
            installation_id: installation_id.into(),
            quota: Arc::new(Mutex::new(Quota::default())),
            sink: None,
        })
    }

    /// Publie chaque jauge lue (kv, alerte au propriétaire).
    pub fn with_quota_sink(mut self, sink: Arc<dyn QuotaSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Idem, quand l'appelant n'a peut-être personne à prévenir.
    pub fn maybe_quota_sink(mut self, sink: Option<Arc<dyn QuotaSink>>) -> Self {
        self.sink = sink;
        self
    }

    /// Dernier état des jauges du plan, vide tant qu'aucun appel n'a abouti.
    pub fn quota(&self) -> Quota {
        self.quota.lock().map(|q| q.clone()).unwrap_or_default()
    }

    fn remember(&self, q: Quota) {
        if q.is_empty() {
            return;
        }
        if let Ok(mut slot) = self.quota.lock() {
            *slot = q.clone();
        }
        if let Some(sink) = &self.sink {
            sink.record(q);
        }
    }

    /// `User-Agent` du client Codex, OS et architecture compris.
    fn user_agent(&self) -> String {
        format!(
            "{}/{} ({} {}; {})",
            self.opts.originator,
            self.opts.client_version,
            std::env::consts::OS,
            os_version(),
            std::env::consts::ARCH
        )
    }

    /// En-têtes communs à toutes les requêtes : l'identité doit être **la même** partout.
    fn identity(&self, r: reqwest::RequestBuilder, t: &CodexToken) -> reqwest::RequestBuilder {
        let mut r = r
            .bearer_auth(&t.access_token)
            .header("originator", &self.opts.originator)
            .header("User-Agent", self.user_agent())
            .header("x-codex-installation-id", &self.installation_id);
        if !t.account_id.is_empty() {
            r = r.header("ChatGPT-Account-ID", &t.account_id);
        }
        if t.fedramp {
            r = r.header("X-OpenAI-Fedramp", "true");
        }
        r
    }

    /// Un appel `POST /responses`, jeton donné. Séparé pour que le 401 puisse le rejouer
    /// une fois, avec un jeton neuf, avant que le moindre octet ne soit parti.
    async fn post_responses(&self, body: &Value, t: &CodexToken) -> Result<reqwest::Response> {
        let url = format!("{}/responses", self.opts.base_url);
        let session = body
            .get("prompt_cache_key")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let mut r = self
            .identity(self.http.post(&url), t)
            .header("Accept", "text/event-stream")
            .json(body);
        // `session-id` et `thread-id` valent la clé de cache de préfixe : le backend
        // regroupe, et le cache reste chaud sur toute la session (issue #17).
        if !session.is_empty() {
            r = r
                .header("session-id", &session)
                .header("thread-id", &session);
        }
        r.send().await.map_err(map_reqwest_error)
    }
}

/// Version de l'OS, telle que Codex CLI l'annonce ; inconnue, elle vaut `0.0.0` plutôt
/// qu'un `User-Agent` tronqué (un en-tête incohérent est puni par le backend).
fn os_version() -> String {
    std::env::var("PENELOPE_OS_VERSION").unwrap_or_else(|_| "0.0.0".into())
}

fn map_reqwest_error(e: reqwest::Error) -> LlmError {
    let kind = if e.is_timeout() || e.is_connect() {
        LlmErrorKind::Transient
    } else {
        LlmErrorKind::Other
    };
    LlmError::new(kind, e.to_string())
}

#[async_trait::async_trait]
impl Provider for CodexProvider {
    fn name(&self) -> &str {
        "codex"
    }

    async fn chat_stream(&self, req: ChatRequest, cancel: CancelToken) -> Result<ChunkStream> {
        // Quota épuisé : inutile d'appeler pour se faire refuser, le routeur se replie.
        let quota = self.quota();
        if !quota.is_empty() && quota.worst_ratio() >= self.opts.quota_stop_ratio {
            let mut e = LlmError::new(
                LlmErrorKind::RateLimited,
                format!(
                    "quota ChatGPT à {:.0} % de la fenêtre : Pénélope se met en retrait \
                     avant la panne",
                    quota.worst_ratio() * 100.0
                ),
            );
            e.error_type = Some(USAGE_LIMIT_REACHED.into());
            e.retry_after = reset_in_seconds(&quota);
            return Err(e);
        }
        let body = to_responses_body(&req, &self.opts);
        let mut token = self.tokens.token().await?;
        let mut resp = self.post_responses(&body, &token).await?;
        // 401 : un rafraîchissement, **un seul** rejeu, avant le premier octet.
        if resp.status().as_u16() == 401 {
            token = self.tokens.refreshed().await?;
            resp = self.post_responses(&body, &token).await?;
        }
        self.remember(quota_from_headers(resp.headers(), now_ms()));
        let served = resp
            .headers()
            .get("openai-model")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let status = resp.status().as_u16();
        if status >= 400 {
            let headers = resp.headers().clone();
            let text = resp.text().await.unwrap_or_default();
            return Err(codex_error(status, &text, &headers));
        }
        stream_from_response(
            resp,
            cancel,
            self.name().to_string(),
            self.opts.stream_idle,
            Box::new(
                ResponsesAccumulator::new(served, self.quota.clone())
                    .with_quota_sink(self.sink.clone()),
            ),
        )
        .await
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        let token = self.tokens.token().await?;
        let url = format!(
            "{}/models?client_version={}",
            self.opts.base_url, self.opts.client_version
        );
        let fetched = async {
            let resp = self
                .identity(self.http.get(&url), &token)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
                .map_err(map_reqwest_error)?;
            self.remember(quota_from_headers(resp.headers(), now_ms()));
            let status = resp.status().as_u16();
            let body: Value = resp
                .json()
                .await
                .map_err(|e| LlmError::new(LlmErrorKind::Other, e.to_string()))?;
            if status >= 400 {
                return Err(LlmError::from_status(status, &body.to_string()));
            }
            Ok(parse_codex_models(&body))
        }
        .await;
        // Le catalogue du plan n'est qu'un confort : sans lui, la liste de repli suffit à
        // router. Une panne de `/models` ne doit pas empêcher un tour (issue #11).
        let models = match fetched {
            Ok(m) if !m.is_empty() => m,
            Ok(_) => fallback_models(&self.opts.models),
            Err(e) => {
                tracing::warn!(error = %e, "catalogue Codex indisponible : liste de repli");
                fallback_models(&self.opts.models)
            }
        };
        // `upsert`, jamais `replace` : le catalogue OpenRouter reste en place.
        self.catalog.upsert(models.clone());
        Ok(models)
    }
}

/// Type d'erreur du backend quand le quota du plan est épuisé.
pub const USAGE_LIMIT_REACHED: &str = "usage_limit_reached";

/// Secondes avant la remise à zéro de la fenêtre la plus chargée.
fn reset_in_seconds(q: &Quota) -> Option<u64> {
    let now = now_ms() / 1000;
    [q.primary, q.secondary]
        .into_iter()
        .flatten()
        .filter(|w| w.reset_at > now)
        .map(|w| (w.reset_at - now) as u64)
        .max()
}

/// Classe une erreur HTTP du backend Codex.
///
/// Un 429 de quota n'est **jamais** rejoué : il faut attendre `resets_at`, que le message
/// dit en clair pour que le propriétaire distingue un quota d'une panne (issue #139).
pub fn codex_error(status: u16, body: &str, headers: &reqwest::header::HeaderMap) -> LlmError {
    let v = serde_json::from_str::<Value>(body).ok();
    let err = v.as_ref().and_then(|v| v.get("error"));
    let error_type = err
        .and_then(|e| e.get("type").or_else(|| e.get("code")))
        .and_then(|t| t.as_str())
        .map(String::from);
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(String::from)
        .unwrap_or_else(|| body.chars().take(400).collect());

    let mut e = match error_type.as_deref() {
        Some(USAGE_LIMIT_REACHED) => {
            let resets_at = err
                .and_then(|e| e.get("resets_at"))
                .and_then(|r| r.as_i64())
                .unwrap_or(0);
            let wait = (resets_at - now_ms() / 1000).max(0) as u64;
            let mut e = LlmError::new(
                LlmErrorKind::RateLimited,
                format!("quota ChatGPT atteint : {message}"),
            );
            if wait > 0 {
                e.retry_after = Some(wait);
            }
            e
        }
        Some("usage_not_included" | "insufficient_quota") => {
            LlmError::new(LlmErrorKind::PaymentRequired, message)
        }
        Some("context_length_exceeded") => LlmError::new(LlmErrorKind::ContextLength, message),
        Some("rate_limit_exceeded" | "slow_down") => {
            LlmError::new(LlmErrorKind::RateLimited, message)
        }
        _ if status == 403 => LlmError::new(
            LlmErrorKind::Auth,
            format!(
                "{message} — le backend Codex refuse ce client. L'en-tête `originator` ou la \
                 version annoncée n'est probablement plus acceptée : voir \
                 `providers.codex.originator` et `providers.codex.client_version`."
            ),
        ),
        _ => LlmError::from_status(status, body),
    };
    e.status = Some(status);
    if e.error_type.is_none() {
        e.error_type = error_type;
    }
    if e.retry_after.is_none() {
        e.retry_after = headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
    }
    if status >= 500 {
        e = e.billed();
    }
    e
}

// ------------------------------------------------------------------ requête

/// Convertit une requête interne en corps de l'API Responses.
///
/// `store: false` et `stream: true` sont de fait obligatoires ; `include` réclame le
/// raisonnement chiffré, qui est réinjecté au tour suivant (backend sans état).
pub fn to_responses_body(req: &ChatRequest, opts: &CodexOptions) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => {
                let text = m.text();
                if !text.trim().is_empty() {
                    instructions.push(text);
                }
            }
            Role::Tool => {
                // Un résultat d'outil se rattache à son appel par `call_id`, jamais par
                // sa place dans la liste.
                let call_id = m.tool_call_id.clone().unwrap_or_default();
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": m.text(),
                }));
            }
            Role::Assistant => {
                // Le raisonnement chiffré du tour précédent précède l'appel qu'il a
                // produit : sans lui, le backend repart sans sa réflexion.
                for item in reasoning_items(m) {
                    input.push(item);
                }
                let text = m.text();
                if !text.trim().is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
                for c in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "name": c.name,
                        "arguments": c.arguments.to_string(),
                        "call_id": c.id,
                    }));
                }
            }
            Role::User => input.push(json!({
                "type": "message",
                "role": "user",
                "content": user_content(m),
            })),
        }
    }

    let mut b = json!({
        "model": strip_provider(&req.model),
        "instructions": instructions.join("\n\n"),
        "input": input,
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    let obj = b.as_object_mut().expect("objet");
    if !req.tools.is_empty() {
        obj.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    // L'API Responses met la fonction **à plat**, sans objet `function`.
                    .map(|t| {
                        json!({
                            "type": "function",
                            "name": t.name,
                            "description": t.description,
                            "strict": false,
                            "parameters": t.parameters,
                        })
                    })
                    .collect(),
            ),
        );
        obj.insert(
            "tool_choice".into(),
            json!(match req.tool_choice {
                Some(ToolChoice::None) => "none",
                Some(ToolChoice::Required) => "required",
                _ => "auto",
            }),
        );
        obj.insert("parallel_tool_calls".into(), json!(true));
    }
    let mut reasoning = serde_json::Map::new();
    if let Some(effort) = &req.reasoning_effort {
        reasoning.insert("effort".into(), json!(effort));
    }
    if !opts.reasoning_summary.is_empty() {
        reasoning.insert("summary".into(), json!(opts.reasoning_summary));
    }
    if !reasoning.is_empty() {
        obj.insert("reasoning".into(), Value::Object(reasoning));
    }
    let mut text = serde_json::Map::new();
    if !opts.verbosity.is_empty() {
        text.insert("verbosity".into(), json!(opts.verbosity));
    }
    if let Some(f) = &req.response_format {
        text.insert("format".into(), f.clone());
    }
    if !text.is_empty() {
        obj.insert("text".into(), Value::Object(text));
    }
    if let Some(m) = req.max_tokens {
        obj.insert("max_output_tokens".into(), json!(m));
    }
    if let Some(sid) = req.session_id.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("prompt_cache_key".into(), json!(sid));
    }
    b
}

/// Items `reasoning` à réinjecter pour un message d'assistant : ceux que le backend a
/// rendus, avec leur contenu chiffré. Tout autre bloc (OpenRouter) est ignoré.
fn reasoning_items(m: &ChatMessage) -> Vec<Value> {
    let Some(details) = &m.reasoning_details else {
        return Vec::new();
    };
    let items: Vec<&Value> = match details {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter(|item| {
            item.get("type").and_then(|t| t.as_str()) == Some("reasoning")
                && item.get("encrypted_content").is_some()
        })
        .cloned()
        .collect()
}

/// Contenu d'un message utilisateur : texte et images, au dialecte Responses.
fn user_content(m: &ChatMessage) -> Value {
    if m.content.is_empty() {
        return json!([{"type": "input_text", "text": ""}]);
    }
    Value::Array(
        m.content
            .iter()
            .map(|c| match c {
                Content::Text { text } => json!({"type": "input_text", "text": text}),
                Content::ImageUrl { url, .. } => json!({"type": "input_image", "image_url": url}),
                // Le backend Codex ne prend pas l'audio : le texte porte la mention.
                Content::InputAudio { format, .. } => {
                    json!({"type": "input_text", "text": format!("[audio {format} non transmis]")})
                }
            })
            .collect(),
    )
}

// ------------------------------------------------------------------- réponse

/// Accumule les événements `response.*` d'un flux Responses.
pub struct ResponsesAccumulator {
    /// Modèle réellement servi, lu dans l'en-tête `openai-model`.
    served_model: String,
    started: bool,
    saw_tool_call: bool,
    completed: bool,
    finish: Option<FinishReason>,
    quota: Arc<Mutex<Quota>>,
    sink: Option<Arc<dyn QuotaSink>>,
}

impl ResponsesAccumulator {
    pub fn new(served_model: String, quota: Arc<Mutex<Quota>>) -> Self {
        ResponsesAccumulator {
            served_model,
            started: false,
            saw_tool_call: false,
            completed: false,
            finish: None,
            quota: quota.clone(),
            sink: None,
        }
    }

    /// Publie les jauges lues en cours de flux (`codex.rate_limits`).
    pub fn with_quota_sink(mut self, sink: Option<Arc<dyn QuotaSink>>) -> Self {
        self.sink = sink;
        self
    }

    /// Accumulateur nu, pour les tests et les appels sans jauge partagée.
    pub fn plain() -> Self {
        Self::new(String::new(), Arc::new(Mutex::new(Quota::default())))
    }

    fn start(&mut self, v: &Value, out: &mut Vec<StreamChunk>) {
        if self.started {
            return;
        }
        self.started = true;
        let resp = v.get("response");
        // Le modèle réellement servi est celui de l'en-tête `openai-model` ; celui de
        // l'événement ne vient qu'à défaut (issue #142, repli côté serveur).
        let model = if self.served_model.is_empty() {
            resp.and_then(|r| r.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string()
        } else {
            self.served_model.clone()
        };
        out.push(StreamChunk::Started {
            id: resp
                .and_then(|r| r.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string(),
            model,
        });
    }
}

impl EventAccumulator for ResponsesAccumulator {
    fn push_payload(&mut self, data: &str) -> Vec<StreamChunk> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return vec![];
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        let mut out: Vec<StreamChunk> = Vec::new();
        let text_of = |key: &str| {
            v.get(key)
                .and_then(|d| d.as_str())
                .filter(|d| !d.is_empty())
                .map(String::from)
        };
        match kind {
            "response.created" => self.start(&v, &mut out),
            "response.output_text.delta" => {
                self.start(&v, &mut out);
                if let Some(text) = text_of("delta") {
                    out.push(StreamChunk::Delta { text });
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.start(&v, &mut out);
                if let Some(text) = text_of("delta") {
                    out.push(StreamChunk::Reasoning { text });
                }
            }
            // Un appel de fonction arrive **complet** ici : Codex ignore les deltas
            // d'arguments, et l'identifiant est celui du serveur (issue #54).
            "response.output_item.done" => {
                self.start(&v, &mut out);
                let Some(item) = v.get("item") else {
                    return out;
                };
                match item
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                {
                    "function_call" => {
                        let name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|i| i.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if !name.is_empty() && !call_id.is_empty() {
                            self.saw_tool_call = true;
                            out.push(StreamChunk::ToolCall(ToolCall {
                                id: call_id,
                                name,
                                arguments: crate::sse::parse_arguments(
                                    item.get("arguments")
                                        .and_then(|a| a.as_str())
                                        .unwrap_or("{}"),
                                ),
                            }));
                        }
                    }
                    // Le raisonnement chiffré repart tel quel au tour suivant.
                    "reasoning" if item.get("encrypted_content").is_some() => {
                        out.push(StreamChunk::ReasoningDetails(json!([item])));
                    }
                    _ => {}
                }
            }
            "response.completed" => {
                self.start(&v, &mut out);
                self.completed = true;
                if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                    out.push(StreamChunk::Usage(parse_responses_usage(u)));
                }
                let finish = self.finish.unwrap_or(if self.saw_tool_call {
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
                });
                out.push(StreamChunk::Done { finish });
            }
            "response.incomplete" => {
                self.start(&v, &mut out);
                self.completed = true;
                let reason = v
                    .get("response")
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or_default()
                    .to_string();
                if reason == "max_output_tokens" {
                    if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                        out.push(StreamChunk::Usage(parse_responses_usage(u)));
                    }
                    out.push(StreamChunk::Done {
                        finish: FinishReason::Length,
                    });
                } else {
                    out.push(StreamChunk::Error {
                        message: format!("réponse incomplète ({reason})"),
                        retryable: true,
                        error_type: (!reason.is_empty()).then_some(reason),
                    });
                }
            }
            "response.failed" => {
                self.completed = true;
                let err = v
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .or_else(|| v.get("error"));
                let error_type = err
                    .and_then(|e| e.get("code").or_else(|| e.get("type")))
                    .and_then(|c| c.as_str())
                    .map(String::from);
                let message = err
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("le backend Codex a abandonné la réponse")
                    .to_string();
                let retryable = matches!(
                    error_type.as_deref(),
                    Some("rate_limit_exceeded" | "slow_down" | "server_error" | "overloaded")
                );
                out.push(StreamChunk::Error {
                    message,
                    retryable,
                    error_type,
                });
            }
            "codex.rate_limits" => {
                let q = quota_from_event(&v, now_ms());
                if !q.is_empty() {
                    if let Ok(mut slot) = self.quota.lock() {
                        *slot = q.clone();
                    }
                    if let Some(sink) = &self.sink {
                        sink.record(q);
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Le backend clôt sur `response.completed`. Une fermeture avant est une coupure, pas
    /// une fin : la dire permet au tour de relancer plutôt que de garder un texte tronqué.
    fn on_eof(&mut self) -> Vec<StreamChunk> {
        if self.completed {
            return vec![];
        }
        vec![StreamChunk::Error {
            message: "flux Codex fermé sans `response.completed`".into(),
            retryable: true,
            error_type: None,
        }]
    }
}

/// Usage d'un `response.completed`.
pub fn parse_responses_usage(u: &Value) -> Usage {
    let get = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Usage {
        prompt: get("input_tokens"),
        completion: get("output_tokens"),
        cached: u
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        cache_write: get("cache_write_tokens"),
        reasoning: u
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        // L'abonnement ne facture pas l'appel : le coût est connu, il vaut zéro.
        cost_usd: Some(0.0),
    }
}

// ----------------------------------------------------------------- catalogue

/// Fenêtre annoncée par le plan quand `/models` ne dit rien.
pub const DEFAULT_CODEX_WINDOW: u64 = 272_000;

/// Analyse `GET /models` du backend Codex. Aucun prix : l'abonnement ne facture pas.
pub fn parse_codex_models(body: &Value) -> Vec<ModelInfo> {
    body.get("models")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(parse_codex_model).collect())
        .unwrap_or_default()
}

fn parse_codex_model(m: &Value) -> Option<ModelInfo> {
    let slug = m.get("slug")?.as_str()?.to_string();
    let window = m
        .get("max_context_window")
        .or_else(|| m.get("context_window"))
        .and_then(|w| w.as_u64())
        .filter(|w| *w > 0)
        .unwrap_or(DEFAULT_CODEX_WINDOW);
    let list = |k: &str| -> Vec<String> {
        m.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut input = list("input_modalities");
    if input.is_empty() {
        input.push("text".into());
    }
    let efforts = m
        .get("supported_reasoning_levels")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        });
    let mut supported = vec!["tools".to_string(), "tool_choice".to_string()];
    if efforts.is_some() {
        supported.push("reasoning".into());
    }
    Some(ModelInfo {
        id: slug.clone(),
        name: slug,
        provider: "codex".into(),
        context_window: window,
        max_output: None,
        price_prompt: 0.0,
        price_completion: 0.0,
        price_cached_read: 0.0,
        price_image: 0.0,
        input_modalities: input,
        output_modalities: vec!["text".into()],
        supported_parameters: supported,
        price_cache_write: 0.0,
        reasoning_efforts: efforts,
        reasoning_mandatory: false,
    })
}

/// Catalogue de repli : ce que la configuration déclare, sans appel réseau.
pub fn fallback_models(models: &[String]) -> Vec<ModelInfo> {
    models
        .iter()
        .map(|slug| ModelInfo {
            provider: "codex".into(),
            input_modalities: vec!["text".into(), "image".into()],
            reasoning_efforts: Some(vec![
                "minimal".into(),
                "low".into(),
                "medium".into(),
                "high".into(),
            ]),
            ..ModelInfo::minimal(slug, "codex", DEFAULT_CODEX_WINDOW)
        })
        .collect()
}

#[cfg(test)]
mod tests;
