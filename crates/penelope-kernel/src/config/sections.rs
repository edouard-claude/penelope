//! Les autres sections de la configuration de référence (§19).

use super::*;

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
    /// Raisonnement du modèle de consolidation : `auto` le garde et le budgète (le tri
    /// d'un candidat gagne à être réfléchi), `off` l'éteint pour rendre tout le budget
    /// de sortie au JSON. Le défaut est `auto` (issue #152).
    pub consolidation_reasoning: String,
    /// Plafond du budget de raisonnement d'un appel de consolidation, en jetons. Le
    /// budget part plus bas et monte quand le modèle s'y heurte ; `max_tokens` de
    /// l'appel vaut ce budget plus la sortie estimée du lot.
    pub consolidation_reasoning_tokens: u32,
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
            consolidation_reasoning: "auto".into(),
            consolidation_reasoning_tokens: 16_000,
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
    /// Hôte local du retour OAuth : `127.0.0.1` ou `localhost`. Slack n'enregistre que
    /// `localhost` dans les URL de rappel d'une app (cf. #159).
    pub callback_host: String,
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
            callback_host: "127.0.0.1".into(),
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
    /// Écoute WebSocket locale, active seulement si un consommateur est déclaré.
    pub runtime_stream_bind: String,
    /// Chaque consommateur possède son propre secret et son filtre d'événements.
    pub runtime_consumers: Vec<RuntimeConsumer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RuntimeConsumer {
    pub name: String,
    /// Nom dans le magasin de secrets, jamais la valeur du jeton.
    pub token_secret: String,
    /// Types exacts d'événements ; vide : tous les types.
    pub kinds: Vec<String>,
}

impl Default for Observability {
    fn default() -> Self {
        Observability {
            otlp_endpoint: String::new(),
            prometheus: "127.0.0.1:9464".into(),
            log_retention_days: 14,
            log_level: "info".into(),
            runtime_stream_bind: "127.0.0.1:9465".into(),
            runtime_consumers: Vec::new(),
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
    /// Binaires à ajouter à l'inventaire de la machine (issue #156), en plus de la liste
    /// connue : ce que le modèle apprend qu'il peut lancer au lieu de bricoler.
    pub inventory_extra: Vec<String>,
    /// Délai au-delà duquel un appel d'outil se voit **proposer** l'arrière-plan plutôt
    /// que d'immobiliser le tour (issue #204). La proposition passe par le texte du
    /// résultat : c'est le modèle qui décide, rien n'est détourné d'office.
    pub background_after: String,
    /// Jobs d'outils simultanés au plus pour une session (issue #204).
    pub jobs_per_session: usize,
    /// Jobs d'outils simultanés au plus pour tout le daemon (issue #204).
    pub jobs_total: usize,
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
            inventory_extra: Vec::new(),
            background_after: "120s".into(),
            jobs_per_session: 3,
            jobs_total: 10,
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

/// Lecture de la conversation pendant la bascule vers le journal d'événements (épopée
/// #208, `design/v1/source-de-verite.md` §4.3). La section disparaît avec le chemin direct.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    /// D'où chaque requête relit la conversation : `journal` (pliage du journal
    /// d'événements) ou `tables` (lignes `messages`, lecture d'avant la V1). La variable
    /// d'environnement `PENELOPE_HISTORY_SOURCE` l'emporte (rejouer une suite sous l'autre).
    pub source: HistorySource,
}

/// Source de lecture de la conversation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    Tables,
    #[default]
    Journal,
}

impl History {
    /// La source en vigueur : la variable d'environnement, sinon le fichier.
    pub fn effective_source(&self) -> HistorySource {
        match std::env::var("PENELOPE_HISTORY_SOURCE").as_deref() {
            Ok("journal") => HistorySource::Journal,
            Ok("tables") => HistorySource::Tables,
            _ => self.source,
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
