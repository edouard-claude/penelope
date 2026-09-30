//! Scénarios de session rejouables sans clé (règle R11 de `design/v1/gel-et-outillage.md`,
//! tâche T0 de `design/v1/source-de-verite.md`).
//!
//! Un scénario est un répertoire `crates/penelope-evals/scenarios/<nom>/` :
//!
//! ```text
//! scenario.toml   entrées ordonnées (messages du propriétaire, commandes, horloge,
//!                 redémarrage, crash, purge), configuration patchée, fichiers semés,
//!                 serveur MCP simulé
//! model.jsonl     une ligne `ScriptLine` par appel de modèle, dans l'ordre
//! expected.jsonl  le monde après le run, normalisé : issues des étapes, sessions,
//!                 messages, résumés, événements, tours, effets, requêtes LLM,
//!                 approbations, artefacts, usage, `tg_outbox`, fichiers, audit
//! surface.jsonl   chaque requête reçue par le mock, rédigée et normalisée
//! ```
//!
//! Trois modes : rejeu (défaut, dans `cargo test --workspace`), `UPDATE_SCENARIOS=1`
//! (régénère `expected.jsonl` et `surface.jsonl`), `RECORD_SCENARIO=<nom>` (enveloppe
//! le vrai fournisseur, réécrit `model.jsonl` puis les deux attendus).
//!
//! Les assertions portent sur le monde et sur ce que le modèle a vu, jamais sur la
//! prose : le texte des réponses vient du script, et les jetons de normalisation
//! (`{{session:1}}`, `{{turn:1}}`, `{{ts+600s}}`, `{{home}}`, `{{hash}}`) rendent deux
//! rejeux identiques octet pour octet.

pub mod harness;
pub mod http;
pub mod normalise;
pub mod world;

use penelope_llm::mock::Scripted;
use penelope_llm::types::{LlmErrorKind, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `scenario.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Horloge de départ (RFC 3339) ; défaut : `TestClock::default()`, 2026-01-01T00:00Z.
    #[serde(default)]
    pub clock: Option<String>,
    /// Alias épinglé sur la session de départ : sans classifieur, le script est plus
    /// court et le tour va droit au modèle de conversation.
    #[serde(default)]
    pub pin_model: Option<String>,
    /// Patchs de configuration, `"chemin.pointé" = valeur`, appliqués à chaque
    /// démarrage (la configuration de test n'est pas relue du disque).
    #[serde(default)]
    pub config: toml::Table,
    /// Fichiers semés dans le workspace par défaut avant la première étape.
    #[serde(default)]
    pub files: Vec<SeedFile>,
    /// Dépôts git semés dans le workspace après les fichiers (#192).
    #[serde(default)]
    pub repos: Vec<SeedRepo>,
    /// Outils d'un serveur MCP simulé, exposés au modèle et inscrits au registre.
    #[serde(default)]
    pub mcp_tools: Vec<McpTool>,
    /// Motifs (expressions régulières) des textes propres à la machine, remplacés par
    /// `{{masked}}` dans les attendus : mémoire du processus, contrôles de l'hôte.
    #[serde(default)]
    pub masks: Vec<String>,
    /// Serveurs MCP simulés derrière le vrai superviseur (connecteur de test, aucun
    /// processus lancé) : ce que les méthodes `mcp.*` administrent. Servis, pas déclarés ;
    /// `mcp.add` les déclare.
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    /// Requêtes SQL en lecture relevées dans le monde après le run (`observed`) : l'effet
    /// d'une méthode RPC sur une table que le relevé ordinaire ne lit pas.
    #[serde(default)]
    pub observe: Vec<Observe>,
    /// Canal de messages simulé : ce qui est envoyé au propriétaire est relevé dans le
    /// monde (lignes `sent`). Sans lui, `send_message` n'a aucun canal.
    #[serde(default)]
    pub messenger: bool,
    /// Routes du faux serveur HTTP local (`127.0.0.1`, port tiré au sort) : `{{http}}`
    /// est sa base dans la configuration, les paramètres RPC et les corps servis ; les
    /// requêtes reçues sont relevées (`http_request`). Quand `providers.local.base_url`
    /// le vise, la synthèse vocale y part pour de vrai (`/audio/speech`, #278).
    #[serde(default)]
    pub http: Vec<http::Route>,
    /// Valeurs semées dans `kv` avant la première étape : un état que le daemon dresse
    /// hors des tours, comme la carte de la machine (`machine.environment`, #260).
    #[serde(default)]
    pub kv: std::collections::BTreeMap<String, String>,
    /// Modèles que le catalogue dit lire les images (`openrouter:…` ou nu) : une photo
    /// leur est montrée telle quelle, sans passer par le modèle de vision.
    #[serde(default)]
    pub vision_models: Vec<String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub name: String,
    /// Noms des outils que le serveur annonce.
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observe {
    /// Nom du relevé dans `expected.jsonl`.
    pub name: String,
    /// Une requête `SELECT` ordonnée : chaque ligne devient un objet colonne → valeur.
    pub sql: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedFile {
    pub path: String,
    #[serde(default)]
    pub content: String,
    /// `content` répété : un gros fichier sans le stocker dans le dépôt.
    #[serde(default = "one")]
    pub repeat: usize,
    /// Où semer : le workspace par défaut, `vault` (le vault de mémoire,
    /// `memory.vault_path`) ou `skills` (le répertoire des skills).
    #[serde(default)]
    pub root: SeedRoot,
}

/// Un dépôt git dans le workspace : `base` porte un commit vide, `branch` en part avec
/// tout ce que `[[files]]` a semé sous `path` (sauf `.penelope/`, ignoré : la
/// configuration y porte l'adresse du faux serveur, qui change à chaque rejeu). Son
/// remote `origin` est un dépôt nu voisin, `<path>.origin.git`, où `base` est poussée.
/// Auteur et dates fixes : les mêmes commits à chaque rejeu.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedRepo {
    pub path: String,
    pub base: String,
    pub branch: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SeedRoot {
    #[default]
    Workspace,
    Vault,
    Skills,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTool {
    pub server: String,
    pub name: String,
    pub description: String,
    /// `readOnlyHint` : l'outil est de classe `read`, donc idempotent et exécuté sans
    /// carte.
    #[serde(default)]
    pub read_only: bool,
    /// Paramètres (chaînes) déclarés dans le schéma d'entrée.
    #[serde(default)]
    pub params: Vec<String>,
    /// Texte rendu par le serveur simulé.
    pub result: String,
}

/// Une entrée du scénario, dans l'ordre.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Message du propriétaire : mis en file, réclamé, joué jusqu'à son issue.
    Message {
        text: String,
        /// Le processus meurt pendant l'appel d'outil MCP simulé ; l'étape suivante doit
        /// être `restart`.
        #[serde(default)]
        crash: Option<Crash>,
        /// Message du propriétaire qui arrive pendant l'appel d'outil MCP simulé : la
        /// boucle le réclame entre deux appels (steering, épopée #208, T13).
        #[serde(default)]
        steer: Option<String>,
        /// Photos jointes, chemins relatifs au workspace (fichiers semés par `[[files]]`),
        /// comme la passerelle les enregistre.
        #[serde(default)]
        photos: Vec<String>,
    },
    /// Message mis en file sans être joué : le suivant l'absorbe (messages fusionnés).
    Enqueue { text: String },
    /// `/compact`, `/fork [titre]`, `/rewind [n]`, `/purge`.
    Command { command: String },
    /// Mise à jour Telegram du propriétaire, par la vraie passerelle sur un transport
    /// simulé : `text` (commande `/…` ou message), ou `click` (le bouton dont le
    /// libellé contient ce texte, sur le dernier écran qui en porte un).
    Telegram {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        click: Option<String>,
    },
    /// Avance de l'horloge de test (`10m`, `3h`, `2d`).
    AdvanceClock { by: String },
    /// Le pilote des workflows passe (#191) : chaque run `running` avance jusqu'à
    /// attendre, s'arrêter ou finir, jusqu'à ce qu'un passage ne change plus rien. Ses
    /// cartes (progression, OK de phase) partent par la vraie passerelle Telegram, comme
    /// dans une étape `telegram` ; l'issue dit où en est chaque run.
    Drive,
    /// Services détruits puis reconstruits sur le même répertoire, reprise au démarrage,
    /// tours en attente joués.
    Restart,
    /// Approuve la première demande en attente de la session et joue la reprise.
    Approve,
    /// Fait comme si le dernier appel facturé avait pesé `prompt` tokens : la session
    /// est alors froide au sens de l'issue #40 après une pause.
    Usage { prompt: u64 },
    /// Appel d'une méthode RPC, comme la socket locale le servirait (`Rpc::handle`, ou
    /// `handle_streaming` pour `chat.stream` et `tail`). Les tours que l'appel met en file
    /// sont joués pendant qu'il attend, comme le ferait le pool de runners. La réponse,
    /// normalisée, entre dans l'issue de l'étape.
    Rpc {
        method: String,
        /// Paramètres ; une chaîne `$session` est la session du scénario, `$nom.chemin`
        /// une valeur d'une réponse liée par `bind` (`$sch.id`, `$liste.0.uid`),
        /// `$json:nom.chemin` la même valeur sérialisée en texte JSON.
        #[serde(default)]
        params: toml::Table,
        /// Garde la réponse sous ce nom pour les étapes suivantes.
        #[serde(default)]
        bind: Option<String>,
        /// Ne garde de la réponse que ces pointeurs JSON (`/id`, `/servers/*/name`) : ce
        /// qui compte, sans ce qui dépend de la machine.
        #[serde(default)]
        pick: Vec<String>,
        /// Pointeurs dont la valeur est masquée (`{{masked}}`), appliqués avant `pick`.
        #[serde(default)]
        mask: Vec<String>,
        /// Motifs (regex) : un texte de la réponse ne garde que ses lignes qui en
        /// satisfont un (`metrics` : les jauges calculées à la demande).
        #[serde(default)]
        lines: Vec<String>,
        /// Éléments d'une réponse en tableau retirés avant tout le reste, par champ :
        /// `{ id = ["service", "dep.*"] }` (`*` final : préfixe). Ce qui dépend de l'hôte
        /// et dont le nombre même change d'une machine à l'autre (`doctor`).
        #[serde(default)]
        without: toml::Table,
        /// L'inverse de `without`, appliqué après lui : ne garde que les éléments dont un
        /// champ répond à un motif, dans l'ordre de la réponse. Une liste explicite de ce
        /// que le scénario vérifie, pour que ce qu'ajoute un hôte ne l'atteigne jamais.
        #[serde(default)]
        only: toml::Table,
        /// L'appel doit échouer ; sans ce drapeau, une erreur fait échouer le scénario.
        #[serde(default)]
        error: bool,
        /// `tail` seulement : message joué pendant que le flux est ouvert.
        #[serde(default)]
        during: Option<String>,
    },
    /// Le navigateur du propriétaire : `GET` d'une adresse du faux serveur HTTP, sans
    /// suivre de redirection (statut, `location`, corps). `url` se résout comme un
    /// paramètre RPC (`$auth.url`).
    Open {
        url: String,
        #[serde(default)]
        bind: Option<String>,
    },
    /// Écrit un fichier entre deux étapes, comme le propriétaire le ferait à la main
    /// (AGENTS.md modifié en cours de session, #236) ; mêmes racines que `[[files]]`.
    File {
        path: String,
        content: String,
        #[serde(default)]
        root: SeedRoot,
    },
    /// Sème `exchanges` échanges dans l'historique, sans appel au modèle. `{i}` et
    /// `{filler}` sont remplacés ; `tokens` est la taille déclarée de chaque message.
    Seed {
        exchanges: usize,
        user: String,
        assistant: String,
        #[serde(default)]
        filler: String,
        #[serde(default = "one")]
        repeat: usize,
        #[serde(default = "seed_tokens")]
        tokens: u64,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Crash {
    DuringTool,
}

fn one() -> usize {
    1
}

fn seed_tokens() -> u64 {
    600
}

impl Step {
    /// Libellé de l'étape dans `expected.jsonl`.
    pub fn label(&self) -> String {
        match self {
            Step::Message {
                text,
                crash: None,
                steer: None,
                photos,
            } if !photos.is_empty() => format!("message : {text} (photos : {})", photos.join(", ")),
            Step::Message {
                text,
                crash: None,
                steer: None,
                ..
            } => format!("message : {text}"),
            Step::Message {
                text,
                crash: Some(_),
                ..
            } => {
                format!("message : {text} (crash pendant l'appel d'outil)")
            }
            Step::Message {
                text,
                steer: Some(steer),
                ..
            } => format!("message : {text} (pendant l'appel d'outil : {steer})"),
            Step::Enqueue { text } => format!("en file : {text}"),
            Step::Command { command } => format!("commande : {command}"),
            Step::Telegram { text: Some(t), .. } => format!("telegram : {t}"),
            Step::Telegram { click, .. } => {
                format!(
                    "telegram : clic « {} »",
                    click.as_deref().unwrap_or_default()
                )
            }
            Step::AdvanceClock { by } => format!("horloge : +{by}"),
            Step::Drive => "pilote des workflows".into(),
            Step::Restart => "redémarrage".into(),
            Step::Approve => "approbation".into(),
            Step::Usage { prompt } => format!("dernier appel facturé : {prompt} tokens"),
            Step::Seed { exchanges, .. } => format!("historique semé : {exchanges} échanges"),
            Step::File { path, .. } => format!("fichier écrit : {path}"),
            Step::Rpc { method, .. } => format!("rpc : {method}"),
            Step::Open { url, .. } => format!("navigateur : {url}"),
        }
    }
}

/// Une ligne de `model.jsonl` : la réponse scriptée d'un appel de modèle. Même
/// vocabulaire que [`Scripted`], sérialisable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptLine {
    Text(String),
    ToolCalls {
        #[serde(default)]
        text: String,
        calls: Vec<ToolCall>,
    },
    /// `kind` : `transient`, `rate_limited`, `context_length`, `auth`, `bad_request`,
    /// `unknown_model`, `payment_required`, `content_filter`, `attachment_rejected`,
    /// `cancelled`, `other`.
    Error {
        kind: String,
        message: String,
    },
    ContextOverflow,
    Images {
        #[serde(default)]
        text: String,
        urls: Vec<String>,
    },
    MidStreamError {
        #[serde(default)]
        text: String,
        message: String,
    },
    Written {
        text: String,
        completion: u64,
        cut: bool,
    },
    ReasonedOnly {
        completion: u64,
        reasoning: u64,
    },
    Panic(String),
}

impl ScriptLine {
    pub fn to_scripted(&self) -> Scripted {
        match self.clone() {
            ScriptLine::Text(t) => Scripted::Text(t),
            ScriptLine::ToolCalls { text, calls } => Scripted::ToolCalls(text, calls),
            ScriptLine::Error { kind, message } => Scripted::Error(error_kind(&kind), message),
            ScriptLine::ContextOverflow => Scripted::ContextOverflow,
            ScriptLine::Images { text, urls } => Scripted::Images(text, urls),
            ScriptLine::MidStreamError { text, message } => Scripted::MidStreamError(text, message),
            ScriptLine::Written {
                text,
                completion,
                cut,
            } => Scripted::Written {
                text,
                completion,
                cut,
            },
            ScriptLine::ReasonedOnly {
                completion,
                reasoning,
            } => Scripted::ReasonedOnly {
                completion,
                reasoning,
            },
            ScriptLine::Panic(m) => Scripted::Panic(m),
        }
    }
}

/// Une ligne de `model.jsonl` et le rôle qu'elle sert : sans `"role"`, le modèle de
/// conversation, dans l'ordre du fichier ; `"role": "trace"` (#273), le modèle du rôle
/// `trace` de la trace des outils, dont les appels se glissent entre ceux du tour sans
/// leur voler une ligne. Chaque rôle a sa file.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptEntry {
    pub role: Option<String>,
    pub line: ScriptLine,
}

impl Serialize for ScriptEntry {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut v = serde_json::to_value(&self.line).map_err(serde::ser::Error::custom)?;
        if let (Some(role), Some(map)) = (&self.role, v.as_object_mut()) {
            map.insert("role".into(), Value::String(role.clone()));
        }
        v.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ScriptEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let mut v = Value::deserialize(d)?;
        let role = v
            .as_object_mut()
            .and_then(|m| m.remove("role"))
            .map(|r| match r {
                Value::String(r) => Ok(r),
                other => Err(serde::de::Error::custom(format!(
                    "`role` attend un nom de rôle, reçu {other}"
                ))),
            })
            .transpose()?;
        let line = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
        Ok(ScriptEntry { role, line })
    }
}

fn error_kind(name: &str) -> LlmErrorKind {
    match name {
        "transient" => LlmErrorKind::Transient,
        "rate_limited" => LlmErrorKind::RateLimited,
        "context_length" => LlmErrorKind::ContextLength,
        "auth" => LlmErrorKind::Auth,
        "bad_request" => LlmErrorKind::BadRequest,
        "unknown_model" => LlmErrorKind::UnknownModel,
        "payment_required" => LlmErrorKind::PaymentRequired,
        "content_filter" => LlmErrorKind::ContentFilter,
        "attachment_rejected" | "attachmentrejected" => LlmErrorKind::AttachmentRejected,
        "cancelled" => LlmErrorKind::Cancelled,
        _ => LlmErrorKind::Other,
    }
}

/// Un scénario chargé.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub dir: PathBuf,
    pub spec: Spec,
    pub script: Vec<ScriptEntry>,
}

pub const SPEC_FILE: &str = "scenario.toml";
pub const MODEL_FILE: &str = "model.jsonl";
pub const EXPECTED_FILE: &str = "expected.jsonl";
pub const SURFACE_FILE: &str = "surface.jsonl";

/// Mode de la suite, lu dans l'environnement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Rejeu sans clé : compare et échoue avec un diff lisible.
    Replay,
    /// `UPDATE_SCENARIOS=1` : régénère `expected.jsonl` et `surface.jsonl`.
    Update,
    /// `RECORD_SCENARIO=<nom>` (ou `all`) : enveloppe le vrai fournisseur, réécrit
    /// `model.jsonl`, puis les deux attendus.
    Record,
}

pub fn mode_for(name: &str) -> Mode {
    let wanted = std::env::var("RECORD_SCENARIO").unwrap_or_default();
    if !wanted.trim().is_empty() && (wanted == name || wanted == "all") {
        return Mode::Record;
    }
    if std::env::var("UPDATE_SCENARIOS").is_ok_and(|v| v == "1") {
        return Mode::Update;
    }
    Mode::Replay
}

/// Charge `scenario.toml` et `model.jsonl` d'un répertoire.
pub fn load(dir: &Path) -> anyhow::Result<Scenario> {
    let spec_path = dir.join(SPEC_FILE);
    let text = std::fs::read_to_string(&spec_path)
        .map_err(|e| anyhow::anyhow!("{} : {e}", spec_path.display()))?;
    let spec: Spec =
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("{} : {e}", spec_path.display()))?;
    let expected_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    anyhow::ensure!(
        spec.name == expected_name,
        "{} : `name = \"{}\"` doit être le nom du répertoire (`{expected_name}`)",
        spec_path.display(),
        spec.name
    );
    anyhow::ensure!(
        !spec.steps.is_empty(),
        "{} : aucune étape",
        spec_path.display()
    );
    let script = load_script(&dir.join(MODEL_FILE))?;
    Ok(Scenario {
        dir: dir.to_path_buf(),
        spec,
        script,
    })
}

fn load_script(path: &Path) -> anyhow::Result<Vec<ScriptEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{} : {e}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|(i, l)| {
            serde_json::from_str::<ScriptEntry>(l)
                .map_err(|e| anyhow::anyhow!("{} ligne {} : {e}", path.display(), i + 1))
        })
        .collect()
}

/// Répertoires de scénarios sous `root`, triés par nom.
pub fn list(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.join(SPEC_FILE).is_file())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

pub fn read_jsonl(path: &Path) -> anyhow::Result<Vec<Value>> {
    let text =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{} : {e}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l)
                .map_err(|e| anyhow::anyhow!("{} ligne {} : {e}", path.display(), i + 1))
        })
        .collect()
}

pub fn write_jsonl(path: &Path, lines: &[Value]) -> anyhow::Result<()> {
    let mut out = String::new();
    for l in lines {
        out.push_str(&serde_json::to_string(l)?);
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| anyhow::anyhow!("{} : {e}", path.display()))
}

/// Joue un scénario dans le mode courant : compare (rejeu) ou réécrit (mise à jour,
/// enregistrement). L'erreur nomme le scénario, le fichier et la première divergence.
pub async fn check(dir: &Path) -> Result<(), String> {
    let scenario = load(dir).map_err(|e| format!("scénario {} : {e:#}", dir.display()))?;
    let name = scenario.spec.name.clone();
    let mode = mode_for(&name);
    let run = harness::run(&scenario, mode)
        .await
        .map_err(|e| format!("scénario {name} : {e:#}"))?;
    let write = |file: &str, lines: &[Value]| {
        write_jsonl(&dir.join(file), lines).map_err(|e| format!("scénario {name} : {e:#}"))
    };
    match mode {
        Mode::Record => {
            let recorded: Vec<Value> = run
                .recorded
                .unwrap_or_default()
                .iter()
                .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
                .collect();
            write(MODEL_FILE, &recorded)?;
            write(EXPECTED_FILE, &run.expected)?;
            write(SURFACE_FILE, &run.surface)
        }
        Mode::Update => {
            write(EXPECTED_FILE, &run.expected)?;
            write(SURFACE_FILE, &run.surface)
        }
        Mode::Replay => {
            let read = |file: &str| -> Result<Vec<Value>, String> {
                let path = dir.join(file);
                if !path.is_file() {
                    return Err(format!(
                        "scénario {name} : {file} absent ; `UPDATE_SCENARIOS=1 cargo test -p \
                         penelope-evals --test scenarios` l'écrit, puis relire le diff"
                    ));
                }
                read_jsonl(&path).map_err(|e| format!("scénario {name} : {e:#}"))
            };
            let expected = read(EXPECTED_FILE)?;
            let surface = read(SURFACE_FILE)?;
            compare_world(&name, &expected, &run.expected)?;
            compare_surface(&name, &surface, &run.surface)
        }
    }
}

/// Rejoue un scénario sans lire ni écrire ses attendus : le monde et la surface, normalisés.
pub async fn replay(dir: &Path) -> anyhow::Result<(Vec<Value>, Vec<Value>)> {
    let scenario = load(dir)?;
    let run = harness::run(&scenario, Mode::Replay).await?;
    Ok((run.expected, run.surface))
}

/// Rejoue un scénario sans lire ni écrire ses attendus et rend ce que les contrôles du
/// journal ont vu (épopée #208, T22) ; un contrôle en échec est une erreur.
pub async fn audit(dir: &Path) -> anyhow::Result<harness::Audit> {
    let scenario = load(dir)?;
    Ok(harness::run(&scenario, Mode::Replay).await?.audit)
}

/// Libellé français d'un groupe de lignes de `expected.jsonl`.
fn group_label(kind: &str) -> &str {
    match kind {
        "outcome" => "issues",
        "session" => "sessions",
        "message" => "messages",
        "summary" => "résumés",
        "event" => "événements",
        "turn" => "tours",
        "effect" => "effects",
        "llm" => "requêtes LLM",
        "approval" => "approbations",
        "artifact" => "artefacts",
        "usage" => "usage",
        "outbox" => "tg_outbox",
        "file" => "fichiers",
        "sent" => "envois",
        "audit" => "audit",
        other => other,
    }
}

/// JSON compact, borné, pour un message d'erreur.
fn short(v: &Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.chars().count() <= 400 {
        return s;
    }
    let cut: String = s.chars().take(400).collect();
    format!("{cut}…")
}

/// Compare le monde attendu et obtenu, groupe par groupe (`type`), et nomme la
/// première divergence.
/// Le premier chemin JSON où deux valeurs diffèrent, et les deux valeurs à cet endroit.
fn first_difference(x: &Value, y: &Value, path: String) -> Option<(String, String, String)> {
    if x == y {
        return None;
    }
    match (x, y) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let (va, vb) = (
                    a.get(k).unwrap_or(&Value::Null),
                    b.get(k).unwrap_or(&Value::Null),
                );
                if let Some(d) = first_difference(va, vb, format!("{path}.{k}")) {
                    return Some(d);
                }
            }
            None
        }
        (Value::Array(a), Value::Array(b)) => {
            for n in 0..a.len().max(b.len()) {
                let (va, vb) = (
                    a.get(n).unwrap_or(&Value::Null),
                    b.get(n).unwrap_or(&Value::Null),
                );
                if let Some(d) = first_difference(va, vb, format!("{path}[{n}]")) {
                    return Some(d);
                }
            }
            None
        }
        // Deux textes longs (un prompt système rendu) : `short` ne montrerait que leur
        // début commun. On cadre le premier caractère qui diffère, des deux côtés.
        (Value::String(a), Value::String(b)) => Some((path, text_window(a, b), text_window(b, a))),
        _ => Some((path, short(x), short(y))),
    }
}

/// `text` cadré autour du premier caractère où il diverge de `other` : la position, 60
/// caractères avant et 200 après, pour lire l'écart sans rejouer.
fn text_window(text: &str, other: &str) -> String {
    let a: Vec<char> = text.chars().collect();
    let b: Vec<char> = other.chars().collect();
    let at = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    if a.len() <= 400 && at < 200 {
        return serde_json::to_string(text).unwrap_or_default();
    }
    let start = at.saturating_sub(60);
    let end = (at + 200).min(a.len());
    let window: String = a[start..end].iter().collect();
    format!(
        "[car. {at}] …{}…",
        serde_json::to_string(&window).unwrap_or_default()
    )
}

pub fn compare_world(name: &str, expected: &[Value], actual: &[Value]) -> Result<(), String> {
    let kind = |v: &Value| v["type"].as_str().unwrap_or("?").to_string();
    let mut kinds: Vec<String> = Vec::new();
    for v in expected.iter().chain(actual.iter()) {
        let k = kind(v);
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    for k in kinds {
        let e: Vec<&Value> = expected.iter().filter(|v| kind(v) == k).collect();
        let a: Vec<&Value> = actual.iter().filter(|v| kind(v) == k).collect();
        if e == a {
            continue;
        }
        let i = e
            .iter()
            .zip(a.iter())
            .position(|(x, y)| x != y)
            .unwrap_or(e.len().min(a.len()));
        let exp = e.get(i).map(|v| short(v)).unwrap_or("[absent]".into());
        let act = a.get(i).map(|v| short(v)).unwrap_or("[absent]".into());
        // Les lignes sont longues et `short` les coupe : on nomme aussi le premier champ
        // qui diffère, avec ses deux valeurs, pour lire la cause sans rejouer (CI).
        let at = match (e.get(i), a.get(i)) {
            (Some(x), Some(y)) => first_difference(x, y, String::new())
                .map(|(path, x, y)| format!(" ; premier écart en {path} : attendu {x}, obtenu {y}"))
                .unwrap_or_default(),
            _ => String::new(),
        };
        return Err(format!(
            "scénario {name} : {EXPECTED_FILE} diffère ({} n°{} sur {} attendu(s), {} obtenu(s) : \
             attendu {exp}, obtenu {act}{at}) : le comportement a changé ; si c'est voulu, \
             UPDATE_SCENARIOS=1 puis relire le diff",
            group_label(&k),
            i + 1,
            e.len(),
            a.len()
        ));
    }
    Ok(())
}

/// Compare les requêtes vues par le modèle, appel par appel, et nomme l'appel et le
/// message qui divergent.
pub fn compare_surface(name: &str, expected: &[Value], actual: &[Value]) -> Result<(), String> {
    let calls = expected.len().max(actual.len());
    for i in 0..calls {
        let n = i + 1;
        let (e, a) = match (expected.get(i), actual.get(i)) {
            (Some(e), Some(a)) if e == a => continue,
            (Some(e), Some(a)) => (e, a),
            (e, a) => {
                return Err(format!(
                    "scénario {name} : {SURFACE_FILE} diffère (appel {n} : attendu {}, obtenu {} ; {} \
                     appel(s) attendu(s), {} obtenu(s)) : ce que le modèle voit a changé ; si \
                     c'est voulu, UPDATE_SCENARIOS=1 puis relire le diff",
                    e.map(short).unwrap_or("[absent]".into()),
                    a.map(short).unwrap_or("[absent]".into()),
                    expected.len(),
                    actual.len()
                ));
            }
        };
        let where_ = match (e["messages"].as_array(), a["messages"].as_array()) {
            (Some(em), Some(am)) if em != am => {
                let m = em
                    .iter()
                    .zip(am.iter())
                    .position(|(x, y)| x != y)
                    .unwrap_or(em.len().min(am.len()));
                format!(
                    "appel {n}, message {} : attendu {}, obtenu {}",
                    m + 1,
                    em.get(m).map(short).unwrap_or("[absent]".into()),
                    am.get(m).map(short).unwrap_or("[absent]".into())
                )
            }
            _ => {
                let field = e
                    .as_object()
                    .into_iter()
                    .flat_map(|o| o.keys())
                    .find(|k| e[k.as_str()] != a[k.as_str()])
                    .cloned()
                    .unwrap_or_else(|| "?".into());
                format!(
                    "appel {n}, champ {field} : attendu {}, obtenu {}",
                    short(&e[field.as_str()]),
                    short(&a[field.as_str()])
                )
            }
        };
        return Err(format!(
            "scénario {name} : {SURFACE_FILE} diffère ({where_}) : ce que le modèle voit a \
             changé ; si c'est voulu, UPDATE_SCENARIOS=1 puis relire le diff"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
