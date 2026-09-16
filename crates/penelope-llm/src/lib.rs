//! `penelope-llm` : providers, catalogue, routage, coûts, comptabilité des tokens (§10).

#![forbid(unsafe_code)]

pub mod catalog;
pub mod emulation;
pub mod mock;
pub mod provider;
pub mod router;
pub mod sse;
pub mod state;
pub mod tokens;
pub mod types;

pub use catalog::{Catalog, ModelInfo};
pub use provider::{
    CancelToken, ChunkStream, OpenAiCompatProvider, OpenRouterProvider, Provider, ProviderSet,
    collect_stream, collect_stream_observed,
};
pub use router::{
    Classification, Complexity, Decision, RouteInput, RouteReason, Router, StickyModel,
};
pub use state::{LlmState, LlmStateMachine, UnknownSendPolicy};
pub use tokens::{TokenEstimator, UsageAnchor, UsageState, transcript_fingerprint};
pub use types::{
    ChatMessage, ChatRequest, ChatResponse, Content, FinishReason, LlmError, LlmErrorKind, Result,
    Role, StreamChunk, ToolCall, ToolChoice, ToolDef, Usage,
};

/// Construit l'ensemble des providers à partir de la configuration, secrets résolus.
///
/// Les clés sont injectées **à la frontière** : elles ne transitent jamais par le modèle
/// ni par les logs (§13.1).
pub fn build_providers(
    cfg: &penelope_kernel::config::Config,
    secrets: &dyn penelope_platform::SecretStore,
    catalog: Catalog,
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
                .with_routing(routing_value(&cfg.providers.openrouter.routing)),
        ))
    } else {
        None
    };

    let compat = if cfg.providers.local.enabled {
        let key = secrets
            .expand(&cfg.providers.local.api_key)
            .unwrap_or_default();
        if !key.is_empty() {
            penelope_observe::register_secret(&key);
        }
        Some(std::sync::Arc::new(OpenAiCompatProvider::new(
            &cfg.providers.local.base_url,
            key,
            catalog.clone(),
        )?))
    } else {
        None
    };

    Ok(ProviderSet {
        openrouter,
        compat,
        catalog,
    })
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
        let set = build_providers(&cfg, &secrets, Catalog::new()).unwrap();
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
        let e = build_providers(&cfg, &secrets, Catalog::new())
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
        let set = build_providers(&cfg, &secrets, Catalog::new()).unwrap();
        assert_eq!(set.get("openrouter:a/b").unwrap().name(), "openrouter");
        assert_eq!(
            set.get("openai_compat:whisper").unwrap().name(),
            "openai_compat"
        );
    }
}
