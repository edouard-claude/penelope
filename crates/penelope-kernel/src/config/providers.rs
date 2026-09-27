//! Sections `[providers]`, `[models]` et `[models.routing]` : fournisseurs et modèles.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Providers {
    pub openrouter: OpenRouter,
    pub local: LocalProvider,
    /// Backend Codex d'un abonnement ChatGPT (issue #142).
    pub codex: Codex,
    /// Endpoints OpenAI-compatibles supplémentaires (embeddings, STT…).
    pub extra: BTreeMap<String, LocalProvider>,
}

/// Préfixes de fournisseur reconnus dans un identifiant `fournisseur:modèle` (§10.2).
///
/// La liste vit ici parce que `penelope-llm` (qui découpe les identifiants) et la
/// validation de configuration (qui refuse un préfixe inconnu) doivent dire la même
/// chose. Un identifiant dont la partie gauche n'est pas de cette liste **et** porte un
/// `/` reste un modèle OpenRouter à suffixe (`x-ai/grok-4:free`).
pub const PROVIDER_PREFIXES: &[&str] = &["openrouter", "openai_compat", "local", "codex"];

/// Vrai si `p` nomme un fournisseur.
pub fn is_provider_prefix(p: &str) -> bool {
    PROVIDER_PREFIXES.contains(&p)
}

/// Vérifie un identifiant de modèle d'alias. Un préfixe inconnu est une faute de frappe
/// (`openroutr:`, `codx:`) : avant, il partait en silence chez OpenRouter, identifiant
/// complet en nom de modèle (issue #142).
pub fn check_model_id(id: &str) -> std::result::Result<(), String> {
    let Some((prefix, rest)) = id.split_once(':') else {
        return Err(format!(
            "`{id}` doit être de la forme `fournisseur:modèle` ({})",
            PROVIDER_PREFIXES.join(", ")
        ));
    };
    if is_provider_prefix(prefix) {
        return if rest.trim().is_empty() {
            Err(format!("`{id}` : aucun modèle après `{prefix}:`"))
        } else {
            Ok(())
        };
    }
    // `x-ai/grok-4:free` : pas un préfixe de fournisseur, un identifiant OpenRouter qui
    // porte un suffixe de variante. Le `/` du vendeur le distingue d'une faute de frappe.
    if prefix.contains('/') {
        return Ok(());
    }
    Err(format!(
        "`{id}` : fournisseur `{prefix}` inconnu (attendu {})",
        PROVIDER_PREFIXES.join(", ")
    ))
}

/// Backend Codex d'un abonnement ChatGPT (issue #142) : les modèles du plan (`gpt-6-astra`,
/// `gpt-5.6-*`) par « Sign in with ChatGPT », sans clé d'API.
///
/// Usage **toléré** par OpenAI (page « Codex for Open Source », déclarations publiques),
/// jamais garanti par contrat : Pénélope emprunte l'identité du client Codex CLI, et le
/// fournisseur peut être coupé du jour au lendemain. Repli documenté : une clé d'API sur
/// `openai_compat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Codex {
    /// Fournisseur actif. Faux tant que le compte n'est pas connecté
    /// (`penelope model auth codex`).
    pub enabled: bool,
    /// Adresse du backend Codex.
    pub base_url: String,
    /// Serveur d'autorisation du compte ChatGPT.
    pub issuer: String,
    /// Identifiant du client OAuth, celui de Codex CLI.
    pub client_id: String,
    /// En-tête `originator` envoyé au backend. Le serveur filtre cette valeur : la changer
    /// sans raison donne un 403 sur toutes les requêtes.
    pub originator: String,
    /// Version de client annoncée (`User-Agent`, `?client_version=`). Épinglée, mise à
    /// jour à la main quand le backend exige plus récent : le catalogue et certains
    /// identifiants de modèle en dépendent, et une version trop ancienne en fait
    /// disparaître (issue #148).
    pub client_version: String,
    /// Silence toléré pendant un flux, comme pour OpenRouter.
    pub stream_idle_timeout: String,
    /// Nouvelles tentatives sur erreur transitoire avant le flux (5xx, coupure). Un 429
    /// de quota n'est jamais rejoué.
    pub request_retries: u32,
    /// Résumé de raisonnement demandé (`auto`, `concise`, `detailed`, ou vide).
    pub reasoning_summary: String,
    /// Verbosité du texte rendu (`low`, `medium`, `high`).
    pub verbosity: String,
    /// Part de la fenêtre de quota qui déclenche une alerte (0 à 1).
    pub quota_alert_ratio: f64,
    /// Part de la fenêtre de quota au-delà de laquelle le fournisseur se met en retrait et
    /// laisse le repli jouer (0 à 1).
    pub quota_stop_ratio: f64,
    /// Modèles servis, en repli quand `GET /models` ne répond pas.
    pub models: Vec<String>,
}

impl Default for Codex {
    fn default() -> Self {
        Codex {
            enabled: false,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            issuer: "https://auth.openai.com".into(),
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".into(),
            originator: "codex_cli_rs".into(),
            client_version: "0.149.0".into(),
            stream_idle_timeout: "120s".into(),
            request_retries: 3,
            reasoning_summary: "auto".into(),
            verbosity: "medium".into(),
            quota_alert_ratio: 0.8,
            quota_stop_ratio: 0.95,
            models: vec![
                "gpt-6-astra".into(),
                "gpt-5.6-sol".into(),
                "gpt-5.6-terra".into(),
                "gpt-5.6-luna".into(),
                "gpt-5.5".into(),
                "gpt-5.4".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenRouter {
    /// Clé d'API, par référence au magasin de secrets.
    pub api_key: String,
    /// Adresse de l'API OpenRouter.
    pub base_url: String,
    /// Nouvelles tentatives sur erreur transitoire **avant** le flux (5xx, délai de
    /// connexion, limite de débit) : attente de 1 s, 2 s, 4 s. 0 : aucune.
    pub request_retries: u32,
    /// Silence toléré **pendant** un flux : au-delà, le flux est coupé et relancé. Tout
    /// octet reçu, commentaire compris, remet le compteur à zéro.
    pub stream_idle_timeout: String,
    /// Période de rechargement du catalogue de modèles.
    pub catalog_refresh: String,
    /// Attribution (`HTTP-Referer`, `X-OpenRouter-Title`, `X-OpenRouter-Categories`).
    pub referer: String,
    /// Titre d'attribution (`X-OpenRouter-Title`).
    pub title: String,
    /// Catégories d'attribution (`X-OpenRouter-Categories`).
    pub categories: String,
    pub routing: OpenRouterRouting,
    /// Fournisseur actif.
    pub enabled: bool,
}

impl Default for OpenRouter {
    fn default() -> Self {
        OpenRouter {
            api_key: "${SECRET:openrouter_api_key}".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            request_retries: 3,
            stream_idle_timeout: "120s".into(),
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
#[serde(default)]
pub struct OpenRouterRouting {
    /// OpenRouter peut passer à un autre provider du même modèle en cas d'échec.
    pub allow_fallbacks: bool,
    /// Ordre imposé des providers. Attention : il désactive le routage collant, donc le
    /// cache de préfixe entre deux tours.
    pub order: Vec<String>,
    /// `deny` : uniquement des providers qui ne conservent pas les données.
    pub data_collection: String,
    /// Uniquement les providers qui acceptent tous les paramètres de la requête.
    pub require_parameters: bool,
    /// Uniquement des endpoints à rétention nulle (ZDR).
    pub zdr: bool,
    /// `price`, `throughput` ou `latency` ; vide = répartition par défaut d'OpenRouter.
    pub sort: String,
    /// Providers autorisés, à l'exclusion des autres ; vide : tous.
    pub only: Vec<String>,
    /// Providers exclus.
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
#[serde(default)]
pub struct LocalProvider {
    /// Type d'endpoint (`openai_compat`).
    pub kind: String,
    /// Adresse de l'endpoint OpenAI-compatible.
    pub base_url: String,
    /// Clé éventuelle, par référence au magasin de secrets.
    pub api_key: String,
    /// Endpoint actif.
    pub enabled: bool,
    /// Modèles servis par l'endpoint.
    pub models: Vec<String>,
    /// Silence toléré pendant un flux, comme pour OpenRouter.
    pub stream_idle_timeout: String,
    /// Fenêtre de contexte annoncée pour les modèles servis par cet endpoint, quand
    /// `GET /models` ne la donne pas.
    pub context_window: u64,
}

impl Default for LocalProvider {
    fn default() -> Self {
        LocalProvider {
            kind: "openai_compat".into(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            api_key: String::new(),
            enabled: false,
            models: Vec::new(),
            stream_idle_timeout: "120s".into(),
            context_window: 32_768,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Models {
    /// Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2).
    pub aliases: BTreeMap<String, String>,
    /// Rôle vers alias : conversation, classification, compaction, relecture de mémoire,
    /// code, images, embeddings, transcription.
    pub roles: BTreeMap<String, String>,
    pub routing: Routing,
    /// Repère des coordonnées que rend le modèle du rôle `image_locate` : `auto` (déduit
    /// de la famille du modèle et des valeurs rendues), `pixels` (pixels de l'image) ou
    /// `per_mille` (0 à 1000 sur chaque axe, comme UI-TARS et Qwen3-VL). `image_inspect`
    /// ramène toujours les points en pixels de l'image, et refuse ceux qui ne tiennent
    /// pas dans le repère.
    pub locate_frame: String,
}

/// Repères de coordonnées d'un modèle de pointage (issues #125 et #128).
pub const LOCATE_FRAMES: &[&str] = &["auto", "pixels", "per_mille"];

/// Forme attendue d'une valeur, d'après la valeur en place : pour un message qui dit quoi
/// écrire au lieu de l'erreur brute du désérialiseur (issue #138).
pub fn expected_shape(current: &serde_json::Value) -> &'static str {
    match current {
        serde_json::Value::Array(_) => "une liste : `[\"a\", \"b\"]`, ou une valeur seule",
        serde_json::Value::Number(_) => "un nombre",
        serde_json::Value::Bool(_) => "`true` ou `false`",
        serde_json::Value::Object(_) => "une table : `{\"clé\": \"valeur\"}`",
        _ => "une chaîne",
    }
}

/// Valeur donnée à un champ liste : une valeur seule vaut une liste d'un élément, jamais
/// découpée (`"a b"` donne `["a b"]`, pas deux éléments) ; une liste passe telle quelle ;
/// le reste est refusé en nommant la forme attendue. Même règle pour `config set` et
/// `mcp edit` (issue #138).
pub fn list_value(
    field: &str,
    current: &serde_json::Value,
    new: serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    if !current.is_array() {
        return Ok(new);
    }
    match new {
        serde_json::Value::Array(_) => Ok(new),
        serde_json::Value::String(_) => Ok(serde_json::Value::Array(vec![new])),
        other => Err(format!(
            "`{field}` attend {} ; reçu `{other}`",
            expected_shape(current)
        )),
    }
}

/// Tables de la configuration dont les clés sont libres : `config set` y ajoute une
/// entrée nouvelle (`models.roles.image_locate`), là où une clé de structure inconnue
/// reste une faute de frappe refusée.
pub const MAP_PATHS: &[&str] = &[
    "models.aliases",
    "models.roles",
    "models.routing.fallback",
    "context.model_thresholds",
    "providers.extra",
];

/// Modèle d'embeddings par défaut : multilingue, servi par OpenRouter, sans serveur local
/// (issue #11).
pub const DEFAULT_EMBEDDING_MODEL: &str = "openrouter:openai/text-embedding-3-small";
/// Synthèse vocale par défaut : Voxtral TTS de Mistral, servi en local par mlx-audio
/// (issue #41).
pub const DEFAULT_TTS_MODEL: &str = "openai_compat:mlx-community/Voxtral-4B-TTS-2603-mlx-4bit";

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
            ("tts", DEFAULT_TTS_MODEL),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();

        let roles = [
            ("chat_default", "main"),
            ("classifier", "fast"),
            ("compaction", "summarizer"),
            ("memory_review", "fast"),
            ("approval_judge", "fast"),
            ("code", "reasoning"),
            ("image_generate", "image"),
            ("image_describe", "vision"),
            ("image_locate", "vision"),
            ("embedding", "embedding"),
            ("stt", "stt"),
            ("tts", "tts"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();

        Models {
            aliases,
            roles,
            routing: Routing::default(),
            locate_frame: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Routing {
    /// Classer la complexité d'un message pour choisir l'alias.
    pub classifier: bool,
    /// Alias d'un message simple.
    pub low: String,
    /// Alias d'un message moyen.
    pub medium: String,
    /// Alias d'un message complexe.
    pub high: String,
    /// Garder l'alias choisi pour la session (sauf l'alias `low`).
    pub sticky: bool,
    /// Alias de repli, dans l'ordre, quand un modèle ne répond pas.
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
