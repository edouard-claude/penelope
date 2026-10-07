//! `penelope-llm` : providers, catalogue, routage, coûts, comptabilité des tokens (§10).

#![forbid(unsafe_code)]

pub mod attachment;
pub mod cache;
pub mod catalog;
pub mod codex;
pub mod json_scan;
pub mod mock;
pub mod provider;
pub mod router;
pub mod sse;
pub mod state;
pub mod tokens;
pub mod types;

pub use catalog::{Catalog, ModelInfo};
pub use codex::{ClientOs, CodexOptions, CodexProvider, CodexToken, Quota, QuotaSink, TokenSource};
pub use provider::{
    CancelToken, ChunkStream, OpenAiCompatProvider, OpenRouterProvider, Provider, ProviderSet,
    collect_stream, collect_stream_observed,
};
pub use router::{
    Classification, Complexity, Decision, RouteInput, RouteReason, Router, StickyModel,
};
pub use state::{LlmState, LlmStateMachine, PlannedCall, RequestKeys, UnknownSendPolicy};
pub use tokens::{TokenEstimator, UsageAnchor, UsageState, transcript_fingerprint};
pub use types::{
    ChatMessage, ChatRequest, ChatResponse, Content, FinishReason, LlmError, LlmErrorKind, Result,
    Role, StreamChunk, ToolCall, ToolChoice, ToolDef, Usage,
};

/// Silence toléré pendant un flux, lu dans la configuration (issue #51).
fn idle_of(raw: &str) -> std::time::Duration {
    penelope_kernel::config::parse_duration(raw).unwrap_or(crate::provider::DEFAULT_STREAM_IDLE)
}

/// Construit l'ensemble des providers à partir de la configuration, secrets résolus.
///
/// Les clés sont injectées **à la frontière** : elles ne transitent jamais par le modèle
/// ni par les logs (§13.1).
pub fn build_providers(
    cfg: &penelope_kernel::config::Config,
    secrets: &dyn penelope_platform::SecretStore,
    catalog: Catalog,
    codex_access: Option<CodexAccess>,
) -> Result<ProviderSet> {
    let openrouter = if cfg.providers.openrouter.enabled {
        let key = secrets
            .expand(&cfg.providers.openrouter.api_key)
            .map_err(|e| LlmError::new(LlmErrorKind::Auth, e.to_string()))?;
        penelope_observe::register_secret(&key);
        Some(std::sync::Arc::new(
            OpenRouterProvider::new(&cfg.providers.openrouter.base_url, key, catalog.clone())?
                .with_identity(
                    cfg.providers.openrouter.referer.clone(),
                    cfg.providers.openrouter.title.clone(),
                )
                .with_categories(cfg.providers.openrouter.categories.clone())
                .with_routing(routing_value(&cfg.providers.openrouter.routing))
                .with_stream_idle(idle_of(&cfg.providers.openrouter.stream_idle_timeout)),
        ))
    } else {
        None
    };

    let compat = if cfg.providers.local.enabled {
        Some(local_endpoint(&cfg.providers.local, secrets, &catalog)?)
    } else {
        None
    };
    // Les endpoints supplémentaires, chacun pour les modèles qu'il liste : le serveur de
    // texte (mlx_lm.server) à côté de celui de la voix (mlx-audio) (#259).
    let mut extra = Vec::new();
    for e in cfg.providers.extra.values().filter(|e| e.enabled) {
        extra.push((e.models.clone(), local_endpoint(e, secrets, &catalog)?));
    }

    // Codex : le daemon détient la connexion au compte ChatGPT (jetons, rotation,
    // verrou) ; ici, on ne branche que ce qu'il fournit (issue #142).
    let codex = match (cfg.providers.codex.enabled, codex_access) {
        (true, Some(access)) => {
            let c = &cfg.providers.codex;
            Some(std::sync::Arc::new(
                CodexProvider::new(
                    CodexOptions {
                        base_url: c.base_url.clone(),
                        originator: c.originator.clone(),
                        client_version: codex::resolve_client_version(
                            &c.client_version,
                            access.cli_version.as_deref(),
                        ),
                        os: access.os,
                        reasoning_summary: c.reasoning_summary.clone(),
                        verbosity: c.verbosity.clone(),
                        stream_idle: idle_of(&c.stream_idle_timeout),
                        models: c.models.clone(),
                        quota_stop_ratio: c.quota_stop_ratio,
                    },
                    access.tokens,
                    catalog.clone(),
                    access.installation_id,
                )?
                .maybe_quota_sink(access.quota_sink),
            ))
        }
        _ => None,
    };

    Ok(ProviderSet {
        openrouter,
        compat,
        extra,
        codex,
        catalog,
    })
}

/// Un endpoint OpenAI-compatible de la configuration, clé résolue.
fn local_endpoint(
    e: &penelope_kernel::config::LocalProvider,
    secrets: &dyn penelope_platform::SecretStore,
    catalog: &Catalog,
) -> Result<std::sync::Arc<OpenAiCompatProvider>> {
    let key = secrets.expand(&e.api_key).unwrap_or_default();
    if !key.is_empty() {
        penelope_observe::register_secret(&key);
    }
    Ok(std::sync::Arc::new(
        OpenAiCompatProvider::new(&e.base_url, key, catalog.clone())?
            .with_stream_idle(idle_of(&e.stream_idle_timeout))
            .with_window(e.context_window),
    ))
}

/// Ce que le daemon apporte au fournisseur Codex : la source de jetons (il tient le
/// magasin de secrets et le verrou de rotation) et l'identifiant d'installation, stable,
/// envoyé sur toutes les requêtes.
pub struct CodexAccess {
    pub tokens: std::sync::Arc<dyn TokenSource>,
    pub installation_id: String,
    /// Où publier les jauges du plan lues à chaque réponse.
    pub quota_sink: Option<std::sync::Arc<dyn QuotaSink>>,
    /// Système annoncé dans le `User-Agent`, lu par la plateforme (#313).
    pub os: codex::ClientOs,
    /// Version du Codex CLI installé, si la découverte en a trouvé un : elle remplace
    /// `client_version = "auto"` (#313).
    pub cli_version: Option<String>,
}

/// Préférences de provider OpenRouter (`provider`). Seuls les écarts au comportement
/// par défaut sont envoyés ; sans écart, pas d'objet du tout.
fn routing_value(r: &penelope_kernel::config::OpenRouterRouting) -> serde_json::Value {
    use serde_json::{Value, json};
    let list = |v: &[String]| Value::Array(v.iter().map(|s| json!(s)).collect());
    let mut v = serde_json::Map::new();
    if !r.allow_fallbacks {
        v.insert("allow_fallbacks".into(), json!(false));
    }
    if !r.order.is_empty() {
        v.insert("order".into(), list(&r.order));
    }
    if !r.data_collection.is_empty() && r.data_collection != "allow" {
        v.insert("data_collection".into(), json!(r.data_collection));
    }
    if r.require_parameters {
        v.insert("require_parameters".into(), json!(true));
    }
    if r.zdr {
        v.insert("zdr".into(), json!(true));
    }
    if !r.sort.is_empty() {
        v.insert("sort".into(), json!(r.sort));
    }
    if !r.only.is_empty() {
        v.insert("only".into(), list(&r.only));
    }
    if !r.ignore.is_empty() {
        v.insert("ignore".into(), list(&r.ignore));
    }
    if !r.quantizations.is_empty() {
        v.insert("quantizations".into(), list(&r.quantizations));
    }
    if v.is_empty() {
        Value::Null
    } else {
        Value::Object(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::config::Config;
    use penelope_platform::MemorySecretStore;

    #[test]
    fn providers_resolve_secrets_at_the_boundary() {
        let secrets = MemorySecretStore::with(&[("openrouter_api_key", "sk-or-v1-test123456789")]);
        let cfg = Config::sample(1);
        let set = build_providers(&cfg, &secrets, Catalog::new(), None).unwrap();
        assert!(set.openrouter.is_some());
        assert!(
            set.compat.is_none(),
            "le provider local est désactivé par défaut"
        );
        // La clé est enregistrée pour la redaction : elle ne fuitera pas dans les logs.
        assert!(penelope_observe::redact("clé sk-or-v1-test123456789").contains("masqué"));
    }

    #[test]
    fn missing_secret_is_an_auth_error() {
        let secrets = MemorySecretStore::new();
        let cfg = Config::sample(1);
        let e = build_providers(&cfg, &secrets, Catalog::new(), None)
            .err()
            .expect("un secret absent doit faire échouer la construction");
        assert_eq!(e.kind, LlmErrorKind::Auth);
    }

    #[test]
    fn routing_value_is_minimal_by_default() {
        let r = penelope_kernel::config::OpenRouterRouting::default();
        assert!(routing_value(&r).is_null(), "rien à envoyer par défaut");

        let r = penelope_kernel::config::OpenRouterRouting {
            data_collection: "deny".into(),
            zdr: true,
            sort: "throughput".into(),
            ignore: vec!["deepinfra".into()],
            ..Default::default()
        };
        let v = routing_value(&r);
        assert_eq!(v["data_collection"], "deny");
        assert_eq!(v["zdr"], true);
        assert_eq!(v["sort"], "throughput");
        assert_eq!(v["ignore"][0], "deepinfra");
        assert!(v.get("allow_fallbacks").is_none());
        assert!(v.get("order").is_none());
    }

    #[test]
    fn provider_set_routes_by_prefix() {
        let secrets = MemorySecretStore::with(&[("openrouter_api_key", "sk-or-v1-x123456789")]);
        let mut cfg = Config::sample(1);
        cfg.providers.local.enabled = true;
        let set = build_providers(&cfg, &secrets, Catalog::new(), None).unwrap();
        assert_eq!(set.get("openrouter:a/b").unwrap().name(), "openrouter");
        assert_eq!(
            set.get("openai_compat:whisper").unwrap().name(),
            "openai_compat"
        );
    }

    /// #259 : un modèle listé par un endpoint de `providers.extra` actif part chez lui,
    /// les autres modèles locaux chez `providers.local` ; un endpoint éteint n'existe pas.
    #[test]
    fn an_extra_endpoint_serves_the_models_it_lists() {
        let secrets = MemorySecretStore::with(&[("openrouter_api_key", "sk-or-v1-x123456789")]);
        let mut cfg = Config::sample(1);
        cfg.providers.local.enabled = true;
        let mlx = penelope_kernel::config::LocalProvider {
            enabled: true,
            base_url: "http://127.0.0.1:8081/v1".into(),
            models: vec!["mlx-community/Qwen3-8B-4bit".into()],
            ..Default::default()
        };
        cfg.providers.extra.insert("mlx".into(), mlx.clone());
        cfg.providers.extra.insert(
            "eteint".into(),
            penelope_kernel::config::LocalProvider {
                enabled: false,
                ..mlx
            },
        );
        let set = build_providers(&cfg, &secrets, Catalog::new(), None).unwrap();
        assert_eq!(set.extra.len(), 1);
        let same = |a: &std::sync::Arc<dyn Provider>, b: &std::sync::Arc<OpenAiCompatProvider>| {
            std::ptr::addr_eq(std::sync::Arc::as_ptr(a), std::sync::Arc::as_ptr(b))
        };
        let text = set.get("local:mlx-community/Qwen3-8B-4bit").unwrap();
        assert!(same(&text, &set.extra[0].1));
        assert_eq!(set.extra[0].1.base_url(), "http://127.0.0.1:8081/v1");
        let voice = set.get("openai_compat:mlx-community/Voxtral-4B-TTS-2603-mlx-4bit");
        assert!(same(&voice.unwrap(), set.compat.as_ref().unwrap()));
    }

    /// #335 : un modèle local qu'aucun endpoint ne sert n'est jamais envoyé à OpenRouter ;
    /// il échoue en disant quoi poser.
    #[test]
    fn a_local_model_without_endpoint_never_goes_online() {
        let secrets = MemorySecretStore::with(&[("openrouter_api_key", "sk-or-v1-x123456789")]);
        let cfg = Config::sample(1);
        assert!(!cfg.providers.local.enabled);
        let set = build_providers(&cfg, &secrets, Catalog::new(), None).unwrap();
        assert!(set.openrouter.is_some());
        for model in [
            "local:mlx-community/Qwen3-1.7B-4bit",
            "openai_compat:whisper",
        ] {
            assert!(set.get(model).is_none(), "{model}");
            let err = set.resolve(model).err().unwrap();
            assert!(
                err.contains("providers.extra") && err.contains("en ligne"),
                "{err}"
            );
        }
    }

    struct NoTokens;

    #[async_trait::async_trait]
    impl TokenSource for NoTokens {
        async fn token(&self) -> Result<CodexToken> {
            Err(LlmError::new(LlmErrorKind::Auth, "pas de compte"))
        }
        async fn refreshed(&self) -> Result<CodexToken> {
            self.token().await
        }
    }

    /// OpenRouter éteint, serveur local à clé, Codex branché sur l'accès fourni par le
    /// daemon : chacun n'existe que s'il est activé, et la clé locale est masquée.
    #[test]
    fn each_provider_exists_only_when_enabled() {
        let secrets = MemorySecretStore::with(&[("local_key", "sk-local-abcdef123456")]);
        let mut cfg = Config::sample(1);
        cfg.providers.openrouter.enabled = false;
        cfg.providers.local.enabled = true;
        cfg.providers.local.api_key = "${SECRET:local_key}".into();
        cfg.providers.codex.enabled = true;
        let access = CodexAccess {
            tokens: std::sync::Arc::new(NoTokens),
            installation_id: "inst-1".into(),
            quota_sink: None,
            os: Default::default(),
            cli_version: None,
        };
        let set = build_providers(&cfg, &secrets, Catalog::new(), Some(access)).unwrap();
        assert!(set.openrouter.is_none());
        assert!(set.compat.is_some());
        assert!(set.codex.is_some());
        assert!(penelope_observe::redact("clé sk-local-abcdef123456").contains("masqué"));

        // Codex activé sans accès du daemon : pas de fournisseur, pas d'erreur.
        let set = build_providers(&cfg, &secrets, Catalog::new(), None).unwrap();
        assert!(set.codex.is_none());
    }

    #[test]
    fn routing_value_carries_every_departure_from_the_defaults() {
        let r = penelope_kernel::config::OpenRouterRouting {
            allow_fallbacks: false,
            order: vec!["z-ai".into()],
            require_parameters: true,
            only: vec!["z-ai".into(), "groq".into()],
            quantizations: vec!["fp8".into()],
            data_collection: "allow".into(),
            ..Default::default()
        };
        let v = routing_value(&r);
        assert_eq!(v["allow_fallbacks"], false);
        assert_eq!(v["order"], serde_json::json!(["z-ai"]));
        assert_eq!(v["require_parameters"], true);
        assert_eq!(v["only"][1], "groq");
        assert_eq!(v["quantizations"][0], "fp8");
        assert!(v.get("data_collection").is_none(), "`allow` est le défaut");
    }
}
