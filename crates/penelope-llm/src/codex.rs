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

mod models;
mod request;
mod response;
pub use models::*;
pub use request::*;
pub use response::*;

#[cfg(test)]
mod tests;
