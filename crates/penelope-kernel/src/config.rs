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
#[serde(default)]
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
    pub voice: Voice,
    pub retention: Retention,
    pub backup: Backup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Owner {
    /// Identifiant Telegram du propriétaire : le seul compte auquel le bot répond (0 :
    /// canal fermé).
    pub telegram_user_id: i64,
    /// Fuseau horaire du propriétaire : planifications, digest, date du jour.
    pub timezone: String,
    /// Langue des réponses.
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
#[serde(default)]
pub struct Telegram {
    /// Jeton du bot, par référence au magasin de secrets.
    pub token: String,
    /// Réception des messages : `polling` (long polling) ou `webhook` (pas encore servi).
    pub mode: String,
    /// Sujets de forum Telegram. Sans effet dans cette version.
    pub topics: bool,
    /// Rendu riche natif de la Bot API plutôt que HTML.
    pub rich_messages: bool,
    /// Heures calmes `HH:MM-HH:MM` : les notifications non urgentes attendent la fin de la
    /// plage.
    pub quiet_hours: String,
    /// Adresse de la Bot API.
    pub api_base: String,
    /// Attente d'un appel `getUpdates` en long polling, en secondes.
    pub poll_timeout_s: u64,
    /// Messages envoyés au plus par seconde, par chat.
    pub rate_per_chat_per_s: f64,
    /// Taille maximale d'un message. Sans effet dans cette version.
    pub text_limit: usize,
    /// Taille maximale d'une légende. Sans effet dans cette version.
    pub caption_limit: usize,
    /// Fragments au-delà desquels une réponse part en document. Sans effet dans cette
    /// version.
    pub max_fragments: usize,
    /// Intervalle entre deux mises à jour du brouillon de réponse, en millisecondes (300 au
    /// moins).
    pub draft_interval_ms: u64,
    /// Adresse du webhook. Sans effet dans cette version.
    pub webhook_url: String,
    /// Ancien interrupteur des groupes, sans effet depuis 0.17.4 : un groupe s'ouvre en
    /// ajoutant son identifiant à `telegram.allowed_chats`.
    pub allow_groups: bool,
    /// Conversations de groupe autorisées, par identifiant (`-100…` pour un supergroupe) :
    /// le propriétaire y parle, y compris en administrateur anonyme ; un sujet donne une
    /// session. `penelope doctor` liste les conversations refusées récemment avec leur
    /// identifiant.
    pub allowed_chats: Vec<i64>,
    /// Attente après un morceau qui ressemble à une coupure de Telegram (4 000 caractères
    /// ou plus) ou un message transféré, en millisecondes : les morceaux d'un même envoi
    /// forment un seul tour. Un message court tapé part tout de suite. 0 : un message, un
    /// tour.
    pub text_group_window_ms: u64,
    /// Messages regroupés à partir desquels Pénélope demande quoi en faire au lieu de
    /// répondre à chacun. 0 : jamais.
    pub burst_messages: usize,
    /// Caractères cumulés à partir desquels elle demande de même. 0 : jamais.
    pub burst_chars: usize,
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
            allowed_chats: Vec::new(),
            text_group_window_ms: 2_000,
            burst_messages: 5,
            burst_chars: 20_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Providers {
    pub openrouter: OpenRouter,
    pub local: LocalProvider,
    /// Endpoints OpenAI-compatibles supplémentaires (embeddings, STT…).
    pub extra: BTreeMap<String, LocalProvider>,
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
    /// Repère des coordonnées que rend le modèle du rôle `image_locate` : `pixels`
    /// (pixels de l'image reçue, comme UI-TARS) ou `per_mille` (0 à 1000 sur chaque axe,
    /// comme Qwen-VL). `image_inspect` les ramène toujours en pixels de l'image.
    pub locate_frame: String,
}

/// Repères de coordonnées d'un modèle de pointage (issue #125).
pub const LOCATE_FRAMES: &[&str] = &["pixels", "per_mille"];

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
            locate_frame: "pixels".into(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    /// Plafond de dépense par jour, en dollars.
    pub daily_usd: f64,
    /// Plafond de dépense par session, en dollars.
    pub session_usd: f64,
    /// Plafond de dépense par run de workflow, en dollars.
    pub run_usd: f64,
    /// Part d'un plafond à partir de laquelle une alerte part.
    pub alert_ratio: f64,
    /// Coût d'un tour de conversation à chaque multiple duquel Pénélope demande si elle
    /// continue (issue #19). 0 : jamais.
    pub turn_checkpoint_usd: f64,
    /// Coût d'un tour au-delà duquel la réponse finale l'indique. 0 : jamais.
    pub show_turn_cost_usd: f64,
    /// Nombre d'appels au modèle dans un tour à chaque multiple duquel le résultat d'outil
    /// suggère de regrouper les commandes ou de déléguer à un sous-agent. 0 : jamais.
    pub delegate_after_calls: u32,
    /// Dépense du jour réservée aux résumés de compaction une fois le plafond du jour
    /// atteint, en dollars ; les plafonds de session et de run ne les arrêtent jamais.
    pub compaction_reserve_usd: f64,
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
            compaction_reserve_usd: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Context {
    /// Part de la fenêtre du modèle à partir de laquelle l'historique est compacté.
    pub compaction_threshold: f64,
    /// Part de la fenêtre gardée intacte en fin d'historique.
    pub tail_ratio: f64,
    /// Taille minimale de la fin d'historique gardée intacte, en jetons.
    pub tail_min_tokens: usize,
    /// Taille maximale de la fin d'historique gardée intacte, en jetons.
    pub tail_max_tokens: usize,
    /// Messages du propriétaire gardés intacts au moins.
    pub min_tail_user_messages: usize,
    /// Part maximale de la fenêtre qu'un résultat d'outil peut occuper.
    pub max_tool_result_share: f64,
    /// Taille à partir de laquelle un résultat d'outil est rangé en artefact et résumé, en
    /// jetons.
    pub large_payload_tokens: usize,
    /// Taille de prompt au-delà de laquelle la compaction se déclenche, quelle que soit la
    /// fenêtre du modèle : une limite de coût, pas de fenêtre (issue #18). 0 : aucune.
    pub max_prompt_tokens: usize,
    /// Seuil de compaction propre à un modèle (identifiant avec ou sans provider). Sans
    /// entrée, le seuil général s'applique, abaissé sur une fenêtre courte pour laisser la
    /// place d'un résultat d'outil et de la réponse.
    pub model_thresholds: BTreeMap<String, f64>,
    /// Marge sous le seuil à partir de laquelle la compaction se prépare en tâche de fond.
    pub background_compaction_margin: f64,
    /// Attentes successives après une compaction en échec, en millisecondes.
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
#[serde(default)]
pub struct Memory {
    /// Répertoire du vault (`{data}` : répertoire de données).
    pub vault_path: String,
    /// Période de commit du vault sous git ; `0s` : désactivé.
    pub vault_git_autocommit: String,
    /// Remote git où pousser le vault ; vide : aucun.
    pub vault_git_remote: String,
    /// Budget du profil injecté (`profil.md`), en jetons.
    pub profile_budget_tokens: usize,
    /// Budget du niveau Cœur injecté (`memoire.md`), en jetons.
    pub core_budget_tokens: usize,
    /// Budget des projets injectés (`projets.md`), en jetons.
    pub project_budget_tokens: usize,
    /// Budget du rappel automatique par tour, en jetons.
    pub recall_budget_tokens: usize,
    /// Temps accordé au rappel automatique avant de répondre sans lui, en millisecondes.
    pub recall_timeout_ms: u64,
    /// Pertinence minimale d'une entrée pour être rappelée automatiquement : rang de
    /// recherche normalisé (1 pour la première d'une liste, 2 pour la première des deux),
    /// sans la récence ni l'importance, qui ne font qu'ordonner.
    pub trigger_threshold: f64,
    /// Entrées rappelées automatiquement au plus par tour.
    pub max_injected_per_turn: usize,
    /// Demi-vie de la récence dans le classement des souvenirs, en jours : un souvenir
    /// ancien passe après un récent équivalent, il reste rappelable.
    pub half_life_days: f64,
    /// Similarité cosinus de doublon. Sans effet dans cette version.
    pub dedup_cosine: f64,
    /// Similarité à partir de laquelle deux candidats sont des doublons.
    pub dedup_jaccard: f64,
    /// Inactivité qui clôt un épisode. Sans effet dans cette version.
    pub episode_idle: String,
    /// Écart de sujet qui clôt un épisode. Sans effet dans cette version.
    pub episode_topic_shift: f64,
    /// Candidats notés au plus par relecture d'un échange ; 0 : relecture désactivée.
    pub review_max_candidates: usize,
    /// Candidats consolidés par appel au modèle, la nuit : au-delà, la réponse ne tient
    /// plus dans la fenêtre de sortie et tout le lot est reporté.
    pub dream_batch: usize,
    /// Heure de la consolidation nocturne (cron, fuseau du propriétaire).
    pub dreaming_cron: String,
    /// Attente avant de reprendre un lot de la consolidation après une erreur passagère
    /// du modèle (flux muet, 5xx, 429), doublée à la seconde reprise.
    pub dream_retry_wait: String,
    /// Heure du digest du matin (cron, fuseau du propriétaire).
    pub digest_cron: String,
    pub promotion: Promotion,
    pub intents: Intents,
    /// Âge d'élagage du journal. Sans effet dans cette version.
    pub prune_episodic_days: i64,
    /// Âge au-delà duquel un écart jamais promu est abandonné, en jours.
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
            half_life_days: 180.0,
            dedup_cosine: 0.92,
            dedup_jaccard: 0.90,
            episode_idle: "2h".into(),
            episode_topic_shift: 0.35,
            review_max_candidates: 5,
            dream_batch: 40,
            dreaming_cron: "30 3 * * *".into(),
            dream_retry_wait: "2m".into(),
            digest_cron: "0 8 * * *".into(),
            promotion: Promotion::default(),
            intents: Intents::default(),
            prune_episodic_days: 180,
            expire_ecart_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Promotion {
    /// Occurrences minimales d'un écart pour devenir une exception.
    pub ecart_min_occurrences: u32,
    /// Sessions distinctes minimales d'un écart.
    pub ecart_min_sessions: u32,
    /// Jours distincts minimaux d'un écart.
    pub ecart_min_days: u32,
    /// Ignoré depuis 0.14.0 : faits, préférences, décisions et corrections passent par la
    /// grille de tri (issue #37). Gardé pour qu'une configuration existante reste valide.
    pub fact_min_recalls: u32,
    /// Ignoré depuis 0.14.0 (grille de tri, issue #37).
    pub fact_min_importance: u32,
    /// Ignoré depuis 0.14.0 (grille de tri, issue #37).
    pub preference_min_sessions: u32,
    /// Part maximale des entrées d'un fichier retirées en une nuit.
    pub max_retire_ratio: f64,
    /// Confiance d'une règle contestée. Sans effet dans cette version.
    pub contested_confidence: f64,
    /// Observations minimales d'une règle contestée. Sans effet dans cette version.
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
#[serde(default)]
pub struct Intents {
    /// Délai minimal entre deux déclenchements d'une intention.
    pub cooldown: String,
    /// Déclenchements au plus d'une intention.
    pub fire_budget: u32,
    /// Durée de vie d'une intention.
    pub expiry: String,
    /// Intentions déclenchées au plus par tour.
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
#[serde(default)]
pub struct Mcp {
    /// `lazy` | `eager`. Sans effet dans cette version.
    pub registry_mode: String,
    /// Processus de serveurs MCP actifs au plus.
    pub max_processes: usize,
    /// Délai par défaut d'un appel MCP (réglé par serveur dans `mcp.d`). Sans effet dans
    /// cette version.
    pub default_timeout: String,
    /// Retour OAuth : `paste_back` (adresse collée dans Telegram) ou `public_callback`.
    pub oauth_redirect_mode: String,
    /// Adresse publique de retour OAuth en mode `public_callback`.
    pub public_callback_url: String,
    /// Adresse du document de métadonnées client OAuth ; vide : enregistrement dynamique.
    pub cimd_url: String,
    /// Version de protocole MCP préférée. Sans effet dans cette version.
    pub preferred_protocol: String,
    /// Inactivité d'arrêt d'un serveur (réglée par serveur dans `mcp.d`). Sans effet dans
    /// cette version.
    pub idle_timeout: String,
    /// Appels simultanés au plus par serveur. Sans effet dans cette version.
    pub max_concurrency_per_server: usize,
    /// Outils MCP gardés décrits d'un tour à l'autre, au plus.
    pub sticky_set_max: usize,
    /// Taille maximale d'un schéma d'outil exposé directement au modèle, en octets.
    pub schema_max_bytes: usize,
    /// Taille totale des schémas exposés directement au modèle, en octets.
    pub eager_total_max_bytes: usize,
    /// Port local du retour OAuth.
    pub callback_port: u16,
    pub policy: McpPolicy,
    /// Attente maximale entre deux redémarrages d'un serveur. Sans effet dans cette
    /// version.
    pub restart_backoff_max: String,
    /// Échecs consécutifs après lesquels un serveur est mis de côté.
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
#[serde(default)]
pub struct McpPolicy {
    /// Politique d'un outil MCP en lecture : `auto`, `ask`, `ask_twice` ou `deny`.
    pub read: String,
    /// Politique d'un outil MCP en écriture.
    pub write: String,
    /// Politique d'un outil MCP destructif.
    pub destructive: String,
    /// Politique d'un outil MCP à effet externe.
    pub external: String,
    /// Politique d'un outil MCP au risque inconnu.
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
#[serde(default)]
pub struct Runners {
    /// Tours traités en parallèle.
    pub count: usize,
    /// Durée du bail d'un tour réclamé ; au-delà, un autre runner le reprend.
    pub lease_ttl: String,
    /// Période de renouvellement du bail : au plus la moitié de `lease_ttl`, sinon un tour
    /// en cours perd son bail.
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
#[serde(default)]
pub struct Sandbox {
    /// Profil du bac à sable de `shell_exec` : `read-only`, `workspace-write` ou `full`.
    pub default_profile: String,
    /// Serveurs MCP autorisés à tourner avec le profil `full` (sans bac à sable).
    pub allow_full_for: Vec<String>,
    /// Serveurs MCP stdio qui gardent leur bac à sable mais joignent le trousseau macOS :
    /// ceux dont le métier est de lire leurs propres identifiants. Le trousseau reste fermé
    /// aux autres, qui y verraient « introuvable » ce qui y est rangé.
    pub allow_keychain_for: Vec<String>,
    /// Répertoires de travail des outils de fichiers et du shell, en plus du défaut.
    pub workspaces: Vec<String>,
    /// Réseau pour **toutes** les commandes de `shell_exec` et des étapes `shell`. Faux :
    /// une commande n'a le réseau que si son appel le demande (`network: true`, carte
    /// d'approbation qui le dit, « Toujours » borné à la famille de commandes) ou si son
    /// étape de workflow le déclare. Une configuration qui porte `true` le garde.
    pub shell_network: bool,
    /// Chemins dont la lecture est refusée aux commandes sous bac à sable, même quand le
    /// profil lit le disque : clés, jetons, base de Pénélope, secrets, configuration.
    /// `{data}`, `{config}`, `{state}` et `~` sont développés.
    pub deny_read: Vec<String>,
}

/// Lectures refusées par défaut : ce qu'une consigne cachée dans un résultat d'outil
/// chercherait à exfiltrer (issue #68).
pub fn default_deny_read() -> Vec<String> {
    [
        "~/.ssh",
        "~/.aws",
        "~/.gnupg",
        "~/.config/gh",
        "~/.netrc",
        "~/.kube",
        "~/.docker/config.json",
        "{data}/penelope.db",
        "{data}/secrets.enc",
        "{data}/mcp.d",
        "{config}",
        "{state}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox {
            default_profile: "workspace-write".into(),
            allow_full_for: Vec::new(),
            allow_keychain_for: Vec::new(),
            workspaces: Vec::new(),
            shell_network: false,
            deny_read: default_deny_read(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Observability {
    /// Export OpenTelemetry. Sans effet dans cette version.
    pub otlp_endpoint: String,
    /// Adresse d'exposition Prometheus. Sans effet dans cette version : les métriques se
    /// lisent par `penelope metrics`.
    pub prometheus: String,
    /// Durée de conservation des journaux, en jours.
    pub log_retention_days: u32,
    /// Niveau de journalisation du daemon (`info`, `debug`, `warn`…), lu au démarrage ;
    /// la variable `PENELOPE_LOG` l'emporte.
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
#[serde(default)]
pub struct Tools {
    /// Shell de `shell_exec`, programme puis arguments (`-c` par défaut) ; vide : shell de la
    /// plateforme.
    pub shell: String,
    /// Délai d'une commande `shell_exec`.
    pub shell_timeout: String,
    /// Hôtes autorisés pour `http_fetch` ; vide : tous.
    pub http_allowlist: Vec<String>,
    /// Refuser les adresses privées et locales dans `http_fetch`.
    pub http_block_private_ips: bool,
    /// Appels identiques qui font arrêter une boucle d'outil.
    pub loop_detector_repeats: usize,
    /// Taille maximale d'une sortie de commande gardée telle quelle, en octets.
    pub max_output_bytes: usize,
    /// Mode d'approbation par défaut d'une session : `ask` (demander tout, lectures du
    /// shell comprises), `reads` (lectures sans demande, le reste selon la politique),
    /// `auto` (tout sans demande sauf le destructif). `/mode` le change pour une session.
    pub approval_mode: String,
    /// Familles de commandes `shell_exec` autorisées d'avance, sans enchaînement : par
    /// exemple `cargo test`, `npm run lint`.
    pub shell_allow: Vec<String>,
    /// Familles de commandes autorisées d'avance **avec** le réseau : `git push`, `gh pr`.
    pub shell_allow_network: Vec<String>,
}

/// Modes d'approbation d'une session (issue #111).
pub const APPROVAL_MODES: &[&str] = &["ask", "reads", "auto"];

impl Default for Tools {
    fn default() -> Self {
        Tools {
            shell: String::new(),
            shell_timeout: "120s".into(),
            http_allowlist: Vec::new(),
            http_block_private_ips: true,
            loop_detector_repeats: 3,
            max_output_bytes: 256 * 1024,
            approval_mode: "reads".into(),
            shell_allow: Vec::new(),
            shell_allow_network: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Workflows {
    /// Durée de conservation de l'espace de travail d'un run terminé, en jours.
    pub workspace_retention_days: i64,
    /// Profondeur maximale de sous-workflows.
    pub max_depth: u32,
    /// Itérations au plus d'un run sans réglage propre. Sans effet dans cette version.
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
#[serde(default)]
pub struct Upgrade {
    /// Canal de mise à jour. Sans effet dans cette version.
    pub channel: String,
    /// Adresse de la liste des releases ; vide : le dépôt GitHub.
    pub base_url: String,
    /// Clé publique minisign des sommes ; vide : celle du binaire de release.
    pub minisign_pubkey: String,
    /// Délai de confirmation de santé. Sans effet dans cette version.
    pub health_timeout: String,
    /// Signal de vie quotidien. Sans effet dans cette version.
    pub heartbeat_daily: bool,
    /// Identité de signature macOS (nom du certificat ou empreinte SHA-1) : le binaire
    /// téléchargé est re-signé avec elle avant la bascule (issue #28). Vide : non re-signé.
    pub codesign_identity: String,
    /// Identifiant fixe de la signature macOS.
    pub codesign_identifier: String,
    /// Répertoire du binaire de release quand une installation source bascule vers les
    /// releases (issue #33).
    pub install_dir: String,
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
            install_dir: "~/.local/bin".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Voice {
    /// Voix préréglée du modèle de synthèse (rôle `tts`).
    pub tts_voice: String,
    /// Longueur maximale d'un texte lu en vocal, en caractères : au-delà, un résumé vocal.
    pub max_chars: usize,
    /// Répondre en vocal quand le propriétaire vient d'envoyer un vocal.
    pub reply_in_kind: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Voice {
            tts_voice: "fr_female".into(),
            max_chars: 1_500,
            reply_in_kind: false,
        }
    }
}

/// Sauvegarde complète vers un dépôt privé (issue #42).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Backup {
    /// Dépôt git privé où pousser les sauvegardes chiffrées ; vide : celui du vault.
    pub git_remote: String,
    /// Heure de la sauvegarde nocturne (cron à cinq champs) ; vide : aucune.
    pub cron: String,
    /// Sauvegardes quotidiennes gardées.
    pub keep_daily: u32,
    /// Sauvegardes hebdomadaires gardées.
    pub keep_weekly: u32,
    /// Sauvegardes mensuelles gardées.
    pub keep_monthly: u32,
    /// Inclure les artefacts et les médias reçus. Lourd, et reconstructible.
    pub include_media: bool,
    /// Taille maximale d'une archive poussée, en octets (limite de fichier de GitHub).
    pub max_push_bytes: u64,
}

impl Default for Backup {
    fn default() -> Self {
        Backup {
            git_remote: String::new(),
            cron: "0 4 * * *".into(),
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 12,
            include_media: false,
            max_push_bytes: 100 * 1024 * 1024,
        }
    }
}

/// Rétention des traces (issue #46) : ce qui n'est ni la mémoire ni la chaîne d'audit
/// finit par disparaître.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Retention {
    /// Jours gardés pour les tours terminés, les requêtes au modèle, les updates Telegram
    /// et les clés de travail. 0 : rien n'est effacé.
    pub days: u32,
    /// Jours gardés pour les pré-images de la mémoire (`mem_history`). 0 : rien n'est
    /// effacé.
    pub memory_history_days: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            days: 90,
            memory_history_days: 30,
        }
    }
}

// ---------------------------------------------------------------- lecture et écriture

const SAMPLE_HEADER: &str = "\
# Configuration de Pénélope.
#
# Seules les clés qui s'écartent des valeurs par défaut figurent ici. Toutes les clés,
# leur valeur par défaut et leur rôle : `penelope config get`, ou la référence des clés
# de docs/install-headless.md. Une modification par `penelope config set` ne touche que
# la clé visée : commentaires et ordre de ce fichier sont conservés.

";

/// Clés présentes dans `raw` et absentes de la configuration relue : ce que ce binaire
/// ne connaît pas. Les tables libres (alias, seuils par modèle…) sont relues avec leurs
/// clés, donc jamais signalées.
fn unknown_keys(prefix: &str, raw: &toml::Table, known: &toml::Table, out: &mut Vec<String>) {
    for (k, v) in raw {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match (v, known.get(k)) {
            (_, None) => out.push(path),
            (toml::Value::Table(r), Some(toml::Value::Table(kn))) => {
                unknown_keys(&path, r, kn, out)
            }
            _ => {}
        }
    }
}

/// Réécrit `existing` en ne touchant que les clés qui changent entre `before` et `after`
/// (issue #76) : commentaires, ordre, clés inconnues et clés omises par le propriétaire
/// restent tels quels, et le fichier n'acquiert pas les clés nouvelles d'une version tant
/// que personne ne les pose.
pub fn edit_toml(existing: &str, before: &Config, after: &Config) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e: toml_edit::TomlError| KernelError::config(e.to_string()))?;
    let b = toml::Value::try_from(before).map_err(|e| KernelError::config(e.to_string()))?;
    let a = toml::Value::try_from(after).map_err(|e| KernelError::config(e.to_string()))?;
    if let (Some(b), Some(a)) = (b.as_table(), a.as_table()) {
        patch_table(doc.as_table_mut(), Some(b), a)?;
    }
    Ok(doc.to_string())
}

fn edit_value(v: &toml::Value) -> Result<toml_edit::Value> {
    v.to_string()
        .parse::<toml_edit::Value>()
        .map_err(|e| KernelError::config(e.to_string()))
}

/// Remplace une valeur en gardant sa décoration (commentaire en fin de ligne).
fn set_value(item: &mut toml_edit::Item, v: &toml::Value) -> Result<()> {
    let mut next = edit_value(v)?;
    if let Some(old) = item.as_value() {
        *next.decor_mut() = old.decor().clone();
    }
    *item = toml_edit::Item::Value(next);
    Ok(())
}

fn patch_table(
    doc: &mut toml_edit::Table,
    before: Option<&toml::Table>,
    after: &toml::Table,
) -> Result<()> {
    for (k, av) in after {
        let bv = before.and_then(|b| b.get(k));
        if bv == Some(av) {
            continue;
        }
        match (av, doc.get_mut(k)) {
            (toml::Value::Table(at), Some(toml_edit::Item::Table(t))) => {
                patch_table(t, bv.and_then(|v| v.as_table()), at)?
            }
            (toml::Value::Table(at), None) => {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                patch_table(&mut t, bv.and_then(|v| v.as_table()), at)?;
                doc.insert(k, toml_edit::Item::Table(t));
            }
            // Valeur scalaire, tableau ou table en ligne : remplacée d'un bloc.
            (_, Some(item)) => set_value(item, av)?,
            (_, None) => {
                doc.insert(k, toml_edit::Item::Value(edit_value(av)?));
            }
        }
    }
    if let Some(b) = before {
        for k in b.keys() {
            if !after.contains_key(k) {
                doc.remove(k);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- validation

impl Config {
    /// Lecture **tolérante** (issue #76) : une clé que ce binaire ne connaît pas (écrite
    /// par une version plus récente, ou faute de frappe) est ignorée et renvoyée pour être
    /// signalée par `doctor`, `config validate` et le démarrage. Un retour arrière ne
    /// dépend ainsi d'aucun état écrit par la version que l'on quitte. Une valeur mal
    /// typée reste une erreur.
    pub fn parse(s: &str) -> Result<(Config, Vec<String>)> {
        let cfg: Config = toml::from_str(s).map_err(|e| KernelError::config(e.to_string()))?;
        let raw: toml::Table = toml::from_str(s).map_err(|e| KernelError::config(e.to_string()))?;
        let known = toml::Value::try_from(&cfg).map_err(|e| KernelError::config(e.to_string()))?;
        let mut unknown = Vec::new();
        if let Some(known) = known.as_table() {
            unknown_keys("", &raw, known, &mut unknown);
        }
        Ok((cfg, unknown))
    }

    /// Lecture **stricte** : toute clé inconnue est une erreur. Pour une saisie, pas pour
    /// le fichier qu'une autre version a pu écrire.
    pub fn from_toml(s: &str) -> Result<Config> {
        let (cfg, unknown) = Config::parse(s)?;
        if !unknown.is_empty() {
            return Err(KernelError::config(format!(
                "clé inconnue : {}",
                unknown.join(", ")
            )));
        }
        Ok(cfg)
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| KernelError::config(e.to_string()))
    }

    /// Fichier de premier démarrage : seules les clés qui diffèrent des valeurs par
    /// défaut, sous un en-tête qui dit où trouver les autres (#76). Un fichier court
    /// suit les défauts des versions suivantes et se relit par une version antérieure.
    pub fn sample_toml(owner_id: i64) -> Result<String> {
        let body = edit_toml("", &Config::default(), &Config::sample(owner_id))?;
        Ok(format!("{SAMPLE_HEADER}{body}"))
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

        if !LOCATE_FRAMES.contains(&self.models.locate_frame.as_str()) {
            return Err(KernelError::config(format!(
                "models.locate_frame doit valoir {} (reçu `{}`)",
                LOCATE_FRAMES.join(", "),
                self.models.locate_frame
            )));
        }
        if !APPROVAL_MODES.contains(&self.tools.approval_mode.as_str()) {
            return Err(KernelError::config(format!(
                "tools.approval_mode doit valoir {} (reçu `{}`)",
                APPROVAL_MODES.join(", "),
                self.tools.approval_mode
            )));
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
        parse_duration(&self.memory.dream_retry_wait)?;
        parse_duration(&self.memory.vault_git_autocommit)?;
        parse_duration(&self.memory.intents.cooldown)?;
        parse_duration(&self.memory.intents.expiry)?;
        parse_duration(&self.mcp.default_timeout)?;
        parse_duration(&self.mcp.idle_timeout)?;
        parse_duration(&self.providers.openrouter.catalog_refresh)?;
        parse_duration(&self.providers.openrouter.stream_idle_timeout)?;
        parse_duration(&self.providers.local.stream_idle_timeout)?;
        let lease_ttl = parse_duration(&self.runners.lease_ttl)?;
        let heartbeat = parse_duration(&self.runners.heartbeat)?;
        // Il faut au moins deux battements par bail : sinon un tour un peu long expire et
        // un autre runner le reprend alors qu'il tourne encore (#43).
        if heartbeat * 2 > lease_ttl {
            return Err(KernelError::config(format!(
                "runners.heartbeat ({}) doit valoir au plus la moitié de runners.lease_ttl \
                 ({}) : au-delà, un tour en cours perd son bail et part en double",
                self.runners.heartbeat, self.runners.lease_ttl
            )));
        }
        parse_duration(&self.tools.shell_timeout)?;
        crate::cron::Cron::parse(&self.memory.dreaming_cron)?;
        if !self.backup.cron.trim().is_empty() {
            crate::cron::Cron::parse(&self.backup.cron)?;
        }
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
        self.model_threshold(model_id)
            .unwrap_or(self.context.compaction_threshold)
    }

    /// Seuil propre au modèle, s'il y en a un (identifiant avec ou sans provider).
    pub fn model_threshold(&self, model_id: &str) -> Option<f64> {
        let bare = model_id.split_once(':').map(|(_, m)| m).unwrap_or(model_id);
        self.context
            .model_thresholds
            .get(model_id)
            .or_else(|| self.context.model_thresholds.get(bare))
            .copied()
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
    /// Sérialise les mutations : lecture de l'instantané, écriture du fichier et
    /// publication sous un seul verrou, sinon deux mutations concurrentes s'écrasent et
    /// se volent leur fichier temporaire (issue #45).
    writing: std::sync::Mutex<()>,
    /// Clés du fichier que ce binaire ne connaît pas, à la dernière lecture (#76).
    unknown: std::sync::Mutex<Vec<String>>,
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
            writing: std::sync::Mutex::new(()),
            unknown: std::sync::Mutex::new(Vec::new()),
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
        let mut unknown = Vec::new();
        let cfg = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let (mut c, u) = Config::parse(&raw)?;
            if !u.is_empty() {
                tracing::warn!(
                    cles = %u.join(", "),
                    "clés de configuration inconnues de cette version : ignorées"
                );
            }
            unknown = u;
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
            atomic_write(&path, Config::sample_toml(owner_id)?.as_bytes())?;
            c
        };
        let cs = ConfigStore::new(cfg, path, store, clock);
        cs.set_unknown(unknown);
        Ok(cs)
    }

    /// Clés du fichier ignorées à la dernière lecture : `doctor` les nomme (#76).
    pub fn unknown_keys(&self) -> Vec<String> {
        match self.unknown.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    fn set_unknown(&self, keys: Vec<String>) {
        match self.unknown.lock() {
            Ok(mut g) => *g = keys,
            Err(p) => *p.into_inner() = keys,
        }
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
    /// Deux mutations concurrentes s'exécutent l'une après l'autre : la seconde part de
    /// ce que la première a publié, jamais d'un instantané périmé (issue #45).
    pub fn mutate<F>(&self, source: &str, f: F) -> Result<Arc<Generation>>
    where
        F: FnOnce(&mut Config) -> Result<Vec<String>>,
    {
        let _writing = self.lock_writing();
        self.apply(source, true, f)
    }

    fn lock_writing(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.writing.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// Corps d'une mutation, verrou déjà tenu. `persist` : écrire le fichier (faux pour
    /// une relecture, dont le fichier est la source).
    fn apply<F>(&self, source: &str, persist: bool, f: F) -> Result<Arc<Generation>>
    where
        F: FnOnce(&mut Config) -> Result<Vec<String>>,
    {
        let cur = self.current.load_full();
        let mut next = (*cur.config).clone();
        let changed = f(&mut next)?;
        next.validate()?;

        if persist {
            // Seules les clés modifiées sont réécrites (#76) : le fichier garde ses
            // commentaires, son ordre, ses clés inconnues, et n'acquiert pas les clés
            // nouvelles de cette version. Sans fichier, on part des valeurs par défaut.
            let text = match std::fs::read_to_string(&self.path) {
                Ok(existing) => edit_toml(&existing, &cur.config, &next)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    format!(
                        "{SAMPLE_HEADER}{}",
                        edit_toml("", &Config::default(), &next)?
                    )
                }
                Err(e) => return Err(e.into()),
            };
            atomic_write(&self.path, text.as_bytes())?;
        }

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
    ///
    /// La lecture est sous le même verrou que les mutations : un `config set` qui écrit
    /// entre la lecture et la publication ne se fait pas écraser par une relecture
    /// périmée (issue #45).
    pub fn reload_from_disk(&self) -> Result<Arc<Generation>> {
        let _writing = self.lock_writing();
        let raw = std::fs::read_to_string(&self.path)?;
        let (parsed, unknown) = Config::parse(&raw)?;
        let g = self.apply("file", false, move |c| {
            let changed = diff_paths(c, &parsed);
            *c = parsed;
            Ok(changed)
        })?;
        self.set_unknown(unknown);
        Ok(g)
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
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    // Nom unique : deux écritures concurrentes ne se renomment pas le fichier l'une de
    // l'autre (« No such file or directory », issue #45).
    let tmp = dir.join(format!(
        ".{}.tmp{}-{}",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "f".into()),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
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

    /// #43 : un battement plus lent que la moitié du bail fait expirer les tours en cours.
    #[test]
    fn a_heartbeat_slower_than_half_the_lease_is_rejected() {
        let mut c = cfg();
        c.runners.heartbeat = "2m".into();
        c.runners.lease_ttl = "60s".into();
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains("runners.heartbeat"), "{e}");
        c.runners.heartbeat = "30s".into();
        c.validate().unwrap();
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

    /// #76 : le fichier écrit par une version plus récente se relit. Section et clé
    /// inconnues sont ignorées et nommées, pas fatales.
    #[test]
    fn unknown_sections_and_keys_are_tolerated_and_named() {
        let raw = "[owner]\ntelegram_user_id = 1\n\n[budget]\ndaily_usd = 9.0\nnouveau_plafond = 3\n\n[futur]\nactif = true\n\n[models.aliases]\nmain = \"x/y\"\n";
        let (c, unknown) = Config::parse(raw).unwrap();
        assert_eq!(c.owner.telegram_user_id, 1);
        assert_eq!(c.budget.daily_usd, 9.0);
        assert_eq!(
            c.models.aliases.get("main").map(String::as_str),
            Some("x/y")
        );
        assert_eq!(unknown, vec!["budget.nouveau_plafond", "futur"]);
        // Une valeur mal typée reste une erreur.
        assert!(Config::parse("[budget]\ndaily_usd = \"beaucoup\"\n").is_err());
    }

    /// #76 : une mutation ne touche que la clé visée ; commentaires, ordre, clés
    /// inconnues et omissions restent, aucune clé nouvelle n'apparaît.
    #[test]
    fn a_mutation_edits_only_the_changed_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "# réglé à la main\n[owner]\ntelegram_user_id = 5 # moi\n\n[futur]\nactif = true\n\n[budget]\n# plafond du jour\ndaily_usd = 20.0\n";
        std::fs::write(&path, original).unwrap();
        let cs =
            ConfigStore::load_or_create(&path, None, std::sync::Arc::new(TestClock::default()), 0)
                .unwrap();
        assert_eq!(cs.unknown_keys(), vec!["futur"]);

        cs.mutate("cli", |c| {
            c.budget.daily_usd = 30.0;
            Ok(vec!["budget.daily_usd".into()])
        })
        .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            original.replace("daily_usd = 20.0", "daily_usd = 30.0"),
            "seule la valeur change"
        );

        // Une clé absente du fichier est ajoutée dans sa section, rien d'autre.
        cs.mutate("cli", |c| {
            c.telegram.quiet_hours = "23:00-06:00".into();
            Ok(vec!["telegram.quiet_hours".into()])
        })
        .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with(&original.replace("daily_usd = 20.0", "daily_usd = 30.0")),
            "{after}"
        );
        assert!(
            after.ends_with("[telegram]\nquiet_hours = \"23:00-06:00\"\n"),
            "{after}"
        );
        let (relu, unknown) = Config::parse(&after).unwrap();
        assert_eq!(relu.telegram.quiet_hours, "23:00-06:00");
        assert_eq!(relu.budget.daily_usd, 30.0);
        assert_eq!(unknown, vec!["futur"]);
    }

    /// #76 : une entrée retirée d'une table libre disparaît du fichier.
    #[test]
    fn a_removed_map_entry_leaves_the_file() {
        let before = cfg();
        let mut after = before.clone();
        after.models.aliases.insert("essai".into(), "a/b".into());
        let text = edit_toml("", &before, &after).unwrap();
        assert_eq!(text, "[models.aliases]\nessai = \"a/b\"\n");
        let text2 = edit_toml(&text, &after, &before).unwrap();
        assert!(!text2.contains("essai"), "{text2}");
    }

    /// #76 : le fichier de premier démarrage ne porte que ce qui s'écarte des défauts.
    #[test]
    fn the_first_file_is_short_and_reads_back() {
        let text = Config::sample_toml(42).unwrap();
        assert!(text.starts_with("# Configuration de Pénélope."), "{text}");
        assert!(text.ends_with("[owner]\ntelegram_user_id = 42\n"), "{text}");
        let (c, unknown) = Config::parse(&text).unwrap();
        assert!(unknown.is_empty());
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::to_value(Config::sample(42)).unwrap()
        );
    }

    /// #76 : les fichiers complets écrits par les versions publiées se relisent sans
    /// clé inconnue. Une clé retirée plus tard sera signalée, jamais fatale.
    #[test]
    fn files_written_by_released_versions_still_load() {
        // Une entrée par version publiée dont le schéma a changé.
        const RELEASED: &[(&str, &str)] = &[(
            "0.17.0",
            include_str!("../tests/fixtures/config-0.17.0.toml"),
        )];
        for (version, raw) in RELEASED {
            let (c, unknown) = Config::parse(raw).unwrap_or_else(|e| panic!("{version} : {e}"));
            assert!(unknown.is_empty(), "{version} : {unknown:?}");
            c.validate().unwrap_or_else(|e| panic!("{version} : {e}"));
        }
    }

    /// #106 : une instance en service garde le réseau qu'elle a écrit ; un fichier sans
    /// la clé prend le nouveau défaut, fermé.
    #[test]
    fn an_explicit_shell_network_survives_the_new_default() {
        let (old, _) = Config::parse(include_str!("../tests/fixtures/config-0.17.0.toml")).unwrap();
        assert!(old.sandbox.shell_network, "valeur écrite par 0.17.0");
        let (fresh, _) = Config::parse("[owner]\nname = \"Anne\"\n").unwrap();
        assert!(!fresh.sandbox.shell_network, "défaut fermé");
        assert!(!Config::default().sandbox.shell_network);
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

    /// #45 : 800 mutations lancées par 4 threads. Aucune n'est refusée, aucune n'est
    /// perdue, et le fichier sur disque porte la dernière génération.
    #[test]
    fn concurrent_mutations_never_lose_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cs = std::sync::Arc::new(ConfigStore::new(
            cfg(),
            &path,
            None,
            std::sync::Arc::new(TestClock::default()),
        ));
        let depart = cs.config().budget.daily_usd;

        std::thread::scope(|s| {
            for _ in 0..4 {
                let cs = cs.clone();
                s.spawn(move || {
                    for _ in 0..200 {
                        cs.mutate("essai", |c| {
                            c.budget.daily_usd += 1.0;
                            Ok(vec!["budget.daily_usd".into()])
                        })
                        .expect("aucune mutation refusée");
                    }
                });
            }
        });

        assert_eq!(cs.config().budget.daily_usd, depart + 800.0);
        assert_eq!(cs.generation(), 801);
        let on_disk = Config::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk.budget.daily_usd,
            cs.config().budget.daily_usd,
            "le fichier reflète la dernière génération"
        );
    }

    /// #45 : une relecture du fichier pendant une mutation. Les deux aboutissent, la
    /// génération publiée est la dernière écrite, et le fichier la reflète.
    #[test]
    fn a_reload_racing_a_mutation_leaves_a_consistent_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cs = std::sync::Arc::new(ConfigStore::new(
            cfg(),
            &path,
            None,
            std::sync::Arc::new(TestClock::default()),
        ));
        // Le fichier porte une modification faite à la main, hors du daemon.
        let mut edite = (*cs.config()).clone();
        edite.runners.count = 7;
        std::fs::write(&path, edite.to_toml().unwrap()).unwrap();

        std::thread::scope(|s| {
            let a = cs.clone();
            s.spawn(move || a.reload_from_disk().expect("relecture"));
            let b = cs.clone();
            s.spawn(move || {
                b.mutate("cli", |c| {
                    c.budget.daily_usd = 123.0;
                    Ok(vec!["budget.daily_usd".into()])
                })
                .expect("mutation")
            });
        });

        let fin = cs.config();
        assert_eq!(cs.generation(), 3, "les deux générations sont publiées");
        assert_eq!(fin.budget.daily_usd, 123.0, "la mutation survit");
        let on_disk = Config::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&*fin).unwrap(),
            serde_json::to_value(&on_disk).unwrap(),
            "le fichier porte la dernière génération"
        );

        // La relecture voit toujours ce que le fichier contient à son tour de verrou : une
        // édition manuelle relue après la mutation entre elle aussi.
        let mut edite = (*cs.config()).clone();
        edite.runners.count = 9;
        std::fs::write(&path, edite.to_toml().unwrap()).unwrap();
        cs.reload_from_disk().unwrap();
        assert_eq!(cs.config().runners.count, 9);
        assert_eq!(cs.config().budget.daily_usd, 123.0);
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
