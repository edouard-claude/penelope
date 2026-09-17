//! Configuration de référence (§19) et **générations à chaud** (§4.4).
//!
//! La configuration effective est une génération immuable `Arc<Config>` publiée via
//! `ArcSwap`. Un tour lit un instantané à son démarrage et le garde jusqu'à sa fin.

use crate::clock::SharedClock;
use crate::error::{KernelError, Result};
use arc_swap::ArcSwap;
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------- utilitaires

/// Analyse une durée lisible : `500ms`, `30s`, `15m`, `6h`, `90d`.
pub fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return Err(KernelError::config("durée vide"));
    }
    let (num, unit) = s.split_at(
        s.find(|c: char| c.is_ascii_alphabetic())
            .ok_or_else(|| KernelError::config(format!("durée sans unité : `{s}`")))?,
    );
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| KernelError::config(format!("durée invalide : `{s}`")))?;
    let ms = match unit.trim() {
        "ms" => n,
        "s" => n * 1000.0,
        "m" => n * 60_000.0,
        "h" => n * 3_600_000.0,
        "d" => n * 86_400_000.0,
        other => {
            return Err(KernelError::config(format!(
                "unité de durée inconnue : `{other}`"
            )));
        }
    };
    if ms < 0.0 {
        return Err(KernelError::config("durée négative"));
    }
    Ok(std::time::Duration::from_millis(ms as u64))
}

/// Plage horaire `HH:MM-HH:MM`, éventuellement à cheval sur minuit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start_min: u32,
    pub end_min: u32,
}

impl TimeRange {
    pub fn parse(s: &str) -> Result<Self> {
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| KernelError::config(format!("plage horaire invalide : `{s}`")))?;
        Ok(TimeRange {
            start_min: parse_hhmm(a.trim())?,
            end_min: parse_hhmm(b.trim())?,
        })
    }

    /// Vrai si `minute_of_day` est dans la plage (bornes incluses côté début).
    pub fn contains(&self, minute_of_day: u32) -> bool {
        if self.start_min <= self.end_min {
            minute_of_day >= self.start_min && minute_of_day < self.end_min
        } else {
            // à cheval sur minuit : 22:00-07:00
            minute_of_day >= self.start_min || minute_of_day < self.end_min
        }
    }
}

fn parse_hhmm(s: &str) -> Result<u32> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| KernelError::config(format!("heure invalide : `{s}`")))?;
    let h: u32 = h
        .parse()
        .map_err(|_| KernelError::config(format!("heure invalide : `{s}`")))?;
    let m: u32 = m
        .parse()
        .map_err(|_| KernelError::config(format!("heure invalide : `{s}`")))?;
    if h > 23 || m > 59 {
        return Err(KernelError::config(format!("heure hors bornes : `{s}`")));
    }
    Ok(h * 60 + m)
}

// ---------------------------------------------------------------- structures

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub owner: Owner,
    pub telegram: Telegram,
    pub providers: Providers,
    pub models: Models,
    pub budget: Budget,
    pub context: Context,
    pub memory: Memory,
    pub mcp: Mcp,
    pub runners: Runners,
    pub sandbox: Sandbox,
    pub observability: Observability,
    pub tools: Tools,
    pub workflows: Workflows,
    pub upgrade: Upgrade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Owner {
    pub telegram_user_id: i64,
    pub timezone: String,
    pub language: String,
}

impl Default for Owner {
    fn default() -> Self {
        Owner {
            telegram_user_id: 0,
            timezone: "Indian/Reunion".into(),
            language: "fr".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Telegram {
    pub token: String,
    /// `polling` | `webhook`
    pub mode: String,
    pub topics: bool,
    pub rich_messages: bool,
    pub quiet_hours: String,
    pub api_base: String,
    pub poll_timeout_s: u64,
    pub rate_per_chat_per_s: f64,
    pub text_limit: usize,
    pub caption_limit: usize,
    pub max_fragments: usize,
    pub draft_interval_ms: u64,
    pub webhook_url: String,
    pub allow_groups: bool,
}

impl Default for Telegram {
    fn default() -> Self {
        Telegram {
            token: "${SECRET:telegram_bot_token}".into(),
            mode: "polling".into(),
            topics: true,
            // HTML par défaut : `sendMessage` + `parse_mode` marche sur toutes les versions
            // de la Bot API. Le rendu riche natif est une option à activer.
            rich_messages: false,
            quiet_hours: "22:00-07:00".into(),
            api_base: "https://api.telegram.org".into(),
            poll_timeout_s: 50,
            rate_per_chat_per_s: 1.0,
            text_limit: 4096,
            caption_limit: 1024,
            max_fragments: 3,
            draft_interval_ms: 700,
            webhook_url: String::new(),
            allow_groups: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Providers {
    pub openrouter: OpenRouter,
    pub local: LocalProvider,
    /// Endpoints OpenAI-compatibles supplémentaires (embeddings, STT…).
    pub extra: BTreeMap<String, LocalProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenRouter {
    pub api_key: String,
    pub base_url: String,
    pub catalog_refresh: String,
    /// Attribution (`HTTP-Referer`, `X-OpenRouter-Title`, `X-OpenRouter-Categories`).
    pub referer: String,
    pub title: String,
    pub categories: String,
    pub routing: OpenRouterRouting,
    pub enabled: bool,
}

impl Default for OpenRouter {
    fn default() -> Self {
        OpenRouter {
            api_key: "${SECRET:openrouter_api_key}".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            catalog_refresh: "6h".into(),
            referer: "https://github.com/edouard-claude/penelope".into(),
            title: "Penelope".into(),
            categories: "personal-agent".into(),
            routing: OpenRouterRouting::default(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenRouterRouting {
    pub allow_fallbacks: bool,
    /// Ordre imposé des providers. Attention : il désactive le routage collant, donc le
    /// cache de préfixe entre deux tours.
    pub order: Vec<String>,
    /// `deny` : uniquement des providers qui ne conservent pas les données.
    pub data_collection: String,
    pub require_parameters: bool,
    /// Uniquement des endpoints à rétention nulle (ZDR).
    pub zdr: bool,
    /// `price`, `throughput` ou `latency` ; vide = répartition par défaut d'OpenRouter.
    pub sort: String,
    pub only: Vec<String>,
    pub ignore: Vec<String>,
    /// Quantifications acceptées (`fp8`, `bf16`…) ; vide = toutes.
    pub quantizations: Vec<String>,
}

impl Default for OpenRouterRouting {
    fn default() -> Self {
        OpenRouterRouting {
            allow_fallbacks: true,
            order: Vec::new(),
            data_collection: "allow".into(),
            require_parameters: false,
            zdr: false,
            sort: String::new(),
            only: Vec::new(),
            ignore: Vec::new(),
            quantizations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalProvider {
    pub kind: String,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    pub models: Vec<String>,
}

impl Default for LocalProvider {
    fn default() -> Self {
        LocalProvider {
            kind: "openai_compat".into(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            api_key: String::new(),
            enabled: false,
            models: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Models {
    pub aliases: BTreeMap<String, String>,
    pub roles: BTreeMap<String, String>,
    pub routing: Routing,
}

/// Modèle d'embeddings par défaut : multilingue, servi par OpenRouter, sans serveur local
/// (issue #11).
pub const DEFAULT_EMBEDDING_MODEL: &str = "openrouter:openai/text-embedding-3-small";
/// Ancien défaut, qui exigeait un serveur local d'embeddings.
pub const LEGACY_EMBEDDING_MODEL: &str = "openai_compat:embeddings-default";

impl Default for Models {
    fn default() -> Self {
        // §10.2 : identifiants donnés à titre d'exemple de configuration initiale.
        let aliases = [
            ("main", "openrouter:deepseek/deepseek-v4-pro"),
            ("fast", "openrouter:deepseek/deepseek-v4-flash"),
            ("reasoning", "openrouter:z-ai/glm-5.2"),
            ("summarizer", "openrouter:deepseek/deepseek-v4-flash"),
            ("vision", "openrouter:google/gemini-3.1-flash-image"),
            ("image", "openrouter:google/gemini-3.1-flash-image"),
            ("embedding", DEFAULT_EMBEDDING_MODEL),
            ("stt", "openai_compat:whisper-default"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();

        let roles = [
            ("chat_default", "main"),
            ("classifier", "fast"),
            ("compaction", "summarizer"),
            ("memory_review", "fast"),
            ("code", "reasoning"),
            ("image_generate", "image"),
            ("image_describe", "vision"),
            ("embedding", "embedding"),
            ("stt", "stt"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();

        Models {
            aliases,
            roles,
            routing: Routing::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Routing {
    pub classifier: bool,
    pub low: String,
    pub medium: String,
    pub high: String,
    pub sticky: bool,
    pub fallback: BTreeMap<String, Vec<String>>,
}

impl Default for Routing {
    fn default() -> Self {
        Routing {
            classifier: true,
            low: "fast".into(),
            medium: "main".into(),
            high: "reasoning".into(),
            sticky: true,
            fallback: [
                ("main".to_string(), vec!["fast".to_string()]),
                ("reasoning".to_string(), vec!["main".to_string()]),
            ]
            .into_iter()
            .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    pub daily_usd: f64,
    pub session_usd: f64,
    pub run_usd: f64,
    pub alert_ratio: f64,
    /// Coût d'un tour de conversation à chaque multiple duquel Pénélope demande si elle
    /// continue (issue #19). 0 : jamais.
    pub turn_checkpoint_usd: f64,
    /// Coût d'un tour au-delà duquel la réponse finale l'indique. 0 : jamais.
    pub show_turn_cost_usd: f64,
    /// Nombre d'appels au modèle dans un tour à chaque multiple duquel le résultat d'outil
    /// suggère de regrouper les commandes ou de déléguer à un sous-agent. 0 : jamais.
    pub delegate_after_calls: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            daily_usd: 20.0,
            session_usd: 5.0,
            run_usd: 5.0,
            alert_ratio: 0.8,
            turn_checkpoint_usd: 1.0,
            show_turn_cost_usd: 0.5,
            delegate_after_calls: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Context {
    pub compaction_threshold: f64,
    pub tail_ratio: f64,
    pub tail_min_tokens: usize,
    pub tail_max_tokens: usize,
    pub min_tail_user_messages: usize,
    pub max_tool_result_share: f64,
    pub large_payload_tokens: usize,
    /// Taille de prompt au-delà de laquelle la compaction se déclenche, quelle que soit la
    /// fenêtre du modèle : une limite de coût, pas de fenêtre (issue #18). 0 : aucune.
    pub max_prompt_tokens: usize,
    pub model_thresholds: BTreeMap<String, f64>,
    pub background_compaction_margin: f64,
    pub cooldown_ms: Vec<u64>,
    /// Titre de 3 à 6 mots donné par le modèle rapide après le premier échange.
    pub auto_title: bool,
}

impl Default for Context {
    fn default() -> Self {
        Context {
            compaction_threshold: 0.70,
            tail_ratio: 0.025,
            tail_min_tokens: 10_000,
            tail_max_tokens: 25_000,
            min_tail_user_messages: 2,
            max_tool_result_share: 0.25,
            large_payload_tokens: 25_000,
            max_prompt_tokens: 120_000,
            model_thresholds: BTreeMap::new(),
            background_compaction_margin: 0.10,
            cooldown_ms: vec![60_000, 300_000, 900_000],
            auto_title: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Memory {
    pub vault_path: String,
    pub vault_git_autocommit: String,
    pub vault_git_remote: String,
    pub profile_budget_tokens: usize,
    pub core_budget_tokens: usize,
    pub project_budget_tokens: usize,
    pub recall_budget_tokens: usize,
    pub recall_timeout_ms: u64,
    pub trigger_threshold: f64,
    pub max_injected_per_turn: usize,
    pub half_life_days: f64,
    pub dedup_cosine: f64,
    pub dedup_jaccard: f64,
    pub episode_idle: String,
    pub episode_topic_shift: f64,
    pub review_max_candidates: usize,
    pub dreaming_cron: String,
    pub digest_cron: String,
    pub promotion: Promotion,
    pub intents: Intents,
    pub prune_episodic_days: i64,
    pub expire_ecart_days: i64,
}

impl Default for Memory {
    fn default() -> Self {
        Memory {
            vault_path: "{data}/vault".into(),
            vault_git_autocommit: "15m".into(),
            vault_git_remote: String::new(),
            profile_budget_tokens: 600,
            core_budget_tokens: 1200,
            project_budget_tokens: 800,
            recall_budget_tokens: 1000,
            recall_timeout_ms: 150,
            trigger_threshold: 0.72,
            max_injected_per_turn: 3,
            half_life_days: 30.0,
            dedup_cosine: 0.92,
            dedup_jaccard: 0.90,
            episode_idle: "2h".into(),
            episode_topic_shift: 0.35,
            review_max_candidates: 5,
            dreaming_cron: "30 3 * * *".into(),
            digest_cron: "0 8 * * *".into(),
            promotion: Promotion::default(),
            intents: Intents::default(),
            prune_episodic_days: 180,
            expire_ecart_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Promotion {
    pub ecart_min_occurrences: u32,
    pub ecart_min_sessions: u32,
    pub ecart_min_days: u32,
    pub fact_min_recalls: u32,
    pub fact_min_importance: u32,
    pub preference_min_sessions: u32,
    pub max_retire_ratio: f64,
    pub contested_confidence: f64,
    pub contested_min_observations: u32,
}

impl Default for Promotion {
    fn default() -> Self {
        Promotion {
            ecart_min_occurrences: 3,
            ecart_min_sessions: 3,
            ecart_min_days: 2,
            fact_min_recalls: 2,
            fact_min_importance: 8,
            preference_min_sessions: 2,
            max_retire_ratio: 0.20,
            contested_confidence: 0.5,
            contested_min_observations: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Intents {
    pub cooldown: String,
    pub fire_budget: u32,
    pub expiry: String,
    pub max_per_turn: usize,
}

impl Default for Intents {
    fn default() -> Self {
        Intents {
            cooldown: "24h".into(),
            fire_budget: 3,
            expiry: "90d".into(),
            max_per_turn: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mcp {
    /// `lazy` | `eager`
    pub registry_mode: String,
    pub max_processes: usize,
    pub default_timeout: String,
    pub oauth_redirect_mode: String,
    pub public_callback_url: String,
    pub cimd_url: String,
    pub preferred_protocol: String,
    pub idle_timeout: String,
    pub max_concurrency_per_server: usize,
    pub sticky_set_max: usize,
    pub schema_max_bytes: usize,
    pub eager_total_max_bytes: usize,
    pub callback_port: u16,
    pub policy: McpPolicy,
    pub restart_backoff_max: String,
    pub max_failures: u32,
}

impl Default for Mcp {
    fn default() -> Self {
        Mcp {
            registry_mode: "lazy".into(),
            max_processes: 24,
            default_timeout: "30s".into(),
            oauth_redirect_mode: "paste_back".into(),
            public_callback_url: String::new(),
            cimd_url: String::new(),
            preferred_protocol: "2026-07-28".into(),
            idle_timeout: "10m".into(),
            max_concurrency_per_server: 4,
            sticky_set_max: 30,
            schema_max_bytes: 8 * 1024,
            eager_total_max_bytes: 64 * 1024,
            callback_port: 7777,
            policy: McpPolicy::default(),
            restart_backoff_max: "5m".into(),
            max_failures: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpPolicy {
    pub read: String,
    pub write: String,
    pub destructive: String,
    pub external: String,
    pub unknown: String,
}

impl Default for McpPolicy {
    fn default() -> Self {
        McpPolicy {
            read: "auto".into(),
            write: "ask".into(),
            destructive: "ask_twice".into(),
            external: "ask".into(),
            unknown: "ask".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Runners {
    pub count: usize,
    pub lease_ttl: String,
    pub heartbeat: String,
}

impl Default for Runners {
    fn default() -> Self {
        Runners {
            count: 4,
            lease_ttl: "60s".into(),
            heartbeat: "15s".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sandbox {
    pub default_profile: String,
    pub allow_full_for: Vec<String>,
    pub workspaces: Vec<String>,
    /// Réseau pour `shell_exec`. Sans lui, `gh`, `git push`, `curl` ou `npm` échouent,
    /// et `gh auth status` croit le jeton invalide faute de pouvoir le vérifier.
    pub shell_network: bool,
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox {
            default_profile: "workspace-write".into(),
            allow_full_for: Vec::new(),
            workspaces: Vec::new(),
            shell_network: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Observability {
    pub otlp_endpoint: String,
    pub prometheus: String,
    pub log_retention_days: u32,
    pub log_level: String,
}

impl Default for Observability {
    fn default() -> Self {
        Observability {
            otlp_endpoint: String::new(),
            prometheus: "127.0.0.1:9464".into(),
            log_retention_days: 14,
            log_level: "info".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tools {
    pub shell: String,
    pub shell_timeout: String,
    pub http_allowlist: Vec<String>,
    pub http_block_private_ips: bool,
    pub loop_detector_repeats: usize,
    pub max_output_bytes: usize,
}

impl Default for Tools {
    fn default() -> Self {
        Tools {
            shell: String::new(),
            shell_timeout: "120s".into(),
            http_allowlist: Vec::new(),
            http_block_private_ips: true,
            loop_detector_repeats: 3,
            max_output_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Workflows {
    pub workspace_retention_days: i64,
    pub max_depth: u32,
    pub default_max_iterations: u32,
}

impl Default for Workflows {
    fn default() -> Self {
        Workflows {
            workspace_retention_days: 7,
            max_depth: 3,
            default_max_iterations: 40,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Upgrade {
    pub channel: String,
    pub base_url: String,
    pub minisign_pubkey: String,
    pub health_timeout: String,
    pub heartbeat_daily: bool,
    /// Identité de signature macOS (nom du certificat ou empreinte SHA-1) : le binaire
    /// téléchargé est re-signé avec elle avant la bascule (issue #28). Vide : non re-signé.
    pub codesign_identity: String,
    pub codesign_identifier: String,
}

impl Default for Upgrade {
    fn default() -> Self {
        Upgrade {
            channel: "stable".into(),
            base_url: String::new(),
            minisign_pubkey: String::new(),
            health_timeout: "60s".into(),
            heartbeat_daily: true,
            codesign_identity: String::new(),
            codesign_identifier: "io.github.edouard-claude.penelope".into(),
        }
    }
}

// ---------------------------------------------------------------- validation

impl Config {
    pub fn from_toml(s: &str) -> Result<Config> {
        toml::from_str(s).map_err(|e| KernelError::config(e.to_string()))
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| KernelError::config(e.to_string()))
    }

    /// Validation sémantique, au-delà du typage TOML (§4.4 étape 2).
    pub fn validate(&self) -> Result<()> {
        if self.owner.telegram_user_id == 0 {
            return Err(KernelError::config(
                "owner.telegram_user_id est obligatoire (aucun propriétaire = bot ouvert)",
            ));
        }
        self.owner.timezone.parse::<chrono_tz::Tz>().map_err(|_| {
            KernelError::config(format!("fuseau inconnu : {}", self.owner.timezone))
        })?;

        if !matches!(self.telegram.mode.as_str(), "polling" | "webhook") {
            return Err(KernelError::config(
                "telegram.mode doit valoir `polling` ou `webhook`",
            ));
        }
        if self.telegram.mode == "webhook" && self.telegram.webhook_url.is_empty() {
            return Err(KernelError::config(
                "telegram.webhook_url est requis en mode webhook",
            ));
        }
        if !self.telegram.quiet_hours.is_empty() {
            TimeRange::parse(&self.telegram.quiet_hours)?;
        }

        for (role, alias) in &self.models.roles {
            if !self.models.aliases.contains_key(alias) {
                return Err(KernelError::config(format!(
                    "models.roles.{role} pointe vers l'alias inconnu `{alias}`"
                )));
            }
        }
        for target in [
            &self.models.routing.low,
            &self.models.routing.medium,
            &self.models.routing.high,
        ] {
            if !self.models.aliases.contains_key(target) {
                return Err(KernelError::config(format!(
                    "models.routing référence l'alias inconnu `{target}`"
                )));
            }
        }
        for (from, chain) in &self.models.routing.fallback {
            if !self.models.aliases.contains_key(from) {
                return Err(KernelError::config(format!(
                    "models.routing.fallback : alias source inconnu `{from}`"
                )));
            }
            for to in chain {
                if !self.models.aliases.contains_key(to) {
                    return Err(KernelError::config(format!(
                        "models.routing.fallback.{from} : alias cible inconnu `{to}`"
                    )));
                }
            }
        }
        for (alias, id) in &self.models.aliases {
            if !id.contains(':') {
                return Err(KernelError::config(format!(
                    "alias `{alias}` : identifiant `{id}` doit être de la forme `provider:model`"
                )));
            }
        }

        if !(0.1..=0.95).contains(&self.context.compaction_threshold) {
            return Err(KernelError::config(
                "context.compaction_threshold doit être entre 0.1 et 0.95",
            ));
        }
        if self.context.tail_min_tokens > self.context.tail_max_tokens {
            return Err(KernelError::config(
                "context.tail_min_tokens > context.tail_max_tokens",
            ));
        }
        if self.context.max_prompt_tokens != 0 && self.context.max_prompt_tokens < 20_000 {
            return Err(KernelError::config(
                "context.max_prompt_tokens doit valoir 0 (aucun plafond) ou au moins 20000 : \
                 en dessous, la queue verbatim et le préfixe ne tiennent plus",
            ));
        }
        if !(0.0..=1.0).contains(&self.context.max_tool_result_share) {
            return Err(KernelError::config(
                "context.max_tool_result_share doit être entre 0 et 1",
            ));
        }

        parse_duration(&self.memory.episode_idle)?;
        parse_duration(&self.memory.vault_git_autocommit)?;
        parse_duration(&self.memory.intents.cooldown)?;
        parse_duration(&self.memory.intents.expiry)?;
        parse_duration(&self.mcp.default_timeout)?;
        parse_duration(&self.mcp.idle_timeout)?;
        parse_duration(&self.providers.openrouter.catalog_refresh)?;
        parse_duration(&self.runners.lease_ttl)?;
        parse_duration(&self.tools.shell_timeout)?;
        crate::cron::Cron::parse(&self.memory.dreaming_cron)?;
        crate::cron::Cron::parse(&self.memory.digest_cron)?;

        if !matches!(self.mcp.registry_mode.as_str(), "lazy" | "eager") {
            return Err(KernelError::config(
                "mcp.registry_mode doit valoir `lazy` ou `eager`",
            ));
        }
        if !matches!(
            self.mcp.oauth_redirect_mode.as_str(),
            "paste_back" | "public_callback"
        ) {
            return Err(KernelError::config(
                "mcp.oauth_redirect_mode doit valoir `paste_back` ou `public_callback`",
            ));
        }
        if self.mcp.oauth_redirect_mode == "public_callback"
            && self.mcp.public_callback_url.is_empty()
        {
            return Err(KernelError::config(
                "mcp.public_callback_url est requis en mode public_callback",
            ));
        }
        for (k, v) in [
            ("read", &self.mcp.policy.read),
            ("write", &self.mcp.policy.write),
            ("destructive", &self.mcp.policy.destructive),
            ("external", &self.mcp.policy.external),
            ("unknown", &self.mcp.policy.unknown),
        ] {
            if !matches!(v.as_str(), "auto" | "ask" | "ask_twice" | "deny") {
                return Err(KernelError::config(format!(
                    "mcp.policy.{k} : valeur inconnue `{v}`"
                )));
            }
        }
        if self.runners.count == 0 || self.runners.count > 64 {
            return Err(KernelError::config("runners.count doit être entre 1 et 64"));
        }
        if !matches!(
            self.sandbox.default_profile.as_str(),
            "readonly" | "workspace-write" | "mcp-stdio" | "full"
        ) {
            return Err(KernelError::config(format!(
                "sandbox.default_profile inconnu : `{}`",
                self.sandbox.default_profile
            )));
        }
        if self.budget.alert_ratio <= 0.0 || self.budget.alert_ratio > 1.0 {
            return Err(KernelError::config(
                "budget.alert_ratio doit être dans ]0, 1]",
            ));
        }
        Ok(())
    }

    /// Alias effectif d'un rôle, avec repli sur `chat_default` puis `main`.
    pub fn role_alias(&self, role: &str) -> String {
        self.models
            .roles
            .get(role)
            .or_else(|| self.models.roles.get("chat_default"))
            .cloned()
            .unwrap_or_else(|| "main".to_string())
    }

    /// Identifiant `provider:model` d'un alias.
    pub fn alias_model(&self, alias: &str) -> Option<&str> {
        self.models.aliases.get(alias).map(|s| s.as_str())
    }

    /// Seuil de compaction effectif pour un modèle (surcharge §19 `context.model_thresholds`).
    pub fn compaction_threshold_for(&self, model_id: &str) -> f64 {
        let bare = model_id.split_once(':').map(|(_, m)| m).unwrap_or(model_id);
        self.context
            .model_thresholds
            .get(model_id)
            .or_else(|| self.context.model_thresholds.get(bare))
            .copied()
            .unwrap_or(self.context.compaction_threshold)
    }

    pub fn quiet_range(&self) -> Option<TimeRange> {
        TimeRange::parse(&self.telegram.quiet_hours).ok()
    }

    /// Configuration minimale valide, utilisée par les tests et `penelope init`.
    pub fn sample(owner_id: i64) -> Config {
        Config {
            owner: Owner {
                telegram_user_id: owner_id,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------- générations

/// Une génération immuable de la configuration.
#[derive(Debug, Clone)]
pub struct Generation {
    pub generation: u64,
    pub config: Arc<Config>,
    pub changed: Vec<String>,
    pub source: String,
    pub ts: String,
}

/// Résultat d'application par un sous-système (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ApplyResult {
    AppliedLive {
        #[serde(rename = "gen")]
        generation: u64,
    },
    Rejected {
        #[serde(rename = "gen")]
        generation: u64,
        reason: String,
    },
    RequiresRestart {
        #[serde(rename = "gen")]
        generation: u64,
        reason: String,
    },
}

impl ApplyResult {
    pub fn generation(&self) -> u64 {
        match self {
            ApplyResult::AppliedLive { generation }
            | ApplyResult::Rejected { generation, .. }
            | ApplyResult::RequiresRestart { generation, .. } => *generation,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            ApplyResult::AppliedLive { .. } => "applied_live",
            ApplyResult::Rejected { .. } => "rejected",
            ApplyResult::RequiresRestart { .. } => "requires_restart",
        }
    }
}

/// Les seuls chemins pour lesquels `RequiresRestart` est acceptable (§4.4).
pub const RESTART_ONLY_PATHS: &[&str] = &["store.path", "rpc.socket", "telegram.token"];

pub fn restart_allowed(path: &str) -> bool {
    RESTART_ONLY_PATHS.iter().any(|p| path.starts_with(p))
}

/// Publication et historique des générations.
pub struct ConfigStore {
    current: ArcSwap<Generation>,
    counter: AtomicU64,
    path: PathBuf,
    store: Option<Store>,
    clock: SharedClock,
    results: std::sync::Mutex<BTreeMap<String, ApplyResult>>,
    tx: tokio::sync::broadcast::Sender<Arc<Generation>>,
}

impl ConfigStore {
    pub fn new(
        config: Config,
        path: impl AsRef<Path>,
        store: Option<Store>,
        clock: SharedClock,
    ) -> Self {
        let boot = Generation {
            generation: 1,
            config: Arc::new(config),
            changed: Vec::new(),
            source: "boot".into(),
            ts: clock.now_rfc3339(),
        };
        let (tx, _) = tokio::sync::broadcast::channel(32);
        ConfigStore {
            current: ArcSwap::from_pointee(boot),
            counter: AtomicU64::new(1),
            path: path.as_ref().to_path_buf(),
            store,
            clock,
            results: std::sync::Mutex::new(BTreeMap::new()),
            tx,
        }
    }

    /// Charge depuis le disque, ou crée le fichier à partir des valeurs par défaut.
    pub fn load_or_create(
        path: impl AsRef<Path>,
        store: Option<Store>,
        clock: SharedClock,
        owner_id: i64,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let cfg = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let mut c = Config::from_toml(&raw)?;
            // L'ancien défaut d'embeddings visait un serveur local désactivé : la recherche
            // restait lexicale sans le dire (issue #11).
            if !c.providers.local.enabled
                && let Some(alias) = c.models.aliases.get_mut("embedding")
                && alias == LEGACY_EMBEDDING_MODEL
            {
                tracing::warn!(
                    ancien = LEGACY_EMBEDDING_MODEL,
                    nouveau = DEFAULT_EMBEDDING_MODEL,
                    "alias `embedding` sans serveur local : bascule sur OpenRouter"
                );
                *alias = DEFAULT_EMBEDDING_MODEL.to_string();
            }
            c
        } else {
            let c = Config::sample(owner_id);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            atomic_write(&path, c.to_toml()?.as_bytes())?;
            c
        };
        Ok(ConfigStore::new(cfg, path, store, clock))
    }

    /// Instantané courant : c'est ce que lit un tour à son démarrage (§4.4).
    pub fn snapshot(&self) -> Arc<Generation> {
        self.current.load_full()
    }

    pub fn config(&self) -> Arc<Config> {
        self.current.load().config.clone()
    }

    pub fn generation(&self) -> u64 {
        self.current.load().generation
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Arc<Generation>> {
        self.tx.subscribe()
    }

    /// Applique une mutation : valide, persiste, publie la génération N+1.
    ///
    /// Une mutation qui échoue à la validation **ne publie rien** : la génération
    /// courante reste en place (§12.6 pour les workflows, même principe ici).
    pub fn mutate<F>(&self, source: &str, f: F) -> Result<Arc<Generation>>
    where
        F: FnOnce(&mut Config) -> Result<Vec<String>>,
    {
        let cur = self.current.load_full();
        let mut next = (*cur.config).clone();
        let changed = f(&mut next)?;
        next.validate()?;

        let toml = next.to_toml()?;
        atomic_write(&self.path, toml.as_bytes())?;

        let gen_no = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let g = Arc::new(Generation {
            generation: gen_no,
            config: Arc::new(next),
            changed: changed.clone(),
            source: source.to_string(),
            ts: self.clock.now_rfc3339(),
        });
        self.current.store(g.clone());

        if let Some(store) = &self.store {
            let snapshot = serde_json::to_string(&*g.config).unwrap_or_default();
            let changed_s = serde_json::to_string(&changed).unwrap_or_default();
            let ts = g.ts.clone();
            let src = g.source.clone();
            let _ = store.write_blocking(move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO config_generations(gen, ts, source, changed, snapshot)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![gen_no as i64, ts, src, changed_s, snapshot],
                )?;
                Ok(())
            });
        }

        let _ = self.tx.send(g.clone());
        Ok(g)
    }

    /// Recharge depuis le fichier (watcher du §2.9).
    pub fn reload_from_disk(&self) -> Result<Arc<Generation>> {
        let raw = std::fs::read_to_string(&self.path)?;
        let parsed = Config::from_toml(&raw)?;
        self.mutate("file", move |c| {
            let changed = diff_paths(c, &parsed);
            *c = parsed;
            Ok(changed)
        })
    }

    /// Enregistre le résultat d'application d'un sous-système.
    ///
    /// Un résultat pour une génération **ancienne** ne peut pas écraser celui d'une
    /// génération plus récente (§4.4).
    pub fn record_apply(&self, subsystem: &str, result: ApplyResult) -> bool {
        let mut guard = match self.results.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(existing) = guard.get(subsystem)
            && existing.generation() > result.generation()
        {
            return false;
        }
        if let ApplyResult::RequiresRestart { generation, reason } = &result
            && !restart_allowed(reason)
        {
            tracing::warn!(
                subsystem,
                generation,
                reason,
                "RequiresRestart refusé : ce chemin doit s'appliquer à chaud"
            );
        }
        if let Some(store) = &self.store {
            let sub = subsystem.to_string();
            let (kind, reason, gen_no) = match &result {
                ApplyResult::AppliedLive { generation } => ("applied_live", None, *generation),
                ApplyResult::Rejected { generation, reason } => {
                    ("rejected", Some(reason.clone()), *generation)
                }
                ApplyResult::RequiresRestart { generation, reason } => {
                    ("requires_restart", Some(reason.clone()), *generation)
                }
            };
            let ts = self.clock.now_rfc3339();
            let _ = store.write_blocking(move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO subsystem_apply_results(gen, subsystem, result, reason, ts)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![gen_no as i64, sub, kind, reason, ts],
                )?;
                Ok(())
            });
        }
        guard.insert(subsystem.to_string(), result);
        true
    }

    pub fn apply_results(&self) -> BTreeMap<String, ApplyResult> {
        match self.results.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }
}

/// Écriture atomique : fichier temporaire dans le même répertoire, puis renommage.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "f".into()),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Liste grossière des chemins modifiés entre deux configurations (pour `changed_paths`).
fn diff_paths(a: &Config, b: &Config) -> Vec<String> {
    let va = serde_json::to_value(a).unwrap_or(serde_json::Value::Null);
    let vb = serde_json::to_value(b).unwrap_or(serde_json::Value::Null);
    let mut out = Vec::new();
    diff_value("", &va, &vb, &mut out);
    out.sort();
    out.dedup();
    out
}

fn diff_value(prefix: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    match (a, b) {
        (serde_json::Value::Object(ma), serde_json::Value::Object(mb)) => {
            let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            for k in keys {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match (ma.get(k), mb.get(k)) {
                    (Some(x), Some(y)) => diff_value(&p, x, y, out),
                    _ => out.push(p),
                }
            }
        }
        _ => {
            if a != b {
                out.push(prefix.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;

    fn cfg() -> Config {
        Config::sample(123)
    }

    #[test]
    fn default_config_is_valid() {
        cfg().validate().unwrap();
    }

    #[test]
    fn owner_is_required() {
        let c = Config::default();
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains("telegram_user_id"), "{e}");
    }

    #[test]
    fn role_pointing_to_unknown_alias_is_rejected() {
        let mut c = cfg();
        c.models.roles.insert("code".into(), "nexistepas".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn toml_roundtrip() {
        let c = cfg();
        let s = c.to_toml().unwrap();
        let back = Config::from_toml(&s).unwrap();
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn unknown_key_is_rejected() {
        let e = Config::from_toml("[owner]\ntelegram_user_id = 1\nnimporte = 2\n").unwrap_err();
        assert!(e.to_string().contains("nimporte"), "{e}");
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("500ms").unwrap().as_millis(), 500);
        assert_eq!(parse_duration("30s").unwrap().as_secs(), 30);
        assert_eq!(parse_duration("15m").unwrap().as_secs(), 900);
        assert_eq!(parse_duration("6h").unwrap().as_secs(), 21_600);
        assert_eq!(parse_duration("90d").unwrap().as_secs(), 7_776_000);
        assert!(parse_duration("12").is_err());
        assert!(parse_duration("3y").is_err());
    }

    #[test]
    fn quiet_hours_cross_midnight() {
        let r = TimeRange::parse("22:00-07:00").unwrap();
        assert!(r.contains(23 * 60));
        assert!(r.contains(3 * 60));
        assert!(!r.contains(12 * 60));
        let r = TimeRange::parse("09:00-17:00").unwrap();
        assert!(r.contains(10 * 60));
        assert!(!r.contains(20 * 60));
    }

    #[test]
    fn model_threshold_override() {
        let mut c = cfg();
        c.context
            .model_thresholds
            .insert("z-ai/glm-5.2".into(), 0.60);
        assert_eq!(c.compaction_threshold_for("openrouter:z-ai/glm-5.2"), 0.60);
        assert_eq!(c.compaction_threshold_for("autre/modele"), 0.70);
    }

    /// CA 4 : générations strictement croissantes, sans perte d'écriture.
    #[test]
    fn ca_4_3_generations_are_monotonic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cs = ConfigStore::new(
            cfg(),
            &path,
            None,
            std::sync::Arc::new(TestClock::default()),
        );
        assert_eq!(cs.generation(), 1);

        let g2 = cs
            .mutate("cli", |c| {
                c.budget.daily_usd = 50.0;
                Ok(vec!["budget.daily_usd".into()])
            })
            .unwrap();
        assert_eq!(g2.generation, 2);

        let g3 = cs
            .mutate("telegram", |c| {
                c.runners.count = 8;
                Ok(vec!["runners.count".into()])
            })
            .unwrap();
        assert_eq!(g3.generation, 3);
        assert_eq!(
            cs.config().budget.daily_usd,
            50.0,
            "pas de perte d'écriture"
        );
        assert_eq!(cs.config().runners.count, 8);

        // Le fichier sur disque reflète la dernière génération.
        let on_disk = Config::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.runners.count, 8);
        assert_eq!(on_disk.budget.daily_usd, 50.0);
    }

    #[test]
    fn invalid_mutation_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cs = ConfigStore::new(
            cfg(),
            &path,
            None,
            std::sync::Arc::new(TestClock::default()),
        );
        let before = cs.generation();
        let r = cs.mutate("cli", |c| {
            c.runners.count = 0;
            Ok(vec!["runners.count".into()])
        });
        assert!(r.is_err());
        assert_eq!(cs.generation(), before);
        assert_eq!(cs.config().runners.count, 4);
    }

    #[test]
    fn snapshot_is_frozen_for_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let cs = ConfigStore::new(
            cfg(),
            dir.path().join("c.toml"),
            None,
            std::sync::Arc::new(TestClock::default()),
        );
        let snap = cs.snapshot();
        cs.mutate("cli", |c| {
            c.budget.daily_usd = 99.0;
            Ok(vec![])
        })
        .unwrap();
        assert_eq!(
            snap.config.budget.daily_usd, 20.0,
            "le tour garde son instantané"
        );
        assert_eq!(cs.config().budget.daily_usd, 99.0);
    }

    #[test]
    fn stale_apply_result_cannot_overwrite_newer() {
        let dir = tempfile::tempdir().unwrap();
        let cs = ConfigStore::new(
            cfg(),
            dir.path().join("c.toml"),
            None,
            std::sync::Arc::new(TestClock::default()),
        );
        assert!(cs.record_apply("mcp", ApplyResult::AppliedLive { generation: 5 }));
        assert!(!cs.record_apply(
            "mcp",
            ApplyResult::Rejected {
                generation: 3,
                reason: "vieux".into()
            }
        ));
        assert_eq!(
            cs.apply_results().get("mcp"),
            Some(&ApplyResult::AppliedLive { generation: 5 })
        );
    }

    #[test]
    fn reload_from_disk_reports_changed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cs = ConfigStore::new(
            cfg(),
            &path,
            None,
            std::sync::Arc::new(TestClock::default()),
        );
        let mut edited = cfg();
        edited.budget.daily_usd = 7.5;
        std::fs::write(&path, edited.to_toml().unwrap()).unwrap();
        let g = cs.reload_from_disk().unwrap();
        assert!(
            g.changed.iter().any(|p| p == "budget.daily_usd"),
            "{:?}",
            g.changed
        );
    }

    #[test]
    fn restart_only_paths_are_limited() {
        assert!(restart_allowed("telegram.token"));
        assert!(restart_allowed("store.path"));
        assert!(!restart_allowed("runners.count"));
    }

    #[test]
    fn atomic_write_replaces_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.toml");
        atomic_write(&p, b"a").unwrap();
        atomic_write(&p, b"bb").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "bb");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "aucun fichier temporaire ne doit rester"
        );
    }
}
