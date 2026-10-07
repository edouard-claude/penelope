//! Catalogue de modèles (§10.1).
//!
//! Synchronisé depuis `GET /api/v1/models` toutes les 6 h : fenêtre, modalités d'entrée
//! et de sortie, prix, paramètres supportés. Le catalogue sert à :
//! - calculer le coût quand le provider ne l'expose pas ;
//! - décider s'il faut émuler le tool calling (§10.1) ;
//! - refuser un alias qui pointe vers un modèle inconnu (§10.2) ;
//! - connaître la fenêtre de contexte pour la compaction (§5.4).

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Identifiant chez le provider, par exemple `deepseek/deepseek-v4-pro`.
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    pub max_output: Option<u64>,
    /// Prix en USD par token (et non par million).
    pub price_prompt: f64,
    pub price_completion: f64,
    pub price_cached_read: f64,
    pub price_image: f64,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub supported_parameters: Vec<String>,
    /// Prix d'écriture dans le cache, par token (`input_cache_write`), 0 si inconnu.
    #[serde(default)]
    pub price_cache_write: f64,
    /// Efforts de raisonnement acceptés, du plus fort au plus faible
    /// (`reasoning.supported_efforts`). `None` : aucun réglage exposé ; liste vide :
    /// toutes les valeurs sont acceptées.
    #[serde(default)]
    pub reasoning_efforts: Option<Vec<String>>,
    /// Raisonnement obligatoire : `effort: "none"` serait refusé par le modèle.
    #[serde(default)]
    pub reasoning_mandatory: bool,
    /// D'où vient `context_window` : le fournisseur, la configuration de l'endpoint, ou
    /// une valeur par défaut (#324).
    #[serde(default)]
    pub window_source: WindowSource,
}

/// Source de la fenêtre de contexte d'un modèle (#324) : `penelope model list` et
/// `/context` la montrent à côté du nombre, parce qu'un seuil de compaction calculé sur
/// un repli n'a pas la même valeur qu'un seuil calculé sur la fenêtre déclarée.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// Déclarée par le fournisseur (`GET /models` d'OpenRouter, de Codex, de l'endpoint).
    #[default]
    Provider,
    /// `providers.*.context_window` : l'endpoint local ne la déclare pas.
    Config,
    /// Valeur par défaut : modèle absent du catalogue, ou fournisseur muet.
    Fallback,
}

impl WindowSource {
    /// Libellé pour le propriétaire.
    pub fn label(self) -> &'static str {
        match self {
            WindowSource::Provider => "fournisseur",
            WindowSource::Config => "configuration",
            WindowSource::Fallback => "repli",
        }
    }
}

/// Fenêtre supposée d'un modèle que le catalogue ne connaît pas : prudente, pour que la
/// compaction parte trop tôt plutôt que trop tard.
pub const FALLBACK_WINDOW: u64 = 128_000;

impl ModelInfo {
    /// Effort le plus léger pour un appel utilitaire (classifieur) : `none` quand le
    /// raisonnement peut être coupé, sinon le plus faible accepté. `None` si le modèle
    /// n'expose aucun réglage : rien n'est alors envoyé.
    /// Le modèle réfléchit : il déclare des niveaux d'effort, ou l'impose. Un modèle sans
    /// raisonnement n'a ni budget ni effort à recevoir (issue #152).
    pub fn reasons(&self) -> bool {
        self.reasoning_mandatory || self.reasoning_efforts.is_some()
    }

    pub fn lightest_effort(&self) -> Option<String> {
        let efforts = self.reasoning_efforts.as_ref()?;
        // Raisonnement facultatif : on le **coupe**, même quand la liste déclarée n'offre
        // aucun niveau bas (issue #152). `deepseek-v4-flash` annonce
        // `supported_efforts: ["xhigh","high"]` avec `mandatory: false` : chercher le plus
        // faible de la liste rendait `high`, et la consolidation dépensait tout son budget
        // de sortie à réfléchir — 8 000 tokens pour un seul candidat, zéro opération. Le
        // fournisseur traduit `none` en ce qu'il faut (`reasoning: {enabled: false}`).
        if !self.reasoning_mandatory {
            return Some("none".into());
        }
        if efforts.is_empty() {
            return Some("minimal".into());
        }
        efforts.iter().rev().find(|e| *e != "none").cloned()
    }

    pub fn supports_tools(&self) -> bool {
        self.supported_parameters
            .iter()
            .any(|p| p == "tools" || p == "tool_choice" || p == "functions")
    }
    pub fn supports_reasoning(&self) -> bool {
        self.supported_parameters
            .iter()
            .any(|p| p == "reasoning" || p == "reasoning_effort" || p == "include_reasoning")
    }
    pub fn supports_structured_output(&self) -> bool {
        self.supported_parameters
            .iter()
            .any(|p| p == "response_format" || p == "structured_outputs")
    }
    pub fn accepts_images(&self) -> bool {
        self.input_modalities.iter().any(|m| m == "image")
    }
    pub fn produces_images(&self) -> bool {
        self.output_modalities.iter().any(|m| m == "image")
    }

    /// Coût d'un appel, à partir de l'usage.
    pub fn cost(&self, u: &crate::types::Usage) -> f64 {
        let fresh_prompt = u.prompt.saturating_sub(u.cached + u.cache_write) as f64;
        let write_price = if self.price_cache_write > 0.0 {
            self.price_cache_write
        } else {
            self.price_prompt
        };
        fresh_prompt * self.price_prompt
            + u.cached as f64 * self.price_cached_read
            + u.cache_write as f64 * write_price
            + u.completion as f64 * self.price_completion
    }

    /// Modèle minimal, pour les tests et les endpoints locaux sans catalogue.
    pub fn minimal(id: &str, provider: &str, window: u64) -> Self {
        ModelInfo {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            context_window: window,
            max_output: None,
            price_prompt: 0.0,
            price_completion: 0.0,
            price_cached_read: 0.0,
            price_image: 0.0,
            input_modalities: vec!["text".into()],
            output_modalities: vec!["text".into()],
            supported_parameters: vec!["tools".into(), "tool_choice".into()],
            price_cache_write: 0.0,
            reasoning_efforts: None,
            reasoning_mandatory: false,
            window_source: WindowSource::Provider,
        }
    }
}

/// Catalogue publié par `ArcSwap` : rafraîchi à chaud sans interrompre les tours.
#[derive(Clone)]
pub struct Catalog {
    inner: Arc<ArcSwap<CatalogData>>,
}

#[derive(Debug, Default)]
pub struct CatalogData {
    pub models: BTreeMap<String, ModelInfo>,
    pub fetched_at_ms: i64,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}

/// Ce que le catalogue sait des capacités d'un modèle, pour la résolution des rôles
/// (#332) : un principal qui lit les images sert la vision. Les embeddings ne sont jamais
/// le fait d'un modèle de conversation.
impl penelope_kernel::config::ModelCaps for Catalog {
    fn supports(&self, model_id: &str, cap: penelope_kernel::config::Capability) -> Option<bool> {
        use penelope_kernel::config::Capability;
        if cap == Capability::Embedding {
            return Some(false);
        }
        let info = self.get(model_id)?;
        Some(match cap {
            Capability::Vision => info.accepts_images(),
            Capability::ImageGenerate => info.produces_images(),
            Capability::Embedding => false,
        })
    }
}

impl Catalog {
    pub fn new() -> Self {
        Catalog {
            inner: Arc::new(ArcSwap::from_pointee(CatalogData::default())),
        }
    }

    pub fn snapshot(&self) -> Arc<CatalogData> {
        self.inner.load_full()
    }

    /// Remplace le catalogue d'OpenRouter ; les modèles d'un endpoint local, rangés à
    /// part (#259), et ceux de Codex restent (#324) : chaque fournisseur est rafraîchi à
    /// son tour, et celui d'OpenRouter, passé après Codex, effaçait sa fenêtre.
    pub fn replace(&self, models: Vec<ModelInfo>, fetched_at_ms: i64) {
        let mut map: BTreeMap<String, ModelInfo> = self
            .snapshot()
            .models
            .iter()
            .filter(|(k, m)| k.starts_with(LOCAL_KEY) || m.provider == "codex")
            .map(|(k, m)| (k.clone(), m.clone()))
            .collect();
        map.extend(models.into_iter().map(|m| (key_of(&m), m)));
        self.inner.store(Arc::new(CatalogData {
            models: map,
            fetched_at_ms,
        }));
    }

    /// Ajoute ou remplace quelques entrées sans écraser le reste (catalogue manuel
    /// d'un endpoint OpenAI-compatible).
    pub fn upsert(&self, models: Vec<ModelInfo>) {
        let cur = self.snapshot();
        let mut map = cur.models.clone();
        for m in models {
            map.insert(key_of(&m), m);
        }
        self.inner.store(Arc::new(CatalogData {
            models: map,
            fetched_at_ms: cur.fetched_at_ms,
        }));
    }

    /// Entrée d'un modèle `fournisseur:modèle`. Un modèle `local:` ou `openai_compat:` ne
    /// se lit que dans les entrées de l'endpoint local : un identifiant qui coïncide avec
    /// un modèle d'OpenRouter n'en prend ni le prix ni la fenêtre (#259). Un identifiant
    /// nu est d'abord un modèle d'OpenRouter, puis, à défaut, un modèle local ; un
    /// identifiant `openrouter:` ou `codex:` n'est jamais local.
    pub fn get(&self, id: &str) -> Option<ModelInfo> {
        let bare = strip_provider(id);
        let snap = self.snapshot();
        let local = || snap.models.get(&format!("{LOCAL_KEY}{bare}")).cloned();
        if is_local_provider(provider_of(id)) {
            return local();
        }
        let found = snap.models.get(bare).cloned();
        // Seul un identifiant nu peut être local ; `openrouter:` ou `codex:` non.
        if id == bare {
            found.or_else(local)
        } else {
            found
        }
    }

    /// Entrée du modèle qu'un fournisseur dit avoir servi (`model` de la réponse, sans
    /// préfixe) : c'est elle qui donne le prix d'un appel (#259).
    pub fn get_served(&self, provider: &str, served: &str) -> Option<ModelInfo> {
        if is_local_provider(provider) {
            self.get(&format!("local:{}", strip_provider(served)))
        } else {
            self.get(served)
        }
    }

    pub fn len(&self) -> usize {
        self.snapshot().models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn fetched_at_ms(&self) -> i64 {
        self.snapshot().fetched_at_ms
    }

    /// Liste filtrée (`penelope model list --filter`, `/models`).
    pub fn list(&self, filter: Option<&str>) -> Vec<ModelInfo> {
        let snap = self.snapshot();
        let mut v: Vec<ModelInfo> = snap
            .models
            .values()
            .filter(|m| match filter {
                None => true,
                Some(f) => {
                    let f = f.to_lowercase();
                    m.id.to_lowercase().contains(&f)
                        || m.name.to_lowercase().contains(&f)
                        || m.provider.to_lowercase().contains(&f)
                }
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Fenêtre de contexte, avec repli prudent si le modèle est inconnu.
    pub fn window_of(&self, id: &str) -> u64 {
        self.window(id).0
    }

    /// Fenêtre de contexte et sa source. Une fenêtre nulle (un modèle d'OpenRouter sans
    /// `context_length`) vaut un modèle inconnu : le repli, plutôt qu'un seuil à zéro.
    pub fn window(&self, id: &str) -> (u64, WindowSource) {
        match self.get(id) {
            Some(m) if m.context_window > 0 => (m.context_window, m.window_source),
            _ => (FALLBACK_WINDOW, WindowSource::Fallback),
        }
    }
}

/// Vrai pour le fournisseur d'un endpoint OpenAI-compatible (`ModelInfo::provider`,
/// `Provider::name`).
fn is_local_provider(p: &str) -> bool {
    matches!(p, "openai_compat" | "local")
}
/// Préfixe de clé des modèles d'un endpoint local : ils ne partagent pas l'espace des
/// identifiants d'OpenRouter (#259).
const LOCAL_KEY: &str = "local:";

/// Clé d'une entrée : l'identifiant, préfixé pour un modèle d'endpoint local.
fn key_of(m: &ModelInfo) -> String {
    if is_local_provider(&m.provider) {
        format!("{LOCAL_KEY}{}", m.id)
    } else {
        m.id.clone()
    }
}

/// Retire le préfixe de provider d'un identifiant `provider:model`.
pub fn strip_provider(id: &str) -> &str {
    match id.split_once(':') {
        Some((prefix, rest)) if is_provider_prefix(prefix) => rest,
        _ => id,
    }
}

/// Préfixe de provider d'un identifiant `provider:model`.
pub fn provider_of(id: &str) -> &str {
    match id.split_once(':') {
        Some((prefix, _)) if is_provider_prefix(prefix) => prefix,
        _ => "openrouter",
    }
}

/// Un préfixe de fournisseur, tel que la configuration les connaît : la liste vit dans
/// `penelope-kernel` pour que le découpage et la validation disent la même chose (#142).
fn is_provider_prefix(p: &str) -> bool {
    penelope_kernel::config::is_provider_prefix(p)
}

/// Analyse la réponse `GET /api/v1/models` d'OpenRouter.
///
/// Les prix y sont exprimés en USD **par token**, sous forme de chaînes.
pub fn parse_openrouter_models(body: &Value) -> Vec<ModelInfo> {
    let Some(data) = body.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    data.iter().filter_map(parse_one).collect()
}

fn parse_one(m: &Value) -> Option<ModelInfo> {
    let id = m.get("id")?.as_str()?.to_string();
    let pricing = m.get("pricing");
    let price = |k: &str| -> f64 {
        pricing
            .and_then(|p| p.get(k))
            .and_then(|v| match v {
                Value::String(s) => s.parse::<f64>().ok(),
                Value::Number(n) => n.as_f64(),
                _ => None,
            })
            .unwrap_or(0.0)
            .max(0.0)
    };
    let arch = m.get("architecture");
    let modalities = |k: &str| -> Vec<String> {
        arch.and_then(|a| a.get(k))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| vec!["text".into()])
    };

    Some(ModelInfo {
        name: m
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or(&id)
            .to_string(),
        provider: id.split('/').next().unwrap_or("").to_string(),
        context_window: m
            .get("context_length")
            .and_then(|c| c.as_u64())
            .or_else(|| {
                m.get("top_provider")
                    .and_then(|t| t.get("context_length"))
                    .and_then(|c| c.as_u64())
            })
            .unwrap_or(0),
        max_output: m
            .get("top_provider")
            .and_then(|t| t.get("max_completion_tokens"))
            .and_then(|c| c.as_u64()),
        price_prompt: price("prompt"),
        price_completion: price("completion"),
        price_cached_read: {
            let c = price("input_cache_read");
            if c > 0.0 { c } else { price("prompt") * 0.1 }
        },
        price_image: price("image"),
        input_modalities: modalities("input_modalities"),
        output_modalities: modalities("output_modalities"),
        supported_parameters: m
            .get("supported_parameters")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        price_cache_write: price("input_cache_write"),
        // `supported_efforts` : absent = pas de réglage, `null` = toutes les valeurs.
        reasoning_efforts: m
            .get("reasoning")
            .and_then(|r| match r.get("supported_efforts") {
                None => None,
                Some(Value::Array(a)) => Some(
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect(),
                ),
                Some(_) => Some(Vec::new()),
            }),
        reasoning_mandatory: m
            .get("reasoning")
            .and_then(|r| r.get("mandatory"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        window_source: WindowSource::Provider,
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Usage;
    use serde_json::json;

    fn sample_body() -> Value {
        json!({"data":[
            {
                "id":"deepseek/deepseek-v4-pro",
                "name":"DeepSeek V4 Pro",
                "context_length": 256000,
                "pricing": {"prompt":"0.0000004","completion":"0.0000016",
                            "input_cache_read":"0.00000004","image":"0"},
                "architecture": {"input_modalities":["text","image"],
                                 "output_modalities":["text"]},
                "supported_parameters":["tools","tool_choice","response_format","reasoning"],
                "top_provider": {"max_completion_tokens": 32000},
                "reasoning": {"supported_efforts": ["high","medium","low","none"],
                              "default_effort": "medium", "mandatory": false}
            },
            {
                "id":"vieux/modele-sans-outils",
                "context_length": 8192,
                "pricing": {"prompt":"0.000001","completion":"0.000002"},
                "architecture": {"input_modalities":["text"],"output_modalities":["text"]},
                "supported_parameters":[]
            }
        ]})
    }

    #[test]
    fn parses_openrouter_catalog() {
        let models = parse_openrouter_models(&sample_body());
        assert_eq!(models.len(), 2);
        let m = &models[0];
        assert_eq!(m.context_window, 256_000);
        assert!(m.supports_tools());
        assert!(m.accepts_images());
        assert!(!m.produces_images());
        assert_eq!(m.max_output, Some(32_000));
        assert!(!models[1].supports_tools());
    }

    /// #259 : un modèle local dont l'identifiant coïncide avec un modèle d'OpenRouter
    /// n'écrase pas son entrée, ni dans un sens ni dans l'autre ; chacun lit la sienne.
    #[test]
    fn a_local_model_never_shadows_an_openrouter_entry() {
        let c = Catalog::new();
        let mut routed = ModelInfo::minimal("qwen/qwen3-8b", "openrouter", 40_960);
        routed.price_prompt = 1e-6;
        c.replace(vec![routed], 1);
        c.upsert(vec![ModelInfo::minimal(
            "qwen/qwen3-8b",
            "openai_compat",
            32_768,
        )]);
        assert_eq!(c.len(), 2);
        let or = c.get("openrouter:qwen/qwen3-8b").unwrap();
        assert_eq!(
            (or.provider.as_str(), or.price_prompt),
            ("openrouter", 1e-6)
        );
        assert_eq!(c.get("qwen/qwen3-8b").unwrap().provider, "openrouter");
        let local = c.get("local:qwen/qwen3-8b").unwrap();
        assert_eq!((local.price_prompt, local.context_window), (0.0, 32_768));
        assert_eq!(
            c.get("openai_compat:qwen/qwen3-8b").unwrap().provider,
            "openai_compat"
        );
        assert_eq!(c.window_of("local:qwen/qwen3-8b"), 32_768);
        assert_eq!(c.window_of("openrouter:qwen/qwen3-8b"), 40_960);
        assert_eq!(
            c.get_served("openai_compat", "qwen/qwen3-8b")
                .unwrap()
                .price_prompt,
            0.0
        );
        assert_eq!(
            c.get_served("openrouter", "qwen/qwen3-8b")
                .unwrap()
                .price_prompt,
            1e-6
        );

        // Le rafraîchissement d'OpenRouter garde les modèles locaux.
        c.replace(Vec::new(), 2);
        assert!(c.get("local:qwen/qwen3-8b").is_some());
        assert!(c.get("openrouter:qwen/qwen3-8b").is_none());
        // Un identifiant nu sans modèle d'OpenRouter trouve le modèle local.
        assert_eq!(c.get("qwen/qwen3-8b").unwrap().provider, "openai_compat");
        // Un modèle local absent ne prend pas l'entrée d'OpenRouter.
        c.replace(vec![ModelInfo::minimal("a/b", "openrouter", 1)], 3);
        assert!(c.get("local:a/b").is_none());
    }

    /// #324 : le catalogue d'OpenRouter, rafraîchi après celui de Codex, ne l'efface
    /// plus ; chaque fenêtre dit sa source, et une fenêtre nulle vaut le repli.
    #[test]
    fn openrouter_refresh_keeps_codex_and_windows_carry_their_source() {
        let c = Catalog::new();
        c.upsert(crate::codex::fallback_models(&["gpt-6-astra".into()]));
        c.upsert(vec![ModelInfo {
            window_source: WindowSource::Config,
            ..ModelInfo::minimal("qwen", "openai_compat", 32_768)
        }]);
        let mut models = parse_openrouter_models(&sample_body());
        models[1].context_window = 0;
        c.replace(models, 1);
        assert_eq!(c.len(), 4);
        assert_eq!(
            c.window("codex:gpt-6-astra"),
            (crate::codex::DEFAULT_CODEX_WINDOW, WindowSource::Fallback)
        );
        assert_eq!(
            c.window("openrouter:deepseek/deepseek-v4-pro"),
            (256_000, WindowSource::Provider)
        );
        assert_eq!(c.window("local:qwen"), (32_768, WindowSource::Config));
        assert_eq!(
            c.window("openrouter:vieux/modele-sans-outils"),
            (FALLBACK_WINDOW, WindowSource::Fallback)
        );
        assert_eq!(
            c.window("openrouter:inconnu/x"),
            (FALLBACK_WINDOW, WindowSource::Fallback)
        );
    }

    #[test]
    fn missing_cache_price_falls_back_to_a_tenth() {
        let models = parse_openrouter_models(&sample_body());
        let m = &models[1];
        assert!((m.price_cached_read - m.price_prompt * 0.1).abs() < 1e-12);
    }

    #[test]
    fn cost_discounts_cached_tokens() {
        let models = parse_openrouter_models(&sample_body());
        let m = &models[0];
        let full = m.cost(&Usage {
            prompt: 100_000,
            completion: 1_000,
            ..Default::default()
        });
        let cached = m.cost(&Usage {
            prompt: 100_000,
            completion: 1_000,
            cached: 90_000,
            ..Default::default()
        });
        assert!(cached < full, "le cache doit réduire le coût");
        assert!(cached > 0.0);
    }

    #[test]
    fn lightest_reasoning_effort_respects_the_model_capabilities() {
        let models = parse_openrouter_models(&sample_body());
        assert_eq!(models[0].lightest_effort().as_deref(), Some("none"));
        assert_eq!(models[1].lightest_effort(), None, "aucun réglage exposé");

        let mut m = models[0].clone();
        m.reasoning_mandatory = true;
        assert_eq!(m.lightest_effort().as_deref(), Some("low"));
        m.reasoning_efforts = Some(Vec::new());
        assert_eq!(m.lightest_effort().as_deref(), Some("minimal"));

        // #152 : une liste sans niveau bas ne force pas à réfléchir quand le
        // raisonnement est facultatif. C'est le cas de `deepseek-v4-flash`
        // (`["xhigh","high"]`, `mandatory: false`), qui dépensait tout son budget de
        // sortie en raisonnement une nuit sur deux.
        let mut flash = models[0].clone();
        flash.reasoning_efforts = Some(vec!["xhigh".into(), "high".into()]);
        flash.reasoning_mandatory = false;
        assert_eq!(flash.lightest_effort().as_deref(), Some("none"));
        // Obligatoire : le plus faible déclaré, jamais `none`.
        flash.reasoning_mandatory = true;
        assert_eq!(flash.lightest_effort().as_deref(), Some("high"));
        // Liste vide et facultatif : coupé aussi.
        flash.reasoning_efforts = Some(Vec::new());
        flash.reasoning_mandatory = false;
        assert_eq!(flash.lightest_effort().as_deref(), Some("none"));
    }

    #[test]
    fn catalog_lookup_strips_provider_prefix() {
        let c = Catalog::new();
        c.replace(parse_openrouter_models(&sample_body()), 0);
        assert!(c.get("openrouter:deepseek/deepseek-v4-pro").is_some());
        assert!(c.get("deepseek/deepseek-v4-pro").is_some());
        assert!(c.get("inexistant/x").is_none());
        assert_eq!(c.window_of("openrouter:deepseek/deepseek-v4-pro"), 256_000);
        assert_eq!(c.window_of("inconnu"), 128_000, "repli prudent");
    }

    #[test]
    fn provider_prefix_parsing() {
        assert_eq!(provider_of("openrouter:a/b"), "openrouter");
        assert_eq!(provider_of("openai_compat:whisper"), "openai_compat");
        assert_eq!(provider_of("a/b"), "openrouter");
        assert_eq!(strip_provider("openrouter:a/b"), "a/b");
        // Un identifiant qui contient `:` sans être un préfixe connu reste intact.
        assert_eq!(strip_provider("modele:v2"), "modele:v2");
        // #142 : le backend Codex est un fournisseur à part entière.
        assert_eq!(provider_of("codex:gpt-6-astra"), "codex");
        assert_eq!(strip_provider("codex:gpt-6-astra"), "gpt-6-astra");
        // Une variante OpenRouter (`:free`) n'est pas un préfixe de fournisseur.
        assert_eq!(provider_of("x-ai/grok-4:free"), "openrouter");
        assert_eq!(strip_provider("x-ai/grok-4:free"), "x-ai/grok-4:free");
    }

    #[test]
    fn list_filters_and_sorts() {
        let c = Catalog::new();
        c.replace(parse_openrouter_models(&sample_body()), 0);
        assert_eq!(c.list(Some("deepseek")).len(), 1);
        assert_eq!(c.list(None).len(), 2);
        assert_eq!(c.list(None)[0].id, "deepseek/deepseek-v4-pro");
    }

    #[test]
    fn upsert_keeps_existing_entries() {
        let c = Catalog::new();
        c.replace(parse_openrouter_models(&sample_body()), 0);
        c.upsert(vec![ModelInfo::minimal(
            "embeddings-default",
            "local",
            8192,
        )]);
        assert_eq!(c.len(), 3);
        assert!(c.get("openai_compat:embeddings-default").is_some());
    }

    #[test]
    fn malformed_catalog_yields_nothing_instead_of_panicking() {
        assert!(parse_openrouter_models(&json!({"pas de data": 1})).is_empty());
        assert!(parse_openrouter_models(&json!({"data": [{"sans id": 1}]})).is_empty());
    }
}
