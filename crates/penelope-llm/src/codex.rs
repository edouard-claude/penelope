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
        })
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
            *slot = q;
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
            Box::new(ResponsesAccumulator::new(served, self.quota.clone())),
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
        }
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
                if let Ok(mut slot) = self.quota.lock() {
                    let q = quota_from_event(&v, now_ms());
                    if !q.is_empty() {
                        *slot = q;
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
mod tests {
    use super::*;

    fn opts() -> CodexOptions {
        CodexOptions::default()
    }

    fn chunks(acc: &mut ResponsesAccumulator, events: &[Value]) -> Vec<StreamChunk> {
        events
            .iter()
            .flat_map(|e| acc.push_payload(&e.to_string()))
            .collect()
    }

    /// Serveur simulé : répond dans l'ordre du script, garde chaque requête entière.
    async fn scripted_server(script: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        tokio::spawn(async move {
            let mut queue = script.into_iter();
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut got: Vec<u8> = Vec::new();
                let mut buf = vec![0u8; 16_384];
                loop {
                    let n = tokio::time::timeout(
                        std::time::Duration::from_millis(200),
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
                    if let Some(head) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                        let text = String::from_utf8_lossy(&got[..head]).to_lowercase();
                        let len: usize = text
                            .split("content-length:")
                            .nth(1)
                            .and_then(|r| r.split('\r').next())
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if got.len() - (head + 4) >= len {
                            break;
                        }
                    }
                }
                recorder
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&got).to_string());
                let (status, body) = queue.next().unwrap_or((500, "{}".to_string()));
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    /// Source de jetons de test : compte les rafraîchissements.
    struct FakeTokens {
        refreshes: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl TokenSource for FakeTokens {
        async fn token(&self) -> Result<CodexToken> {
            Ok(CodexToken {
                access_token: "jeton-1".into(),
                account_id: "acc_1".into(),
                plan_type: "pro".into(),
                fedramp: false,
            })
        }
        async fn refreshed(&self) -> Result<CodexToken> {
            self.refreshes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(CodexToken {
                access_token: "jeton-2".into(),
                ..self.token().await?
            })
        }
    }

    fn provider(base_url: &str, refreshes: Arc<std::sync::atomic::AtomicUsize>) -> CodexProvider {
        CodexProvider::new(
            CodexOptions {
                base_url: base_url.to_string(),
                models: vec!["gpt-6-astra".into()],
                ..CodexOptions::default()
            },
            Arc::new(FakeTokens { refreshes }),
            Catalog::new(),
            "inst-1",
        )
        .unwrap()
    }

    fn request(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            session_id: Some("s-42".into()),
            messages: vec![ChatMessage::user("salut")],
            ..Default::default()
        }
    }

    /// #142 : la **même** identité part sur `/responses` et sur `/models` — une identité
    /// incohérente vaut des heures de « servers overloaded ».
    #[tokio::test]
    async fn every_request_carries_the_same_identity() {
        let sse = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n\
                   data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n";
        let (url, seen) = scripted_server(vec![
            (200, sse.to_string()),
            (
                200,
                json!({"models": [{"slug": "gpt-6-astra"}]}).to_string(),
            ),
        ])
        .await;
        let p = provider(&url, Default::default());
        let _ = p
            .chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
            .await
            .expect("flux");
        p.fetch_models().await.expect("catalogue");

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        for r in &reqs {
            let lower = r.to_lowercase();
            assert!(lower.contains("originator: codex_cli_rs"), "{r}");
            assert!(lower.contains("user-agent: codex_cli_rs/"), "{r}");
            assert!(lower.contains("chatgpt-account-id: acc_1"), "{r}");
            assert!(lower.contains("x-codex-installation-id: inst-1"), "{r}");
            assert!(lower.contains("authorization: bearer jeton-1"), "{r}");
        }
        // Le catalogue est demandé pour la version de client annoncée.
        assert!(reqs[1].contains("client_version=0.104.0"), "{}", reqs[1]);
        // La session nomme le cache de préfixe, en corps comme en en-tête.
        assert!(
            reqs[0].to_lowercase().contains("session-id: s-42"),
            "{}",
            reqs[0]
        );
    }

    /// #142 : un 401 vaut **un** rafraîchissement et **un** rejeu, pas deux.
    #[tokio::test]
    async fn a_401_is_refreshed_once_and_replayed_once() {
        let sse = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n\
                   data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n";
        let (url, seen) = scripted_server(vec![
            (401, json!({"error": {"message": "expiré"}}).to_string()),
            (200, sse.to_string()),
        ])
        .await;
        let refreshes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = provider(&url, refreshes.clone());
        p.chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
            .await
            .expect("le rejeu passe");
        assert_eq!(refreshes.load(std::sync::atomic::Ordering::SeqCst), 1);
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2, "un rejeu, pas deux");
        assert!(
            reqs[1].to_lowercase().contains("bearer jeton-2"),
            "{}",
            reqs[1]
        );

        // Un second 401 d'affilée ne se rejoue pas : c'est une erreur d'authentification.
        let (url, _) = scripted_server(vec![
            (401, "{}".to_string()),
            (401, "{}".to_string()),
            (200, sse.to_string()),
        ])
        .await;
        let p = provider(&url, Default::default());
        let e = p
            .chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
            .await
            .expect_err("401 persistant");
        assert_eq!(e.kind, LlmErrorKind::Auth);
    }

    /// #142 : passé le seuil, le fournisseur se met en retrait **avant** l'appel : le
    /// routeur se replie au lieu d'aller chercher un 429.
    #[tokio::test]
    async fn a_spent_quota_steps_aside_before_calling() {
        let (url, seen) = scripted_server(vec![(200, String::new())]).await;
        let p = provider(&url, Default::default());
        p.remember(Quota {
            primary: Some(QuotaWindow {
                used_percent: 96.0,
                window_minutes: 300,
                reset_at: now_ms() / 1000 + 600,
            }),
            ..Default::default()
        });
        let e = p
            .chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
            .await
            .expect_err("quota");
        assert_eq!(e.kind, LlmErrorKind::RateLimited);
        assert_eq!(e.error_type.as_deref(), Some(USAGE_LIMIT_REACHED));
        assert!(e.retry_after.unwrap_or(0) > 0);
        assert!(seen.lock().unwrap().is_empty(), "aucun appel n'est parti");
    }

    /// #142 : le corps est celui de l'API Responses — items typés, outils à plat, pas de
    /// stockage côté serveur, cache de préfixe nommé.
    #[test]
    fn the_body_speaks_the_responses_dialect() {
        let req = ChatRequest {
            model: "codex:gpt-6-astra".into(),
            session_id: Some("s-42".into()),
            messages: vec![
                ChatMessage::system("Tu es Pénélope."),
                ChatMessage::user("liste les projets"),
            ],
            tools: vec![ToolDef::new(
                "fs_list",
                "liste un répertoire",
                json!({"type": "object"}),
            )],
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        let b = to_responses_body(&req, &opts());
        assert_eq!(
            b["model"], "gpt-6-astra",
            "le préfixe ne part pas au serveur"
        );
        assert_eq!(b["instructions"], "Tu es Pénélope.");
        assert_eq!(b["store"], false);
        assert_eq!(b["stream"], true);
        assert_eq!(b["include"][0], "reasoning.encrypted_content");
        assert_eq!(b["prompt_cache_key"], "s-42");
        assert_eq!(b["input"][0]["type"], "message");
        assert_eq!(b["input"][0]["content"][0]["type"], "input_text");
        // L'outil est à plat : pas d'objet `function` intermédiaire.
        assert_eq!(b["tools"][0]["name"], "fs_list");
        assert_eq!(b["tools"][0]["type"], "function");
        assert!(b["tools"][0].get("function").is_none());
        assert_eq!(b["tool_choice"], "auto");
        assert_eq!(b["reasoning"]["effort"], "medium");
        assert_eq!(b["reasoning"]["summary"], "auto");
        assert_eq!(b["text"]["verbosity"], "medium");
    }

    /// #142 : le backend est sans état — le raisonnement chiffré revient avec l'appel
    /// qu'il a produit, et le résultat d'outil se rattache par `call_id`.
    #[test]
    fn encrypted_reasoning_and_tool_results_go_back() {
        let assistant = ChatMessage {
            tool_calls: vec![ToolCall {
                id: "call_7".into(),
                name: "fs_list".into(),
                arguments: json!({"path": "."}),
            }],
            content: vec![],
            reasoning_details: Some(json!([
                {"type": "reasoning", "id": "rs_1", "encrypted_content": "chiffré", "summary": []}
            ])),
            ..ChatMessage::assistant("")
        };
        let req = ChatRequest {
            model: "codex:gpt-6-astra".into(),
            messages: vec![
                ChatMessage::user("regarde"),
                assistant,
                ChatMessage::tool_result("call_7", "fs_list", "a.rs\nb.rs"),
                ChatMessage::user("et maintenant ?"),
            ],
            ..Default::default()
        };
        let b = to_responses_body(&req, &opts());
        let input = b["input"].as_array().expect("liste");
        assert_eq!(
            input[1]["type"], "reasoning",
            "avant l'appel qu'il a produit"
        );
        assert_eq!(input[1]["encrypted_content"], "chiffré");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_7");
        assert_eq!(input[2]["arguments"], r#"{"path":"."}"#);
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_7");
        assert_eq!(input[3]["output"], "a.rs\nb.rs");
    }

    /// #142 : texte, appel d'outil complet et usage, depuis les événements `response.*`.
    #[test]
    fn the_stream_yields_text_a_tool_call_and_usage() {
        let mut acc = ResponsesAccumulator::new("gpt-6-astra".into(), Default::default());
        let out = chunks(
            &mut acc,
            &[
                json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-6"}}),
                json!({"type": "response.output_text.delta", "delta": "bon"}),
                json!({"type": "response.reasoning_summary_text.delta", "delta": "je réfléchis"}),
                json!({"type": "response.output_item.done", "item": {
                    "type": "reasoning", "id": "rs_1", "encrypted_content": "chiffré"}}),
                json!({"type": "response.output_item.done", "item": {
                    "type": "function_call", "name": "fs_list",
                    "arguments": "{\"path\":\".\"}", "call_id": "call_7"}}),
                json!({"type": "response.completed", "response": {"usage": {
                    "input_tokens": 1200, "input_tokens_details": {"cached_tokens": 1000},
                    "output_tokens": 80, "output_tokens_details": {"reasoning_tokens": 60}}}}),
            ],
        );
        assert!(matches!(
            &out[0],
            StreamChunk::Started { id, model } if id == "resp_1" && model == "gpt-6-astra"
        ));
        assert!(matches!(&out[1], StreamChunk::Delta { text } if text == "bon"));
        assert!(matches!(&out[2], StreamChunk::Reasoning { .. }));
        assert!(
            matches!(&out[3], StreamChunk::ReasoningDetails(v) if v[0]["encrypted_content"] == "chiffré")
        );
        let call = out
            .iter()
            .find_map(|c| match c {
                StreamChunk::ToolCall(t) => Some(t.clone()),
                _ => None,
            })
            .expect("appel d'outil");
        assert_eq!(call.id, "call_7", "l'identifiant vient du serveur");
        assert_eq!(call.arguments["path"], ".");
        let usage = out
            .iter()
            .find_map(|c| match c {
                StreamChunk::Usage(u) => Some(*u),
                _ => None,
            })
            .expect("usage");
        assert_eq!(
            (usage.prompt, usage.cached, usage.completion),
            (1200, 1000, 80)
        );
        assert_eq!(usage.reasoning, 60);
        assert_eq!(usage.cost_usd, Some(0.0), "l'abonnement ne facture pas");
        assert!(matches!(
            out.last(),
            Some(StreamChunk::Done {
                finish: FinishReason::ToolCalls
            })
        ));
        assert!(acc.on_eof().is_empty(), "flux clos proprement");
    }

    /// #142 : une fermeture sans `response.completed` est une coupure, pas une fin.
    #[test]
    fn a_stream_cut_before_completion_is_an_error() {
        let mut acc = ResponsesAccumulator::plain();
        let _ = chunks(
            &mut acc,
            &[json!({"type": "response.output_text.delta", "delta": "moitié"})],
        );
        assert!(matches!(
            acc.on_eof().first(),
            Some(StreamChunk::Error {
                retryable: true,
                ..
            })
        ));
    }

    /// #142 : `response.failed` porte la cause ; le dépassement de fenêtre et la limite de
    /// sortie ne se confondent pas avec une panne.
    #[test]
    fn failures_carry_their_cause() {
        let mut acc = ResponsesAccumulator::plain();
        let out = chunks(
            &mut acc,
            &[json!({"type": "response.failed", "response": {"error": {
                "code": "context_length_exceeded", "message": "trop long"}}})],
        );
        assert!(matches!(
            &out[0],
            StreamChunk::Error { error_type, retryable: false, .. }
                if error_type.as_deref() == Some("context_length_exceeded")
        ));

        let mut acc = ResponsesAccumulator::plain();
        let out = chunks(
            &mut acc,
            &[json!({"type": "response.incomplete", "response": {
                "incomplete_details": {"reason": "max_output_tokens"},
                "usage": {"input_tokens": 10, "output_tokens": 5}}})],
        );
        assert!(matches!(
            out.last(),
            Some(StreamChunk::Done {
                finish: FinishReason::Length
            })
        ));
    }

    /// #142 : un 429 de quota n'est pas une panne — il dit quand revenir et ne se rejoue
    /// pas (issue #139).
    #[test]
    fn a_quota_429_says_when_to_come_back() {
        let resets = now_ms() / 1000 + 3_600;
        let body = json!({"error": {"type": "usage_limit_reached", "plan_type": "plus",
                                    "resets_at": resets, "message": "limite atteinte"}});
        let e = codex_error(429, &body.to_string(), &reqwest::header::HeaderMap::new());
        assert_eq!(e.kind, LlmErrorKind::RateLimited);
        assert_eq!(e.error_type.as_deref(), Some(USAGE_LIMIT_REACHED));
        assert!(
            (3_500..=3_600).contains(&e.retry_after.unwrap_or(0)),
            "{:?}",
            e.retry_after
        );
        assert!(e.message.contains("quota ChatGPT"));

        // Un 403 nomme la cause probable : l'identité empruntée n'est plus acceptée.
        let e = codex_error(403, "{}", &reqwest::header::HeaderMap::new());
        assert_eq!(e.kind, LlmErrorKind::Auth);
        assert!(e.message.contains("originator"), "{}", e.message);
    }

    /// #142 : les jauges du plan se lisent en en-tête comme en événement.
    #[test]
    fn quota_is_read_from_headers_and_events() {
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in [
            ("x-codex-primary-used-percent", "42.5"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1790000000"),
            ("x-codex-secondary-used-percent", "12"),
            ("x-codex-active-limit", "primary"),
        ] {
            h.insert(k, v.parse().unwrap());
        }
        let q = quota_from_headers(&h, 7);
        let p = q.primary.expect("fenêtre principale");
        assert_eq!(
            (p.used_percent, p.window_minutes, p.reset_at),
            (42.5, 300, 1_790_000_000)
        );
        assert_eq!(q.active_limit, "primary");
        assert_eq!(q.read_at_ms, 7);
        assert!((q.worst_ratio() - 0.425).abs() < 1e-9);

        let e = json!({"type": "codex.rate_limits", "plan_type": "pro", "rate_limits": {
            "primary": {"used_percent": 80.0, "window_minutes": 300, "reset_at": 1790000000},
            "secondary": {"used_percent": 5.0, "window_minutes": 10080, "reset_at": 1791000000}}});
        let q = quota_from_event(&e, 9);
        assert_eq!(q.plan_type, "pro");
        assert_eq!(q.primary.unwrap().used_percent, 80.0);
        assert_eq!(q.secondary.unwrap().window_minutes, 10_080);

        // Des en-têtes muets ne remplacent pas ce qui est déjà su.
        assert!(quota_from_headers(&reqwest::header::HeaderMap::new(), 0).is_empty());
    }

    /// #142 : le catalogue du plan porte la vraie fenêtre, les outils, aucun prix ; sans
    /// réseau, la liste de repli suffit à router.
    #[test]
    fn the_catalog_has_windows_tools_and_no_price() {
        let body = json!({"models": [
            {"slug": "gpt-6-astra", "context_window": 200000, "max_context_window": 272000,
             "input_modalities": ["text", "image"],
             "supported_reasoning_levels": ["low", "medium", "high"]},
            {"slug": "gpt-5.6-terra"},
        ]});
        let models = parse_codex_models(&body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-6-astra");
        assert_eq!(models[0].context_window, 272_000);
        assert!(models[0].supports_tools());
        assert!(models[0].accepts_images());
        assert!(models[0].supports_reasoning());
        assert_eq!(models[0].price_prompt, 0.0);
        assert_eq!(models[1].context_window, DEFAULT_CODEX_WINDOW);

        let fallback = fallback_models(&["gpt-6-astra".to_string()]);
        assert_eq!(fallback.len(), 1);
        assert!(fallback[0].supports_tools());
        assert_eq!(fallback[0].provider, "codex");
    }
}
