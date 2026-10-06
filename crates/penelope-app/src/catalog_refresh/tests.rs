use super::*;
use penelope_context::CompactionParams;
use penelope_llm::catalog::{FALLBACK_WINDOW, WindowSource, parse_openrouter_models};
use penelope_llm::codex::parse_codex_models;
use penelope_llm::{CancelToken, Catalog, ChatRequest, ChunkStream, ModelInfo, Provider};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Un fournisseur qui écrit son catalogue comme le vrai : `replace` pour OpenRouter,
/// `upsert` pour Codex.
struct Fake {
    kind: &'static str,
    catalog: Catalog,
    fetches: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for Fake {
    fn name(&self) -> &str {
        self.kind
    }

    async fn chat_stream(
        &self,
        _: ChatRequest,
        _: CancelToken,
    ) -> penelope_llm::Result<ChunkStream> {
        unreachable!("aucun appel de conversation ici")
    }

    async fn fetch_models(&self) -> penelope_llm::Result<Vec<ModelInfo>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        if self.kind == "codex" {
            let models = parse_codex_models(&json!({"models": [
                {"slug": "gpt-6-astra", "max_context_window": 872_000}
            ]}));
            self.catalog.upsert(models.clone());
            Ok(models)
        } else {
            let models = parse_openrouter_models(&json!({"data": [
                {"id": "deepseek/deepseek-v4-flash", "context_length": 1_000_000}
            ]}));
            self.catalog.replace(models.clone(), 1);
            Ok(models)
        }
    }
}

/// Codex et OpenRouter ; l'endpoint local n'a pas de clé.
struct Source {
    codex: Arc<Fake>,
    openrouter: Arc<Fake>,
}

impl Source {
    fn new(catalog: &Catalog) -> Source {
        let fake = |kind| {
            Arc::new(Fake {
                kind,
                catalog: catalog.clone(),
                fetches: AtomicUsize::new(0),
            })
        };
        Source {
            codex: fake("codex"),
            openrouter: fake("openrouter"),
        }
    }
}

#[async_trait::async_trait]
impl ProviderSource for Source {
    async fn provider_for(&self, model_id: &str) -> Result<Arc<dyn Provider>, String> {
        match provider_of(model_id) {
            "codex" => Ok(self.codex.clone()),
            "openrouter" => Ok(self.openrouter.clone()),
            _ => Err("aucune clé".into()),
        }
    }

    fn provider_override_active(&self) -> Option<Arc<dyn Provider>> {
        None
    }
}

/// `main` sur Codex, un alias sur OpenRouter, un autre sur un endpoint local.
fn config() -> Config {
    let mut cfg = Config::default();
    cfg.providers.codex.enabled = true;
    cfg.providers.local.enabled = true;
    let aliases = &mut cfg.models.aliases;
    aliases.clear();
    aliases.insert("main".into(), "codex:gpt-6-astra".into());
    aliases.insert(
        "flash".into(),
        "openrouter:deepseek/deepseek-v4-flash".into(),
    );
    aliases.insert("qwen".into(), "local:qwen".into());
    cfg.models
        .roles
        .insert("chat_default".into(), "main".into());
    cfg
}

/// #324 : avec `main` sur Codex, le catalogue d'OpenRouter est chargé aussi, et le seuil
/// de compaction d'un modèle OpenRouter suit la fenêtre qu'il déclare.
#[tokio::test]
async fn every_provider_aimed_by_an_alias_is_loaded() {
    let cfg = config();
    let catalog = Catalog::new();
    let source = Source::new(&catalog);
    let mut r = Refresher::default();
    let t0 = Instant::now();

    assert_eq!(r.pass(&cfg, &source, t0).await, 2, "local sans clé");
    let flash = "openrouter:deepseek/deepseek-v4-flash";
    assert_eq!(catalog.window(flash), (1_000_000, WindowSource::Provider));
    // Codex, chargé avant OpenRouter, n'est pas effacé par son `replace`.
    assert_eq!(
        catalog.window("codex:gpt-6-astra"),
        (872_000, WindowSource::Provider)
    );
    let threshold = |window| CompactionParams::from_config(&cfg, window, flash).threshold_tokens();
    assert_eq!(
        CompactionParams::from_config(&cfg, catalog.window_of(flash), flash).threshold_tokens(),
        threshold(1_000_000)
    );
    assert!(threshold(1_000_000) > threshold(FALLBACK_WINDOW));

    // Chaque endpoint à son échéance : rien avant, l'échec retenté après une minute,
    // les succès après la cadence.
    assert_eq!(r.pass(&cfg, &source, t0 + Duration::from_secs(1)).await, 0);
    assert_eq!(source.codex.fetches.load(Ordering::SeqCst), 1);
    assert_eq!(r.pass(&cfg, &source, t0 + RETRY).await, 0);
    assert_eq!(source.openrouter.fetches.load(Ordering::SeqCst), 1);
    assert_eq!(r.pass(&cfg, &source, t0 + DEFAULT_EVERY).await, 2);
    assert_eq!(source.openrouter.fetches.load(Ordering::SeqCst), 2);
}

#[test]
fn each_alias_resolves_to_the_endpoint_that_serves_it() {
    let mut cfg = config();
    let mut voice = cfg.providers.local.clone();
    voice.models = vec!["whisper".into()];
    cfg.providers.extra.insert("voix".into(), voice);
    cfg.models
        .aliases
        .insert("stt".into(), "local:whisper".into());
    let t = targets(&cfg);
    assert_eq!(
        t.keys().map(String::as_str).collect::<Vec<_>>(),
        ["codex", "extra:voix", "local", "openrouter"]
    );
    assert_eq!(t["codex"], "codex:gpt-6-astra", "le modèle de main d'abord");

    // Un fournisseur coupé n'est pas rafraîchi.
    cfg.providers.codex.enabled = false;
    cfg.providers.extra.get_mut("voix").unwrap().enabled = false;
    assert!(endpoint_of(&cfg, "codex:gpt-6-astra").is_none());
    assert_eq!(endpoint_of(&cfg, "local:whisper").as_deref(), Some("local"));
}
