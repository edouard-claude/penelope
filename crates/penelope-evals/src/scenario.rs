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
    /// Semé dans le vault de mémoire (`memory.vault_path`) plutôt que dans le workspace.
    #[serde(default)]
    pub vault: bool,
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
        /// L'appel doit échouer ; sans ce drapeau, une erreur fait échouer le scénario.
        #[serde(default)]
        error: bool,
        /// `tail` seulement : message joué pendant que le flux est ouvert.
        #[serde(default)]
        during: Option<String>,
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
            Step::Restart => "redémarrage".into(),
            Step::Approve => "approbation".into(),
            Step::Usage { prompt } => format!("dernier appel facturé : {prompt} tokens"),
            Step::Seed { exchanges, .. } => format!("historique semé : {exchanges} échanges"),
            Step::Rpc { method, .. } => format!("rpc : {method}"),
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
    /// `unknown_model`, `payment_required`, `content_filter`, `cancelled`, `other`.
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
        "cancelled" => LlmErrorKind::Cancelled,
        _ => LlmErrorKind::Other,
    }
}

/// Un scénario chargé.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub dir: PathBuf,
    pub spec: Spec,
    pub script: Vec<ScriptLine>,
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

fn load_script(path: &Path) -> anyhow::Result<Vec<ScriptLine>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{} : {e}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|(i, l)| {
            serde_json::from_str::<ScriptLine>(l)
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
        return Err(format!(
            "scénario {name} : {EXPECTED_FILE} diffère ({} n°{} sur {} attendu(s), {} obtenu(s) : \
             attendu {exp}, obtenu {act}) : le comportement a changé ; si c'est voulu, \
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn script_lines_round_trip_and_map_to_scripted() {
        let lines = vec![
            ScriptLine::Text("bonjour".into()),
            ScriptLine::ToolCalls {
                text: String::new(),
                calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    arguments: json!({"path": "a.txt"}),
                }],
            },
            ScriptLine::Error {
                kind: "transient".into(),
                message: "503".into(),
            },
            ScriptLine::ContextOverflow,
            ScriptLine::MidStreamError {
                text: "début".into(),
                message: "coupure".into(),
            },
        ];
        for l in &lines {
            let s = serde_json::to_string(l).unwrap();
            let back: ScriptLine = serde_json::from_str(&s).unwrap();
            assert_eq!(&back, l);
        }
        assert!(matches!(lines[0].to_scripted(), Scripted::Text(t) if t == "bonjour"));
        assert!(matches!(lines[1].to_scripted(), Scripted::ToolCalls(_, c) if c.len() == 1));
        assert!(matches!(
            lines[2].to_scripted(),
            Scripted::Error(LlmErrorKind::Transient, _)
        ));
        assert!(matches!(lines[3].to_scripted(), Scripted::ContextOverflow));
        assert_eq!(error_kind("inconnu"), LlmErrorKind::Other);
    }

    #[test]
    fn the_world_diff_names_the_group_and_the_line() {
        let expected = vec![
            json!({"type": "session", "id": "{{session:1}}"}),
            json!({"type": "effect", "tool": "schedule_create", "state": "completed"}),
        ];
        let actual = vec![json!({"type": "session", "id": "{{session:1}}"})];
        let err = compare_world("reminders-daily", &expected, &actual).unwrap_err();
        assert!(err.starts_with("scénario reminders-daily : expected.jsonl diffère (effects n°1"));
        assert!(err.contains("schedule_create"), "{err}");
        assert!(err.contains("obtenu [absent]"), "{err}");
        assert!(err.contains("UPDATE_SCENARIOS=1"), "{err}");
        assert!(compare_world("x", &expected, &expected).is_ok());
    }

    #[test]
    fn the_surface_diff_names_the_call_and_the_message() {
        let expected = vec![json!({"call": 1, "model": "m", "messages": [
            {"role": "system", "text": "S"}, {"role": "user", "text": "bonjour"}]})];
        let mut actual = expected.clone();
        actual[0]["messages"][1]["text"] = json!("salut");
        let err = compare_surface("tour-simple", &expected, &actual).unwrap_err();
        assert!(err.contains("appel 1, message 2"), "{err}");
        assert!(err.contains("bonjour") && err.contains("salut"), "{err}");
        let mut other = expected.clone();
        other[0]["model"] = json!("autre");
        let err = compare_surface("tour-simple", &expected, &other).unwrap_err();
        assert!(err.contains("appel 1, champ model"), "{err}");
        let err = compare_surface("tour-simple", &expected, &[]).unwrap_err();
        assert!(err.contains("1 appel(s) attendu(s), 0 obtenu(s)"), "{err}");
    }

    #[test]
    fn the_spec_format_is_read_as_documented() {
        let spec: Spec = toml::from_str(
            r#"
name = "exemple"
description = "un exemple"
pin_model = "main"

[config]
"context.large_payload_tokens" = 300

[[files]]
path = "notes.txt"
content = "ligne\n"
repeat = 3

[[mcp_tools]]
server = "banc"
name = "lire"
description = "Lit un fichier distant."
read_only = true
params = ["path"]
result = "contenu"

[[steps]]
kind = "message"
text = "bonjour"

[[steps]]
kind = "message"
text = "lis le fichier"
crash = "during_tool"

[[steps]]
kind = "restart"

[[steps]]
kind = "command"
command = "/compact"

[[steps]]
kind = "advance_clock"
by = "10m"

[[steps]]
kind = "seed"
exchanges = 2
user = "question {i}"
assistant = "réponse {i}"
"#,
        )
        .unwrap();
        assert_eq!(spec.steps.len(), 6);
        assert!(matches!(
            spec.steps[1],
            Step::Message {
                crash: Some(Crash::DuringTool),
                ..
            }
        ));
        assert_eq!(spec.steps[2].label(), "redémarrage");
        assert_eq!(spec.files[0].repeat, 3);
        assert!(spec.mcp_tools[0].read_only);
        assert_eq!(spec.config.len(), 1);
        assert!(toml::from_str::<Spec>("name = \"x\"\n[[steps]]\nkind = \"inconnu\"\n").is_err());
    }

    #[test]
    fn modes_follow_the_environment_variables() {
        // Les variables ne sont pas posées par cette suite : le défaut est le rejeu.
        if std::env::var("UPDATE_SCENARIOS").is_err() && std::env::var("RECORD_SCENARIO").is_err() {
            assert_eq!(mode_for("x"), Mode::Replay);
        }
    }
}
