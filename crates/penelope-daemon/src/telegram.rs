//! Passerelle Telegram (§14) : réception, commandes, approbations, brouillons, envoi.
//!
//! Trois boucles :
//! - **scrutation** : `getUpdates` en long polling, offset et updates persistés, chaque
//!   update traité une seule fois même après un crash ;
//! - **brouillons** : les fragments du bus deviennent un aperçu `sendMessageDraft`,
//!   limité en fréquence, jamais bloquant ;
//! - **envoi** : les réponses finales, cartes et retours de commandes passent par
//!   `tg_outbox`, dans l'ordre, avec reprise et repli en texte brut.

use crate::agent::{TurnEvent, TurnOutcome, decide_approval};
use crate::bus::{BusKind, ChannelDelivery, Origin};
use crate::executor::Messenger;
use crate::runtime::Daemon;
use penelope_hitl::{ApprovalRequest, ApprovalState, Decision};
use penelope_kernel::api::method as m;
use penelope_kernel::risk::PolicyWindow;
use penelope_store::rusqlite::params;
use penelope_telegram::actions::{Action, ClickOutcome, kind as k};
use penelope_telegram::api::{BotTransport, HttpTransport, inline_keyboard, reaction};
use penelope_telegram::render::ButtonSpec;
use penelope_telegram::{Bot, Incoming, TgError, classify, html_to_plain, markdown_to_html};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

mod screens;

/// Longueur d'un fragment Markdown avant conversion HTML : marge pour les balises.
const FRAGMENT_CHARS: usize = 3_500;
const MAX_ATTEMPTS: i64 = 6;
/// Une valeur JSON telle qu'on la montre dans une bulle : une chaîne sans guillemets, un
/// nombre tel quel, une absence en « ? », jamais `null` (issue #115).
pub(crate) fn shown(v: &Value) -> String {
    match v {
        Value::Null => "?".into(),
        Value::String(s) if s.is_empty() => "?".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Carte d'approbation d'un appel d'outil, lisible par le propriétaire (issue #116).
pub(crate) struct ApprovalCard {
    /// Ce que Pénélope cherche à faire, en une phrase.
    pub intention: String,
    /// L'action telle qu'elle sera faite : la commande exacte, ou l'outil et ses valeurs.
    pub action: String,
    /// Une ligne : qualificatifs, classe de risque, politique.
    pub details: String,
    /// Portée d'une règle « Toujours » : les familles qu'un clic réglerait
    /// (`« yt-dlp », « ffmpeg » (réseau)`). `None` : aucune règle n'est possible, et le
    /// bouton n'est pas posé du tout — la carte dit ce qui l'empêche (issues #141, #150).
    pub always: Option<String>,
}

/// Compose la carte d'une demande d'approbation.
pub(crate) fn approval_card(a: &ApprovalRequest) -> ApprovalCard {
    let args = crate::agent::without_intention(&a.payload["arguments"]);
    let intention = a.payload["why"]
        .as_str()
        .filter(|w| !w.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(|| format!("Pénélope veut utiliser `{}`.", a.subject));
    let (server, tool) = match crate::agent::server_of(&a.subject) {
        Some(srv) => {
            let tool = a
                .subject
                .strip_prefix(&format!("mcp__{srv}__"))
                .unwrap_or(&a.subject)
                .to_string();
            (Some(srv), tool)
        }
        None => (None, a.subject.clone()),
    };
    let mut quals: Vec<String> = Vec::new();
    let action = match (a.subject.as_str(), args["command"].as_str()) {
        ("shell_exec", Some(command)) => {
            if crate::executor::wants_network(&a.subject, &a.payload["arguments"]) {
                quals.push("réseau".into());
            }
            if args["output"].as_str() == Some("full") {
                quals.push("sortie complète".into());
            }
            if let Some(cwd) = args["cwd"].as_str() {
                quals.push(format!("dans `{cwd}`"));
            }
            if let Some(ms) = args["timeout_ms"].as_u64() {
                quals.push(format!("délai {} s", ms / 1_000));
            }
            format!("```\n{}\n```", command.replace("```", "ʼʼʼ"))
        }
        _ => {
            let fields: Vec<String> = args
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| {
                            let v = match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            let v: String = if v.chars().count() > 120 {
                                format!("{}…", v.chars().take(119).collect::<String>())
                            } else {
                                v
                            };
                            format!("{k} = {v}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            if fields.is_empty() {
                format!("`{tool}`")
            } else {
                format!("`{tool}` : {}", fields.join(" · "))
            }
        }
    };
    if let Some(srv) = server {
        quals.push(format!("serveur `{srv}`"));
    }
    // La demande est stockée rédigée : la carte dit combien de valeurs sont masquées,
    // la commande exécutée, elle, reste entière (issue #134).
    let masked = args
        .to_string()
        .matches(penelope_observe::redact::MASK)
        .count();
    if masked > 0 {
        quals.push(format!("🔒 {masked} valeur(s) masquée(s)"));
    }
    quals.push(format!("classe {}", a.risk.as_str()));
    if let Some(reason) = a.payload["reason"].as_str().filter(|r| !r.is_empty()) {
        quals.push(reason.to_string());
    }
    // Le libellé nomme **toutes** les familles qu'un clic autoriserait : une liste
    // `a && b` en crée une par famille, et le propriétaire doit les voir avant (#150).
    let patterns = crate::agent::arg_patterns(&a.subject, a.payload.get("arguments"));
    let describe = |p: &Value| {
        use penelope_hitl::policy::{CMD_PREFIX_OP, ORIGIN_OP, PATH_PREFIX_OP};
        let op = |k: &str, op: &str| p[k][op].as_str().map(String::from);
        if let Some(family) = op("command", CMD_PREFIX_OP) {
            format!("« {family} »")
        } else if let Some(dir) = op("path", PATH_PREFIX_OP) {
            if dir.is_empty() {
                "ce répertoire".into()
            } else {
                format!("`{dir}`")
            }
        } else if let Some(host) = op("url", ORIGIN_OP) {
            host
        } else {
            penelope_hitl::policy::describe_pattern(p)
        }
    };
    let always = (!patterns.is_empty()).then(|| {
        let network = patterns.iter().any(|p| p["network"] == Value::Bool(true));
        let names: Vec<String> = patterns.iter().map(&describe).collect();
        format!(
            "{}{}",
            names.join(", "),
            if network { " (réseau)" } else { "" }
        )
    });
    // Une commande composée n'a pas de famille : « Toujours » l'autoriserait une fois,
    // sans créer de règle (#111). La carte le dit avant le clic (#141).
    if crate::agent::always_creates_no_rule(&a.subject, a.payload.get("arguments")) {
        // Dire *ce qui* empêche la règle, et par où sortir : une commande par appel
        // (issue #150). Sans cela, « pas de règle possible » se lit comme une fatalité.
        let why = a.payload["arguments"]["command"]
            .as_str()
            .and_then(penelope_hitl::cmdline::why_composed)
            .unwrap_or_else(|| "commande composée".into());
        quals.push(format!(
            "pas de règle possible ({why}) — demande-lui une commande par appel"
        ));
    }
    ApprovalCard {
        intention,
        action,
        details: quals.join(" · "),
        always,
    }
}

/// État d'une connexion de fournisseur à compte, en une bulle (issue #142).
fn codex_status_text(v: &Value) -> String {
    let status = &v["status"];
    if status.is_null() {
        return "🔌 Aucun compte ChatGPT connecté. `/model auth codex` pour le faire.".into();
    }
    if let Some(why) = status["disconnected"].as_str() {
        return format!(
            "🔌 Compte ChatGPT déconnecté ({why}). `/model auth codex` pour reconnecter."
        );
    }
    format!(
        "✅ Compte ChatGPT connecté : plan {}, compte {}{}.",
        shown(&status["plan"]),
        shown(&status["account"]),
        if v["enabled"] == Value::Bool(true) {
            ""
        } else {
            " — fournisseur éteint (`providers.codex.enabled`)"
        }
    )
}

/// Mention des messages en attente abandonnés par la fermeture d'une session.
fn cancelled_note(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "\n⏹ 1 message en attente dans la session a été abandonné.".into(),
        n => format!("\n⏹ {n} messages en attente dans la session ont été abandonnés."),
    }
}

/// Mention du travail que la session quittée poursuit en fond (issue #112).
fn background_note(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "\n⏳ L'ancienne session continue en fond (1 tour en file) : sa réponse t'attend \
              à ton retour."
            .into(),
        n => format!(
            "\n⏳ L'ancienne session continue en fond ({n} tours en file) : ses réponses \
             t'attendent à ton retour."
        ),
    }
}

/// Formulaire d'étape `user` en cours dans un chat.
const BOT_USERNAME_KEY: &str = "tg.bot_username";

/// Lien `https://t.me/<bot>?start=<charge>` vers un écran ou une commande, quand le bot est
/// connu (issue #30).
pub async fn deep_link(d: &Daemon, payload: &str) -> Option<String> {
    let bot = d.kv_get(BOT_USERNAME_KEY).await.ok().flatten()?;
    (!bot.is_empty() && bot != "?").then(|| penelope_telegram::render::deep_link(&bot, payload))
}

/// Montant en dollars à la française : `5,02`, `20`.
fn fmt_usd(x: f64) -> String {
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    s.replace('.', ",")
}

/// Clé du formulaire en cours, **par sujet** (issue #149). Un seul formulaire par chat,
/// quel que soit le sujet, faisait qu'un formulaire ouvert dans un sujet avalait le texte
/// tapé dans un autre, et répondait dans Général.
fn form_key(chat_id: i64, topic_id: Option<i64>) -> String {
    match topic_id {
        Some(t) => format!("tg.form.{chat_id}.{t}"),
        None => format!("tg.form.{chat_id}"),
    }
}

fn approval_reason_key(chat_id: i64, topic_id: Option<i64>) -> String {
    match topic_id {
        Some(t) => format!("tg.await_reason.{chat_id}.{t}"),
        None => format!("tg.await_reason.{chat_id}"),
    }
}

/// Sujet où vit un formulaire, d'après sa charge : toutes ses phrases y retournent.
fn form_topic(pending: &Value) -> Option<i64> {
    pending["topic"].as_i64()
}

/// Durée de validité du bouton « Réessayer » d'un tour échoué.
const RETRY_TTL_MS: i64 = 24 * 3600 * 1000;

pub struct TelegramGateway {
    pub daemon: Arc<Daemon>,
    pub bot: Arc<Bot>,
    owner_id: i64,
    draft_interval: Duration,
    poll_timeout_s: u64,
    outbox_wake: Notify,
    /// Albums en cours de réception, par `media_group_id`.
    albums: Arc<std::sync::Mutex<HashMap<String, Album>>>,
    /// Morceaux d'un même envoi texte en cours de réception, par chat (issue #49).
    bursts: Arc<std::sync::Mutex<HashMap<i64, TextBurst>>>,
    /// Sorties mises de côté des sessions en arrière-plan : lues et réécrites sous verrou.
    held_lock: tokio::sync::Mutex<()>,
    /// Indicateurs d'activité des tours en cours, par tour (issue #121).
    activities: Arc<std::sync::Mutex<HashMap<String, Activity>>>,
    /// Intervalle de renvoi de l'indicateur, en millisecondes (Telegram l'efface au bout
    /// de cinq secondes).
    activity_every_ms: std::sync::atomic::AtomicU64,
}

/// Indicateur d'activité d'un tour : renvoyé tant qu'il vit, l'action suit ce qu'il fait.
struct Activity {
    action: Arc<std::sync::Mutex<&'static str>>,
    task: tokio::task::JoinHandle<()>,
}

/// Action d'activité d'un outil : un fichier qui part, une voix, une image, sinon « écrit ».
fn activity_for(tool: &str) -> &'static str {
    match tool {
        "send_file" | "artifact_read" => "upload_document",
        "send_voice" => "record_voice",
        "image_generate" => "upload_photo",
        _ => "typing",
    }
}

/// Résumé d'un appel d'outil pour la ligne d'état du brouillon : la commande, le
/// chemin, la requête ou l'adresse, raccourcis (issue #121).
fn tool_status(name: &str, args: &Value) -> String {
    let detail = ["command", "path", "query", "url", "name", "id"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .map(|d| {
            let d = d.split_whitespace().collect::<Vec<_>>().join(" ");
            if d.chars().count() > 60 {
                format!("{}…", d.chars().take(59).collect::<String>())
            } else {
                d
            }
        });
    match detail {
        Some(d) => format!("⚙️ {name} · {d}"),
        None => format!("⚙️ {name}…"),
    }
}

/// Sortie d'une session en arrière-plan, mise de côté jusqu'à son retour au focus du chat
/// (issue #10).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Held {
    Text {
        text: String,
        reply_to: Option<i64>,
        /// Réponse finale d'un tour : réaction ✅ sur le message d'origine.
        answer: bool,
        /// Suites proposées en boutons (boucle arrêtée, issue #31).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        choices: Vec<String>,
    },
    Approval {
        id: String,
    },
    Failure {
        error: String,
        reply_to: Option<i64>,
    },
    File {
        path: String,
        caption: Option<String>,
    },
    /// Message vocal (issue #41).
    Voice {
        path: String,
        duration_s: u32,
        caption: Option<String>,
    },
}

fn held_key(session_id: &str) -> String {
    format!("tg.held.{session_id}")
}

/// « 1 réponse », « 3 approbations ».
fn count_of(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n > 1 { many } else { one })
}

/// Photos d'un même album, regroupées en un seul tour (§14.4).
struct Album {
    origin: Origin,
    images: Vec<std::path::PathBuf>,
    caption: Option<String>,
    update_id: i64,
}

/// Fenêtre de regroupement d'un album : Telegram envoie ses photos une par une.
const ALBUM_WINDOW: Duration = Duration::from_millis(1_500);

/// Un long texte collé arrive découpé par Telegram en messages de 4 096 caractères : un
/// morceau de cette taille appelle la suite sans séparateur (issue #49).
const TELEGRAM_TEXT_LIMIT: usize = 4_000;
/// Préfixe des notes d'échec d'envoi : elles n'en appellent pas d'autres (issue #101).
const FAILURE_NOTE: &str = "failnote-";
/// Silence qui clôt une rafale après un message court, sa fin probable (issue #96).
const TAIL_QUIET: Duration = Duration::from_millis(300);

/// Morceaux d'un même envoi, en attente de leur fin de fenêtre (issue #49).
#[derive(Debug, Clone)]
struct TextBurst {
    origin: Origin,
    session: String,
    parts: Vec<String>,
    message_ids: Vec<i64>,
    /// `update_id` du premier morceau : clé de déduplication du tour.
    update_id: i64,
    chars: usize,
    /// Instant à partir duquel la rafale est considérée comme finie.
    deadline: std::time::Instant,
}

impl TextBurst {
    /// Recolle les morceaux : un morceau à la limite de Telegram est la suite du
    /// précédent, les autres sont des messages distincts.
    fn joined(&self) -> String {
        let mut out = String::new();
        for (i, part) in self.parts.iter().enumerate() {
            if i > 0 && self.parts[i - 1].chars().count() < TELEGRAM_TEXT_LIMIT {
                out.push_str("\n\n");
            }
            out.push_str(part);
        }
        out
    }
}
/// Ce que `/stop` a trouvé, et ce qu'il en dit (issue #155).
///
/// Le 21/09, `/stop` puis `/stop tout` ont répondu « Rien à arrêter » alors qu'un run était
/// **bloqué** dans le sujet depuis vingt minutes et que la dernière réponse le disait « en
/// cours ». Seuls les runs `running` étaient regardés ; un run bloqué — précisément celui
/// qui *paraît* en cours — n'était ni touché ni même nommé.
#[derive(Debug, Default)]
pub(crate) struct StopReport {
    pub running: bool,
    pub queued: usize,
    pub burst: usize,
    pub sessions: usize,
    /// Runs mis en pause.
    pub paused: usize,
    /// Runs laissés ouverts, nommés : ils ne sont pas annulés à la place du propriétaire.
    pub left: Vec<String>,
    /// Tous les runs ouverts du chat : identifiant et état.
    pub open: Vec<(String, String)>,
    /// Ingestions de documents en cours (issue #155).
    pub ingests: usize,
    /// Celles que `/stop tout` vient d'interrompre.
    pub cancelled_ingests: usize,
    pub tout: bool,
}

impl StopReport {
    pub(crate) fn render(&self) -> String {
        let mut note = match (self.running, self.queued) {
            // « Rien à arrêter » seulement quand il n'y a vraiment rien : un run ouvert
            // compte, quel que soit son état.
            (false, 0) if self.open.is_empty() && self.ingests == 0 => {
                "Rien à arrêter.".to_string()
            }
            (false, 0) => "⏹ Aucun tour en cours.".to_string(),
            (true, 0) => "⏹ Tour arrêté.".to_string(),
            (false, n) => format!("⏹ {n} message(s) en attente annulé(s)."),
            (true, n) => format!("⏹ Tour arrêté, {n} message(s) en attente annulé(s)."),
        };
        if self.burst > 0 {
            note.push_str(&format!(
                " {} morceau(x) reçus à l'instant écartés.",
                self.burst
            ));
        }
        if self.sessions > 0 {
            note.push_str(&format!(
                " {} autre(s) session(s) de ce chat vidée(s).",
                self.sessions
            ));
        }
        if self.paused > 0 {
            note.push_str(&format!(
                " {} run(s) de workflow mis en pause.",
                self.paused
            ));
        }
        if self.cancelled_ingests > 0 {
            note.push_str(&format!(
                " {} ingestion(s) de document interrompue(s).",
                self.cancelled_ingests
            ));
        }
        if !self.left.is_empty() {
            note.push_str(&format!(
                "\n\nRun(s) laissé(s) ouvert(s) : {}. Ils ne sont pas annulés à ta place : \
                 `/run cancel <id>` pour en finir, la carte du run pour réessayer ou passer \
                 l'étape.",
                self.left.join(", ")
            ));
        }
        // Ne promettre que ce qui est fait : l'ingestion n'était pas interrompue malgré la
        // phrase qui l'annonçait, et les runs ouverts n'étaient pas nommés.
        if !self.tout && (!self.open.is_empty() || self.ingests > 0) {
            let mut rest: Vec<String> = Vec::new();
            if !self.open.is_empty() {
                rest.push(format!(
                    "{} run(s) ouvert(s) ({})",
                    self.open.len(),
                    self.open
                        .iter()
                        .map(|(id, st)| format!("`{id}` {st}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if self.ingests > 0 {
                rest.push(format!("{} ingestion(s) de document", self.ingests));
            }
            note.push_str(&format!(
                "\n\nContinue : {}. `/stop tout` met en pause ce qui tourne, interrompt les \
                 ingestions et nomme le reste.",
                rest.join(", ")
            ));
        }
        note
    }
}

impl TelegramGateway {
    /// Construit la passerelle depuis la configuration. `Ok(None)` : Telegram n'est
    /// pas configuré (pas de propriétaire ou pas de jeton), ce n'est pas une erreur.
    pub async fn from_config(daemon: Arc<Daemon>) -> Result<Option<Arc<Self>>, String> {
        let s = &daemon.services;
        let cfg = s.config.config();
        if cfg.owner.telegram_user_id == 0 {
            return Ok(None);
        }
        let token = match s.platform.secrets.expand(&cfg.telegram.token) {
            Ok(t) if !t.trim().is_empty() => t,
            _ => return Ok(None),
        };
        let transport =
            HttpTransport::new(&cfg.telegram.api_base, &token).map_err(|e| e.to_string())?;
        Ok(Some(Self::with_transport(
            daemon.clone(),
            Arc::new(transport),
        )))
    }

    pub fn with_transport(daemon: Arc<Daemon>, transport: Arc<dyn BotTransport>) -> Arc<Self> {
        let cfg = daemon.services.config.config();
        let bot = Arc::new(Bot::new(
            transport,
            cfg.telegram.rate_per_chat_per_s,
            daemon.services.clock.clone(),
        ));
        Arc::new(TelegramGateway {
            owner_id: cfg.owner.telegram_user_id,
            draft_interval: Duration::from_millis(cfg.telegram.draft_interval_ms.max(300)),
            poll_timeout_s: cfg.telegram.poll_timeout_s,
            outbox_wake: Notify::new(),
            albums: Arc::new(std::sync::Mutex::new(HashMap::new())),
            bursts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            held_lock: tokio::sync::Mutex::new(()),
            activities: Arc::new(std::sync::Mutex::new(HashMap::new())),
            activity_every_ms: std::sync::atomic::AtomicU64::new(4_000),
            daemon,
            bot,
        })
    }

    /// Intervalle de l'indicateur d'activité (tests).
    pub fn set_activity_every(&self, every: Duration) {
        self.activity_every_ms.store(
            every.as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Un indicateur d'activité, jetable : hors de la file d'envoi durable, et son échec ne
    /// touche jamais le tour (issue #121).
    fn chat_action(&self, chat_id: i64, topic_id: Option<i64>, action: &str) {
        let bot = self.bot.clone();
        let mut payload = json!({"chat_id": chat_id, "action": action});
        if let Some(t) = topic_id {
            payload["message_thread_id"] = json!(t);
        }
        tokio::spawn(async move {
            let _ = bot
                .call(
                    penelope_telegram::api::method::SEND_CHAT_ACTION,
                    None,
                    payload,
                )
                .await;
        });
    }

    /// Démarre l'indicateur d'un tour : renvoyé toutes les quatre secondes tant que le tour
    /// vit, dans son sujet, arrêté avec lui (issue #121).
    fn start_activity(&self, turn_id: &str, session_id: &str, chat_id: i64, topic_id: Option<i64>) {
        let action = Arc::new(std::sync::Mutex::new("typing"));
        let every = Duration::from_millis(
            self.activity_every_ms
                .load(std::sync::atomic::Ordering::Relaxed)
                .max(10),
        );
        let (bot, daemon, a, sid) = (
            self.bot.clone(),
            self.daemon.clone(),
            action.clone(),
            session_id.to_string(),
        );
        let task = tokio::spawn(async move {
            // Un tour dont la fin aurait échappé au bus ne garde pas l'indicateur allumé.
            while daemon.bus.is_active(&sid) && !daemon.handle.is_shutting_down() {
                let current = *a.lock().unwrap_or_else(|p| p.into_inner());
                let mut payload = json!({"chat_id": chat_id, "action": current});
                if let Some(t) = topic_id {
                    payload["message_thread_id"] = json!(t);
                }
                let _ = bot
                    .call(
                        penelope_telegram::api::method::SEND_CHAT_ACTION,
                        None,
                        payload,
                    )
                    .await;
                tokio::time::sleep(every).await;
            }
        });
        if let Ok(mut g) = self.activities.lock()
            && let Some(old) = g.insert(turn_id.to_string(), Activity { action, task })
        {
            old.task.abort();
        }
    }

    fn set_activity(&self, turn_id: &str, action: &'static str) {
        if let Ok(g) = self.activities.lock()
            && let Some(a) = g.get(turn_id)
            && let Ok(mut current) = a.action.lock()
        {
            *current = action;
        }
    }

    fn stop_activity(&self, turn_id: &str) {
        if let Ok(mut g) = self.activities.lock()
            && let Some(a) = g.remove(turn_id)
        {
            a.task.abort();
        }
    }

    /// Branche la passerelle dans le daemon (messages, livraison).
    pub fn register(self: &Arc<Self>) {
        let hooks = &self.daemon.hooks;
        if let Ok(mut g) = hooks.messenger.write() {
            *g = Some(self.clone());
        }
        if let Ok(mut g) = hooks.telegram.write() {
            *g = Some(self.clone());
        }
        self.daemon.services.elicitations.attach(self.clone());
    }

    /// Vérifie le jeton, publie les commandes, lance les boucles.
    pub async fn start(self: &Arc<Self>) -> Result<Vec<tokio::task::JoinHandle<()>>, String> {
        let me =
            self.bot.get_me().await.map_err(|e| {
                format!("jeton Telegram refusé ({e}) : vérifier `telegram_bot_token`")
            })?;
        let username = me.get("username").and_then(|u| u.as_str()).unwrap_or("?");
        tracing::info!(bot = username, "Telegram connecté");
        // Liens profonds des textes longs (digest, audit) vers un écran précis (issue #30).
        let _ = self.daemon.kv_set(BOT_USERNAME_KEY, username).await;
        if let Err(e) = self
            .bot
            .set_commands(penelope_telegram::commands::to_bot_commands())
            .await
        {
            tracing::warn!(error = %e, "setMyCommands refusé");
        }
        self.register();
        // Effets restés incertains après un arrêt brutal : la question part sans attendre
        // que le propriétaire pense à `/approvals` (#83).
        if let Err(e) = self.announce_uncertain_effects().await {
            tracing::warn!(error = %e, "effets incertains non annoncés");
        }
        // Surveillées : une panique relance la boucle au lieu de rendre le bot muet (#84).
        let (a, b, c) = (self.clone(), self.clone(), self.clone());
        let d = self.daemon.clone();
        Ok(vec![
            crate::tasks::spawn_supervised(d.clone(), "telegram.poll", move || {
                a.clone().poll_loop()
            }),
            crate::tasks::spawn_supervised(d.clone(), "telegram.drafts", move || {
                b.clone().draft_loop()
            }),
            crate::tasks::spawn_supervised(d, "telegram.outbox", move || c.clone().outbox_loop()),
        ])
    }

    fn shutting_down(&self) -> bool {
        self.daemon.handle.is_shutting_down()
    }

    /// Pousse chaque demande `effect_unknown` en attente, une fois : dans le chat de sa
    /// session, sinon en privé au propriétaire.
    pub async fn announce_uncertain_effects(&self) -> anyhow::Result<usize> {
        let s = &self.daemon.services;
        let mut sent = 0;
        for a in s.approvals.pending(200).await? {
            if a.kind != penelope_hitl::ApprovalKind::EffectUnknown {
                continue;
            }
            let flag = format!("tg.card.effect.{}", a.id.as_str());
            if self.daemon.kv_get(&flag).await?.is_some() {
                continue;
            }
            let (chat_id, topic_id) = self
                .recorded_approval_destination(&a)
                .await
                .unwrap_or_else(|| self.home_chat());
            self.send_approval_card(chat_id, topic_id, &a).await?;
            self.daemon.kv_set(&flag, "1").await?;
            sent += 1;
        }
        Ok(sent)
    }

    // ================================================================ réception

    async fn poll_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        while !self.shutting_down() {
            let offset = self
                .daemon
                .kv_get("tg.offset")
                .await
                .ok()
                .flatten()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            match self.bot.get_updates(offset, self.poll_timeout_s).await {
                Ok(updates) => {
                    backoff = Duration::from_secs(1);
                    for u in updates {
                        let id = u.get("update_id").and_then(|v| v.as_i64()).unwrap_or(0);
                        if let Err(e) = self.process_update(&u).await {
                            tracing::error!(update = id, error = %e, "update Telegram en échec");
                            // Jamais de silence : le propriétaire voit l'erreur au lieu de
                            // retaper ou de croire que c'est fait (issue #71).
                            self.report_failure(&u, &e).await;
                        }
                        let _ = self.daemon.kv_set("tg.offset", &(id + 1).to_string()).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "getUpdates en échec, nouvelle tentative");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    /// Dit au propriétaire qu'un update n'a pas pu être traité, dans la conversation où il
    /// l'a envoyé (issue #71).
    async fn report_failure(&self, update: &Value, error: &anyhow::Error) {
        let msg = update
            .get("message")
            .or_else(|| update.get("edited_message"))
            .or_else(|| update.pointer("/callback_query/message"));
        let chat_id = msg
            .and_then(|m| m.pointer("/chat/id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(self.owner_id);
        let topic_id = msg
            .and_then(|m| m.get("message_thread_id"))
            .and_then(|v| v.as_i64());
        let reply_to = msg
            .and_then(|m| m.get("message_id"))
            .and_then(|v| v.as_i64());
        let text = format!("⚠️ Ta demande n'a pas pu être traitée : {error}");
        if let Err(e) = self.reply(chat_id, topic_id, reply_to, &text).await {
            tracing::warn!(error = %e, "échec non signalé au propriétaire");
        }
    }

    /// Traite un update. Idempotent : un update déjà traité est ignoré.
    pub async fn process_update(self: &Arc<Self>, update: &Value) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let update_id = update
            .get("update_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let (now, raw) = (s.clock.now_rfc3339(), update.to_string());
        let already = s
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO tg_updates(update_id, received_at, processed, payload)
                     VALUES(?1, ?2, 0, ?3)",
                    params![update_id, now, raw],
                )?;
                let processed: i64 = tx.query_row(
                    "SELECT processed FROM tg_updates WHERE update_id = ?1",
                    [update_id],
                    |r| r.get(0),
                )?;
                Ok(processed == 1)
            })
            .await?;
        if already {
            return Ok(());
        }

        // Conversations autorisées relues à chaque update : `config set` s'applique à
        // chaud (issue #113).
        let access = penelope_telegram::Access {
            owner_id: self.owner_id,
            allowed_chats: s.config.config().telegram.allowed_chats.clone(),
        };
        // Nom du sujet Telegram : il peut donner son sujet de travail à la session (#119).
        // Titre du groupe : il nomme où livre une planification (#124).
        let names = topic_name_of(update)
            .map(|(chat, topic, name)| (chat, topic_name_key(chat, topic), name))
            .into_iter()
            .chain(chat_title_of(update).map(|(chat, t)| (chat, chat_title_key(chat), t)));
        for (chat, key, name) in names {
            if access.allowed_chats.contains(&chat)
                && self.daemon.kv_get(&key).await.ok().flatten().as_deref() != Some(name.as_str())
            {
                let _ = self.daemon.kv_set(&key, &name).await;
            }
        }
        let incoming = classify(update, &access);
        self.handle(incoming).await?;

        // Traité : seul `update_id` sert encore (déduplication). Le texte intégral n'a
        // plus de raison d'être gardé (issue #46).
        s.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE tg_updates SET processed = 1, payload = '{}' WHERE update_id = ?1",
                    [update_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn handle(self: &Arc<Self>, incoming: Incoming) -> anyhow::Result<()> {
        match incoming {
            Incoming::Text {
                update_id,
                chat_id,
                message_id,
                topic_id,
                text,
                forwarded,
                ..
            } => {
                // Renommage demandé depuis le menu `/sessions` il y a moins de 5 min : ce
                // message est le titre.
                let title_key = format!("tg.await_title.{chat_id}");
                if let Some(raw) = self.daemon.kv_get(&title_key).await?
                    && let Some((target, at)) = raw.split_once(' ')
                    && self.daemon.services.clock.now_ms() - at.parse::<i64>().unwrap_or(0)
                        < 5 * 60_000
                {
                    let target = target.to_string();
                    self.daemon.kv_set(&title_key, "").await?;
                    let note = match crate::titles::clean(&text) {
                        Some(title) => {
                            self.daemon
                                .services
                                .sessions
                                .set_title(&target, &title, false)
                                .await?;
                            format!("✏️ Session renommée : « {title} ».")
                        }
                        None => "Titre vide : rien n'a changé.".to_string(),
                    };
                    return self.reply(chat_id, topic_id, Some(message_id), &note).await;
                }
                // Entretien d'accueil en cours : ce message répond à la question posée.
                let onboard_key = format!("tg.onboard.{chat_id}");
                if let Some(raw) = self.daemon.kv_get(&onboard_key).await?
                    && let Ok(v) = serde_json::from_str::<Value>(&raw)
                    && let Some(rel) = v["rel"].as_str()
                {
                    let n = v["n"].as_u64().unwrap_or(0) as u32;
                    return match crate::onboarding::answer(&self.daemon, rel, n, Some(&text)).await
                    {
                        Ok(sitting) => self.onboarding_ask(chat_id, topic_id, &sitting).await,
                        Err(e) => {
                            self.reply(chat_id, topic_id, Some(message_id), &format!("⚠️ {e}"))
                                .await
                        }
                    };
                }
                // Un formulaire d'étape `user` est en cours : ce message remplit le champ.
                if let Some(raw) = self.form_pending(chat_id, topic_id).await?
                    && !raw.is_empty()
                {
                    return self.form_input(chat_id, &raw, Some(&text)).await;
                }
                // Une saisie était attendue par une étape `user` de workflow.
                let input_key = format!("tg.await_input.{chat_id}");
                if let Some(raw) = self.daemon.kv_get(&input_key).await?
                    && !raw.is_empty()
                {
                    self.daemon.kv_set(&input_key, "").await?;
                    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                    let note = match crate::workflow::answer(
                        &self.daemon,
                        v["run"].as_str().unwrap_or_default(),
                        v["visit"].as_str().unwrap_or_default(),
                        v["choice"].as_str().unwrap_or_default(),
                        Some(&text),
                    )
                    .await
                    {
                        Ok(()) => "✔️ Réponse transmise au workflow.".to_string(),
                        Err(e) => format!("ℹ️ {e}"),
                    };
                    return self.reply(chat_id, topic_id, Some(message_id), &note).await;
                }

                // Une raison de refus était attendue : ce message la donne.
                let reason_key = approval_reason_key(chat_id, topic_id);
                if let Some(approval_id) = self.daemon.kv_get(&reason_key).await?
                    && !approval_id.is_empty()
                {
                    self.daemon.kv_set(&reason_key, "").await?;
                    let d = Decision::deny("telegram", Some(text.clone()));
                    self.finalize_decision(&approval_id, &d, chat_id, topic_id)
                        .await?;
                    return Ok(());
                }

                // Profil vide : l'accueil est proposé une fois, sans retenir le message.
                if self.daemon.kv_get("tg.onboard.proposed").await?.is_none()
                    && crate::onboarding::profile_is_empty(&self.daemon).await
                {
                    self.daemon
                        .kv_set(
                            "tg.onboard.proposed",
                            &self.daemon.services.clock.now_rfc3339(),
                        )
                        .await?;
                    self.propose_onboarding(chat_id, topic_id).await?;
                }
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: Some(message_id),
                };
                let session = self.daemon.chat_session_for(&origin).await?;
                let content = if forwarded {
                    penelope_observe::injection::wrap_untrusted("message transféré", &text)
                } else {
                    text
                };
                self.react(chat_id, message_id, reaction::RECEIVED);
                // Les morceaux d'un même envoi attendent la fin de la fenêtre et partent
                // en un seul tour (issue #49) ; un message court tapé part tout de suite
                // (issue #96).
                self.buffer_text(&origin, &session, message_id, update_id, content, forwarded)
                    .await?;
            }
            Incoming::Command {
                chat_id,
                topic_id,
                message_id,
                command,
                args,
                ..
            } => {
                Box::pin(self.command(chat_id, topic_id, message_id, &command, &args)).await?;
            }
            Incoming::Callback {
                callback_id,
                data,
                message_id,
                chat_id,
                topic_id,
                from_id,
                ..
            } => {
                self.callback(&callback_id, &data, chat_id, topic_id, message_id, from_id)
                    .await?;
            }
            Incoming::StoppedGeneration { draft_id, .. } => {
                if let Some(session) = self.daemon.bus.session_for_draft(draft_id) {
                    self.daemon.bus.cancel_session(&session);
                }
            }
            Incoming::Voice {
                update_id,
                chat_id,
                message_id,
                topic_id,
                file_id,
                file_name,
                mime_type,
                file_size,
                ..
            } => {
                // Téléchargement puis transcription : détachés, sinon `/stop` et les
                // boutons attendent la fin (issue #69).
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me
                        .voice(
                            update_id,
                            chat_id,
                            topic_id,
                            message_id,
                            &file_id,
                            file_name.as_deref(),
                            mime_type.as_deref(),
                            file_size,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "vocal Telegram non traité");
                    }
                });
            }
            photo @ Incoming::Photo { .. } => {
                // Téléchargement de la photo : détaché aussi (issue #69).
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.photo(photo).await {
                        tracing::warn!(error = %e, "photo Telegram non traitée");
                    }
                });
            }
            document @ Incoming::Document { .. } => {
                // Téléchargement du document : détaché comme vocaux et photos, sinon
                // `/stop` et les boutons attendent jusqu'à 20 Mo (issue #98).
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.document(document).await {
                        tracing::warn!(error = %e, "document Telegram non traité");
                    }
                });
            }
            Incoming::OAuthCallback { chat_id, url, .. } => {
                // Adresse de retour collée (§8.5, `paste_back`) : elle ne sert qu'une fois.
                match crate::mcp_auth::complete(&self.daemon, &url).await {
                    Ok(server) => crate::mcp_auth::reconnect_and_tell(&self.daemon, &server).await,
                    Err(e) => {
                        self.reply(
                            chat_id,
                            None,
                            None,
                            &format!("🔐 Autorisation impossible : {e}"),
                        )
                        .await?
                    }
                }
            }
            Incoming::Edited { .. } => {}
            Incoming::Unauthorized { from_id, .. } => {
                // Aucune réponse : ne pas confirmer l'existence du bot à un inconnu.
                tracing::warn!(from = from_id, "message Telegram d'un inconnu ignoré");
            }
            Incoming::ForeignChat {
                chat_id,
                chat_type,
                title,
                from_id,
                ..
            } => {
                // Silence dans la conversation, mais l'identifiant est dit : c'est ce qu'il
                // faut ajouter à `telegram.allowed_chats` (issue #113).
                tracing::warn!(
                    chat = chat_id,
                    kind = %chat_type,
                    title = %title,
                    from = from_id,
                    "conversation Telegram non autorisée ignorée : \
                     telegram.allowed_chats"
                );
                record_seen_chat(&self.daemon.services, chat_id, &chat_type, &title).await;
            }
            Incoming::Ignored { reason, .. } => {
                tracing::debug!(%reason, "update ignoré");
            }
        }
        Ok(())
    }

    // ================================================================ commandes

    async fn command(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let rpc = crate::rpc::Rpc::new(d.clone());
        let args = args.trim();
        let reply_to = Some(message_id);

        let text: String = match command {
            "start" | "help" => {
                // `/start <charge>` : lien profond vers un écran (issue #30).
                if !args.is_empty() {
                    return self
                        .open_deep_link(chat_id, topic_id, message_id, args)
                        .await;
                }
                return self
                    .show_screen(chat_id, topic_id, reply_to, "help", &json!({}), None)
                    .await;
            }
            "new" => {
                // `/new !titre` ferme l'ancienne session sans redemander, `/new ~titre` la
                // garde en fond : ce sont les deux boutons de la question ci-dessous.
                let (choice, args) = match args.trim_start().chars().next() {
                    Some('!') => (Some(true), args.trim_start()[1..].trim()),
                    Some('~') => (Some(false), args.trim_start()[1..].trim()),
                    _ => (None, args),
                };
                if let Some(old) = s.sessions.find_by_topic(chat_id, topic_id).await? {
                    let queued = s.turns.queued_for(old.id.as_str()).await?;
                    // Une session qui travaille encore : dire ce que `/new` lui ferait,
                    // avant de le faire (issue #112).
                    if choice.is_none() && queued > 0 {
                        let rows = vec![
                            vec![
                                self.command_button_with(
                                    "⏳ Garder l'ancienne en fond",
                                    "new",
                                    &format!("~{args}"),
                                )
                                .await?,
                            ],
                            vec![
                                self.command_button_with(
                                    &format!("🗑 Fermer ({queued} tour(s) perdu(s))"),
                                    "new",
                                    &format!("!{args}"),
                                )
                                .await?,
                            ],
                        ];
                        let text = format!(
                            "La session « {} » a {queued} tour(s) en file ou en cours. \
                             `/new` la ferme et **ils seront perdus**. La garder en fond : \
                             elle finit son travail et ses réponses t'attendent à ton retour \
                             (`/sessions`).",
                            crate::titles::label(&old)
                        );
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                screens::Screen { text, rows },
                                None,
                            )
                            .await;
                    }
                    if choice != Some(false) {
                        crate::session_ops::silence(d, old.id.as_str(), "nouvelle session").await?;
                        s.sessions.set_state(old.id.as_str(), "closed").await?;
                        // `/new` clôt aussi l'épisode en cours : il est relu (§6.6).
                        crate::episodes::spawn_ingest(
                            d.clone(),
                            old.id.to_string(),
                            old.episode_seq,
                            crate::episodes::Boundary::NewSession,
                        );
                    }
                }
                let title = (!args.is_empty())
                    .then(|| crate::titles::clean(args))
                    .flatten();
                let sess = s
                    .sessions
                    .create(penelope_kernel::session::SessionKind::Chat, title.clone())
                    .await?;
                s.sessions
                    .bind_telegram(sess.id.as_str(), chat_id, topic_id)
                    .await?;
                if let Some(title) = title {
                    // Notes d'une session sur le même sujet : proposées (issue #32).
                    let similar = crate::session_notes::similar(s, sess.id.as_str(), &title).await;
                    let text = format!("🆕 Nouvelle session « {title} » (`{}`).", sess.id);
                    if similar.is_empty() {
                        text
                    } else {
                        let mut rows = Vec::new();
                        for (from, label) in &similar {
                            let t = s
                                .actions
                                .create(
                                    k::SCREEN_DO,
                                    "notes.adopt",
                                    json!({"params": {"from": from, "to": sess.id.to_string()}, "back": null}),
                                    24 * 3_600_000,
                                    true,
                                )
                                .await?;
                            rows.push(vec![ButtonSpec::callback(
                                &format!(
                                    "📓 Reprendre les notes de « {} »",
                                    label.chars().take(40).collect::<String>()
                                ),
                                &t.token,
                                "",
                            )]);
                        }
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                screens::Screen {
                                    text: format!(
                                        "{text}\nDes notes de travail existent sur un sujet proche."
                                    ),
                                    rows,
                                },
                                None,
                            )
                            .await;
                    }
                } else {
                    // Sans titre : le message sera complété quand le titre automatique arrive.
                    let text = format!(
                        "🆕 Nouvelle session `{}`. Son titre suivra le premier échange.",
                        sess.id
                    );
                    let sent = self
                        .bot
                        .send_text(
                            chat_id,
                            topic_id,
                            &markdown_to_html(&text),
                            None,
                            Some(message_id),
                        )
                        .await;
                    match sent
                        .ok()
                        .and_then(|v| v.get("message_id").and_then(|m| m.as_i64()))
                    {
                        Some(mid) => {
                            d.kv_set(
                                &format!("tg.new_session.{}", sess.id),
                                &format!("{chat_id}:{mid}"),
                            )
                            .await?;
                            return Ok(());
                        }
                        None => text,
                    }
                }
            }
            "title" => {
                let session = d.chat_session_for(&origin).await?;
                match crate::titles::clean(args) {
                    None => {
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                Self::typed_screen(
                                    "✏️ Nouveau titre de la session : `/title` suivi du titre.",
                                    "✏️ Écrire le titre",
                                    "/title ",
                                ),
                                None,
                            )
                            .await;
                    }
                    Some(title) => {
                        s.sessions.set_title(&session, &title, false).await?;
                        format!("✏️ Session renommée : « {title} ».")
                    }
                }
            }
            "sessions" => {
                let all = matches!(args, "all" | "toutes" | "tout");
                return self
                    .send_sessions_menu(chat_id, topic_id, 0, all, None)
                    .await;
            }
            "compact" => {
                // Un résumé prend de quelques secondes à une minute : la file des updates
                // n'attend pas, le bilan arrive en réponse quand il est prêt.
                let session = d.chat_session_for(&origin).await?;
                self.react(chat_id, message_id, reaction::RECEIVED);
                let (daemon, messenger) = (d.clone(), d.hooks.messenger());
                tokio::spawn(async move {
                    let text = match crate::compaction::compact(
                        &daemon,
                        &session,
                        crate::compaction::Trigger::Manual,
                        None,
                    )
                    .await
                    {
                        Ok(r) => crate::compaction::report_text(&r),
                        Err(e) => format!("❌ {e}"),
                    };
                    if let Some(m) = messenger {
                        let _ = m.send_text(&origin, &text).await;
                    }
                });
                return Ok(());
            }
            "fork" => {
                let session = d.chat_session_for(&origin).await?;
                let title = (!args.is_empty()).then(|| args.to_string());
                match crate::session_ops::fork(d, &session, title).await {
                    Ok(v) => {
                        let fork = v["session"].as_str().unwrap_or_default().to_string();
                        let background = self.bind_chat(&fork, chat_id, topic_id).await?;
                        s.sessions.touch(&fork).await?;
                        let back = s
                            .actions
                            .create(
                                k::SESSION_SWITCH,
                                &session,
                                json!({"notice": true}),
                                7 * 24 * 3_600_000,
                                false,
                            )
                            .await?;
                        let screen = screens::Screen {
                            text: format!(
                                "🍴 Session dupliquée ({} messages) : la suite se passe dans \
                                 `{fork}`.{}",
                                shown(&v["messages"]),
                                background_note(background)
                            ),
                            rows: vec![vec![ButtonSpec::callback(
                                "↪️ Revenir à l'original",
                                &back.token,
                                "",
                            )]],
                        };
                        return self
                            .send_screen(chat_id, topic_id, reply_to, screen, None)
                            .await;
                    }
                    Err(e) => format!("❌ {e}"),
                }
            }
            "rewind" if args.is_empty() => {
                return self
                    .show_screen(chat_id, topic_id, reply_to, "rewind", &json!({}), None)
                    .await;
            }
            "rewind" => {
                let session = d.chat_session_for(&origin).await?;
                let turns = args.trim().parse::<usize>().unwrap_or(1);
                match crate::session_ops::rewind(d, &session, turns).await {
                    Ok(v) => format!(
                        "⏪ {turns} échange(s) défait(s) ({} messages mis de côté dans `{}`).",
                        shown(&v["removed"]),
                        v["archive"].as_str().unwrap_or("?")
                    ),
                    Err(e) => format!("❌ {e}"),
                }
            }
            "export" => {
                let session = if args.is_empty() {
                    d.chat_session_for(&origin).await?
                } else {
                    args.to_string()
                };
                // Écriture puis téléversement : détachés, la boucle des updates continue
                // de lire `/stop` et les boutons (issue #69).
                let (me, d2) = (self.clone(), d.clone());
                tokio::spawn(async move {
                    let note = match crate::session_ops::export(&d2, "session", Some(&session))
                        .await
                    {
                        Ok(v) => {
                            let path =
                                std::path::PathBuf::from(v["path"].as_str().unwrap_or_default());
                            match me
                                .bot
                                .send_document(chat_id, topic_id, &path, Some("Export JSONL"))
                                .await
                            {
                                Ok(_) => return,
                                Err(e) => format!(
                                    "📦 Export écrit dans `{}`, envoi impossible : {e}",
                                    path.display()
                                ),
                            }
                        }
                        Err(e) => format!("❌ {e}"),
                    };
                    let _ = me.reply(chat_id, topic_id, reply_to, &note).await;
                });
                return Ok(());
            }
            "upgrade" => {
                // Installer ou revenir en arrière : toujours confirmé (issue #30).
                // Installation depuis les sources : la carte de bascule (issue #33).
                if args == "install"
                    && crate::upgrade::running_binary()
                        .is_ok_and(|b| crate::upgrade::is_source_build(&b))
                {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "upgrade.switch",
                            &json!({}),
                            None,
                        )
                        .await;
                }
                let confirm = match args {
                    "install" => Some((
                        "upgrade.install",
                        json!({}),
                        "Installer la dernière version publiée puis redémarrer ?".to_string(),
                    )),
                    "rollback" => Some((
                        "upgrade.rollback",
                        json!({}),
                        "Revenir au binaire précédent puis redémarrer ?".to_string(),
                    )),
                    tag if tag.starts_with('v') && !tag.contains(char::is_whitespace) => Some((
                        "upgrade.install",
                        json!({"tag": tag}),
                        format!("Installer {tag} puis redémarrer ?"),
                    )),
                    _ => None,
                };
                let args = match confirm {
                    Some((op, params, question)) => json!({
                        "op": op, "params": params, "question": question,
                        "back": {"screen": "upgrade", "args": {}},
                    }),
                    None => {
                        // L'écran s'affiche tout de suite ; la vérification suit en fond.
                        let daemon = d.clone();
                        tokio::spawn(async move {
                            let rpc = crate::rpc::Rpc::new(daemon.clone());
                            if let Ok(v) = rpc.call(m::UPGRADE, json!({"check": true})).await {
                                let cached =
                                    json!({"latest": v["latest"], "up_to_date": v["up_to_date"]});
                                let _ = daemon
                                    .kv_set("tg.upgrade.last_check", &cached.to_string())
                                    .await;
                            }
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "upgrade", &json!({}), None)
                            .await;
                    }
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                    .await;
            }
            "stop" => {
                // Arrêter, c'est aussi vider la file : sinon le flot reprend aussitôt
                // (issue #49).
                let session = d.chat_session_for(&origin).await?;
                let tout = matches!(args.trim(), "tout" | "all");
                let burst = self
                    .bursts
                    .lock()
                    .ok()
                    .and_then(|mut g| g.remove(&chat_id))
                    .map(|b| b.parts.len())
                    .unwrap_or(0);
                let running = d.bus.cancel_session(&session);
                // Ingestions en cours de cette session : comptées pour toute forme de
                // `/stop`, annulées par `/stop tout` (issue #155).
                let mut ingests = d.bus.ingests_of(&session);
                let mut cancelled_ingests = if tout {
                    d.bus.cancel_ingests(&session)
                } else {
                    0
                };
                let mut queued = crate::session_ops::silence(d, &session, "arrêt demandé").await?;
                let mut sessions = 0;
                let mut runs = 0;
                // Les runs ouverts de ce chat, quel que soit leur état : un run `blocked`
                // paraît « en cours » au propriétaire, et c'est précisément celui que
                // `/stop tout` ignorait en répondant « Rien à arrêter » (issue #155).
                let open: Vec<penelope_workflow::runs::Run> = s
                    .runs
                    .list(None, 50)
                    .await?
                    .into_iter()
                    .filter(|r| {
                        matches!(
                            r.state,
                            penelope_workflow::RunState::Running
                                | penelope_workflow::RunState::Blocked
                                | penelope_workflow::RunState::Paused
                        )
                    })
                    .collect();
                let mut left: Vec<String> = Vec::new();
                if tout {
                    // Sessions du chat, **et** sous-agents dont le parent est dans ce chat :
                    // une session de sous-agent n'a pas de chat Telegram, elle était sautée.
                    let all = s.sessions.list(None, 200).await?;
                    let here: std::collections::HashSet<String> = all
                        .iter()
                        .filter(|x| x.tg_chat_id == Some(chat_id))
                        .map(|x| x.id.to_string())
                        .collect();
                    for other in &all {
                        let id = other.id.to_string();
                        if id == session {
                            continue;
                        }
                        let mine = other.tg_chat_id == Some(chat_id)
                            || other.parent_id.as_ref().is_some_and(|p| here.contains(p));
                        if !mine {
                            continue;
                        }
                        d.bus.cancel_session(&id);
                        ingests += d.bus.ingests_of(&id);
                        cancelled_ingests += d.bus.cancel_ingests(&id);
                        let n = crate::session_ops::silence(d, &id, "arrêt demandé").await?;
                        if n > 0 || d.bus.is_active(&id) {
                            sessions += 1;
                        }
                        queued += n;
                    }
                    for run in &open {
                        if run.state == penelope_workflow::RunState::Running {
                            if crate::workflow::control(
                                d,
                                &run.id,
                                &penelope_workflow::Control::Pause,
                            )
                            .await
                            .is_ok()
                            {
                                runs += 1;
                            }
                        } else {
                            // Ni mis en pause (il l'est déjà ou il attend), ni annulé à la
                            // place du propriétaire : nommé, avec de quoi décider.
                            left.push(format!("`{}` ({})", run.id, run.state.as_str()));
                        }
                    }
                }
                let has_left = !left.is_empty();
                let note = StopReport {
                    running,
                    queued,
                    burst,
                    sessions,
                    paused: runs,
                    left,
                    open: open
                        .iter()
                        .map(|r| (r.id.clone(), r.state.as_str().to_string()))
                        .collect(),
                    ingests,
                    cancelled_ingests,
                    tout,
                }
                .render();
                // Des runs sont restés ouverts : l'écran `runs` porte un bouton par run
                // (⏸ ▶️ ⏹, l'arrêt sous confirmation). « Laisser », c'est ne pas cliquer.
                // Le propriétaire décide, la commande ne décide pas pour lui (issue #155).
                if tout && has_left {
                    let _ = self.reply(chat_id, topic_id, reply_to, &note).await;
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "runs", &json!({}), None)
                        .await;
                }
                note
            }
            "switch" => {
                if args.is_empty() {
                    return self
                        .send_sessions_menu(chat_id, topic_id, 0, false, None)
                        .await;
                } else {
                    match crate::session_ops::resolve(s, args).await {
                        Err(e) => format!("❌ {e}"),
                        Ok(sess) => {
                            let id = sess.id.to_string();
                            if sess.state != "active" {
                                s.sessions.set_state(&id, "active").await?;
                            }
                            let background = self.bind_chat(&id, chat_id, topic_id).await?;
                            s.sessions.touch(&id).await?;
                            format!(
                                "↪️ Session « {} » reprise (`{id}`).{}",
                                crate::titles::label(&sess),
                                background_note(background)
                            )
                        }
                    }
                }
            }
            "close" => {
                let target = if args.is_empty() {
                    s.sessions
                        .find_by_topic(chat_id, topic_id)
                        .await?
                        .map(|x| x.id.to_string())
                        .ok_or_else(|| "aucune session liée à ce chat".to_string())
                } else {
                    crate::session_ops::resolve(s, args)
                        .await
                        .map(|x| x.id.to_string())
                };
                match target {
                    Err(e) => format!("❌ {e}"),
                    Ok(id) => {
                        let label = match s.sessions.get(&id).await? {
                            Some(sess) => format!("« {} »", crate::titles::label(&sess)),
                            None => format!("`{id}`"),
                        };
                        let args = json!({
                            "op": "session.close", "params": {"session": id},
                            "question": format!(
                                "Fermer la session {label} ? Sa file d'attente est vidée."
                            ),
                            "back": null,
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                            .await;
                    }
                }
            }
            "purge" => {
                let target = if args.is_empty() {
                    s.sessions
                        .find_by_topic(chat_id, topic_id)
                        .await?
                        .map(|x| x.id.to_string())
                        .ok_or_else(|| "aucune session liée à ce chat".to_string())
                } else {
                    crate::session_ops::resolve(s, args)
                        .await
                        .map(|x| x.id.to_string())
                };
                match target {
                    Err(e) => format!("❌ {e}"),
                    Ok(id) => {
                        let label = match s.sessions.get(&id).await? {
                            Some(sess) => format!("« {} »", crate::titles::label(&sess)),
                            None => format!("`{id}`"),
                        };
                        let args = json!({
                            "op": "session.purge",
                            "params": {"session": id, "reason": "demande du propriétaire"},
                            "question": format!(
                                "Effacer le contenu de la session {label} ? Messages, résumés, \
                                 artefacts et requêtes partent définitivement ; la chaîne \
                                 d'audit garde ses lignes, sans leur contenu."
                            ),
                            "back": null,
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                            .await;
                    }
                }
            }
            "model" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let session = d.chat_session_for(&origin).await?;
                match parts.as_slice() {
                    // Sans argument : l'état de la session et un bouton par modèle.
                    [] => {
                        return self
                            .send_model_menu(chat_id, topic_id, reply_to, &session)
                            .await;
                    }
                    ["auto", switch] => {
                        let on = match *switch {
                            "on" | "oui" => Some(true),
                            "off" | "non" => Some(false),
                            _ => None,
                        };
                        match on {
                            None => "Usage : `/model auto on` ou `/model auto off`".into(),
                            Some(on) => {
                                rpc.call(
                                    m::CONFIG_SET,
                                    json!({"path": "models.routing.classifier", "value": on}),
                                )
                                .await?;
                                if on {
                                    "🔀 Routage adaptatif activé : le classifieur choisit l'alias à chaque message.".into()
                                } else {
                                    "📌 Routage fixe : les sessions non épinglées passent par `main`.".into()
                                }
                            }
                        }
                    }
                    // Connexion d'un fournisseur à compte (#142) : le code s'affiche
                    // ici, et Pénélope confirme dès qu'il est saisi. Ni le code ni les
                    // jetons ne passent par une carte ni par une demande (#134).
                    ["auth", rest @ ..] => {
                        let provider = rest
                            .iter()
                            .find(|w| !w.starts_with('-') && **w != "status" && **w != "logout")
                            .copied()
                            .unwrap_or("codex")
                            .to_string();
                        let action = if rest.iter().any(|w| w.trim_start_matches('-') == "logout") {
                            "logout"
                        } else if rest.iter().any(|w| w.trim_start_matches('-') == "status") {
                            "status"
                        } else {
                            "start"
                        };
                        let params = json!({"provider": provider, "action": action});
                        match (action, rpc.call(m::MODEL_AUTH, params).await) {
                            (_, Err(e)) => format!("❌ {e}"),
                            ("logout", Ok(_)) => {
                                format!("🔌 `{provider}` déconnecté : jeton révoqué et oublié.")
                            }
                            ("status", Ok(v)) => codex_status_text(&v),
                            (_, Ok(v)) => {
                                // L'attente se fait en fond : le tour Telegram ne reste
                                // pas suspendu un quart d'heure.
                                let g = self.clone();
                                let daemon = d.clone();
                                tokio::spawn(async move {
                                    let text = match crate::rpc::Rpc::new(daemon)
                                        .call(
                                            m::MODEL_AUTH,
                                            json!({"provider": "codex", "action": "wait"}),
                                        )
                                        .await
                                    {
                                        Ok(v) => format!(
                                            "✅ Connecté : plan {}, compte {}.\nDonner un \
                                             alias : `/model code codex:gpt-6-astra`",
                                            shown(&v["plan"]),
                                            shown(&v["account"])
                                        ),
                                        Err(e) => format!("❌ Connexion abandonnée : {e}"),
                                    };
                                    if let Err(e) = g.reply(chat_id, topic_id, None, &text).await {
                                        tracing::warn!(error = %e, "confirmation de connexion non envoyée");
                                    }
                                });
                                format!(
                                    "🔐 Ouvrir {}\net saisir le code : `{}`\n\nJe confirme ici \
                                     dès que c'est validé (quinze minutes).",
                                    shown(&v["url"]),
                                    shown(&v["user_code"])
                                )
                            }
                        }
                    }
                    // Un alias seul : l'épingler sur la session, `auto` pour revenir.
                    [alias] => match rpc
                        .call(
                            m::SESSION_MODEL,
                            json!({"session": session, "alias": alias}),
                        )
                        .await
                    {
                        Ok(v) => model_pin_notice(&v),
                        Err(e) => format!("❌ {e}"),
                    },
                    // Un alias et un modèle : changer ce que vise l'alias, partout.
                    [alias, model, ..] => {
                        let model = normalise_model_id(model);
                        match rpc
                            .call(m::MODEL_SET, json!({"alias": alias, "model": model}))
                            .await
                        {
                            Ok(v) => format!(
                                "✅ `{alias}` → `{model}` (génération {}).",
                                shown(&v["generation"])
                            ),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                }
            }
            // Foyer du propriétaire (issue #143) : ce qui n'appartient à aucune session
            // — alertes de budget, rappels, digest du rêve, cartes OAuth — arrive ici
            // plutôt que dans un chat privé que plus personne ne lit.
            "home" | "foyer" => {
                let reset = matches!(args.trim(), "off" | "non" | "privé" | "prive");
                let (chat, topic) = if reset {
                    (0, 0)
                } else {
                    (chat_id, topic_id.unwrap_or(0))
                };
                match rpc
                    .call(
                        m::CONFIG_SET,
                        json!({"path": "telegram.home", "value": {"chat": chat, "topic": topic}}),
                    )
                    .await
                {
                    Err(e) => format!("❌ {e}"),
                    Ok(_) if reset => "🏠 Foyer effacé : les avis sans session repartent \
                                       dans le chat privé."
                        .into(),
                    Ok(_) => format!(
                        "🏠 Foyer réglé sur ce {}. Les avis sans session (budget, rappels, \
                         digest, cartes MCP) arriveront ici. `/home off` pour revenir au \
                         chat privé.",
                        if topic_id.is_some() { "sujet" } else { "chat" }
                    ),
                }
            }
            "models" => {
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "models",
                        &json!({"filter": args}),
                        None,
                    )
                    .await;
            }
            // Sujet de travail de la session (issue #119) : sans argument, l'état et un
            // bouton par projet connu.
            "projet" => {
                let session = d.chat_session_for(&origin).await?;
                let wanted = args.trim();
                match rpc
                    .call(
                        m::SESSION_PROJECT,
                        json!({"session": session, "project": wanted}),
                    )
                    .await
                {
                    Err(e) => format!("❌ {e}"),
                    Ok(v) if !wanted.is_empty() => match v["project"].as_str() {
                        Some(p) => format!(
                            "📁 Sujet de la session : **{p}**. La mémoire d'office s'y limite dès \
                             le prochain message ; le reste reste au rappel."
                        ),
                        None => "📁 Session sans sujet : seules les entrées sans projet sont \
                                 injectées d'office."
                            .into(),
                    },
                    Ok(v) => {
                        let current = v["project"].as_str();
                        let mut rows = Vec::new();
                        for p in v["known"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .iter()
                            .take(12)
                        {
                            let p = p.as_str().unwrap_or_default();
                            let mark = if Some(p) == current { "✅ " } else { "" };
                            rows.push(vec![
                                self.command_button_with(&format!("{mark}{p}"), "projet", p)
                                    .await?,
                            ]);
                        }
                        rows.push(vec![
                            self.command_button_with(
                                if current.is_none() {
                                    "✅ Aucun"
                                } else {
                                    "Aucun"
                                },
                                "projet",
                                "aucun",
                            )
                            .await?,
                        ]);
                        let state = match (current, v["how"].as_str()) {
                            (Some(p), Some("explicite")) => format!("**{p}** (choisi)"),
                            (Some(p), Some(how)) => format!("**{p}** (déduit du {how})"),
                            (Some(p), None) => format!("**{p}**"),
                            (None, Some(_)) => "aucun (choisi)".into(),
                            (None, None) => "aucun pour l'instant".into(),
                        };
                        let text = format!(
                            "📁 Sujet de la session : {state}.\n\nLe profil et les entrées sans \
                             projet sont toujours là ; celles d'un projet n'entrent d'office que \
                             dans une session de ce projet. Les autres restent au rappel et à \
                             `mem_search`."
                        );
                        let mut payload = json!({
                            "chat_id": chat_id,
                            "text": markdown_to_html(&text),
                            "parse_mode": "HTML",
                            "reply_markup": inline_keyboard(&rows),
                            "message_thread_id": topic_id,
                        });
                        if let Some(r) = reply_to {
                            payload["reply_parameters"] =
                                json!({"message_id": r, "allow_sending_without_reply": true});
                        }
                        return self
                            .outbox_push(chat_id, topic_id, "sendMessage", payload)
                            .await;
                    }
                }
            }
            // Mode d'approbation de la session (issue #111) : sans argument, l'état et un
            // bouton par mode.
            "mode" => {
                let session = d.chat_session_for(&origin).await?;
                let wanted = args.trim();
                let v = rpc
                    .call(m::SESSION_MODE, json!({"session": session, "mode": wanted}))
                    .await;
                match v {
                    Err(e) => format!("❌ {e}"),
                    Ok(v) if !wanted.is_empty() => {
                        format!(
                            "🛡 Mode de la session : **{}**.",
                            v["label"].as_str().unwrap_or("?")
                        )
                    }
                    Ok(v) => {
                        let current = v["mode"].as_str().unwrap_or("reads");
                        let mut rows = Vec::new();
                        for (mode, label) in [
                            ("ask", "Demander tout"),
                            ("reads", "Lectures sans demande"),
                            ("auto", "Tout sauf le destructif"),
                        ] {
                            let mark = if mode == current { "✅ " } else { "" };
                            rows.push(vec![
                                self.command_button_with(&format!("{mark}{label}"), "mode", mode)
                                    .await?,
                            ]);
                        }
                        let text = format!(
                            "🛡 Mode de la session : **{}**.\n\nDemander tout : même une lecture \
                             du shell attend ton accord. Lectures sans demande (défaut) : `ls`, \
                             `cat`, `grep`, `git status` passent, le reste selon tes règles. \
                             Tout sauf le destructif : plus de demande, sauf suppression et \
                             réglages sensibles.",
                            v["label"].as_str().unwrap_or("?")
                        );
                        let mut payload = json!({
                            "chat_id": chat_id,
                            "text": markdown_to_html(&text),
                            "parse_mode": "HTML",
                            "reply_markup": inline_keyboard(&rows),
                            "message_thread_id": topic_id,
                        });
                        if let Some(r) = reply_to {
                            payload["reply_parameters"] =
                                json!({"message_id": r, "allow_sending_without_reply": true});
                        }
                        return self
                            .outbox_push(chat_id, topic_id, "sendMessage", payload)
                            .await;
                    }
                }
            }
            "schedules" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let by_id = |method: &'static str, id: &str| rpc.call(method, json!({"id": id}));
                match parts.as_slice() {
                    ["rm", id] => {
                        let args = json!({
                            "op": "schedule.rm", "params": {"id": id},
                            "question": format!("Supprimer le déclencheur `{id}` ?"),
                            "back": {"screen": "schedules", "args": {}},
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &args, None)
                            .await;
                    }
                    // Livrer ici, dans cette conversation et ce sujet (#124).
                    ["ici" | "here", id] => {
                        match crate::scheduler::retarget(
                            &self.daemon.services,
                            id,
                            chat_id,
                            topic_id,
                        )
                        .await
                        {
                            Ok(to) => format!("📍 `{id}` livrera désormais ici : {to}."),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    [op @ ("pause" | "resume" | "run"), id] => {
                        let method = match *op {
                            "pause" => m::SCHEDULE_PAUSE,
                            "resume" => m::SCHEDULE_RESUME,
                            "rm" => m::SCHEDULE_RM,
                            _ => m::SCHEDULE_RUN_NOW,
                        };
                        match by_id(method, id).await {
                            Ok(_) => match *op {
                                "pause" => format!("⏸ `{id}` en pause."),
                                "resume" => format!("▶️ `{id}` repris."),
                                "rm" => format!("🗑 `{id}` supprimé."),
                                _ => format!("⚡ `{id}` déclenché."),
                            },
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    _ => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "schedules", &json!({}), None)
                            .await;
                    }
                }
            }
            "mcp" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let call =
                    |method: &'static str, name: &str| rpc.call(method, json!({"name": name}));
                match parts.as_slice() {
                    [] => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "mcp", &json!({}), None)
                            .await;
                    }
                    ["auth", name] => {
                        return self.send_oauth_card(chat_id, topic_id, name).await;
                    }
                    ["restart", name] => match call(m::MCP_RESTART, name).await {
                        Ok(v) => format!(
                            "🔄 `{name}` redémarré : {} outil(s), état {}.",
                            shown(&v["tool_count"]),
                            v["state"].as_str().unwrap_or("?")
                        ),
                        Err(e) => format!("❌ {e}"),
                    },
                    ["logs", name] => match call(m::MCP_LOGS, name).await {
                        Ok(v) => {
                            let lines: Vec<String> = v["lines"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                                .iter()
                                .rev()
                                .take(30)
                                .rev()
                                .filter_map(|l| l.as_str().map(String::from))
                                .collect();
                            if lines.is_empty() {
                                format!("Aucune ligne de journal pour `{name}`.")
                            } else {
                                format!("```\n{}\n```", lines.join("\n").replace("```", "ʼʼʼ"))
                            }
                        }
                        Err(e) => format!("❌ {e}"),
                    },
                    ["test", name] => match call(m::MCP_TEST, name).await {
                        Ok(v) if v["ok"].as_bool() == Some(true) => format!(
                            "✅ `{name}` répond : protocole {}, {} outil(s){}, {} ms.",
                            v["protocol"].as_str().unwrap_or("?"),
                            shown(&v["tools"]),
                            match v["call"]["tool"].as_str() {
                                Some(t) => format!(", appel de `{t}` réussi"),
                                None => ", aucun outil en lecture sans argument à essayer".into(),
                            },
                            shown(&v["ms"])
                        ),
                        Ok(v) => {
                            format!("❌ `{name}` : {}", v["error"].as_str().unwrap_or("échec"))
                        }
                        Err(e) => format!("❌ {e}"),
                    },
                    [op @ ("enable" | "disable"), name] => {
                        let method = if *op == "enable" {
                            m::MCP_ENABLE
                        } else {
                            m::MCP_DISABLE
                        };
                        match call(method, name).await {
                            Ok(_) if *op == "enable" => format!("▶️ `{name}` activé."),
                            Ok(_) => format!("⏸ `{name}` désactivé."),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    [name] => {
                        return self
                            .show_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                "mcp.server",
                                &json!({"name": name}),
                                None,
                            )
                            .await;
                    }
                    _ => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "mcp", &json!({}), None)
                            .await;
                    }
                }
            }
            "budget" => {
                let session = d.chat_session_for(&origin).await?;
                match args.split_whitespace().collect::<Vec<_>>().as_slice() {
                    // Plafond propre à la session de travail (issue #32).
                    ["session", amount] => {
                        let usd = match *amount {
                            "off" | "défaut" | "defaut" | "0" => None,
                            raw => match raw.trim_end_matches('$').replace(',', ".").parse::<f64>()
                            {
                                Ok(v) if v > 0.0 => Some(v),
                                _ => {
                                    return self
                                        .send_screen(
                                            chat_id,
                                            topic_id,
                                            reply_to,
                                            Self::typed_screen(
                                                &format!("Montant illisible : `{raw}`. Un nombre de dollars, ou `off`."),
                                                "✏️ Écrire le plafond",
                                                "/budget session ",
                                            ),
                                            None,
                                        )
                                        .await;
                                }
                            },
                        };
                        s.sessions.set_budget(&session, usd).await?;
                        let cfg = s.config.config();
                        let (daily, limit, _) =
                            s.budget.limits(&cfg.budget, Some(&session), None).await?;
                        format!(
                            "💰 Plafond de cette session : {} $ ({}). Le jour reste plafonné à {} $.",
                            fmt_usd(limit),
                            if usd.is_some() {
                                "propre à la session"
                            } else {
                                "celui de la configuration"
                            },
                            fmt_usd(daily)
                        )
                    }
                    ["session"] => {
                        return self
                            .send_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                Self::typed_screen(
                                    "💰 **Plafond de la session** : `/budget session` suivi d'un montant en dollars (`off` pour revenir à la configuration).",
                                    "✏️ Écrire le plafond",
                                    "/budget session ",
                                ),
                                None,
                            )
                            .await;
                    }
                    _ => self.budget_text(&session, args).await?,
                }
            }
            "usage" => {
                let session = d.chat_session_for(&origin).await?;
                self.usage_text(&session, args).await?
            }
            "audit" => {
                // L'audit relit toute la mémoire : détaché (issue #69).
                let (me, d2) = (self.clone(), d.clone());
                tokio::spawn(async move {
                    let note = match crate::mem_audit::run(&d2).await {
                        Ok(a) => crate::mem_audit::summary(&a),
                        Err(e) => format!("❌ {e}"),
                    };
                    let _ = me.reply(chat_id, topic_id, reply_to, &note).await;
                });
                return Ok(());
            }
            "accueil" => {
                let part = crate::onboarding::Part::parse(args);
                if !args.is_empty() && part.is_none() {
                    "Partie inconnue : profil, outils, style ou limites.".to_string()
                } else {
                    return self.onboarding_next(chat_id, topic_id, part).await;
                }
            }
            "dream" => {
                // Une passe peut prendre une minute : le bilan arrive quand il est prêt.
                self.react(chat_id, message_id, reaction::RECEIVED);
                let (daemon, messenger) = (d.clone(), d.hooks.messenger());
                let dry_run = args.contains("dry");
                tokio::spawn(async move {
                    let text = match crate::dream::run(&daemon, dry_run).await {
                        Ok(o) => format!(
                            "🌙 {}{}",
                            if o.dry_run { "(à blanc) " } else { "" },
                            o.report.render()
                        ),
                        Err(e) => format!("❌ {e}"),
                    };
                    if let Some(m) = messenger {
                        let _ = m.send_text(&origin, &text).await;
                    }
                });
                return Ok(());
            }
            "appris" => {
                let days = args.trim().parse::<i64>().unwrap_or(7);
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "learned",
                        &json!({"days": days}),
                        None,
                    )
                    .await;
            }
            "pratique" => {
                let (screen, screen_args) = if args.is_empty() {
                    ("practices", json!({}))
                } else {
                    (
                        "practice",
                        json!({"slug": penelope_platform::slugify(args)}),
                    )
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, screen, &screen_args, None)
                    .await;
            }
            "retiens" => {
                if args.is_empty() {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "🧠 **Retenir** : `/retiens` suivi de ce qu'il faut garder en \
                                 mémoire de fond (une ligne, sans secret).",
                                "✏️ Écrire",
                                "/retiens ",
                            ),
                            None,
                        )
                        .await;
                } else {
                    let vault = crate::conversation::vault_dir(s);
                    let session = d.chat_session_for(&origin).await?;
                    match crate::vault_ops::remember(
                        s,
                        &vault,
                        penelope_memory::Level::Coeur,
                        args,
                        &session,
                    )
                    .await
                    {
                        Ok(uid) => format!("🧠 Retenu (`{uid}`)."),
                        Err(e) => format!("❌ {e}"),
                    }
                }
            }
            "oublie" => {
                if !args.is_empty()
                    && let Some(e) = s.memory.get(args).await?
                {
                    let confirm = json!({
                        "op": "mem.forget", "params": {"uid": args},
                        "question": format!("Oublier « {} » ?", e.text),
                        "back": {"screen": "forget", "args": {}},
                    });
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                        .await;
                }
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "forget",
                        &json!({"query": args}),
                        None,
                    )
                    .await;
            }
            "forget" => {
                if args.is_empty() {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "forget.sessions",
                            &json!({}),
                            None,
                        )
                        .await;
                }
                match crate::session_ops::resolve(s, args).await {
                    Err(e) => format!("❌ {e}"),
                    Ok(sess) => {
                        let confirm = json!({
                            "op": "session.forget", "params": {"session": sess.id.to_string()},
                            "question": format!(
                                "Oublier tout ce que la mémoire a retenu de la session « {} » ?",
                                crate::titles::label(&sess)
                            ),
                            "back": {"screen": "forget.sessions", "args": {}},
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                            .await;
                    }
                }
            }
            "secret" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                match parts.first().copied() {
                    None | Some("list") => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "secrets", &json!({}), None)
                            .await;
                    }
                    Some("rm") if parts.len() > 1 => {
                        let confirm = json!({
                            "op": "secret.rm", "params": {"name": parts[1]},
                            "question": format!("Supprimer le secret `{}` ?", parts[1]),
                            "back": {"screen": "secrets", "args": {}},
                        });
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                            .await;
                    }
                    _ => "Un secret ne se saisit **jamais** dans une conversation. En SSH : \
                          `penelope secret set <nom>` puis coller la valeur à l'invite"
                        .into(),
                }
            }
            "approvals" => {
                let pending = s.approvals.pending(20).await?;
                if pending.is_empty() {
                    "Aucune demande en attente.".into()
                } else {
                    for a in &pending {
                        self.send_approval_card(chat_id, topic_id, a).await?;
                    }
                    format!("{} demande(s) en attente.", pending.len())
                }
            }
            "recall" => {
                if args.is_empty() {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "🔎 **Rechercher en mémoire** : `/recall` suivi des mots-clés.",
                                "🔎 Chercher",
                                "/recall ",
                            ),
                            None,
                        )
                        .await;
                }
                let hits = rpc.call(m::MEM_SEARCH, json!({"query": args})).await?;
                let hits = hits.as_array().cloned().unwrap_or_default();
                if hits.is_empty() {
                    format!("🔎 Rien en mémoire pour « {args} ».")
                } else {
                    let mut t = format!(
                        "🔎 **Mémoire** : {} résultat(s) pour « {args} »\n",
                        hits.len()
                    );
                    for h in hits.iter().take(10) {
                        t.push_str(&format!(
                            "\n- {} _({})_",
                            h["text"].as_str().unwrap_or_default(),
                            h["file"].as_str().unwrap_or("?")
                        ));
                    }
                    t
                }
            }
            "run" => {
                let mut words = args.splitn(2, char::is_whitespace);
                match words.next().filter(|w| !w.is_empty()) {
                    None => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "wf", &json!({}), None)
                            .await;
                    }
                    Some(id) => {
                        let rest = words.next().unwrap_or_default().trim();
                        // Le plan précède toute exécution, même avec paramètres explicites.
                        if let Some(w) = s.workflows.get(id) {
                            return self
                                .run_by_conversation(chat_id, topic_id, message_id, &w, rest)
                                .await;
                        }
                        format!("❌ Workflow `{id}` introuvable.")
                    }
                }
            }
            "resume" => {
                if args.is_empty() {
                    return self
                        .show_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            "runs",
                            &json!({"filter": "stuck"}),
                            None,
                        )
                        .await;
                }
                match crate::workflow::control(d, args, &penelope_workflow::Control::Resume).await {
                    Ok(state) => format!("▶️ Run `{args}` : {}.", state.as_str()),
                    Err(e) => format!("❌ {e}"),
                }
            }
            "wf" | "runs" | "skills" | "intentions" | "policies" | "status" | "doctor"
            | "config" => {
                let screen = match command {
                    "wf" if !args.is_empty() => {
                        return self
                            .show_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                "wf.detail",
                                &json!({"id": args}),
                                None,
                            )
                            .await;
                    }
                    other => other,
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, screen, &json!({}), None)
                    .await;
            }
            "skill" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let (screen, screen_args) = match parts.as_slice() {
                    [] => ("skills", json!({})),
                    ["rollback", name] => (
                        "confirm",
                        json!({
                            "op": "skill.rollback", "params": {"name": name},
                            "question": format!("Revenir à la version précédente de `{name}` ?"),
                            "back": {"screen": "skills", "args": {}},
                        }),
                    ),
                    [name, ..] => ("skill", json!({"name": name})),
                };
                return self
                    .show_screen(chat_id, topic_id, reply_to, screen, &screen_args, None)
                    .await;
            }
            "logs" => {
                let component = args.split_whitespace().next().unwrap_or_default();
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "logs",
                        &json!({"component": component}),
                        None,
                    )
                    .await;
            }
            "restart" => {
                let confirm = json!({
                    "op": "restart", "params": {},
                    "question": "Redémarrer le daemon ? Les tours en cours reprennent au redémarrage.",
                    "back": null,
                });
                return self
                    .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                    .await;
            }
            "note" => {
                if args.is_empty() {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "📝 **Noter** : `/note` suivi du texte, rangé dans le journal du jour.",
                                "✏️ Écrire",
                                "/note ",
                            ),
                            None,
                        )
                        .await;
                }
                let vault = crate::conversation::vault_dir(s);
                let session = d.chat_session_for(&origin).await?;
                match crate::vault_ops::remember(
                    s,
                    &vault,
                    penelope_memory::Level::Episodic,
                    args,
                    &session,
                )
                .await
                {
                    Ok(_) => "📝 Noté dans le journal du jour.".into(),
                    Err(e) => format!("❌ {e}"),
                }
            }
            "mien" => "📄 Envoie un document avec la légende `/mien` : il est ingéré comme rédigé \
                       par toi, donc fiable et rappelable. Sans légende, un document reçu reste \
                       une source non fiable."
                .into(),
            "p" => {
                let mut words = args.splitn(3, char::is_whitespace);
                match (words.next().filter(|w| !w.is_empty()), words.next()) {
                    (None, _) => {
                        return self
                            .show_screen(chat_id, topic_id, reply_to, "prompts", &json!({}), None)
                            .await;
                    }
                    (Some(server), None) => {
                        return self
                            .show_screen(
                                chat_id,
                                topic_id,
                                reply_to,
                                "prompts",
                                &json!({"server": server}),
                                None,
                            )
                            .await;
                    }
                    (Some(server), Some(prompt)) => {
                        let params = parse_params(words.next().unwrap_or_default());
                        match self
                            .run_mcp_prompt(chat_id, topic_id, server, prompt, params)
                            .await
                        {
                            Ok(n) => format!(
                                "💬 Prompt `{prompt}` de `{server}` : {n} message(s) envoyé(s) au modèle."
                            ),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                }
            }
            "quiet" => {
                if args.is_empty() {
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "quiet", &json!({}), None)
                        .await;
                }
                let range = if matches!(args, "off" | "non" | "aucune") {
                    ""
                } else {
                    args
                };
                match rpc.call(m::QUIET, json!({"range": range})).await {
                    Ok(_) if range.is_empty() => "🔔 Heures silencieuses désactivées.".into(),
                    Ok(_) => format!("🌙 Heures silencieuses : {range}."),
                    Err(e) => format!("❌ {e}"),
                }
            }
            other => format!("Commande inconnue : `/{other}`. Voir `/help`."),
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// Réponse suivie d'un bouton par suite proposée ; un clic envoie la suite comme message
    /// du propriétaire dans la session (issue #31).
    /// `/run <workflow>` sans paramètres : la demande part au modèle, qui complète les
    /// paramètres avec ses outils et propose le plan avant le gate (issues #35, #186).
    pub(super) async fn run_by_conversation(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        w: &penelope_workflow::Workflow,
        provided: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let id = &w.metadata.id;
        let title = if w.metadata.name.is_empty() {
            id.clone()
        } else {
            w.metadata.name.clone()
        };
        let describe = |required: bool| {
            w.metadata
                .parameters
                .iter()
                .filter(|p| p.required == required)
                .map(|p| format!("{} ({})", p.id, p.label))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (required, optional) = (describe(true), describe(false));
        let mut ask = format!(
            "/run {id}\n\n[Je veux lancer le workflow `{id}` (« {title} ») sans avoir donné ses \
             paramètres."
        );
        if !required.is_empty() {
            ask.push_str(&format!(" Requis : {required}."));
        }
        if !optional.is_empty() {
            ask.push_str(&format!(" Facultatifs : {optional}."));
        }
        if !provided.is_empty() {
            ask.push_str(&format!(
                " Paramètres fournis par le propriétaire : {provided}."
            ));
        }
        ask.push_str(
            " Complète-les avec tes outils, demande-moi seulement ce qui manque, puis propose \
             un plan structuré avec `workflow_plan` (`id`, `goal`, `steps`, `params`, `brief`) ; \
             découvre cet outil avec `tool_search` si nécessaire. \
             Montre-moi le plan pour correction ; ne lance aucun run avant « vas-y ».]",
        );
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = d.chat_session_for(&origin).await?;
        d.enqueue_message(&session, &ask, &origin, None).await?;
        self.send_screen(
            chat_id,
            topic_id,
            Some(message_id),
            screens::Screen {
                text: format!(
                    "💬 Je prépare le plan de « {title} » avec toi. Tu pourras le corriger avant « vas-y »."
                ),
                rows: vec![],
            },
            None,
        )
        .await
    }

    async fn send_choices(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        session_id: &str,
        text: &str,
        choices: &[String],
    ) -> anyhow::Result<()> {
        let mut rows = Vec::new();
        for choice in choices {
            let t = self
                .daemon
                .services
                .actions
                .create(
                    k::SAY,
                    session_id,
                    json!({"text": choice}),
                    7 * 24 * 3_600_000,
                    true,
                )
                .await?;
            rows.push(vec![ButtonSpec::callback(choice, &t.token, "")]);
        }
        let screen = screens::Screen {
            text: text.to_string(),
            rows,
        };
        self.send_screen(chat_id, topic_id, reply_to, screen, None)
            .await
    }

    /// Menu `/model` : état du modèle de la session et un bouton par choix.
    async fn send_model_menu(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        session: &str,
    ) -> anyhow::Result<()> {
        let view = self.daemon.session_model_view(session).await?;
        let (html, keyboard) = self.model_menu(&view).await?;
        let mut payload = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "reply_markup": keyboard,
            "message_thread_id": topic_id,
        });
        if let Some(r) = reply_to {
            payload["reply_parameters"] =
                json!({"message_id": r, "allow_sending_without_reply": true});
        }
        self.outbox_push(chat_id, topic_id, "sendMessage", payload)
            .await
    }

    /// Texte et boutons du menu. Les jetons sont réutilisables : on peut changer d'avis
    /// depuis le même message pendant une semaine.
    async fn model_menu(&self, view: &Value) -> anyhow::Result<(String, Value)> {
        let s = &self.daemon.services;
        let session = view["session"].as_str().unwrap_or_default();
        let pinned = view["pinned"].as_str();
        let ttl = 7 * 24 * 3_600_000;

        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for c in view["choices"].as_array().cloned().unwrap_or_default() {
            let alias = c["alias"].as_str().unwrap_or("?");
            let model = short_model(c["model"].as_str().unwrap_or("?"));
            let token = s
                .actions
                .create(k::MODEL_PIN, session, json!({"alias": alias}), ttl, false)
                .await?;
            let mark = if pinned == Some(alias) { "✅ " } else { "" };
            rows.push(vec![ButtonSpec::callback(
                &format!("{mark}{alias} · {model}"),
                &token.token,
                "",
            )]);
        }
        let auto = s
            .actions
            .create(k::MODEL_PIN, session, json!({"alias": null}), ttl, false)
            .await?;
        let mark = if pinned.is_none() { "✅ " } else { "" };
        rows.push(vec![ButtonSpec::callback(
            &format!("{mark}🔀 Automatique"),
            &auto.token,
            "",
        )]);

        let state = match pinned {
            Some(alias) => format!(
                "Épinglé sur `{alias}` · `{}` : tous les messages de la session l'utilisent.",
                short_model(view["pinned_model"].as_str().unwrap_or("?"))
            ),
            None => {
                let how = if view["classifier"].as_bool().unwrap_or(false) {
                    "le classifieur choisit à chaque message"
                } else {
                    "tout passe par `main`"
                };
                let why = view["last_boundary"]
                    .as_str()
                    .map(|b| format!(" Reclassé à une frontière : {b}."))
                    .unwrap_or_default();
                match view["last_alias"].as_str() {
                    Some(last) => format!(
                        "Automatique ({how}). Dernier message : `{last}` · `{}`.{why}",
                        short_model(view["last_model"].as_str().unwrap_or("?"))
                    ),
                    None => format!("Automatique ({how})."),
                }
            }
        };
        let markdown = format!("**Modèle de cette session**\n\n{state}");
        Ok((markdown_to_html(&markdown), inline_keyboard(&rows)))
    }

    /// Clic sur un bouton du menu `/model`.
    /// Bouton d'une question de workflow : le choix part au run, ou attend la saisie.
    async fn workflow_choice_clicked(
        &self,
        callback_id: &str,
        action: &penelope_telegram::actions::Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let run = action.target.as_str();
        let visit = action.args["visit"].as_str().unwrap_or_default();
        let choice = action.args["choice"].as_str().unwrap_or_default();
        let wants_input = action.args["input"].as_bool().unwrap_or(false);
        let _ = self
            .bot
            .answer_callback(callback_id, Some(choice), false)
            .await;
        let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        if action.args["form"].as_bool().unwrap_or(false) {
            let Some(schema) = crate::workflow::form_of(&self.daemon, run, visit).await else {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        "ℹ️ cette question n'est plus d'actualité",
                    )
                    .await;
            };
            let state = match penelope_telegram::forms::FormState::new(visit, schema) {
                Ok(st) => st,
                Err(e) => {
                    return self
                        .reply(chat_id, topic_id, None, &format!("❌ {e}"))
                        .await;
                }
            };
            let pending = json!({"run": run, "visit": visit, "choice": choice, "state": state,
                       "topic": topic_id, "since": self.daemon.services.clock.now_rfc3339()});
            self.daemon
                .kv_set(&form_key(chat_id, topic_id), &pending.to_string())
                .await?;
            return self.send_form_step(chat_id, &pending).await;
        }
        let note = if wants_input {
            self.daemon
                .kv_set(
                    &format!("tg.await_input.{chat_id}"),
                    &json!({"run": run, "visit": visit, "choice": choice}).to_string(),
                )
                .await?;
            format!("✏️ « {choice} » : précise en un message.")
        } else {
            match crate::workflow::answer(&self.daemon, run, visit, choice, None).await {
                Ok(()) => format!("✔️ « {choice} »"),
                Err(e) => format!("ℹ️ {e}"),
            }
        };
        self.reply(chat_id, topic_id, None, &note).await
    }

    async fn model_pin_clicked(
        &self,
        callback_id: &str,
        action: &penelope_telegram::actions::Action,
        chat_id: i64,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let session = action.target.as_str();
        let alias = action.args.get("alias").and_then(|a| a.as_str());
        let rpc = crate::rpc::Rpc::new(self.daemon.clone());
        let result = rpc
            .call(
                m::SESSION_MODEL,
                json!({"session": session, "alias": alias.unwrap_or("auto")}),
            )
            .await;
        match result {
            Ok(view) => {
                let toast = match alias {
                    Some(a) => format!("Session épinglée sur {a}"),
                    None => "Session en automatique".to_string(),
                };
                let _ = self
                    .bot
                    .answer_callback(callback_id, Some(&toast), false)
                    .await;
                let (html, keyboard) = self.model_menu(&view).await?;
                let _ = self
                    .bot
                    .edit_text(chat_id, message_id, &html, Some(keyboard))
                    .await;
            }
            Err(e) => {
                let _ = self
                    .bot
                    .answer_callback(callback_id, Some(&e.to_string()), true)
                    .await;
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------- rafales (#49)

    /// Met un morceau de côté le temps de la fenêtre de regroupement. Tant que la rafale
    /// n'est pas finie, un nouveau message s'y ajoute au lieu de créer un tour de plus.
    /// Fenêtre adaptative (issue #96) : seul un morceau qui ressemble à une coupure de
    /// Telegram (≥ 4 000 caractères) ou un message transféré ouvre ou prolonge une rafale ;
    /// un message court tapé part aussitôt, ou ferme la rafale ouverte après un court
    /// silence s'il en est la fin.
    ///
    /// ```text
    ///  court, rien d'ouvert ─────────────► tour tout de suite
    ///  long ou transféré ────────────────► rafale, attente de la fenêtre (2 s)
    ///  court, rafale ouverte ────────────► dernier morceau probable : 300 ms de silence
    /// ```
    async fn buffer_text(
        self: &Arc<Self>,
        origin: &Origin,
        session: &str,
        message_id: i64,
        update_id: i64,
        text: String,
        forwarded: bool,
    ) -> anyhow::Result<()> {
        let cfg = self.daemon.services.config.config();
        let window = cfg.telegram.text_group_window_ms;
        let chat_id = match origin {
            Origin::Telegram { chat_id, .. } => *chat_id,
            _ => 0,
        };
        let piece = forwarded || text.chars().count() >= TELEGRAM_TEXT_LIMIT;
        let open = self
            .bursts
            .lock()
            .map(|g| g.contains_key(&chat_id))
            .unwrap_or(false);
        if window == 0 || (!piece && !open) {
            self.daemon
                .enqueue_message(session, &text, origin, Some(format!("tg:{update_id}")))
                .await?;
            // Signe de vie dès la mise en file, avant que le tour démarre (issue #121).
            if let Some((chat, topic)) = origin.telegram_chat() {
                self.chat_action(chat, topic, "typing");
            }
            return Ok(());
        }
        let wait = if piece {
            Duration::from_millis(window)
        } else {
            TAIL_QUIET.min(Duration::from_millis(window))
        };
        let deadline = std::time::Instant::now() + wait;
        let first = {
            let mut g = self
                .bursts
                .lock()
                .map_err(|_| anyhow::anyhow!("rafales verrouillées"))?;
            match g.get_mut(&chat_id) {
                Some(b) => {
                    b.chars += text.chars().count();
                    b.parts.push(text);
                    b.message_ids.push(message_id);
                    b.deadline = deadline;
                    false
                }
                None => {
                    g.insert(
                        chat_id,
                        TextBurst {
                            origin: origin.clone(),
                            session: session.to_string(),
                            chars: text.chars().count(),
                            parts: vec![text],
                            message_ids: vec![message_id],
                            update_id,
                            deadline,
                        },
                    );
                    true
                }
            }
        };
        if first {
            let me = self.clone();
            tokio::spawn(async move { me.flush_burst(chat_id).await });
        }
        Ok(())
    }

    /// Attend la fin de la rafale, puis en fait un tour unique (ou demande quoi en faire).
    async fn flush_burst(self: Arc<Self>, chat_id: i64) {
        loop {
            let left = {
                let g = match self.bursts.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                match g.get(&chat_id) {
                    Some(b) => b
                        .deadline
                        .saturating_duration_since(std::time::Instant::now()),
                    None => return,
                }
            };
            if left.is_zero() {
                break;
            }
            tokio::time::sleep(left).await;
        }
        let burst = match self.bursts.lock() {
            Ok(mut g) => g.remove(&chat_id),
            Err(_) => None,
        };
        let Some(burst) = burst else { return };
        if let Err(e) = self.deliver_burst(burst).await {
            tracing::warn!(error = %e, "rafale Telegram non transmise");
        }
    }

    /// Un seul tour pour toute la rafale, sauf si elle dépasse les seuils : Pénélope
    /// demande alors quoi en faire avant de dépenser quoi que ce soit.
    async fn deliver_burst(self: &Arc<Self>, burst: TextBurst) -> anyhow::Result<()> {
        let cfg = self.daemon.services.config.config();
        let too_many =
            cfg.telegram.burst_messages > 0 && burst.parts.len() >= cfg.telegram.burst_messages;
        let too_long = cfg.telegram.burst_chars > 0 && burst.chars >= cfg.telegram.burst_chars;
        if too_many || too_long {
            return self.ask_about_burst(burst).await;
        }
        self.daemon
            .enqueue_message(
                &burst.session,
                &burst.joined(),
                &burst.origin,
                Some(format!("tg:{}", burst.update_id)),
            )
            .await?;
        if let Some((chat, topic)) = burst.origin.telegram_chat() {
            self.chat_action(chat, topic, "typing");
        }
        Ok(())
    }

    /// Carte de rafale : ce qui est arrivé, et quatre façons de le traiter.
    async fn ask_about_burst(&self, burst: TextBurst) -> anyhow::Result<()> {
        let (chat_id, topic_id, reply_to) = match burst.origin {
            Origin::Telegram {
                chat_id,
                topic_id,
                message_id,
            } => (chat_id, topic_id, message_id),
            _ => (0, None, None),
        };
        let id = penelope_kernel::ids::Ulid::new().to_string();
        let stored = json!({
            "session": burst.session,
            "message_id": reply_to,
            "parts": burst.parts,
            "joined": burst.joined(),
        });
        self.daemon
            .kv_set(&format!("tg.burst.{id}"), &stored.to_string())
            .await?;
        let screen = screens::Screen {
            text: format!(
                "📥 Tu m'as envoyé {} messages ({} caractères). Qu'est-ce que j'en fais ?",
                burst.parts.len(),
                burst.chars
            ),
            rows: vec![
                vec![
                    self.op(
                        "📄 Un seul document",
                        "burst.one",
                        json!({"id": id}),
                        Value::Null,
                    )
                    .await?,
                ],
                vec![
                    self.op(
                        "📥 Ingérer sans répondre",
                        "burst.ingest",
                        json!({"id": id}),
                        Value::Null,
                    )
                    .await?,
                ],
                vec![
                    self.op("1️⃣ Un par un", "burst.each", json!({"id": id}), Value::Null)
                        .await?,
                    self.op(
                        "🗑 Tout annuler",
                        "burst.drop",
                        json!({"id": id}),
                        Value::Null,
                    )
                    .await?,
                ],
            ],
        };
        self.send_screen(chat_id, topic_id, reply_to, screen, None)
            .await
    }

    /// Vocal ou fichier audio (§14.4) : téléchargement, transcription par le rôle `stt`,
    /// texte montré en citation, puis traité comme un message tapé.
    /// Photo : enregistrée, puis confiée au tour (vision, §10.4). Les photos d'un album
    /// attendent leurs voisines pendant [`ALBUM_WINDOW`] et partent ensemble.
    async fn photo(&self, incoming: Incoming) -> anyhow::Result<()> {
        let Incoming::Photo {
            update_id,
            chat_id,
            message_id,
            topic_id,
            file_ids,
            file_size,
            media_group,
            caption,
            ..
        } = incoming
        else {
            return Ok(());
        };
        let reply_to = Some(message_id);
        let Some(file_id) = file_ids.last() else {
            return Ok(());
        };
        if file_size.unwrap_or(0) as usize > crate::media::IMAGE_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "📷 Photo trop lourde (10 Mo au plus).",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let saved = match self.bot.download_file(file_id).await {
            Ok((bytes, _)) => crate::media::save_photo(&self.daemon.services, &bytes),
            Err(e) => Err(format!("téléchargement impossible : {e}")),
        };
        let path = match saved {
            Ok(p) => p,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("📷 Photo ignorée : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let Some(group) = media_group else {
            return enqueue_photos(&self.daemon, &origin, vec![path], caption, update_id).await;
        };
        let first = {
            let mut albums = self
                .albums
                .lock()
                .map_err(|_| anyhow::anyhow!("albums verrouillés"))?;
            match albums.get_mut(&group) {
                Some(album) => {
                    album.images.push(path);
                    if album.caption.is_none() {
                        album.caption = caption;
                    }
                    false
                }
                None => {
                    albums.insert(
                        group.clone(),
                        Album {
                            origin,
                            images: vec![path],
                            caption,
                            update_id,
                        },
                    );
                    true
                }
            }
        };
        if first {
            let (daemon, albums) = (self.daemon.clone(), self.albums.clone());
            tokio::spawn(async move {
                tokio::time::sleep(ALBUM_WINDOW).await;
                let album = albums.lock().ok().and_then(|mut g| g.remove(&group));
                if let Some(a) = album
                    && let Err(e) =
                        enqueue_photos(&daemon, &a.origin, a.images, a.caption, a.update_id).await
                {
                    tracing::warn!(error = %e, "album Telegram non transmis");
                }
            });
        }
        Ok(())
    }

    /// Document : ingéré dans le vault s'il est lisible (§6.13), sinon rangé comme pièce
    /// jointe. Une légende est une demande : elle part en tour, document joint. `/mien` en
    /// tête de légende déclare un document rédigé par le propriétaire.
    async fn document(&self, incoming: Incoming) -> anyhow::Result<()> {
        let Incoming::Document {
            update_id,
            chat_id,
            message_id,
            topic_id,
            file_id,
            file_name,
            file_size,
            caption,
            ..
        } = incoming
        else {
            return Ok(());
        };
        let reply_to = Some(message_id);
        if file_size.unwrap_or(0) as usize > penelope_telegram::api::DOWNLOAD_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "📄 Fichier trop gros : un bot Telegram ne télécharge pas plus de 20 Mo. \
                     Le déposer dans `vault/inbox/` fonctionne aussi.",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let bytes = match self.bot.download_file(&file_id).await {
            Ok((b, _)) => b,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("📄 Téléchargement impossible : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = self.daemon.chat_session_for(&origin).await?;
        let caption = caption.unwrap_or_default();
        let caption = caption.trim();
        let (owner, request) = match caption.strip_prefix("/mien") {
            Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                (true, rest.trim().to_string())
            }
            _ => (false, caption.to_string()),
        };
        // L'ingestion appelle le modèle : la file des updates n'attend pas.
        let daemon = self.daemon.clone();
        tokio::spawn(async move {
            let dedup = Some(format!("tg:{update_id}"));
            let messenger = daemon.hooks.messenger();
            let say = |text: String| {
                let (m, o) = (messenger.clone(), origin.clone());
                async move {
                    if let Some(m) = m {
                        let _ = m.send_text(&o, &text).await;
                    }
                }
            };
            if penelope_memory::ingest::is_ingestible(&file_name) {
                let trust = if owner {
                    penelope_memory::Origin::Owner
                } else {
                    penelope_memory::Origin::Untrusted
                };
                // L'ingestion est déclarée pour cette session : `/stop tout` peut
                // l'interrompre (issue #155). Le jeton est retiré quoi qu'il arrive.
                let (ingest_id, cancel) = daemon.bus.start_ingest(&session);
                let outcome = crate::ingest::ingest(
                    &daemon,
                    &file_name,
                    bytes,
                    "telegram",
                    trust,
                    Some(&session),
                    &cancel,
                )
                .await;
                daemon.bus.end_ingest(&session, ingest_id);
                match outcome {
                    Ok(doc) => {
                        say(doc.report()).await;
                        if let (Some(m), Some(id)) = (&messenger, &doc.approval_id) {
                            let _ = m.send_approval(&origin, id).await;
                        }
                        if !request.is_empty() {
                            let text = doc.turn_text(&request);
                            if let Err(e) = daemon
                                .enqueue_message(&session, &text, &origin, dedup)
                                .await
                            {
                                tracing::warn!(error = %e, "demande sur document non transmise");
                            }
                        }
                    }
                    Err(e) => say(format!("📄 `{file_name}` non ingéré : {e}")).await,
                }
                return;
            }
            let (note, joined) = store_attachment(&daemon, &session, &file_name, &bytes).await;
            say(note).await;
            if let (false, Some(joined)) = (request.is_empty(), joined) {
                let text = format!("{request}\n\n{joined}");
                if let Err(e) = daemon
                    .enqueue_message(&session, &text, &origin, dedup)
                    .await
                {
                    tracing::warn!(error = %e, "demande sur pièce jointe non transmise");
                }
            }
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn voice(
        &self,
        update_id: i64,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        file_id: &str,
        file_name: Option<&str>,
        mime_type: Option<&str>,
        file_size: Option<i64>,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        if file_size.unwrap_or(0) as usize > penelope_telegram::api::DOWNLOAD_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "🎙️ Fichier trop gros : un bot Telegram ne télécharge pas plus de 20 Mo.",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let (audio, path) = match self.bot.download_file(file_id).await {
            Ok(x) => x,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("🎙️ Téléchargement du vocal impossible : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = self.daemon.chat_session_for(&origin).await?;
        let filename = audio_filename(&path, file_name, mime_type);
        let text = match self.daemon.transcribe(audio, &filename, &session).await {
            Ok(t) => t,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("🎙️ Transcription impossible : {e}"),
                    )
                    .await;
            }
        };
        if text.trim().is_empty() {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "🎙️ Rien d'audible dans ce vocal.",
                )
                .await;
        }
        let quoted: String = text
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.reply(chat_id, topic_id, reply_to, &format!("🎙️\n{quoted}"))
            .await?;
        self.daemon
            .enqueue_message(
                &session,
                &if self.daemon.services.config.config().voice.reply_in_kind {
                    format!(
                        "(message vocal transcrit ; réponds en vocal avec `send_voice` si la \
                         réponse s'y prête) {text}"
                    )
                } else {
                    format!("(message vocal transcrit) {text}")
                },
                &origin,
                Some(format!("tg:{update_id}")),
            )
            .await?;
        Ok(())
    }

    /// `/budget` : dépense du jour et de la session, requêtes les plus chères.
    /// `/budget sessions|requêtes|modèles|jours` : un regroupement précis.
    /// `/usage [session|turn|model|day|role|upstream]` : tokens d'entrée, part en cache,
    /// sortie et coût (issue #20). `turn` se limite à la session du chat.
    async fn usage_text(&self, session: &str, args: &str) -> anyhow::Result<String> {
        let s = &self.daemon.services;
        let by = match args.trim() {
            "" | "sessions" => "session",
            "requêtes" | "requetes" | "tours" | "turns" => "turn",
            "modèles" | "modeles" | "models" => "model",
            other => other,
        };
        if !penelope_kernel::budget::USAGE_AXES.contains(&by) {
            return Ok(format!(
                "Regroupement inconnu `{by}`. Choix : {}.",
                penelope_kernel::budget::USAGE_AXES.join(", ")
            ));
        }
        let today = s.budget.today();
        let (scope, since, title) = match by {
            "turn" => (Some(session), None, "Requêtes de la session"),
            "day" => (None, None, "Par jour"),
            _ => (None, Some(today.as_str()), "Aujourd'hui"),
        };
        let rows = s.budget.report(by, scope, since, 10).await?;
        if rows.is_empty() {
            return Ok("Aucune consommation enregistrée.".into());
        }
        let k = |n: i64| {
            if n >= 1_000_000 {
                format!("{:.1} M", n as f64 / 1e6).replace('.', ",")
            } else if n >= 1_000 {
                format!("{} k", n / 1_000)
            } else {
                n.to_string()
            }
        };
        let mut t = format!("**{title}, par {by}**\n");
        for r in &rows {
            let label = r
                .label
                .as_deref()
                .map(|l| format!("« {l} »"))
                .unwrap_or_else(|| format!("`{}`", if r.key.is_empty() { "?" } else { &r.key }));
            t.push_str(&format!(
                "\n- {} · {label} · {} appel(s) · entrée {} (cache {:.0} %) · sortie {}",
                crate::budget_alert::usd(r.cost_usd),
                r.calls,
                k(r.prompt),
                r.cache_ratio() * 100.0,
                k(r.completion)
            ));
        }
        Ok(t)
    }

    async fn budget_text(&self, session: &str, args: &str) -> anyhow::Result<String> {
        let s = &self.daemon.services;
        let cfg = s.config.config();
        let usd = |x: f64| format!("{:.4} $", x).replace('.', ",");
        let axis = match args.trim() {
            "" => None,
            "sessions" | "session" => Some(("session", None, "Sessions les plus chères")),
            "requêtes" | "requetes" | "tours" | "turn" => {
                Some(("turn", Some(session), "Requêtes les plus chères (session)"))
            }
            "modèles" | "modeles" | "model" => Some(("model", None, "Par modèle")),
            "jours" | "day" => Some(("day", None, "Par jour")),
            "rôles" | "roles" | "role" => Some(("role", None, "Par usage")),
            other => {
                return Ok(format!(
                    "Regroupement inconnu `{other}`. Choix : sessions, requêtes, modèles, jours, rôles."
                ));
            }
        };

        let row_line = |r: &penelope_kernel::budget::UsageRow| {
            let label = r
                .label
                .as_deref()
                .map(|l| format!(" « {l} »"))
                .unwrap_or_default();
            let key = if r.key.is_empty() {
                "?"
            } else {
                r.key.as_str()
            };
            let est = if r.estimated > 0 { " (estimé)" } else { "" };
            format!(
                "- {}{est} · `{key}`{label} · {} appel(s)\n",
                usd(r.cost_usd),
                r.calls
            )
        };

        if let Some((by, scope, title)) = axis {
            let rows = s.budget.report(by, scope, None, 15).await?;
            if rows.is_empty() {
                return Ok("Aucune consommation enregistrée.".into());
            }
            let mut t = format!("**{title}**\n\n");
            for r in &rows {
                t.push_str(&row_line(r));
            }
            return Ok(t);
        }

        let today = s.budget.spent_today().await?;
        let in_session = s.budget.spent_session(session).await?;
        let (daily_limit, session_limit, _) =
            s.budget.limits(&cfg.budget, Some(session), None).await?;
        let own = s
            .sessions
            .get(session)
            .await?
            .and_then(|x| x.budget_usd)
            .is_some();
        let mut t = format!(
            "💶 Aujourd'hui : {} sur {} · session : {} sur {}{}\n",
            usd(today),
            usd(daily_limit),
            usd(in_session),
            usd(session_limit),
            if own { " (plafond propre)" } else { "" }
        );
        let view = crate::compaction::context_view(s, session, None).await?;
        if let Some(prompt) = view["last_prompt_tokens"].as_i64() {
            let cached = view["last_cached_tokens"].as_i64().unwrap_or(0);
            t.push_str(&format!(
                "📏 {} ({:.0} % en cache)\n",
                context_line(&view),
                if prompt > 0 {
                    cached as f64 * 100.0 / prompt as f64
                } else {
                    0.0
                },
            ));
        }
        let turns = s.budget.report("turn", Some(session), None, 5).await?;
        if !turns.is_empty() {
            t.push_str("\n**Requêtes les plus chères de la session**\n\n");
            for r in &turns {
                t.push_str(&row_line(r));
            }
        }
        let today_day = s.budget.today();
        let models = s.budget.report("model", None, Some(&today_day), 5).await?;
        if !models.is_empty() {
            t.push_str("\n**Par modèle, aujourd'hui**\n\n");
            for r in &models {
                t.push_str(&format!(
                    "- {} · `{}` · {} appel(s)\n",
                    usd(r.cost_usd),
                    r.key,
                    r.calls
                ));
            }
        }
        // L'abonnement ChatGPT ne facture pas l'appel : sa limite est le quota du plan,
        // que les plafonds en dollars ne voient pas (#142).
        if cfg.providers.codex.enabled
            && let Some(q) = crate::codex_quota::snapshot(s).await
        {
            t.push_str(&format!(
                "\n**Abonnement ChatGPT** (hors plafonds en dollars)\n\n- {}\n",
                crate::codex_quota::gauge_line(&q, s.clock.now_ms())
            ));
        }
        t.push_str(
            "\nDétail : `/budget sessions`, `/budget requêtes`, `/budget modèles` · plafond de \
             cette session : `/budget session 20`",
        );
        Ok(t)
    }

    // ================================================================ boutons

    async fn callback(
        self: &Arc<Self>,
        callback_id: &str,
        data: &str,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        from_id: i64,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let outcome = s.actions.click(data, from_id).await?;
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::MODEL_PIN
        {
            return self
                .model_pin_clicked(callback_id, action, chat_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && (action.action == k::OAUTH_RETRY || action.action == k::OAUTH_PASTED)
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            if action.action == k::OAUTH_RETRY {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                return self.send_oauth_card(chat_id, None, &action.target).await;
            }
            return self
                .reply(
                    chat_id,
                    None,
                    None,
                    "📋 Colle ici l'adresse complète affichée par le navigateur après \
                     l'autorisation (elle contient `code=` et `state=`).",
                )
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::SESSION_SWITCH
                    | k::SESSION_MENU
                    | k::SESSION_FORK
                    | k::SESSION_RENAME
                    | k::SESSION_CLOSE
                    | k::SESSIONS_PAGE
            )
        {
            return self
                .session_menu_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && (action.action == k::BUDGET_RAISE || action.action == k::BUDGET_STOP)
        {
            return self
                .budget_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::SAY
        {
            let text = action.args["text"].as_str().unwrap_or_default().to_string();
            let _ = self
                .bot
                .answer_callback(callback_id, Some(&text), false)
                .await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            let origin = Origin::Telegram {
                chat_id,
                topic_id,
                message_id: None,
            };
            self.reply(chat_id, topic_id, Some(message_id), &format!("➡️ {text}"))
                .await?;
            self.daemon
                .enqueue_message(&action.target, &text, &origin, None)
                .await?;
            return Ok(());
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::SCREEN | k::SCREEN_DO | k::RUN_COMMAND
            )
        {
            return self
                .screen_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::ONBOARD_START
                    | k::ONBOARD_ANSWER
                    | k::ONBOARD_PAUSE
                    | k::ONBOARD_WRITE
                    | k::ONBOARD_CANCEL
            )
        {
            return self
                .onboarding_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::ELICIT_RETRY
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return self.elicitation_retry_clicked(action, chat_id).await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::ELICIT_ACCEPT | k::ELICIT_DECLINE | k::ELICIT_CANCEL | k::ELICIT_DONE
            )
        {
            return self
                .elicitation_clicked(callback_id, action, chat_id, message_id)
                .await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && matches!(
                action.action.as_str(),
                k::FORM_NEXT | k::FORM_PREV | k::FORM_SUBMIT | k::FORM_DECLINE
            )
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return self.form_clicked(action, chat_id, topic_id).await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::REGENERATE
        {
            let _ = self.bot.answer_callback(callback_id, None, false).await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return self.retry_clicked(action, chat_id).await;
        }
        if let ClickOutcome::Accepted(action) = &outcome
            && action.action == k::CHOICE
            && action.args.get("visit").is_some()
        {
            return self
                .workflow_choice_clicked(callback_id, action, chat_id, topic_id, message_id)
                .await;
        }
        // `answerCallbackQuery` d'abord : Telegram attend une réponse sous une seconde.
        let notice = match &outcome {
            ClickOutcome::Accepted(_) => None,
            ClickOutcome::AlreadyHandled(_) => Some("Déjà traité."),
            ClickOutcome::Expired => Some("Ce bouton a expiré."),
            ClickOutcome::Unknown => Some("Action inconnue."),
            ClickOutcome::NotOwner => Some("Non autorisé."),
        };
        let _ = self.bot.answer_callback(callback_id, notice, false).await;

        let ClickOutcome::Accepted(action) = outcome else {
            if matches!(
                outcome,
                ClickOutcome::Expired | ClickOutcome::AlreadyHandled(_)
            ) {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            }
            return Ok(());
        };

        let approval_id = action.target.clone();
        let Some(approval) = s.approvals.get(&approval_id).await? else {
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return Ok(());
        };
        let (chat_id, topic_id) = self
            .approval_destination(&approval, chat_id, topic_id)
            .await;
        let double = approval.payload["double"].as_bool().unwrap_or(false);

        match action.action.as_str() {
            k::APPROVE if double => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.send_destructive_confirm(chat_id, topic_id, &approval)
                    .await?;
            }
            k::APPROVE | k::CONFIRM_DESTRUCTIVE => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.finalize_decision(
                    &approval_id,
                    &Decision::approve_once("telegram"),
                    chat_id,
                    topic_id,
                )
                .await?;
            }
            k::APPROVE_RUN => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let d = Decision {
                    window: PolicyWindow::Session,
                    choice: "Pour cette session".into(),
                    ..Decision::approve_once("telegram")
                };
                self.finalize_decision(&approval_id, &d, chat_id, topic_id)
                    .await?;
            }
            k::APPROVE_ALWAYS => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.finalize_decision(
                    &approval_id,
                    &Decision::approve_always("telegram"),
                    chat_id,
                    topic_id,
                )
                .await?;
            }
            k::DENY => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                // « Pas encore » d'un lancement de workflow porte sa raison (issue #35).
                let reason = action.args["reason"].as_str().map(String::from);
                self.finalize_decision(
                    &approval_id,
                    &Decision::deny("telegram", reason),
                    chat_id,
                    topic_id,
                )
                .await?;
            }
            // Contradiction (#145) : remplacer, garder les deux avec un contexte, ou
            // ignorer. Les trois passent par le même chemin d'application.
            k::MEMORY_ACCEPT | k::MEMORY_AS_EXCEPTION | k::MEMORY_REJECT
                if approval.payload["contradiction"].as_bool() == Some(true) =>
            {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let keep = action.action != k::MEMORY_REJECT;
                let decision = if keep {
                    Decision {
                        choice: if action.action == k::MEMORY_ACCEPT {
                            "Remplacer".into()
                        } else {
                            "Exception".into()
                        },
                        ..Decision::approve_once("telegram")
                    }
                } else {
                    Decision {
                        choice: "Ignorer".into(),
                        ..Decision::deny("telegram", None)
                    }
                };
                let won = decide_approval(s, &approval_id, &decision).await?;
                let note = if !won {
                    "ℹ️ Déjà tranché.".to_string()
                } else {
                    match crate::ingest::apply_contradiction(
                        &self.daemon,
                        &approval_id,
                        &action.action,
                    )
                    .await
                    {
                        Ok(note) => note,
                        Err(e) => format!("❌ {e}"),
                    }
                };
                self.reply(chat_id, topic_id, None, &note).await?;
            }
            k::MEMORY_ACCEPT | k::MEMORY_REJECT => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let accept = action.action == k::MEMORY_ACCEPT;
                let decision = if accept {
                    Decision {
                        choice: "Tout".into(),
                        ..Decision::approve_once("telegram")
                    }
                } else {
                    Decision {
                        choice: "Rien".into(),
                        ..Decision::deny("telegram", None)
                    }
                };
                let won = decide_approval(s, &approval_id, &decision).await?;
                let note = match (accept, won) {
                    (false, _) => "🗑 Propositions écartées : rien n'entre en mémoire.".to_string(),
                    (true, true) => {
                        let confirm = s
                            .approvals
                            .get(&approval_id)
                            .await?
                            .is_some_and(|a| a.payload["confirm"].as_bool() == Some(true));
                        match crate::ingest::apply_memory_proposal(&self.daemon, &approval_id).await
                        {
                            Ok(n) if confirm => format!(
                                "✅ {n} règle(s) confirmée(s) : elles entrent en mémoire à la \
                                 prochaine consolidation (`/dream` pour tout de suite)."
                            ),
                            Ok(n) => format!("🧠 {n} fait(s) ajouté(s) à `notes.md`."),
                            Err(e) => format!("❌ {e}"),
                        }
                    }
                    (true, false) => "ℹ️ Déjà tranché.".to_string(),
                };
                self.reply(chat_id, topic_id, None, &note).await?;
            }
            // Effet incertain (#83) : la décision tranche le ledger, sans règle.
            k::EFFECT_VERIFY | k::EFFECT_RETRY | k::EFFECT_IGNORE => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                let decision = match action.action.as_str() {
                    k::EFFECT_VERIFY => Decision {
                        choice: crate::agent::EFFECT_DONE.into(),
                        ..Decision::approve_once("telegram")
                    },
                    k::EFFECT_RETRY => Decision {
                        choice: crate::agent::EFFECT_RETRY.into(),
                        ..Decision::approve_once("telegram")
                    },
                    _ => Decision {
                        choice: crate::agent::EFFECT_IGNORE.into(),
                        ..Decision::deny("telegram", None)
                    },
                };
                self.finalize_decision(&approval_id, &decision, chat_id, topic_id)
                    .await?;
            }
            k::DENY_REASON => {
                let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                self.daemon
                    .kv_set(&approval_reason_key(chat_id, topic_id), &approval_id)
                    .await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    "✏️ Donne la raison du refus en un message.",
                )
                .await?;
            }
            other => {
                tracing::warn!(action = other, "action Telegram non prise en charge");
            }
        }
        Ok(())
    }

    /// Tranche, confirme, et remet la suite du tour en file.
    async fn finalize_decision(
        &self,
        approval_id: &str,
        decision: &Decision,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let won = decide_approval(s, approval_id, decision).await?;
        let a = s.approvals.get(approval_id).await?;
        let Some(a) = a else { return Ok(()) };

        // Une autre décision est passée avant (CLI) : on le dit, sans rien rejouer.
        let first = won == decision.approved && a.decided_via.as_deref() == Some("telegram");
        let checkpoint = a.payload["checkpoint"].as_bool() == Some(true);
        let launch = a.subject == "workflow_start";
        let effect = a.kind == penelope_hitl::ApprovalKind::EffectUnknown;
        let note = match (first, a.state) {
            (true, _) if effect => match decision.choice.as_str() {
                crate::agent::EFFECT_DONE => {
                    format!("✅ Noté : `{}` a eu lieu, je ne le relance pas.", a.subject)
                }
                crate::agent::EFFECT_RETRY => format!("🔁 Je relance `{}`.", a.subject),
                _ => format!("⏭ `{}` reste tel quel, sans relance.", a.subject),
            },
            (false, st) => format!(
                "ℹ️ Déjà tranché : {} via {}.",
                st.as_str(),
                a.decided_via.clone().unwrap_or_default()
            ),
            (true, ApprovalState::Approved) if checkpoint => "▶️ Je continue.".to_string(),
            (true, _) if checkpoint => "⏹ Tour arrêté.".to_string(),
            (true, ApprovalState::Approved) if launch => "▶️ Je lance.".to_string(),
            (true, _) if launch => "⏸ Pas encore : on continue d'en parler.".to_string(),
            (true, ApprovalState::Approved) => format!("✅ {} : `{}`.", decision.choice, a.subject),
            (true, _) => format!("❌ Refusé : `{}`.", a.subject),
        };
        self.reply(chat_id, topic_id, None, &note).await?;

        if first && let Some(session) = &a.session_id {
            let origin = Origin::Telegram {
                chat_id,
                topic_id,
                message_id: None,
            };
            self.daemon
                .enqueue_resume(session, approval_id, &origin)
                .await?;
        }
        Ok(())
    }

    async fn recorded_approval_destination(
        &self,
        a: &ApprovalRequest,
    ) -> Option<(i64, Option<i64>)> {
        let key = format!("tg.approval_destination.{}", a.id.as_str());
        if let Ok(Some(raw)) = self.daemon.kv_get(&key).await
            && let Ok(v) = serde_json::from_str::<Value>(&raw)
            && let Some(chat) = v["chat_id"].as_i64()
        {
            return Some((chat, v["topic_id"].as_i64()));
        }
        if let Some(run_id) = a.run_id.as_deref()
            && let Some(destination) = crate::workflow::origin_of(&self.daemon, run_id)
                .await
                .telegram_chat()
        {
            return Some(destination);
        }
        if let Some(sid) = a.session_id.as_deref()
            && let Ok(Some(session)) = self.daemon.services.sessions.get(sid).await
            && let Some(chat) = session.tg_chat_id
        {
            return Some((chat, session.tg_topic_id));
        }
        None
    }

    async fn approval_destination(
        &self,
        a: &ApprovalRequest,
        clicked_chat: i64,
        clicked_topic: Option<i64>,
    ) -> (i64, Option<i64>) {
        // Le message du callback donne la destination la plus précise. Telegram ne
        // répète parfois pas son sujet : la destination persistée de la carte tranche.
        if clicked_topic.is_some() {
            return (clicked_chat, clicked_topic);
        }
        self.recorded_approval_destination(a)
            .await
            .unwrap_or((clicked_chat, clicked_topic))
    }

    // ================================================================ cartes

    pub async fn send_approval_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        // Le clic peut arriver après un redémarrage ; le run technique n'a pas de
        // coordonnées Telegram, mais cette destination survit dans le store (#165).
        self.daemon
            .kv_set(
                &format!("tg.approval_destination.{}", a.id.as_str()),
                &json!({"chat_id": chat_id, "topic_id": topic_id}).to_string(),
            )
            .await?;
        if a.kind == penelope_hitl::ApprovalKind::BudgetExceeded
            && a.payload["budget"].as_bool() == Some(true)
        {
            return self.send_budget_card(chat_id, topic_id, a).await;
        }
        if a.kind == penelope_hitl::ApprovalKind::MemoryProposal {
            return self.send_memory_card(chat_id, topic_id, a).await;
        }
        if a.subject == "workflow_start" {
            return self.send_launch_card(chat_id, topic_id, a).await;
        }
        if a.kind == penelope_hitl::ApprovalKind::EffectUnknown {
            return self.send_effect_card(chat_id, topic_id, a).await;
        }
        let s = &self.daemon.services;
        // Point de contrôle de coût d'un tour (issue #19) : continuer ou arrêter.
        if a.payload["checkpoint"].as_bool() == Some(true) {
            let ttl = 24 * 3_600_000;
            let mut row = Vec::new();
            for (label, action) in [("▶️ Continuer", k::APPROVE), ("⏹ Arrêter", k::DENY)] {
                let t = s
                    .actions
                    .create(action, a.id.as_str(), json!({}), ttl, true)
                    .await?;
                row.push(ButtonSpec::callback(label, &t.token, ""));
            }
            let text = format!(
                "💸 {}",
                a.payload["reason"]
                    .as_str()
                    .unwrap_or("Ce tour coûte cher, je continue ?")
            );
            return self
                .outbox_push(
                    chat_id,
                    topic_id,
                    "sendMessage",
                    json!({
                        "chat_id": chat_id,
                        "text": markdown_to_html(&text),
                        "parse_mode": "HTML",
                        "reply_markup": inline_keyboard(&[row]),
                        "message_thread_id": topic_id,
                    }),
                )
                .await;
        }
        let args = serde_json::to_string_pretty(&a.payload["arguments"])
            .unwrap_or_default()
            .replace("```", "ʼʼʼ");
        let args: String = if args.chars().count() > 1_500 {
            format!("{}…", args.chars().take(1_500).collect::<String>())
        } else {
            args
        };
        let server = crate::agent::server_of(&a.subject).unwrap_or_else(|| "natif".into());
        let double = a.payload["double"].as_bool().unwrap_or(false);
        let card = approval_card(a);
        let mut vars = BTreeMap::new();
        vars.insert("intention".into(), card.intention);
        vars.insert("action".into(), card.action);
        vars.insert("details".into(), card.details);
        // Anciennes variables, pour un gabarit surchargé écrit avant #116.
        vars.insert("outil".into(), a.subject.clone());
        vars.insert("serveur".into(), server);
        vars.insert("risque".into(), a.risk.as_str().to_string());
        vars.insert("arguments".into(), args);
        vars.insert(
            "raison".into(),
            a.payload["reason"].as_str().unwrap_or("").to_string(),
        );
        let mut alerte = if double {
            "⚠️ Seconde confirmation demandée.".to_string()
        } else {
            String::new()
        };
        // Réseau demandé par une commande (#106) : quatre mots ; le bouton « Toujours » dit
        // sur quoi il porte (#116).
        if crate::executor::wants_network(&a.subject, &a.payload["arguments"]) {
            if !alerte.is_empty() {
                alerte.push('\n');
            }
            alerte.push_str("🌐 Accès au réseau demandé.");
        }
        // Outil MCP dont la description porte une consigne : le propriétaire le voit avant
        // d'accepter (#92).
        if let Some(t) = s.mcp_tools.get(&a.subject).await.ok().flatten() {
            let rules: Vec<String> = t
                .flags()
                .iter()
                .map(|f| f.split(" «").next().unwrap_or(f).to_string())
                .collect();
            if !rules.is_empty() {
                if !alerte.is_empty() {
                    alerte.push('\n');
                }
                alerte.push_str(&format!(
                    "⚠️ La description de cet outil contient une consigne selon le détecteur \
                     local ({}) : le modèle a pu être manipulé.",
                    rules.join(", ")
                ));
            }
        }
        vars.insert("alerte".into(), alerte);

        let ttl = 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [
            k::APPROVE,
            k::APPROVE_RUN,
            k::APPROVE_ALWAYS,
            k::DENY,
            k::DENY_REASON,
        ] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("tool_approval")
            .ok_or_else(|| anyhow::anyhow!("gabarit tool_approval absent"))?;
        let rendered = match tpl.render(&vars, &tokens, &[]) {
            Ok(r) => r,
            // Une carte qui ne se rend pas part quand même, en texte brut : une demande
            // invisible bloque le tour sans que personne le sache (issue #129).
            Err(e) => {
                return self
                    .send_plain_approval(chat_id, topic_id, a, &vars, &tokens, &e.to_string())
                    .await;
            }
        };
        // Le libellé « Pour ce run » du gabarit vaut « pour cette session » en conversation.
        // Sans arguments vus, pas de fenêtre ni de « Toujours » : une règle couvrirait
        // l'outil entier (#83, régression de #67).
        let windows = a.payload.get("arguments").is_some();
        let windowed = [
            tokens.get(k::APPROVE_RUN).cloned().unwrap_or_default(),
            tokens.get(k::APPROVE_ALWAYS).cloned().unwrap_or_default(),
        ];
        let buttons: Vec<Vec<ButtonSpec>> = rendered
            .buttons
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|b| {
                        windows
                            || !matches!(&b.action, penelope_telegram::render::ButtonAction::Callback { token } if windowed.contains(token))
                    })
                    // Aucune règle possible : pas de bouton dans la case de « Toujours ».
                    // Une coche verte à sa place se lisait comme un « Toujours » nouvelle
                    // formule, et n'autorisait qu'une fois (issue #150).
                    .filter(|b| {
                        card.always.is_some()
                            || !matches!(&b.action, penelope_telegram::render::ButtonAction::Callback { token } if Some(token) == tokens.get(k::APPROVE_ALWAYS))
                    })
                    .map(|b| {
                        let mut b = b.clone();
                        if b.label.contains("Pour ce run") {
                            b.label = "✅ Pour cette session".into();
                        }
                        // « Toujours » dit sur quoi il porte : la famille de commandes, le
                        // répertoire, l'hôte (#116). Quand il ne peut créer aucune règle, il
                        // le dit plutôt que de laisser croire au contraire (#141).
                        if b.label.contains("Toujours")
                            && let Some(scope) = &card.always
                        {
                            b.label = format!("♾️ Toujours pour {scope}");
                        }
                        b
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Demande d'approbation en texte brut, quand son gabarit ne se rend pas : le
    /// propriétaire la voit, sait qu'elle est simplifiée, et peut répondre ; l'échec
    /// laisse un événement (issue #129).
    async fn send_plain_approval(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
        vars: &BTreeMap<String, String>,
        tokens: &BTreeMap<String, String>,
        error: &str,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        tracing::warn!(approval = %a.id.as_str(), error, "carte d'approbation simplifiée");
        let _ = s
            .events
            .append(penelope_kernel::event::EventDraft::new(
                "telegram.card_degraded",
                json!({"approval": a.id.as_str(), "template": "tool_approval", "error": error}),
            ))
            .await;
        let part = |k: &str| vars.get(k).cloned().unwrap_or_default();
        let mut text = format!(
            "⚠️ Carte simplifiée : son gabarit ne se rend pas ({error}).\n\n{}\n\n{}\n\n{}",
            part("intention"),
            part("action"),
            part("details")
        );
        if text.chars().count() > 3_800 {
            text = format!("{}…", text.chars().take(3_800).collect::<String>());
        }
        let row: Vec<ButtonSpec> = [("✅ Approuver", k::APPROVE), ("❌ Refuser", k::DENY)]
            .into_iter()
            .filter_map(|(label, action)| {
                tokens
                    .get(action)
                    .map(|t| ButtonSpec::callback(label, t, ""))
            })
            .collect();
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": text,
                "reply_markup": inline_keyboard(&[row]),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Carte d'un effet resté incertain après un arrêt brutal (#83) : « C'est fait »,
    /// « Relancer » ou « Ignorer », jamais de fenêtre ni de règle.
    async fn send_effect_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let request = serde_json::to_string_pretty(&a.payload["request"])
            .unwrap_or_default()
            .replace("```", "ʼʼʼ");
        let request: String = if request.chars().count() > 1_500 {
            format!("{}…", request.chars().take(1_500).collect::<String>())
        } else {
            request
        };
        let tz = s.config.config().owner.timezone.clone();
        let when = chrono::DateTime::parse_from_rfc3339(&a.created_at)
            .ok()
            .map(|t| match tz.parse::<chrono_tz::Tz>() {
                Ok(tz) => t.with_timezone(&tz).format("%d/%m à %H:%M").to_string(),
                Err(_) => t.format("%d/%m à %H:%M UTC").to_string(),
            })
            .unwrap_or_default();
        let mut vars = BTreeMap::new();
        vars.insert("effet".into(), a.subject.clone());
        vars.insert("horodatage".into(), when);
        vars.insert("requete".into(), request);
        let ttl = 7 * 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [k::EFFECT_VERIFY, k::EFFECT_RETRY, k::EFFECT_IGNORE] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("effect_unknown")
            .ok_or_else(|| anyhow::anyhow!("gabarit effect_unknown absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rendered.buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Carte de lancement d'un workflow proposé en conversation (issue #35) : workflow,
    /// paramètres complétés et brief ; « Lancer » ou « Pas encore », sans « Toujours ».
    async fn send_launch_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let args = &a.payload["arguments"];
        let id = args["id"].as_str().unwrap_or("?");
        let (name, description) = match s.workflows.get(id) {
            Some(w) => (
                if w.metadata.name.is_empty() {
                    id.to_string()
                } else {
                    w.metadata.name.clone()
                },
                w.metadata
                    .description
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            ),
            None => (id.to_string(), "Workflow introuvable.".to_string()),
        };
        let mut text = format!("▶️ **Lancer « {name} » ?** (`{id}`)");
        if !description.is_empty() {
            text.push_str(&format!("\n{description}"));
        }
        let params = args["params"].as_object().cloned().unwrap_or_default();
        if !params.is_empty() {
            text.push_str("\n\n**Paramètres**");
            for (k, v) in &params {
                let v = v
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string());
                let v: String = v.chars().take(200).collect();
                text.push_str(&format!("\n- `{k}` : {v}"));
            }
        }
        if let Some(brief) = args["brief"]
            .as_str()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        {
            let short: String = brief.chars().take(1_200).collect();
            let more = if brief.chars().count() > 1_200 {
                "…"
            } else {
                ""
            };
            text.push_str(&format!("\n\n**Brief**\n{short}{more}"));
        }
        let ttl = 24 * 3_600_000;
        let launch = s
            .actions
            .create(k::APPROVE, a.id.as_str(), json!({}), ttl, true)
            .await?;
        let later = s
            .actions
            .create(
                k::DENY,
                a.id.as_str(),
                json!({"reason": "pas encore : le propriétaire veut continuer la discussion \
                                  avant de lancer"}),
                ttl,
                true,
            )
            .await?;
        let rows = vec![vec![
            ButtonSpec::callback("▶️ Lancer", &launch.token, ""),
            ButtonSpec::callback("⏸ Pas encore", &later.token, ""),
        ]];
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(&text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rows),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Carte « plafond atteint : continuer ? » (issue #32) : +5 $, +20 $ ou arrêter.
    async fn send_budget_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let scope = a.payload["scope"].as_str().unwrap_or("session");
        let spent = a.payload["spent"].as_f64().unwrap_or(0.0);
        let limit = a.payload["limit"].as_f64().unwrap_or(0.0);
        let place = match scope {
            "jour" => "aujourd'hui".to_string(),
            "run" => format!(
                "dans le run `{}`",
                a.payload["run_id"]
                    .as_str()
                    .or(a.run_id.as_deref())
                    .unwrap_or("?")
            ),
            _ => match a.session_id.as_deref() {
                Some(sid) => match s.sessions.get(sid).await? {
                    Some(sess) => format!("dans « {} »", crate::titles::label(&sess)),
                    None => "dans cette session".into(),
                },
                None => "dans cette session".into(),
            },
        };
        let hint = match scope {
            "jour" => "\nLe relèvement vaut pour aujourd'hui seulement.",
            "run" => "\nLe run reprend après relèvement.",
            _ => "\nLe tour reprend là où il s'est arrêté.",
        };
        let ttl = 24 * 3_600_000;
        let mut row = Vec::new();
        for (label, action, amount) in [
            ("+5 $", k::BUDGET_RAISE, 5.0),
            ("+20 $", k::BUDGET_RAISE, 20.0),
            ("⏹ Arrêter", k::BUDGET_STOP, 0.0),
        ] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({"amount": amount}), ttl, true)
                .await?;
            row.push(ButtonSpec::callback(label, &t.token, ""));
        }
        let text = format!(
            "💸 {} $ dépensés sur {} $ {place} : continuer ?{hint}",
            fmt_usd(spent),
            fmt_usd(limit)
        );
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(&text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&[row]),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Relève le plafond visé par une carte de budget et reprend le tour ou le run, ou
    /// arrête.
    async fn budget_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        clicked_topic: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        let Some(a) = s.approvals.get(&action.target).await? else {
            let _ = self
                .bot
                .answer_callback(callback_id, Some("Demande introuvable."), false)
                .await;
            return Ok(());
        };
        let (chat_id, topic_id) = self.approval_destination(&a, chat_id, clicked_topic).await;
        let scope = a.payload["scope"].as_str().unwrap_or("session").to_string();
        let spent = a.payload["spent"].as_f64().unwrap_or(0.0);
        let limit = a.payload["limit"].as_f64().unwrap_or(0.0);
        if action.action == k::BUDGET_STOP {
            let _ = self
                .bot
                .answer_callback(callback_id, Some("Arrêté"), false)
                .await;
            decide_approval(s, a.id.as_str(), &Decision::deny("telegram", None)).await?;
            return self
                .reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!("⏹ Arrêté : le plafond reste à {} $.", fmt_usd(limit)),
                )
                .await;
        }
        let amount = action.args["amount"].as_f64().unwrap_or(5.0);
        let raised = spent.max(limit) + amount;
        match scope.as_str() {
            "jour" => s.budget.raise_daily(raised).await?,
            "run" => {
                if let Some(run) = a.payload["run_id"].as_str().or(a.run_id.as_deref()) {
                    s.budget.raise_run(run, raised).await?;
                }
            }
            _ => {
                if let Some(sid) = a.session_id.as_deref() {
                    s.sessions.set_budget(sid, Some(raised)).await?;
                }
            }
        }
        let won = decide_approval(
            s,
            a.id.as_str(),
            &Decision {
                choice: format!("+{} $", fmt_usd(amount)),
                ..Decision::approve_once("telegram")
            },
        )
        .await?;
        let _ = self
            .bot
            .answer_callback(
                callback_id,
                Some(&format!("Plafond : {} $", fmt_usd(raised))),
                false,
            )
            .await;
        if !won {
            return self
                .reply(chat_id, topic_id, None, "ℹ️ Déjà tranché.")
                .await;
        }
        let note = match scope.as_str() {
            "jour" => format!(
                "💰 Plafond du jour relevé à {} $ pour aujourd'hui. Renvoie ta demande pour \
                 reprendre.",
                fmt_usd(raised)
            ),
            "run" => {
                if let Some(run) = a.payload["run_id"].as_str().or(a.run_id.as_deref())
                    && let Err(e) =
                        crate::workflow::control(d, run, &penelope_workflow::Control::Resume).await
                {
                    tracing::debug!(error = %e, "reprise du run après relèvement");
                }
                format!(
                    "💰 Plafond du run relevé à {} $ : il reprend.",
                    fmt_usd(raised)
                )
            }
            _ => {
                if let Some(sid) = a.session_id.as_deref() {
                    let origin = Origin::Telegram {
                        chat_id,
                        topic_id,
                        message_id: None,
                    };
                    d.enqueue_resume(sid, a.id.as_str(), &origin).await?;
                }
                format!(
                    "💰 Plafond de la session relevé à {} $ : je reprends.",
                    fmt_usd(raised)
                )
            }
        };
        self.reply(chat_id, topic_id, None, &note).await
    }

    /// Menu `/sessions` : un bouton par session (bascule), un « ⋯ » par session (forker,
    /// renommer, fermer), pagination, sessions fermées masquées sauf `all` (issue #14).
    /// `edit` : message à remplacer plutôt qu'un nouvel envoi.
    async fn send_sessions_menu(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        page: usize,
        all: bool,
        edit: Option<i64>,
    ) -> anyhow::Result<()> {
        const PER_PAGE: usize = 12;
        let d = &self.daemon;
        let s = &d.services;
        let current = s
            .sessions
            .find_by_topic(chat_id, topic_id)
            .await?
            .map(|x| x.id.to_string());
        let sessions: Vec<_> = s
            .sessions
            .list(Some(penelope_kernel::session::SessionKind::Chat), 1_000)
            .await?
            .into_iter()
            .filter(|x| all || x.state != "closed")
            .collect();
        let busy: std::collections::BTreeMap<String, i64> = s
            .store
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT session_id, COUNT(*) FROM turn_queue
                     WHERE state IN ('pending', 'leased') GROUP BY session_id",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        let pages = sessions.len().div_ceil(PER_PAGE).max(1);
        let page = page.min(pages - 1);
        let ttl = 24 * 3_600_000;
        let make = |label: String, action: &'static str, target: String, args: Value| async move {
            s.actions
                .create(action, &target, args, ttl, true)
                .await
                .map(|t| ButtonSpec::callback(&label, &t.token, ""))
        };
        let nav = json!({"page": page, "all": all});
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for (i, sess) in sessions
            .iter()
            .enumerate()
            .skip(page * PER_PAGE)
            .take(PER_PAGE)
        {
            let id = sess.id.to_string();
            let mut label = String::new();
            if current.as_deref() == Some(id.as_str()) {
                label.push_str("▶️ ");
            } else if sess.state == "closed" {
                label.push_str("🔒 ");
            }
            // Une session qui travaille en fond, avec sa file (issue #112).
            if let Some(n) = busy.get(&id).filter(|n| **n > 0) {
                label.push_str(&format!("⏳{n} "));
            }
            let title = crate::titles::label(sess);
            label.push_str(&title.chars().take(48).collect::<String>());
            // Son sujet de travail, qui filtre sa mémoire d'office (#119).
            if let (Some(p), _) = crate::session_project::of_session(s, &id).await {
                label.push_str(&format!(" · 📁{p}"));
            }
            // La plus récente porte aussi l'heure de sa dernière activité.
            if i == 0
                && let Some(hm) = sess.last_activity.as_deref().and_then(|t| t.get(11..16))
            {
                label.push_str(&format!(" {hm}"));
            }
            rows.push(vec![
                make(label, k::SESSION_SWITCH, id.clone(), nav.clone()).await?,
                make("⋯".into(), k::SESSION_MENU, id, nav.clone()).await?,
            ]);
        }
        let mut footer = Vec::new();
        if page > 0 {
            footer.push(
                make(
                    "« Plus récentes".into(),
                    k::SESSIONS_PAGE,
                    String::new(),
                    json!({"page": page - 1, "all": all}),
                )
                .await?,
            );
        }
        if page + 1 < pages {
            footer.push(
                make(
                    "Plus anciennes »".into(),
                    k::SESSIONS_PAGE,
                    String::new(),
                    json!({"page": page + 1, "all": all}),
                )
                .await?,
            );
        }
        footer.push(
            make(
                if all {
                    "Masquer les fermées".into()
                } else {
                    "Voir les fermées".into()
                },
                k::SESSIONS_PAGE,
                String::new(),
                json!({"page": 0, "all": !all}),
            )
            .await?,
        );
        rows.push(footer);
        let text = if sessions.is_empty() {
            "**Sessions** : aucune.".to_string()
        } else {
            format!(
                "**Sessions** ({} · page {}/{pages})\nUn clic bascule ce chat sur la session ; \
                 « ⋯ » pour forker, renommer ou fermer. ▶️ session de ce chat, ⏳ tour en cours \
                 ou en attente.",
                sessions.len(),
                page + 1
            )
        };
        let html = markdown_to_html(&text);
        let keyboard = inline_keyboard(&rows);
        if let Some(message_id) = edit {
            match self
                .bot
                .edit_text(chat_id, message_id, &html, Some(keyboard.clone()))
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if e.to_string().contains("not modified") => return Ok(()),
                Err(_) => {}
            }
        }
        self.bot
            .send_text(chat_id, topic_id, &html, Some(keyboard), None)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Boutons du menu `/sessions`.
    async fn session_menu_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let page = action.args["page"].as_u64().unwrap_or(0) as usize;
        let all = action.args["all"].as_bool().unwrap_or(false);
        let target = action.target.clone();
        let toast = match action.action.as_str() {
            k::SESSION_SWITCH => {
                let Some(sess) = s.sessions.get(&target).await? else {
                    let _ = self
                        .bot
                        .answer_callback(callback_id, Some("Session introuvable."), false)
                        .await;
                    return self
                        .send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
                        .await;
                };
                if sess.state != "active" {
                    s.sessions.set_state(&target, "active").await?;
                }
                let background = self.bind_chat(&target, chat_id, topic_id).await?;
                s.sessions.touch(&target).await?;
                let mut t = format!("Session « {} »", crate::titles::label(&sess));
                if background > 0 {
                    t.push_str(&format!(
                        " ({background} tour(s) continuent en fond ailleurs)"
                    ));
                }
                // Bouton d'une notification « réponses en attente » : pas de menu à redessiner.
                if action.args["notice"].as_bool() == Some(true) {
                    let _ = self.bot.answer_callback(callback_id, Some(&t), false).await;
                    let _ = self.bot.edit_markup(chat_id, message_id, None).await;
                    return Ok(());
                }
                Some(t)
            }
            k::SESSION_FORK => match crate::session_ops::fork(d, &target, None).await {
                Ok(v) => {
                    let fork = v["session"].as_str().unwrap_or_default().to_string();
                    self.bind_chat(&fork, chat_id, topic_id).await?;
                    s.sessions.touch(&fork).await?;
                    Some("Session dupliquée : la suite se passe dans le fork.".to_string())
                }
                Err(e) => Some(format!("Fork impossible : {e}")),
            },
            k::SESSION_CLOSE => match crate::session_ops::close(d, &target).await {
                Ok(v) => Some(format!(
                    "Session fermée{}",
                    match v["cancelled"].as_u64().unwrap_or(0) {
                        0 => String::new(),
                        n => format!(", {n} en attente annulé(s)"),
                    }
                )),
                Err(e) => Some(format!("Fermeture impossible : {e}")),
            },
            k::SESSION_RENAME => {
                d.kv_set(
                    &format!("tg.await_title.{chat_id}"),
                    &format!("{target} {}", s.clock.now_ms()),
                )
                .await?;
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        "✏️ Envoie le nouveau titre de la session en un message (dans les \
                         5 minutes).",
                    )
                    .await;
            }
            k::SESSION_MENU => {
                let _ = self.bot.answer_callback(callback_id, None, false).await;
                return self
                    .send_session_actions(chat_id, topic_id, message_id, &target, page, all)
                    .await;
            }
            _ => None,
        };
        let _ = self
            .bot
            .answer_callback(callback_id, toast.as_deref(), false)
            .await;
        self.send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
            .await
    }

    /// Sous-menu d'une session : basculer, forker, renommer, fermer, retour.
    async fn send_session_actions(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        session_id: &str,
        page: usize,
        all: bool,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let Some(sess) = s.sessions.get(session_id).await? else {
            return self
                .send_sessions_menu(chat_id, topic_id, page, all, Some(message_id))
                .await;
        };
        let ttl = 24 * 3_600_000;
        let nav = json!({"page": page, "all": all});
        let mut rows = Vec::new();
        for pair in [
            [
                ("↪️ Basculer", k::SESSION_SWITCH),
                ("🍴 Forker", k::SESSION_FORK),
            ],
            [
                ("✏️ Renommer", k::SESSION_RENAME),
                ("🔒 Fermer", k::SESSION_CLOSE),
            ],
        ] {
            let mut row = Vec::new();
            for (label, action) in pair {
                let t = s
                    .actions
                    .create(action, session_id, nav.clone(), ttl, true)
                    .await?;
                row.push(ButtonSpec::callback(label, &t.token, ""));
            }
            rows.push(row);
        }
        let back = s
            .actions
            .create(k::SESSIONS_PAGE, "", nav, ttl, true)
            .await?;
        rows.push(vec![ButtonSpec::callback("↩️ Retour", &back.token, "")]);
        let text = format!(
            "**{}**\n`{}` · {}",
            crate::titles::label(&sess),
            sess.id,
            if sess.state == "closed" {
                "fermée"
            } else {
                "active"
            }
        );
        self.bot
            .edit_text(
                chat_id,
                message_id,
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Lie une session au chat : elle a le focus. Celles qui le perdent finissent leur tour
    /// en cours, dont la sortie est mise de côté, et leur file est annulée ; la session liée
    /// reçoit ce qui l'attendait (issue #10). Renvoie le nombre de tours annulés.
    /// Donne le fil à `session_id`. La session qu'il quitte garde sa file : ses tours
    /// s'exécutent en fond et ce qu'ils produisent est retenu jusqu'au retour, comme la
    /// réponse du tour en vol (issue #112). Renvoie le nombre de tours qu'elle a encore.
    async fn bind_chat(
        &self,
        session_id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<usize> {
        let d = &self.daemon;
        let mut background = 0;
        for other in d
            .services
            .sessions
            .bind_telegram(session_id, chat_id, topic_id)
            .await?
        {
            background += d.services.turns.queued_for(&other).await? as usize;
        }
        self.flush_held(session_id).await?;
        Ok(background)
    }

    /// Vrai quand une autre session a le focus du chat : la sortie de `session_id` est
    /// alors mise de côté. Seules les sessions de conversation actives sont concernées, et un
    /// chat sans session liée reçoit tout.
    async fn out_of_focus(&self, session_id: &str, chat_id: i64, topic_id: Option<i64>) -> bool {
        let s = &self.daemon.services;
        match s.sessions.find_by_topic(chat_id, topic_id).await {
            Ok(Some(focused)) if focused.id.as_str() != session_id => {}
            _ => return false,
        }
        matches!(
            s.sessions.get(session_id).await,
            Ok(Some(sess)) if sess.kind == penelope_kernel::session::SessionKind::Chat
                && sess.state == "active"
        )
    }

    /// Met une sortie de côté et tient à jour l'unique notification de la session, avec
    /// son bouton pour basculer.
    async fn hold(
        &self,
        session_id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
        item: Held,
    ) -> anyhow::Result<()> {
        use penelope_telegram::render::escape_html;
        let _guard = self.held_lock.lock().await;
        let d = &self.daemon;
        let key = held_key(session_id);
        let mut held: Value = d
            .kv_get(&key)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| json!({"chat_id": chat_id, "topic_id": topic_id, "items": []}));
        let mut items: Vec<Held> =
            serde_json::from_value(held["items"].clone()).unwrap_or_default();
        items.push(item);
        held["items"] = serde_json::to_value(&items)?;

        let approvals = items
            .iter()
            .filter(|h| matches!(h, Held::Approval { .. }))
            .count();
        let mut parts = Vec::new();
        if items.len() > approvals {
            parts.push(count_of(items.len() - approvals, "réponse", "réponses"));
        }
        if approvals > 0 {
            parts.push(count_of(approvals, "approbation", "approbations"));
        }
        let title = d
            .services
            .sessions
            .get(session_id)
            .await?
            .and_then(|sess| sess.title)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "(sans titre)".into());
        let html = format!(
            "📬 {} en attente dans « {} »",
            parts.join(" et "),
            escape_html(&title)
        );
        let token = d
            .services
            .actions
            .create(
                k::SESSION_SWITCH,
                session_id,
                json!({"notice": true}),
                7 * 24 * 3_600_000,
                true,
            )
            .await?;
        let keyboard =
            inline_keyboard(&[vec![ButtonSpec::callback("↪️ Basculer", &token.token, "")]]);
        let edited = match held["notice"].as_i64() {
            Some(id) => self
                .bot
                .edit_text(chat_id, id, &html, Some(keyboard.clone()))
                .await
                .is_ok(),
            None => false,
        };
        if !edited {
            let mut payload = json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "disable_notification": true,
                "reply_markup": keyboard,
            });
            if let Some(t) = topic_id {
                payload["message_thread_id"] = json!(t);
            }
            let sent = self
                .bot
                .call(
                    penelope_telegram::api::method::SEND_MESSAGE,
                    Some(chat_id),
                    payload,
                )
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            held["notice"] = sent["message_id"].clone();
        }
        d.kv_set(&key, &held.to_string()).await?;
        Ok(())
    }

    /// Retour au focus : les sorties mises de côté partent dans l'ordre, approbations encore
    /// ouvertes comprises. Renvoie le nombre de sorties envoyées.
    async fn flush_held(&self, session_id: &str) -> anyhow::Result<usize> {
        let _guard = self.held_lock.lock().await;
        let d = &self.daemon;
        let key = held_key(session_id);
        let Some(held) = d
            .kv_get(&key)
            .await?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            return Ok(0);
        };
        d.kv_delete(&key).await?;
        let chat_id = held["chat_id"].as_i64().unwrap_or(self.owner_id);
        let topic_id = held["topic_id"].as_i64();
        let items: Vec<Held> = serde_json::from_value(held["items"].clone()).unwrap_or_default();
        if let Some(notice) = held["notice"].as_i64() {
            let _ = self
                .bot
                .edit_text(
                    chat_id,
                    notice,
                    "📬 Réponses mises de côté : envoyées ci-dessous.",
                    None,
                )
                .await;
        }
        for item in &items {
            match item {
                Held::Text {
                    text,
                    reply_to,
                    answer,
                    choices,
                } => {
                    if choices.is_empty() {
                        self.reply(chat_id, topic_id, *reply_to, text).await?;
                    } else {
                        self.send_choices(chat_id, topic_id, *reply_to, session_id, text, choices)
                            .await?;
                    }
                    if let (true, Some(mid)) = (answer, reply_to) {
                        self.react(chat_id, *mid, reaction::DONE);
                    }
                }
                Held::Approval { id } => {
                    if let Some(a) = d.services.approvals.get(id).await?
                        && a.state == ApprovalState::Pending
                    {
                        self.send_approval_card(chat_id, topic_id, &a).await?;
                    }
                }
                Held::Failure { error, reply_to } => {
                    self.send_failure(chat_id, topic_id, *reply_to, session_id, error, None)
                        .await?;
                }
                Held::File { path, caption } => {
                    if let Err(e) = self
                        .bot
                        .send_document(chat_id, topic_id, Path::new(path), caption.as_deref())
                        .await
                    {
                        tracing::warn!(error = %e, "fichier mis de côté non envoyé");
                    }
                }
                Held::Voice {
                    path,
                    duration_s,
                    caption,
                } => {
                    if let Err(e) = self
                        .bot
                        .send_voice(
                            chat_id,
                            topic_id,
                            Path::new(path),
                            *duration_s,
                            caption.as_deref(),
                            None,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "vocal mis de côté non envoyé");
                    }
                }
            }
        }
        Ok(items.len())
    }

    /// Écran courant d'un formulaire : le champ à remplir, ou le récapitulatif à envoyer.
    async fn send_form_step(&self, chat_id: i64, pending: &Value) -> anyhow::Result<()> {
        use penelope_telegram::forms::{FieldKind, FormState};
        let state: FormState = serde_json::from_value(pending["state"].clone())?;
        let s = &self.daemon.services;
        let ttl = 24 * 3_600_000;
        let target = chat_id.to_string();
        let button = |label: &str, action: &str, args: Value| {
            let (label, action, target) = (label.to_string(), action.to_string(), target.clone());
            async move {
                s.actions
                    .create(&action, &target, args, ttl, true)
                    .await
                    .map(|t| ButtonSpec::callback(&label, &t.token, ""))
            }
        };
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        let text = if state.done {
            rows.push(vec![
                button("✅ Envoyer", k::FORM_SUBMIT, json!({})).await?,
                button("↩️ Modifier", k::FORM_PREV, json!({})).await?,
            ]);
            let mut last = vec![button("✖️ Abandonner", k::FORM_DECLINE, json!({})).await?];
            // Élicitation MCP : refuser reste possible jusqu'à l'envoi.
            if let Some(id) = pending["elicitation"].as_str() {
                let t = s
                    .actions
                    .create(k::ELICIT_DECLINE, id, json!({}), ttl, true)
                    .await?;
                last.push(ButtonSpec::callback("🚫 Refuser", &t.token, ""));
            }
            rows.push(last);
            format!(
                "📝 « {} »\n\n{}",
                pending["choice"].as_str().unwrap_or_default(),
                state.summary()
            )
        } else {
            let Some(field) = state.current() else {
                return Ok(());
            };
            for label in field.button_labels() {
                rows.push(vec![
                    button(&label, k::FORM_NEXT, json!({"answer": label})).await?,
                ]);
            }
            let mut nav = Vec::new();
            if state.cursor > 0 {
                nav.push(button("↩️ Précédent", k::FORM_PREV, json!({})).await?);
            }
            if !field.required || state.values.contains_key(&field.name) {
                nav.push(button("⏭ Passer", k::FORM_NEXT, json!({})).await?);
            }
            nav.push(button("✖️ Abandonner", k::FORM_DECLINE, json!({})).await?);
            rows.push(nav);
            let hint = match &field.kind {
                FieldKind::Enum { multi: true, .. } => {
                    "Un bouton, ou plusieurs options séparées par des virgules."
                }
                FieldKind::Enum { .. } | FieldKind::Boolean => "Un bouton.",
                FieldKind::Number { integer: true } => "Un nombre entier, en un message.",
                FieldKind::Number { .. } => "Un nombre, en un message.",
                FieldKind::Text { .. } => "En un message.",
            };
            let current = state
                .values
                .get(&field.name)
                .map(|v| {
                    format!(
                        "\nValeur actuelle : `{}`",
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string())
                    )
                })
                .unwrap_or_default();
            format!(
                "📝 {} · **{}**{}\n{}{}{current}",
                state.progress(),
                field.title,
                if field.required { " *" } else { "" },
                if field.description.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", field.description)
                },
                hint
            )
        };
        self.bot
            .send_text(
                chat_id,
                pending["topic"].as_i64(),
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Formulaire en cours dans ce sujet. Une clé d'avant #149 (`tg.form.{chat}`, tous
    /// sujets confondus) est reprise une dernière fois, puis réécrite par sujet.
    async fn form_pending(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<Option<String>> {
        let d = &self.daemon;
        let here = d.kv_get(&form_key(chat_id, topic_id)).await?;
        if here.as_deref().is_some_and(|r| !r.is_empty()) || topic_id.is_none() {
            return Ok(here);
        }
        let Some(legacy) = d
            .kv_get(&form_key(chat_id, None))
            .await?
            .filter(|r| !r.is_empty())
        else {
            return Ok(here);
        };
        let mut pending: Value = serde_json::from_str(&legacy)?;
        pending["topic"] = json!(topic_id);
        d.kv_set(&form_key(chat_id, None), "").await?;
        d.kv_set(&form_key(chat_id, topic_id), &pending.to_string())
            .await?;
        Ok(Some(pending.to_string()))
    }

    /// Une réponse au champ courant (message ou bouton ; `None` : passer le champ).
    async fn form_input(
        &self,
        chat_id: i64,
        raw: &str,
        answer: Option<&str>,
    ) -> anyhow::Result<()> {
        use penelope_telegram::forms::FormState;
        let mut pending: Value = serde_json::from_str(raw)?;
        let topic = form_topic(&pending);
        let mut state: FormState = serde_json::from_value(pending["state"].clone())?;
        let applied = match answer {
            Some(a) => state.answer(a),
            None => state.skip(),
        };
        if let Err(e) = applied {
            // L'erreur de validation retourne là où la carte vit, et dit où répondre :
            // un formulaire ne lit que son propre sujet (issue #149).
            let here = if topic.is_some() {
                " — réponds **ici**, ou « ✖️ Abandonner »"
            } else {
                ""
            };
            self.reply(chat_id, topic, None, &format!("⚠️ {e}{here}"))
                .await?;
            return self.send_form_step(chat_id, &pending).await;
        }
        pending["state"] = serde_json::to_value(&state)?;
        self.daemon
            .kv_set(&form_key(chat_id, topic), &pending.to_string())
            .await?;
        self.send_form_step(chat_id, &pending).await
    }

    /// Boutons d'un formulaire : passer, revenir, envoyer, abandonner.
    async fn form_clicked(
        &self,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> anyhow::Result<()> {
        use penelope_telegram::forms::FormState;
        let d = &self.daemon;
        let Some(raw) = self
            .form_pending(chat_id, topic_id)
            .await?
            .filter(|r| !r.is_empty())
        else {
            return self
                .reply(chat_id, topic_id, None, "ℹ️ aucun formulaire en cours")
                .await;
        };
        let mut pending: Value = serde_json::from_str(&raw)?;
        // Le sujet du formulaire prime sur celui du clic : la carte peut être ailleurs.
        let topic_id = form_topic(&pending).or(topic_id);
        let mut state: FormState = serde_json::from_value(pending["state"].clone())?;
        match action.action.as_str() {
            k::FORM_NEXT => {
                return self
                    .form_input(chat_id, &raw, action.args["answer"].as_str())
                    .await;
            }
            k::FORM_PREV => {
                state.prev();
                pending["state"] = serde_json::to_value(&state)?;
                d.kv_set(&form_key(chat_id, topic_id), &pending.to_string())
                    .await?;
                self.send_form_step(chat_id, &pending).await
            }
            k::FORM_DECLINE => {
                d.kv_set(&form_key(chat_id, topic_id), "").await?;
                if pending["workflow"].is_string() || pending["prompt"].is_object() {
                    return self
                        .reply(
                            chat_id,
                            None,
                            None,
                            "✖️ Formulaire abandonné : rien n'est lancé.",
                        )
                        .await;
                }
                if let Some(id) = pending["elicitation"].as_str() {
                    return self
                        .finish_elicitation(
                            chat_id,
                            id,
                            crate::elicitation::Action::Cancel,
                            "✖️ Formulaire abandonné : `{server}` reçoit une annulation.",
                            true,
                            None,
                        )
                        .await;
                }
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    "✖️ Formulaire abandonné : la question du workflow reste ouverte \
                     (`/runs`).",
                )
                .await
            }
            _ => {
                let values = match state.submit() {
                    Ok(v) => v,
                    Err(e) => {
                        self.reply(chat_id, topic_id, None, &format!("⚠️ {e}"))
                            .await?;
                        return self.send_form_step(chat_id, &pending).await;
                    }
                };
                d.kv_set(&form_key(chat_id, topic_id), "").await?;
                // Paramètres d'un workflow lancé depuis `/wf` ou `/run` (issue #30).
                if let Some(workflow) = pending["workflow"].as_str() {
                    let origin = Origin::Telegram {
                        chat_id,
                        topic_id,
                        message_id: None,
                    };
                    let note =
                        match crate::workflow::start_run(d, workflow, values, &origin, None, 0)
                            .await
                        {
                            Ok(run) => format!(
                                "▶️ Run `{}` lancé (« {} »).",
                                run.id,
                                pending["choice"].as_str().unwrap_or(workflow)
                            ),
                            Err(e) => format!("❌ {e}"),
                        };
                    return self.reply(chat_id, topic_id, None, &note).await;
                }
                // Arguments d'un prompt MCP (`/p`).
                if let (Some(server), Some(prompt)) = (
                    pending["prompt"]["server"].as_str(),
                    pending["prompt"]["name"].as_str(),
                ) {
                    let note = match self
                        .run_mcp_prompt(chat_id, topic_id, server, prompt, values)
                        .await
                    {
                        Ok(n) => {
                            format!("💬 Prompt `{prompt}` : {n} message(s) envoyé(s) au modèle.")
                        }
                        Err(e) => format!("❌ {e}"),
                    };
                    return self.reply(chat_id, topic_id, None, &note).await;
                }
                if let Some(id) = pending["elicitation"].as_str() {
                    return self
                        .finish_elicitation(
                            chat_id,
                            id,
                            crate::elicitation::Action::Accept(Some(values)),
                            "✔️ Formulaire envoyé à `{server}`.",
                            true,
                            None,
                        )
                        .await;
                }
                let note = match crate::workflow::answer(
                    d,
                    pending["run"].as_str().unwrap_or_default(),
                    pending["visit"].as_str().unwrap_or_default(),
                    pending["choice"].as_str().unwrap_or_default(),
                    Some(&values.to_string()),
                )
                .await
                {
                    Ok(()) => "✔️ Formulaire transmis au workflow.".to_string(),
                    Err(e) => format!("ℹ️ {e}"),
                };
                self.reply(chat_id, topic_id, None, &note).await
            }
        }
    }

    // ============================================================ accueil

    async fn propose_onboarding(&self, chat_id: i64, topic_id: Option<i64>) -> anyhow::Result<()> {
        let t = self
            .daemon
            .services
            .actions
            .create(k::ONBOARD_START, "", json!({}), 7 * 24 * 3_600_000, true)
            .await?;
        let rows = vec![vec![ButtonSpec::callback(
            "📋 Commencer l'accueil",
            &t.token,
            "",
        )]];
        self.bot
            .send_text(
                chat_id,
                topic_id,
                &markdown_to_html(
                    "👋 Ton profil est encore vide. Neuf questions (rôle, projets, outils, style, \
                     limites) et je te connais dès aujourd'hui ; chacune peut être passée.",
                ),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// `/accueil [partie]` : reprend la séance en cours ou en ouvre une (issue #21).
    async fn onboarding_next(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        part: Option<crate::onboarding::Part>,
    ) -> anyhow::Result<()> {
        let sitting = crate::onboarding::start(&self.daemon, part).await?;
        self.onboarding_ask(chat_id, topic_id, &sitting).await
    }

    /// Pose la question suivante, ou montre le récapitulatif à valider.
    async fn onboarding_ask(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        sitting: &crate::onboarding::Sitting,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let key = format!("tg.onboard.{chat_id}");
        let ttl = 7 * 24 * 3_600_000;
        let button = |label: String, action: &'static str, args: Value| {
            let rel = sitting.rel.clone();
            async move {
                s.actions
                    .create(action, &rel, args, ttl, true)
                    .await
                    .map(|t| ButtonSpec::callback(&label, &t.token, ""))
            }
        };
        let Some(q) = sitting.next() else {
            d.kv_set(&key, "").await?;
            let plan = crate::onboarding::plan(d, sitting).await?;
            if plan.is_empty() && plan.keep.is_empty() {
                crate::onboarding::cancel(d).await?;
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        "Aucune réponse à retenir : rien n'est écrit.",
                    )
                    .await;
            }
            let rows = vec![vec![
                button("✅ Écrire".into(), k::ONBOARD_WRITE, json!({})).await?,
                button("✖️ Annuler".into(), k::ONBOARD_CANCEL, json!({})).await?,
            ]];
            return self
                .bot
                .send_text(
                    chat_id,
                    topic_id,
                    &markdown_to_html(&crate::onboarding::plan_text(&plan)),
                    Some(inline_keyboard(&rows)),
                    None,
                )
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!(e.to_string()));
        };
        d.kv_set(&key, &json!({"rel": sitting.rel, "n": q.n}).to_string())
            .await?;
        let (i, total) = sitting.position(q.n);
        let mut hint = q.hint.to_string();
        if q.n == 4
            && let Some(sup) = d.hooks.mcp_supervisor()
        {
            let servers: Vec<String> = sup.statuses().await.into_iter().map(|st| st.name).collect();
            if !servers.is_empty() {
                hint.push_str(&format!(" Serveurs MCP déclarés : {}.", servers.join(", ")));
            }
        }
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        if !q.choices.is_empty() {
            let mut row = Vec::new();
            for c in q.choices {
                row.push(
                    button(
                        c.to_string(),
                        k::ONBOARD_ANSWER,
                        json!({"n": q.n, "answer": c}),
                    )
                    .await?,
                );
            }
            rows.push(row);
        }
        rows.push(vec![
            button(
                "⏭ Passer".into(),
                k::ONBOARD_ANSWER,
                json!({"n": q.n, "answer": null}),
            )
            .await?,
            button("⏸ Plus tard".into(), k::ONBOARD_PAUSE, json!({})).await?,
        ]);
        let mut text = format!("📋 **Accueil · {i}/{total}**\n\n{}", q.text);
        if !hint.trim().is_empty() {
            text.push_str(&format!("\n_{}_", hint.trim()));
        }
        if q.choices.is_empty() {
            text.push_str("\n\nRéponds en un message.");
        }
        self.bot
            .send_text(
                chat_id,
                topic_id,
                &markdown_to_html(&text),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Boutons de l'accueil.
    async fn onboarding_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let _ = self.bot.answer_callback(callback_id, None, false).await;
        let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        let rel = action.target.as_str();
        match action.action.as_str() {
            k::ONBOARD_START => self.onboarding_next(chat_id, topic_id, None).await,
            k::ONBOARD_ANSWER => {
                let n = action.args["n"].as_u64().unwrap_or(0) as u32;
                match crate::onboarding::answer(d, rel, n, action.args["answer"].as_str()).await {
                    Ok(sitting) => self.onboarding_ask(chat_id, topic_id, &sitting).await,
                    Err(e) => {
                        self.reply(chat_id, topic_id, None, &format!("⚠️ {e}"))
                            .await
                    }
                }
            }
            k::ONBOARD_PAUSE => {
                d.kv_set(&format!("tg.onboard.{chat_id}"), "").await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    "⏸ Accueil en pause : `/accueil` reprend à la première question sans réponse.",
                )
                .await
            }
            k::ONBOARD_WRITE => {
                let Some(sitting) = crate::onboarding::load(d, rel) else {
                    return self
                        .reply(chat_id, topic_id, None, "ℹ️ séance d'accueil introuvable")
                        .await;
                };
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: None,
                };
                let session = d.chat_session_for(&origin).await?;
                let (added, replaced) = crate::onboarding::write(d, &sitting, &session).await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!(
                        "✅ Accueil enregistré : {added} ajout(s), {replaced} remplacement(s) \
                         dans `profil.md` et `memoire.md`. `/accueil limites` (ou profil, \
                         outils, style) pour revenir sur une partie."
                    ),
                )
                .await
            }
            _ => {
                crate::onboarding::cancel(d).await?;
                d.kv_set(&format!("tg.onboard.{chat_id}"), "").await?;
                self.reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!("✖️ Rien n'est écrit ; la séance reste lisible dans `{rel}`."),
                )
                .await
            }
        }
    }

    // ============================================================ élicitation MCP

    /// Texte d'une carte d'élicitation : le serveur est nommé, son message cité et échappé,
    /// un lien montré en entier avec son domaine (§8.4, issue #12).
    fn elicitation_html(r: &crate::elicitation::Request) -> String {
        use crate::elicitation::Kind;
        use penelope_telegram::render::escape_html;
        let server = escape_html(&r.server);
        let quote = match r.message.trim() {
            "" => String::new(),
            m => format!("\n\n<blockquote>{}</blockquote>", escape_html(m)),
        };
        match &r.kind {
            Kind::Form { schema } if r.field_count() > 0 => {
                let fields = penelope_telegram::forms::fields_from_schema(schema)
                    .map(|f| {
                        f.iter()
                            .map(|f| format!("{}{}", f.title, if f.required { " *" } else { "" }))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                format!(
                    "📝 <b>Le serveur MCP <code>{server}</code> demande des informations</b>\
                     {quote}\nChamps : {}",
                    escape_html(&fields)
                )
            }
            Kind::Form { .. } => format!(
                "🔐 <b>Le serveur MCP <code>{server}</code> demande ta confirmation</b>{quote}"
            ),
            Kind::Url { url, .. } => {
                let host = r.host().unwrap_or_default();
                let warning = if host.split('.').any(|l| l.starts_with("xn--")) {
                    "\n⚠️ Domaine en Punycode : ses caractères peuvent imiter un autre site."
                } else {
                    ""
                };
                format!(
                    "🌐 <b>Le serveur MCP <code>{server}</code> demande d'ouvrir un lien</b>\
                     {quote}\nDomaine : <b>{}</b>{warning}\n<code>{}</code>",
                    escape_html(&host),
                    escape_html(url)
                )
            }
        }
    }

    async fn elicit_button(
        &self,
        label: &str,
        action: &str,
        r: &crate::elicitation::Request,
    ) -> anyhow::Result<ButtonSpec> {
        let ttl = r.timeout.as_millis() as i64 + 3_600_000;
        let t = self
            .daemon
            .services
            .actions
            .create(action, &r.id, json!({}), ttl, true)
            .await?;
        Ok(ButtonSpec::callback(label, &t.token, ""))
    }

    /// Remplace la carte (texte d'origine, puis l'issue) ; à défaut, un nouveau message.
    async fn elicitation_update(
        &self,
        r: &crate::elicitation::Request,
        card: Option<i64>,
        note: &str,
        keyboard: Option<Value>,
    ) -> anyhow::Result<()> {
        let html = format!(
            "{}\n\n{}",
            Self::elicitation_html(r),
            markdown_to_html(note)
        );
        let (chat_id, topic_id) = self.elicitation_chat(r).await;
        if let Some(id) = card
            && self
                .bot
                .edit_text(chat_id, id, &html, keyboard.clone())
                .await
                .is_ok()
        {
            return Ok(());
        }
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": keyboard,
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Bouton « Relancer » d'une demande annulée.
    async fn elicit_retry_button(
        &self,
        r: &crate::elicitation::Request,
        session: &str,
    ) -> Option<ButtonSpec> {
        let t = self
            .daemon
            .services
            .actions
            .create(
                k::ELICIT_RETRY,
                &r.id,
                json!({
                    "session": session,
                    "server": r.server,
                    "what": crate::elicitation::first_line(&r.message),
                }),
                24 * 3_600_000,
                true,
            )
            .await
            .ok()?;
        Some(ButtonSpec::callback("🔄 Relancer", &t.token, ""))
    }

    /// Relance demandée depuis le message d'annulation : le texte repart comme un
    /// message du propriétaire dans **sa** session, et le modèle refait l'appel (#143).
    async fn elicitation_retry_clicked(
        &self,
        action: &penelope_telegram::actions::Action,
        chat_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let session = action.args["session"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let server = action.args["server"].as_str().unwrap_or("le serveur");
        let what = action.args["what"].as_str().unwrap_or_default();
        if session.is_empty() {
            let (c, t) = self.home_chat();
            return self
                .reply(
                    c,
                    t,
                    None,
                    "Cette demande n'a pas de conversation à relancer.",
                )
                .await
                .map(|_| ());
        }
        let topic_id = d
            .services
            .sessions
            .get(&session)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.tg_topic_id);
        let text = format!(
            "Relance la demande `{server}` qui a expiré{}. Cette fois je réponds tout de \
             suite à la confirmation.",
            if what.is_empty() {
                String::new()
            } else {
                format!(" ({what})")
            }
        );
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        d.enqueue_message(&session, &text, &origin, None).await?;
        Ok(())
    }

    /// Où poser une carte d'élicitation : la conversation de l'appel, sinon le foyer du
    /// propriétaire (issue #143).
    async fn elicitation_chat(&self, r: &crate::elicitation::Request) -> (i64, Option<i64>) {
        match r.to.chat_id {
            Some(chat_id) => (chat_id, r.to.topic_id),
            None => self.home_chat(),
        }
    }

    /// Répond au serveur, met la carte à jour et le dit dans le chat. `note` : `{server}`
    /// est remplacé par le nom du serveur.
    /// `card` : le message à modifier en place. La file ne rend pas d'identifiant à
    /// l'envoi (issue #143) : celui du message cliqué fait l'affaire, et l'issue reste
    /// écrite là où la carte se trouve.
    async fn finish_elicitation(
        &self,
        chat_id: i64,
        id: &str,
        answer: crate::elicitation::Action,
        note: &str,
        echo: bool,
        card: Option<i64>,
    ) -> anyhow::Result<()> {
        let broker = &self.daemon.services.elicitations;
        let card = broker.request(id).and_then(|(_, c)| c).or(card);
        match broker.resolve(id, answer) {
            Ok(request) => {
                let topic = self.elicitation_chat(&request).await.1;
                if let Some(raw) = self.form_pending(chat_id, topic).await?
                    && serde_json::from_str::<Value>(&raw)
                        .is_ok_and(|p| p["elicitation"].as_str() == Some(id))
                {
                    self.daemon.kv_set(&form_key(chat_id, topic), "").await?;
                }
                let note = note.replace("{server}", &request.server);
                self.elicitation_update(&request, card, &note, None).await?;
                // La carte est plus haut dans le chat : l'issue est redite en bas.
                if echo && card.is_some() {
                    self.reply(chat_id, None, None, &note).await?;
                }
                Ok(())
            }
            Err(e) => self.reply(chat_id, None, None, &format!("ℹ️ {e}")).await,
        }
    }

    /// Boutons d'une carte d'élicitation.
    async fn elicitation_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        message_id: i64,
    ) -> anyhow::Result<()> {
        use crate::elicitation::{Action as Answer, Kind};
        let broker = self.daemon.services.elicitations.clone();
        let Some((request, card)) = broker.request(&action.target) else {
            let _ = self
                .bot
                .answer_callback(callback_id, Some("Demande expirée ou déjà traitée."), false)
                .await;
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
            return Ok(());
        };
        let _ = self.bot.answer_callback(callback_id, None, false).await;
        let elsewhere = card != Some(message_id);
        if elsewhere {
            // Bouton d'un autre message (récapitulatif du formulaire) : il a servi.
            let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        }
        let card = card.or(Some(message_id));
        let (answer, note) = match action.action.as_str() {
            k::ELICIT_DECLINE => (Answer::Decline, "🚫 Refusé : `{server}` en est informé."),
            k::ELICIT_CANCEL => (Answer::Cancel, "✖️ Annulé : `{server}` en est informé."),
            k::ELICIT_DONE => (Answer::Accept(None), "✅ Terminé : `{server}` reprend."),
            _ => match &request.kind {
                Kind::Form { schema } if request.field_count() > 0 => {
                    let state =
                        match penelope_telegram::forms::FormState::new(&request.id, schema.clone())
                        {
                            Ok(st) => st,
                            Err(e) => {
                                let _ = broker.resolve(&request.id, Answer::Cancel);
                                return self
                                    .elicitation_update(
                                        &request,
                                        card,
                                        &format!(
                                            "❌ Formulaire illisible ({e}) : demande annulée."
                                        ),
                                        None,
                                    )
                                    .await;
                            }
                        };
                    let title: String = request.message.chars().take(80).collect();
                    // Le formulaire vit là où la carte est arrivée (#143), pas dans le
                    // chat tout entier (#149).
                    let topic = self.elicitation_chat(&request).await.1;
                    let pending = json!({
                        "elicitation": request.id,
                        "choice": format!("{} · {title}", request.server),
                        "state": state,
                        "topic": topic,
                        "since": self.daemon.services.clock.now_rfc3339(),
                    });
                    self.daemon
                        .kv_set(&form_key(chat_id, topic), &pending.to_string())
                        .await?;
                    let rows = vec![vec![
                        self.elicit_button("🚫 Refuser", k::ELICIT_DECLINE, &request)
                            .await?,
                        self.elicit_button("✖️ Annuler", k::ELICIT_CANCEL, &request)
                            .await?,
                    ]];
                    self.elicitation_update(
                        &request,
                        card,
                        "📝 Formulaire en cours ci-dessous.",
                        Some(inline_keyboard(&rows)),
                    )
                    .await?;
                    return self.send_form_step(chat_id, &pending).await;
                }
                Kind::Form { .. } => (
                    Answer::Accept(Some(json!({}))),
                    "✅ Accepté : `{server}` continue.",
                ),
                Kind::Url {
                    url,
                    elicitation_id,
                } => {
                    let host = request.host().unwrap_or_default();
                    let open = ButtonSpec::url(&format!("🌐 Ouvrir {host}"), url);
                    if elicitation_id.is_none() {
                        // MRTR : le serveur reprend quand le propriétaire a terminé.
                        let rows = vec![
                            vec![open],
                            vec![
                                self.elicit_button("✅ J'ai terminé", k::ELICIT_DONE, &request)
                                    .await?,
                                self.elicit_button("✖️ Annuler", k::ELICIT_CANCEL, &request)
                                    .await?,
                            ],
                        ];
                        return self
                            .elicitation_update(
                                &request,
                                card,
                                "Ouvre le lien, puis « J'ai terminé ».",
                                Some(inline_keyboard(&rows)),
                            )
                            .await;
                    }
                    if let Err(e) = broker.resolve(&request.id, Answer::Accept(None)) {
                        return self.reply(chat_id, None, None, &format!("ℹ️ {e}")).await;
                    }
                    return self
                        .elicitation_update(
                            &request,
                            card,
                            &format!(
                                "✅ Accepté : ouvre le lien, `{}` signalera la fin.",
                                request.server
                            ),
                            Some(inline_keyboard(&[vec![open]])),
                        )
                        .await;
                }
            },
        };
        self.finish_elicitation(chat_id, &request.id, answer, note, elsewhere, card)
            .await
    }

    /// Échec d'un tour, avec un bouton « Réessayer » qui relance la réponse sur le même
    /// transcript.
    async fn send_failure(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        session_id: &str,
        error: &str,
        turn_cost: Option<f64>,
    ) -> anyhow::Result<()> {
        let token = self
            .daemon
            .services
            .actions
            .create(
                k::REGENERATE,
                session_id,
                json!({"topic_id": topic_id}),
                RETRY_TTL_MS,
                true,
            )
            .await?;
        let error: String = error.chars().take(3_500).collect();
        // Plafond d'appels atteint : ce n'est pas un échec, et le même bouton continue
        // avec tout ce qui est déjà fait (issue #139).
        let (text, label) = if error.starts_with(crate::agent::CALLS_EXHAUSTED) {
            let n = crate::agent::TURN_CALLS;
            (
                format!(
                    "⏸ J'ai utilisé mes {n} appels pour ce tour et je m'arrête là. « Continuer » \
                     m'en redonne {n} : je reprends avec ce que j'ai déjà fait, dernier résultat \
                     compris.{} Pour une tâche longue, je peux aussi déléguer à un sous-agent.",
                    turn_cost
                        .filter(|c| *c > 0.0)
                        .map(|c| format!(" Coût de ce tour : {c:.2} $."))
                        .unwrap_or_default()
                ),
                format!("▶️ Continuer ({n} appels de plus)"),
            )
        } else {
            (format!("❌ {error}"), "🔁 Réessayer".to_string())
        };
        let mut payload = json!({
            "chat_id": chat_id,
            "text": markdown_to_html(&text),
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
            "reply_markup": inline_keyboard(&[vec![ButtonSpec::callback(
                &label,
                &token.token,
                "",
            )]]),
        });
        if let Some(t) = topic_id {
            payload["message_thread_id"] = json!(t);
        }
        if let Some(r) = reply_to {
            payload["reply_parameters"] =
                json!({"message_id": r, "allow_sending_without_reply": true});
        }
        self.outbox_push(chat_id, topic_id, "sendMessage", payload)
            .await
    }

    /// Bouton « Réessayer » : relance, sauf si la conversation a continué depuis.
    async fn retry_clicked(&self, action: &Action, chat_id: i64) -> anyhow::Result<()> {
        let d = &self.daemon;
        let session = action.target.clone();
        let topic_id = action.args["topic_id"].as_i64();
        let answered = d
            .services
            .context
            .history
            .last_entry(&session)
            .await?
            .is_some_and(|e| {
                e.message.role == penelope_llm::types::Role::Assistant
                    && e.message.tool_calls.is_empty()
            });
        if answered {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    None,
                    "La conversation a continué depuis cet échec : rien à relancer.",
                )
                .await;
        }
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        d.enqueue_retry(&session, &origin, &action.token).await?;
        Ok(())
    }

    /// Carte `mcp_oauth_required` : bouton d'autorisation, collage, relance (§8.5).
    async fn send_oauth_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        server: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let Some(cfg) = (match d.hooks.mcp_supervisor() {
            Some(sup) => sup.config_of(server).await,
            None => None,
        }) else {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!("Serveur MCP `{server}` inconnu (`/mcp`)."),
                )
                .await;
        };
        let start = match crate::mcp_auth::start(d, &cfg, None).await {
            Ok(st) => st,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        &format!("🔐 Autorisation impossible : {e}"),
                    )
                    .await;
            }
        };
        let ttl = crate::mcp_auth::REQUEST_TTL_MS;
        let mut tokens = BTreeMap::new();
        for action in [k::OAUTH_PASTED, k::OAUTH_RETRY] {
            let t = s
                .actions
                .create(action, server, json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let mut vars = BTreeMap::new();
        vars.insert("serveur".into(), server.to_string());
        vars.insert(
            "scopes".into(),
            if start.scopes.is_empty() {
                "par défaut".into()
            } else {
                start.scopes.join(" ")
            },
        );
        vars.insert(
            "mode".into(),
            if start.mode == "paste_back" {
                "si la page finit sur une erreur 127.0.0.1, colle son adresse ici (10 min)".into()
            } else {
                "retour automatique".into()
            },
        );
        vars.insert("url".into(), start.url.clone());
        let tpl = s
            .templates
            .get("mcp_oauth_required")
            .ok_or_else(|| anyhow::anyhow!("gabarit mcp_oauth_required absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rendered.buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    /// Carte `memory_proposal` : les faits proposés, « Tout » ou « Rien ».
    async fn send_memory_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let items: Vec<String> = a.payload["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|i| i.as_str().map(|t| format!("- {t}")))
            .collect();
        let mut vars = BTreeMap::new();
        vars.insert(
            "items".into(),
            format!(
                "{}\n\nSource : `{}`",
                items.join("\n"),
                a.payload["source"].as_str().unwrap_or("?")
            ),
        );
        let ttl = 7 * 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [k::MEMORY_ACCEPT, k::MEMORY_AS_EXCEPTION, k::MEMORY_REJECT] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("memory_proposal")
            .ok_or_else(|| anyhow::anyhow!("gabarit memory_proposal absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // Un fait tiré d'un document n'a pas de contexte d'exception : « Tout » ou
        // « Rien ». Une contradiction, si : elle garde les trois boutons (issue #145).
        let clash = a.payload["contradiction"].as_bool() == Some(true);
        let buttons: Vec<Vec<ButtonSpec>> = rendered
            .buttons
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|b| clash || !b.label.contains("exception"))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    async fn send_destructive_confirm(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let confirm = s
            .actions
            .create(
                k::CONFIRM_DESTRUCTIVE,
                a.id.as_str(),
                json!({}),
                600_000,
                true,
            )
            .await?;
        let deny = s
            .actions
            .create(k::DENY, a.id.as_str(), json!({}), 600_000, true)
            .await?;
        let buttons = vec![vec![
            ButtonSpec::callback("⚠️ Confirmer", &confirm.token, "danger"),
            ButtonSpec::callback("Annuler", &deny.token, ""),
        ]];
        let text = format!(
            "⚠️ **Seconde confirmation**\n\n`{}` est une action destructive. Confirmer ?",
            a.subject
        );
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(&text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }

    // ================================================================ envoi

    /// Répond en Markdown, découpé en fragments, par la file d'envoi.
    pub async fn reply(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        reply_to: Option<i64>,
        markdown: &str,
    ) -> anyhow::Result<()> {
        let body = if markdown.trim().is_empty() {
            "(réponse vide)".to_string()
        } else {
            markdown.to_string()
        };
        let fragments = penelope_telegram::split_message(&body, FRAGMENT_CHARS);
        for (i, fragment) in fragments.iter().enumerate() {
            let mut payload = json!({
                "chat_id": chat_id,
                "text": markdown_to_html(fragment),
                "parse_mode": "HTML",
                "link_preview_options": {"is_disabled": true},
            });
            if let Some(t) = topic_id {
                payload["message_thread_id"] = json!(t);
            }
            if let (0, Some(r)) = (i, reply_to) {
                payload["reply_parameters"] =
                    json!({"message_id": r, "allow_sending_without_reply": true});
            }
            self.outbox_push(chat_id, topic_id, "sendMessage", payload)
                .await?;
        }
        Ok(())
    }

    /// Envoie un texte, ou le joint en document quand il déborderait en chapelet de
    /// messages (§14.1, `telegram.max_fragments`). Le digest du matin s'en sert : six
    /// messages dont un qui coupe un identifiant en deux ne se lisent pas (issue #145).
    pub async fn reply_or_document(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        markdown: &str,
    ) -> anyhow::Result<()> {
        let fragments = penelope_telegram::split_message(markdown, FRAGMENT_CHARS);
        let max_fragments = self.daemon.services.config.config().telegram.max_fragments;
        if max_fragments > 0
            && penelope_telegram::render::should_send_as_document(fragments.len(), max_fragments)
            && let Some(first) = fragments.first()
            && self
                .send_long_as_document(chat_id, topic_id, markdown, first)
                .await
                .is_ok()
        {
            return Ok(());
        }
        self.reply(chat_id, topic_id, None, markdown).await
    }

    /// Texte trop long pour une bulle : un fichier joint, et une ligne qui dit ce que
    /// c'est (issue #145). Le fichier vit dans le répertoire de données, pas dans `/tmp`.
    async fn send_long_as_document(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        body: &str,
        head: &str,
    ) -> anyhow::Result<()> {
        let dirs = &self.daemon.services.platform.dirs;
        let dir = dirs.data().join("outgoing");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "penelope-{}.md",
            self.daemon.services.clock.now_rfc3339().replace(':', "-")
        ));
        std::fs::write(&path, penelope_observe::redact(body))?;
        let caption: String = head
            .lines()
            .next()
            .unwrap_or("Rapport")
            .chars()
            .take(180)
            .collect();
        // Un document part en `multipart`, pas en JSON : il ne passe pas par la file.
        let sent = self
            .bot
            .send_document(
                chat_id,
                topic_id,
                &path,
                Some(&format!("{caption} (texte complet en pièce jointe)")),
            )
            .await;
        // Telegram garde le fichier : le nôtre n'a plus de raison de traîner.
        let _ = std::fs::remove_file(&path);
        sent.map(|_| ()).map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    async fn outbox_push(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        method: &str,
        mut payload: Value,
    ) -> anyhow::Result<()> {
        if let Some(o) = payload.as_object_mut() {
            o.retain(|_, v| !v.is_null());
            // Ce qui part sur Telegram et reste dans la file est rédigé comme les
            // journaux : clés, mots de passe, valeurs lues (issue #134).
            for k in ["text", "caption"] {
                if let Some(v) = o.get_mut(k)
                    && let Some(t) = v.as_str()
                {
                    let red = penelope_observe::redact(t);
                    // Un message d'échec ne se lit pas sur cinq bulles, et une erreur
                    // bavarde recopie ce qu'elle n'aurait pas dû voir : le détail reste
                    // au journal (issue #148).
                    let cut = shorten_failure(&red);
                    if cut != t {
                        if cut != red {
                            tracing::warn!(error = %red, "échec tronqué avant envoi");
                        }
                        *v = Value::String(cut);
                    }
                }
            }
        }
        let s = &self.daemon.services;
        let (id, method, now) = (
            format!("o_{}", penelope_kernel::ids::Ulid::new()),
            method.to_string(),
            s.clock.now_rfc3339(),
        );
        s.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, topic_id, method, payload, state,
                        attempts, created_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6)",
                    params![id, chat_id, topic_id, method, payload.to_string(), now],
                )?;
                Ok(())
            })
            .await?;
        self.outbox_wake.notify_one();
        Ok(())
    }

    async fn outbox_loop(self: Arc<Self>) {
        while !self.shutting_down() {
            match self.flush_outbox().await {
                Ok(0) => {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(1), self.outbox_wake.notified())
                            .await;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "file d'envoi Telegram");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    /// Envoie ce qui est dû, dans l'ordre. Renvoie le nombre de lignes traitées.
    pub async fn flush_outbox(&self) -> anyhow::Result<usize> {
        let s = &self.daemon.services;
        let now = s.clock.now_rfc3339();
        let rows: Vec<(String, i64, String, String, i64)> = s
            .store
            .read(move |c| {
                // Tête de file par chat (issue #101) : un message qui attend sa nouvelle
                // tentative retient ceux qui le suivent dans le même chat, sinon la
                // réponse se lit dans le désordre. Les autres chats passent.
                let mut st = c.prepare(
                    "SELECT o.id, o.chat_id, o.method, o.payload, o.attempts FROM tg_outbox o
                     WHERE o.state = 'pending' AND (o.not_before IS NULL OR o.not_before <= ?1)
                       AND NOT EXISTS (
                         SELECT 1 FROM tg_outbox p
                         WHERE p.chat_id = o.chat_id AND p.state = 'pending'
                           AND p.not_before > ?1
                           AND (p.created_at < o.created_at
                                OR (p.created_at = o.created_at AND p.rowid < o.rowid)))
                     ORDER BY o.created_at, o.rowid LIMIT 25",
                )?;
                let rows = st.query_map([now], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?;
        let n = rows.len();
        let mut held: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for (id, chat_id, method, payload, attempts) in rows {
            // Un envoi de ce chat a échoué pendant ce passage : la suite attend.
            if held.contains(&chat_id) {
                continue;
            }
            let body: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            let result = match self.bot.call(&method, Some(chat_id), body.clone()).await {
                Err(TgError::Api {
                    code: 400,
                    description,
                }) if description.to_lowercase().contains("parse")
                    && body.get("parse_mode").is_some() =>
                {
                    // Telegram refuse le HTML : même contenu, en texte brut.
                    let mut plain = body.clone();
                    if let Some(o) = plain.as_object_mut() {
                        o.remove("parse_mode");
                        let t = o
                            .get("text")
                            .and_then(|t| t.as_str())
                            .map(html_to_plain)
                            .unwrap_or_default();
                        o.insert("text".into(), json!(t));
                    }
                    self.bot.call(&method, Some(chat_id), plain).await
                }
                other => other,
            };
            let now = s.clock.now_rfc3339();
            match result {
                Ok(v) => {
                    let message_id = v.get("message_id").and_then(|m| m.as_i64());
                    s.store
                        .write(move |tx| {
                            tx.execute(
                                "UPDATE tg_outbox SET state='sent', sent_at=?2, message_id=?3,
                                    attempts=attempts+1 WHERE id=?1",
                                params![id, now, message_id],
                            )?;
                            Ok(())
                        })
                        .await?;
                }
                Err(e) => {
                    let attempts = attempts + 1;
                    let give_up = attempts >= MAX_ATTEMPTS
                        || matches!(e, TgError::Api { code, .. } if (400..500).contains(&code) && code != 429);
                    let delay_ms = penelope_telegram::api::backoff_ms(attempts as u32) as i64;
                    let not_before =
                        chrono::DateTime::from_timestamp_millis(s.clock.now_ms() + delay_ms)
                            .unwrap_or_default()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    let err = e.to_string();
                    tracing::warn!(error = %err, attempts, give_up, "envoi Telegram en échec");
                    let row = id.clone();
                    let stored = err.clone();
                    s.store
                        .write(move |tx| {
                            tx.execute(
                                "UPDATE tg_outbox SET attempts=?2, error=?3, not_before=?4,
                                    state = CASE WHEN ?5 = 1 THEN 'failed' ELSE state END
                                 WHERE id=?1",
                                params![row, attempts, stored, not_before, give_up as i64],
                            )?;
                            Ok(())
                        })
                        .await?;
                    if !give_up {
                        held.insert(chat_id);
                    } else if !id.starts_with(FAILURE_NOTE) {
                        // Jamais de silence (#71) : le propriétaire sait qu'un message
                        // n'est pas parti, en texte brut, sans rien qui puisse être refusé
                        // à nouveau. Une note qui échoue n'en appelle pas une autre.
                        self.push_failure_note(chat_id, &err).await;
                    }
                }
            }
        }
        Ok(n)
    }

    /// Note d'échec définitif d'un envoi, en texte brut (issue #101).
    async fn push_failure_note(&self, chat_id: i64, error: &str) {
        let s = &self.daemon.services;
        let id = format!("{FAILURE_NOTE}{}", penelope_kernel::ids::Ulid::new());
        let text = format!(
            "⚠️ Un message n'a pas pu être envoyé ({}). Le détail est dans le journal du \
             daemon ; `/status` compte ces échecs.",
            error.chars().take(200).collect::<String>()
        );
        let payload = json!({"chat_id": chat_id, "text": text}).to_string();
        let now = s.clock.now_rfc3339();
        let _ = s
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                     VALUES(?1, ?2, 'sendMessage', ?3, 'pending', ?4)",
                    params![id, chat_id, payload, now],
                )?;
                Ok(())
            })
            .await;
    }

    /// Réaction d'état sur le message du propriétaire, sans jamais bloquer.
    fn react(&self, chat_id: i64, message_id: i64, emoji: &'static str) {
        let bot = self.bot.clone();
        tokio::spawn(async move {
            let _ = bot.set_reaction(chat_id, message_id, emoji).await;
        });
    }

    // ================================================================ brouillons

    async fn draft_loop(self: Arc<Self>) {
        struct Draft {
            chat_id: i64,
            topic_id: Option<i64>,
            draft_id: i64,
            text: String,
            last: Instant,
            /// Dernière vérification du focus : une session quittée cesse d'écrire.
            checked: Instant,
            /// Brouillon en vol : un seul à la fois, le suivant porte le dernier texte
            /// (issue #70).
            in_flight: Option<tokio::task::JoinHandle<()>>,
            /// Texte du dernier brouillon effectivement envoyé.
            sent: String,
        }
        const FOCUS_EVERY: Duration = Duration::from_secs(2);
        let mut rx = self.daemon.bus.subscribe();
        let mut drafts: HashMap<String, Draft> = HashMap::new();
        while !self.shutting_down() {
            let ev = match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Err(_) => continue,
                Ok(Ok(ev)) => ev,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
            };
            let Some((chat_id, topic_id)) = ev.origin.telegram_chat() else {
                continue;
            };
            // Indicateur d'activité, dans toute conversation, sujet compris (issue #121).
            match &ev.kind {
                BusKind::Started => {
                    if !self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                        self.start_activity(&ev.turn_id, &ev.session_id, chat_id, topic_id);
                    }
                }
                BusKind::Event(TurnEvent::ToolCall { name, .. }) => {
                    self.set_activity(&ev.turn_id, activity_for(name));
                }
                BusKind::Event(TurnEvent::ToolResult { .. }) => {
                    self.set_activity(&ev.turn_id, "typing");
                }
                BusKind::Finished(_) => self.stop_activity(&ev.turn_id),
                _ => {}
            }
            // `sendMessageDraft` ne vaut que pour une conversation privée.
            if chat_id <= 0 {
                continue;
            }
            // Session en arrière-plan : ni brouillon ni « écrit… » (issue #10).
            if let Some(d) = drafts.get_mut(&ev.turn_id)
                && d.checked.elapsed() >= FOCUS_EVERY
            {
                d.checked = Instant::now();
                if self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                    drafts.remove(&ev.turn_id);
                    continue;
                }
            }
            match &ev.kind {
                BusKind::Started => {
                    if self.out_of_focus(&ev.session_id, chat_id, topic_id).await {
                        continue;
                    }
                    drafts.insert(
                        ev.turn_id.clone(),
                        Draft {
                            chat_id,
                            topic_id,
                            draft_id: crate::bus::draft_id_for(&ev.turn_id),
                            text: String::new(),
                            last: Instant::now() - self.draft_interval,
                            checked: Instant::now(),
                            in_flight: None,
                            sent: String::new(),
                        },
                    );
                }
                BusKind::Event(TurnEvent::Delta(t)) => {
                    if let Some(d) = drafts.get_mut(&ev.turn_id) {
                        d.text.push_str(t);
                        // Un seul brouillon en vol : tant qu'il n'est pas parti, le texte
                        // continue de s'accumuler et le suivant portera tout (issue #70).
                        let busy = d.in_flight.as_ref().is_some_and(|h| !h.is_finished());
                        if !busy && d.last.elapsed() >= self.draft_interval && d.text != d.sent {
                            d.last = Instant::now();
                            d.sent = d.text.clone();
                            d.in_flight =
                                self.spawn_draft(d.chat_id, d.topic_id, d.draft_id, &d.text);
                        }
                    }
                }
                BusKind::Event(TurnEvent::ToolCall { name, args }) => {
                    if let Some(d) = drafts.get_mut(&ev.turn_id) {
                        let busy = d.in_flight.as_ref().is_some_and(|h| !h.is_finished());
                        if !busy {
                            d.last = Instant::now();
                            // Ligne d'état : ce qui tourne, pas seulement son nom (#121).
                            let preview =
                                format!("{}\n\n{}", d.text.trim_end(), tool_status(name, args));
                            d.sent = preview.clone();
                            d.in_flight =
                                self.spawn_draft(d.chat_id, d.topic_id, d.draft_id, preview.trim());
                        }
                    }
                }
                BusKind::Finished(_) => {
                    // La réponse finale part tout de suite : le brouillon en vol ne doit
                    // pas prendre le créneau devant elle (issue #70).
                    if let Some(d) = drafts.remove(&ev.turn_id)
                        && let Some(h) = d.in_flight
                    {
                        h.abort();
                    }
                }
                _ => {}
            }
        }
    }

    /// Envoie un brouillon en tâche de fond ; la poignée sert à savoir s'il est encore en
    /// vol (issue #70).
    fn spawn_draft(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        draft_id: i64,
        text: &str,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let text: String = text
            .chars()
            .rev()
            .take(4_000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if text.trim().is_empty() {
            return None;
        }
        let bot = self.bot.clone();
        Some(tokio::spawn(async move {
            let _ = bot.send_draft(chat_id, topic_id, draft_id, &text).await;
        }))
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for TelegramGateway {
    async fn offer_burst(
        &self,
        session_id: &str,
        origin: &Origin,
        parts: Vec<String>,
    ) -> Result<(), String> {
        let chars = parts.iter().map(|part| part.chars().count()).sum();
        self.ask_about_burst(TextBurst {
            origin: origin.clone(),
            session: session_id.to_string(),
            parts,
            message_ids: Vec::new(),
            update_id: 0,
            chars,
            deadline: std::time::Instant::now(),
        })
        .await
        .map_err(|error| error.to_string())
    }

    async fn schedule_alert(
        &self,
        origin: &Origin,
        schedule_id: &str,
        text: &str,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let s = &self.daemon.services;
        let ttl = 7 * 24 * 3_600_000;
        let rerun = s
            .actions
            .create(
                k::SCREEN_DO,
                "schedule.run",
                json!({"params": {"id": schedule_id}, "back": null}),
                ttl,
                true,
            )
            .await
            .map_err(|e| e.to_string())?;
        let show = s
            .actions
            .create(k::SCREEN, "schedules", json!({}), ttl, false)
            .await
            .map_err(|e| e.to_string())?;
        let rows = vec![vec![
            ButtonSpec::callback("🔁 Relancer maintenant", &rerun.token, ""),
            ButtonSpec::callback("📅 Voir la planification", &show.token, ""),
        ]];
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rows),
                "message_thread_id": topic_id,
            }),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn session_titled(&self, session_id: &str, title: &str) {
        let key = format!("tg.new_session.{session_id}");
        let Some((chat_id, message_id)) =
            self.daemon.kv_get(&key).await.ok().flatten().and_then(|v| {
                let (c, m) = v.split_once(':')?;
                Some((c.parse::<i64>().ok()?, m.parse::<i64>().ok()?))
            })
        else {
            return;
        };
        let text = format!("🆕 Nouvelle session « {title} » (`{session_id}`).");
        if let Err(e) = self
            .bot
            .edit_text(chat_id, message_id, &markdown_to_html(&text), None)
            .await
        {
            tracing::debug!(error = %e, "message de nouvelle session non mis à jour");
        }
        let _ = self.daemon.kv_delete(&key).await;
    }

    async fn deliver(
        &self,
        turn_id: &str,
        session_id: &str,
        origin: &Origin,
        outcome: &TurnOutcome,
    ) {
        let Origin::Telegram {
            chat_id,
            topic_id,
            message_id,
        } = origin.clone()
        else {
            return;
        };
        // Session en arrière-plan : rien n'est écrit dans le chat (issue #10).
        if self.out_of_focus(session_id, chat_id, topic_id).await {
            let item = match outcome {
                TurnOutcome::Answered { text, .. } => Some(Held::Text {
                    text: text.clone(),
                    reply_to: message_id,
                    answer: true,
                    choices: Vec::new(),
                }),
                TurnOutcome::AwaitingApproval { approval_id } => Some(Held::Approval {
                    id: approval_id.clone(),
                }),
                TurnOutcome::LoopAborted {
                    answer, choices, ..
                } => Some(Held::Text {
                    text: answer.clone(),
                    reply_to: message_id,
                    answer: false,
                    choices: choices.clone(),
                }),
                TurnOutcome::Cancelled => None,
                TurnOutcome::BudgetExceeded {
                    scope,
                    spent_usd,
                    limit_usd,
                } => Some(Held::Text {
                    text: crate::agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
                    reply_to: message_id,
                    answer: false,
                    choices: Vec::new(),
                }),
                TurnOutcome::Failed { error } => Some(Held::Failure {
                    error: error.clone(),
                    reply_to: message_id,
                }),
            };
            if let Some(item) = item
                && let Err(e) = self.hold(session_id, chat_id, topic_id, item).await
            {
                tracing::error!(error = %e, "sortie d'une session en arrière-plan perdue");
            }
            return;
        }
        let result: anyhow::Result<()> = async {
            match outcome {
                TurnOutcome::Answered { text, .. } => {
                    self.reply(chat_id, topic_id, message_id, text).await?;
                    if let Some(mid) = message_id {
                        self.react(chat_id, mid, reaction::DONE);
                    }
                }
                TurnOutcome::AwaitingApproval { approval_id } => {
                    if let Some(a) = self.daemon.services.approvals.get(approval_id).await? {
                        self.send_approval_card(chat_id, topic_id, &a).await?;
                    }
                    if let Some(mid) = message_id {
                        self.react(chat_id, mid, reaction::WAITING_APPROVAL);
                    }
                }
                TurnOutcome::LoopAborted {
                    answer, choices, ..
                } => {
                    self.send_choices(chat_id, topic_id, message_id, session_id, answer, choices)
                        .await?;
                }
                TurnOutcome::Cancelled => {
                    self.reply(chat_id, topic_id, None, "⏹ Génération arrêtée.")
                        .await?;
                }
                TurnOutcome::BudgetExceeded {
                    scope,
                    spent_usd,
                    limit_usd,
                } => {
                    // Carte de relèvement si la demande est ouverte (issue #32), sinon le texte.
                    let pending = self
                        .daemon
                        .services
                        .approvals
                        .pending(50)
                        .await?
                        .into_iter()
                        .find(|a| {
                            a.kind == penelope_hitl::ApprovalKind::BudgetExceeded
                                && a.session_id.as_deref() == Some(session_id)
                                && a.payload["budget"].as_bool() == Some(true)
                        });
                    match pending {
                        Some(a) => self.send_budget_card(chat_id, topic_id, &a).await?,
                        None => {
                            self.reply(
                                chat_id,
                                topic_id,
                                message_id,
                                &crate::agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
                            )
                            .await?
                        }
                    }
                }
                TurnOutcome::Failed { error } => {
                    let cost = self
                        .daemon
                        .services
                        .budget
                        .turn_totals(turn_id)
                        .await
                        .ok()
                        .map(|(_, c)| c);
                    self.send_failure(chat_id, topic_id, message_id, session_id, error, cost)
                        .await?;
                    if let Some(mid) = message_id {
                        self.react(chat_id, mid, reaction::ERROR);
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::error!(error = %e, "livraison Telegram impossible");
        }
    }
}

/// Demandes d'élicitation MCP : une carte dans le chat privé du propriétaire.
impl TelegramGateway {
    /// Foyer du propriétaire : le chat et le sujet où arrivent les notifications qui
    /// n'appartiennent à aucune session (issue #143). Réglé par `telegram.home` ; sans
    /// lui, le chat privé, comme avant.
    ///
    /// Une session, elle, garde toujours son propre chat et son propre sujet : ce repli
    /// ne s'applique qu'à ce qui n'en a pas.
    pub fn home_chat(&self) -> (i64, Option<i64>) {
        self.daemon
            .services
            .config
            .config()
            .telegram
            .home
            .resolved()
            .unwrap_or((self.owner_id, None))
    }
}

#[async_trait::async_trait]
impl crate::elicitation::OwnerChannel for TelegramGateway {
    async fn show(&self, r: &crate::elicitation::Request) -> Result<Option<i64>, String> {
        use crate::elicitation::Kind;
        let accept = match &r.kind {
            Kind::Form { .. } if r.field_count() > 0 => "📝 Remplir",
            Kind::Form { .. } => "✅ Accepter",
            Kind::Url { .. } => "🌐 Ouvrir le lien",
        };
        let button = |label, action| self.elicit_button(label, action, r);
        let rows = vec![
            vec![
                button(accept, k::ELICIT_ACCEPT)
                    .await
                    .map_err(|e| e.to_string())?,
                button("🚫 Refuser", k::ELICIT_DECLINE)
                    .await
                    .map_err(|e| e.to_string())?,
            ],
            vec![
                button("✖️ Annuler", k::ELICIT_CANCEL)
                    .await
                    .map_err(|e| e.to_string())?,
            ],
        ];
        let html = format!(
            "{}\n\n<i>Sans réponse d'ici {}, la demande est annulée.</i>",
            Self::elicitation_html(r),
            crate::elicitation::human(r.timeout)
        );
        // La carte va **dans la conversation qui a déclenché l'appel** (issue #143) :
        // avant, elle partait dans le chat privé, que le propriétaire ne lit plus depuis
        // qu'il travaille dans un sujet de groupe. Elle passe par la file, comme les
        // cartes d'approbation : une coupure réseau ne la perd plus (#101).
        let (chat_id, topic_id) = self.elicitation_chat(r).await;
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rows),
                "message_thread_id": topic_id,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
        // La file ne rend pas l'identifiant du message : la carte ne sera pas modifiée
        // en place, l'issue arrive en message dans le même sujet.
        Ok(None)
    }

    async fn close(
        &self,
        r: &crate::elicitation::Request,
        card: Option<i64>,
        markdown: &str,
        retry: bool,
    ) {
        // Une demande annulée par le délai se relance d'un bouton, dans la conversation
        // où elle a échoué : trois demandes identiques en une session coûtaient trois
        // cartes et autant d'attentes (issue #143).
        let keyboard = match (retry, r.to.session_id.as_deref()) {
            (true, Some(session)) => self
                .elicit_retry_button(r, session)
                .await
                .map(|b| inline_keyboard(&[vec![b]])),
            _ => None,
        };
        if let Err(e) = self.elicitation_update(r, card, markdown, keyboard).await {
            tracing::warn!(error = %e, "carte d'élicitation non mise à jour");
        }
    }

    /// Rappel à mi-délai, dans la conversation où la carte a été posée (issue #143).
    async fn remind(&self, r: &crate::elicitation::Request, _card: Option<i64>) {
        let (chat_id, topic_id) = self.elicitation_chat(r).await;
        let text = format!(
            "⏳ La confirmation demandée par `{}` attend toujours ({} restantes) : {}",
            r.server,
            crate::elicitation::human(r.timeout / 2),
            crate::elicitation::first_line(&r.message)
        );
        if let Err(e) = self.reply(chat_id, topic_id, None, &text).await {
            tracing::warn!(error = %e, "rappel d'élicitation non envoyé");
        }
    }
}

/// Longueur au-delà de laquelle un message d'échec est tronqué (issue #148).
const MAX_FAILURE_CHARS: usize = 500;

/// Tronque un message d'échec — par convention, ceux qui commencent par ❌ — en renvoyant
/// au journal pour le détail. Les autres messages passent intacts : une réponse longue du
/// modèle part en document, elle ne se coupe pas ici.
fn shorten_failure(text: &str) -> String {
    if !text.trim_start().starts_with('❌') || text.chars().count() <= MAX_FAILURE_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_FAILURE_CHARS).collect();
    format!("{head}…\n\n(message tronqué ; détail dans `penelope logs`)")
}

#[async_trait::async_trait]
impl Messenger for TelegramGateway {
    async fn send_plan_card(
        &self,
        origin: &Origin,
        session: &str,
        draft: &penelope_workflow::plan::PlanDraft,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let version = draft.plan.version();
        let token = self
            .daemon
            .services
            .actions
            .create(
                k::SCREEN_DO,
                "wf.plan.go",
                json!({"params":{"session":session,"version":version},"back":null}),
                24 * 3_600_000,
                true,
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut lines = vec![format!("📋 Plan v{version} · {}", draft.plan.goal())];
        for (index, step) in draft.plan.steps().iter().enumerate() {
            lines.push(format!("{}. {:?} — {}", index + 1, step.phase, step.title));
        }
        lines.push("\nRéponds dans cette conversation pour corriger le plan, ou valide-le.".into());
        self.send_screen(
            chat_id,
            topic_id,
            None,
            screens::Screen {
                text: lines.join("\n"),
                rows: vec![vec![ButtonSpec::callback("✅ Vas-y", &token.token, "")]],
            },
            None,
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn send_text(&self, origin: &Origin, markdown: &str) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        // Un avis interne (digest, rapport de veille) qui déborde part en document
        // plutôt qu'en six bulles (issue #145).
        self.reply_or_document(chat_id, topic_id, markdown)
            .await
            .map_err(|e| e.to_string())
    }

    async fn send_file(
        &self,
        origin: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        self.bot
            .send_document(chat_id, topic_id, path, caption)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn send_session_text(
        &self,
        session_id: &str,
        origin: &Origin,
        markdown: &str,
    ) -> Result<(), String> {
        if let Some((chat_id, topic_id)) = origin.telegram_chat()
            && self.out_of_focus(session_id, chat_id, topic_id).await
        {
            let item = Held::Text {
                text: markdown.to_string(),
                reply_to: None,
                answer: false,
                choices: Vec::new(),
            };
            return self
                .hold(session_id, chat_id, topic_id, item)
                .await
                .map_err(|e| e.to_string());
        }
        self.send_text(origin, markdown).await
    }

    async fn send_session_file(
        &self,
        session_id: &str,
        origin: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        if let Some((chat_id, topic_id)) = origin.telegram_chat()
            && self.out_of_focus(session_id, chat_id, topic_id).await
        {
            let item = Held::File {
                path: path.display().to_string(),
                caption: caption.map(String::from),
            };
            return self
                .hold(session_id, chat_id, topic_id, item)
                .await
                .map_err(|e| e.to_string());
        }
        self.send_file(origin, path, caption).await
    }

    async fn send_session_voice(
        &self,
        session_id: &str,
        origin: &Origin,
        path: &Path,
        duration_s: u32,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        if self.out_of_focus(session_id, chat_id, topic_id).await {
            let item = Held::Voice {
                path: path.display().to_string(),
                duration_s,
                caption: caption.map(String::from),
            };
            return self
                .hold(session_id, chat_id, topic_id, item)
                .await
                .map_err(|e| e.to_string());
        }
        let reply_to = match origin {
            Origin::Telegram { message_id, .. } => *message_id,
            _ => None,
        };
        self.bot
            .send_voice(chat_id, topic_id, path, duration_s, caption, reply_to)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn send_question(
        &self,
        origin: &Origin,
        markdown: &str,
        run_id: &str,
        visit: &str,
        choices: &[String],
        wants_input: bool,
        form: Option<&Value>,
    ) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let s = &self.daemon.services;
        let ttl = 7 * 24 * 3_600_000;
        let labels: Vec<String> = if choices.is_empty() {
            vec![
                if wants_input {
                    "✏️ Répondre"
                } else {
                    "OK"
                }
                .to_string(),
            ]
        } else {
            choices.to_vec()
        };
        let mut rows: Vec<Vec<ButtonSpec>> = Vec::new();
        for label in &labels {
            let t = s
                .actions
                .create(
                    k::CHOICE,
                    run_id,
                    json!({
                        "visit": visit,
                        "choice": label,
                        "input": wants_input,
                        "form": form.is_some(),
                    }),
                    ttl,
                    true,
                )
                .await
                .map_err(|e| e.to_string())?;
            rows.push(vec![ButtonSpec::callback(label, &t.token, "")]);
        }
        self.bot
            .send_text(
                chat_id,
                topic_id,
                &markdown_to_html(markdown),
                Some(inline_keyboard(&rows)),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn upsert_card(&self, origin: &Origin, key: &str, markdown: &str) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let html = markdown_to_html(markdown);
        let kv_key = format!("tg.card.{key}");
        let known = self
            .daemon
            .kv_get(&kv_key)
            .await
            .ok()
            .flatten()
            .and_then(|m| m.parse::<i64>().ok());
        if let Some(message_id) = known {
            match self.bot.edit_text(chat_id, message_id, &html, None).await {
                Ok(_) => return Ok(()),
                // Contenu identique : rien à faire.
                Err(e) if e.to_string().contains("not modified") => return Ok(()),
                // Message trop ancien ou supprimé : une nouvelle carte.
                Err(_) => {}
            }
        }
        let sent = self
            .bot
            .send_text(chat_id, topic_id, &html, None, None)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(id) = sent.get("message_id").and_then(|m| m.as_i64()) {
            let _ = self.daemon.kv_set(&kv_key, &id.to_string()).await;
        }
        Ok(())
    }

    async fn send_approval(&self, origin: &Origin, approval_id: &str) -> Result<(), String> {
        let (chat_id, topic_id) = origin.telegram_chat().unwrap_or_else(|| self.home_chat());
        let a = self
            .daemon
            .services
            .approvals
            .get(approval_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("demande {approval_id} introuvable"))?;
        self.send_approval_card(chat_id, topic_id, &a)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Taille du contexte d'une session, seuil de la compaction de fond et dernière
/// compaction (issue #40).
fn context_line(view: &Value) -> String {
    let prompt = view["last_prompt_tokens"]
        .as_i64()
        .map(|p| format!("{} k tokens au dernier appel", p / 1000))
        .unwrap_or_else(|| "aucun appel encore".into());
    let last = view["last_compaction"]
        .as_str()
        .map(|t| t.chars().take(16).collect::<String>().replace('T', " "))
        .unwrap_or_else(|| "jamais".into());
    let failing = match view["compaction_failures"].as_u64().unwrap_or(0) {
        0 => String::new(),
        n => format!(
            " ; ⚠️ le résumé échoue ({n} fois de suite){}",
            view["cost_per_turn_usd"]
                .as_f64()
                .map(|c| format!(", {c:.3} $ par tour"))
                .unwrap_or_default()
        ),
    };
    format!(
        "Contexte : {prompt}, compaction de fond vers {} k, dernière compaction : {last}{failing}",
        view["background_compaction_at"].as_u64().unwrap_or(0) / 1000
    )
}

/// `clé=valeur clé2=valeur2` : chaque valeur est lue en JSON si possible (nombres,
/// booléens), sinon gardée en texte.
pub fn parse_params(raw: &str) -> Value {
    let mut out = serde_json::Map::new();
    for pair in raw.split_whitespace() {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let value = serde_json::from_str::<Value>(v)
            .ok()
            .filter(|x| !x.is_string())
            .unwrap_or_else(|| json!(v));
        out.insert(k.to_string(), value);
    }
    Value::Object(out)
}

/// Met les photos reçues en file, dans la session de la conversation.
async fn enqueue_photos(
    daemon: &Arc<Daemon>,
    origin: &Origin,
    images: Vec<std::path::PathBuf>,
    caption: Option<String>,
    update_id: i64,
) -> anyhow::Result<()> {
    let session = daemon.chat_session_for(origin).await?;
    daemon
        .enqueue_message_with_images(
            &session,
            caption.as_deref().unwrap_or_default(),
            &images,
            origin,
            Some(format!("tg:{update_id}")),
        )
        .await?;
    Ok(())
}

/// Range une pièce jointe non ingérable. Texte : artefact lisible par `artifact_read` ;
/// binaire : fichier dans le workspace. Renvoie le bilan et la mention à joindre au tour.
async fn store_attachment(
    daemon: &Arc<Daemon>,
    session: &str,
    name: &str,
    bytes: &[u8],
) -> (String, Option<String>) {
    let s = &daemon.services;
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|t| bytes.len() <= 1024 * 1024 && !t.contains('\0'));
    if let Some(text) = text {
        let kind = penelope_context::store::guess_kind(text);
        let stored = s
            .context
            .history
            .put_artifact(Some(session), None, kind, Some(name), text)
            .await;
        return match stored {
            Ok(a) => (
                format!("📎 `{name}` enregistré comme artefact `{}`.", a.id),
                Some(format!(
                    "[Fichier joint : `{name}`, artefact `{}` ({} caractères) : `artifact_read` \
                     pour le lire. Contenu non vérifié.]",
                    a.id,
                    text.chars().count()
                )),
            ),
            Err(e) => (format!("📎 `{name}` non enregistré : {e}"), None),
        };
    }
    match crate::media::save_attachment(s, name, bytes) {
        Ok(path) => (
            format!("📎 `{name}` déposé dans `{}`.", path.display()),
            Some(format!(
                "[Fichier joint : `{name}`, déposé dans `{}` ({} octets).]",
                path.display(),
                bytes.len()
            )),
        ),
        Err(e) => (format!("📎 `{name}` non enregistré : {e}"), None),
    }
}

/// `openrouter:` est implicite : `/model main z-ai/glm-5.3` suffit.
fn normalise_model_id(raw: &str) -> String {
    if raw.contains(':') {
        raw.to_string()
    } else {
        format!("openrouter:{raw}")
    }
}

/// Remplace les `{{variables}}` d'un gabarit, en un seul passage (issue #129).
fn substitute(body: &str, vars: &BTreeMap<String, String>) -> String {
    penelope_telegram::templates::substitute(body, vars)
}

/// `openrouter:z-ai/glm-5.3` devient `glm-5.3` : assez pour reconnaître un modèle.
fn short_model(id: &str) -> String {
    penelope_llm::catalog::strip_provider(id)
        .rsplit('/')
        .next()
        .unwrap_or(id)
        .to_string()
}

/// Réponse à `/model <alias>` ou `/model auto`.
fn model_pin_notice(view: &Value) -> String {
    match view["pinned"].as_str() {
        Some(alias) => format!(
            "📌 Session épinglée sur `{alias}` · `{}`. Retour à l'automatique : `/model auto`.",
            short_model(view["pinned_model"].as_str().unwrap_or("?"))
        ),
        None => "🔀 Session en automatique.".into(),
    }
}

/// Nom de fichier à transmettre au serveur de transcription : l'extension y dit le
/// format. Les vocaux Telegram (`.oga`, Opus dans Ogg) deviennent `.ogg`, que les
/// serveurs OpenAI-compatibles reconnaissent.
fn audio_filename(file_path: &str, file_name: Option<&str>, mime_type: Option<&str>) -> String {
    let source = file_name.unwrap_or(file_path);
    let ext = std::path::Path::new(source)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    let ext = match (ext.as_deref(), mime_type) {
        (Some("oga") | Some("opus"), _) => "ogg".to_string(),
        (Some(e), _) if !e.is_empty() => e.to_string(),
        (_, Some("audio/mpeg")) => "mp3".into(),
        (_, Some("audio/mp4") | Some("audio/x-m4a") | Some("audio/m4a")) => "m4a".into(),
        (_, Some("audio/wav") | Some("audio/x-wav")) => "wav".into(),
        (_, Some("audio/flac")) => "flac".into(),
        _ => "ogg".into(),
    };
    format!("audio.{ext}")
}

/// `/schedules` : un déclencheur par ligne, prochain passage et cible.
fn schedules_text(v: &Value) -> String {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "Aucun déclencheur planifié.".into();
    }
    let mut t = String::from("**Déclencheurs**\n\n");
    for sc in &list {
        let state = sc["state"].as_str().unwrap_or("?");
        let icon = match state {
            "active" => "🟢",
            "paused" => "⏸",
            "done" => "✅",
            _ => "⚪",
        };
        let spec = &sc["spec"];
        let when = match sc["kind"].as_str().unwrap_or("?") {
            "cron" => format!(
                "cron `{}`{}",
                spec["expr"].as_str().unwrap_or("?"),
                if spec["once"].as_bool() == Some(true) {
                    " (une fois)"
                } else {
                    ""
                }
            ),
            "interval" | "mcp_poll" => format!(
                "{} toutes les {} min",
                sc["kind"].as_str().unwrap_or("?"),
                spec["every_ms"].as_u64().unwrap_or(0) / 60_000
            ),
            "watch_file" => format!("fichier `{}`", spec["path"].as_str().unwrap_or("?")),
            other => format!("{other} `{}`", spec["event"].as_str().unwrap_or("?")),
        };
        let target = &sc["target"];
        let what = match target["type"].as_str().unwrap_or("?") {
            "notify" => target["template"].as_str().unwrap_or("").to_string(),
            "prompt" => target["prompt"].as_str().unwrap_or("").to_string(),
            "workflow" => format!("workflow {}", target["workflowId"].as_str().unwrap_or("?")),
            other => other.to_string(),
        };
        t.push_str(&format!(
            "{icon} `{}` · {when} · {}\n",
            sc["id"].as_str().unwrap_or("?"),
            what.chars().take(80).collect::<String>()
        ));
        if let Some(to) = sc["destination"].as_str() {
            t.push_str(&format!("   ↳ vers : {to}\n"));
        }
        if let Some(next) = sc["next_run"].as_str().filter(|_| state == "active") {
            t.push_str(&format!("   ↳ prochain : {next}\n"));
        }
        if let Some(e) = sc["last_error"].as_str() {
            t.push_str(&format!(
                "   ↳ erreur : {}\n",
                e.chars().take(160).collect::<String>()
            ));
        }
    }
    t.push_str(
        "\n`/schedules pause|resume|rm|run <id>` ; `/schedules ici <id>` la fait livrer ici",
    );
    t
}

/// Dernières lignes du journal JSON du jour (le plus récent à défaut), filtrées par
/// composant (cible `tracing` ou texte), rendues `HH:MM:SS NIVEAU cible : message`.
fn recent_log_lines(dir: &Path, component: &str, n: usize) -> Vec<String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|f| f.to_string_lossy())
                .is_some_and(|f| f.starts_with("penelope-") && f.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    let Some(latest) = files.last() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(latest) else {
        return Vec::new();
    };
    let wanted = component.to_lowercase();
    let mut out: Vec<String> = raw
        .lines()
        .rev()
        .filter_map(|line| {
            let v: Value = serde_json::from_str(line).ok()?;
            let target = v["target"].as_str().unwrap_or_default();
            let message = v["fields"]["message"].as_str().unwrap_or_default();
            if !wanted.is_empty()
                && !target.to_lowercase().contains(&wanted)
                && !message.to_lowercase().contains(&wanted)
            {
                return None;
            }
            let time = v["timestamp"]
                .as_str()
                .and_then(|t| t.get(11..19))
                .unwrap_or("");
            let short_target = target.rsplit("::").next().unwrap_or(target);
            Some(format!(
                "{time} {} {short_target} : {}",
                v["level"].as_str().unwrap_or("?"),
                message.chars().take(200).collect::<String>()
            ))
        })
        .take(n)
        .collect();
    out.reverse();
    out
}

fn mcp_state_icon(state: &str) -> &'static str {
    match state {
        "ready" => "🟢",
        "degraded" => "🟡",
        "connecting" => "🔄",
        "failed" => "🔴",
        "disabled" => "⏸",
        "auth_required" => "🔐",
        _ => "⚪",
    }
}

/// `/mcp` : un serveur par ligne, puis les déclarations invalides.
fn mcp_list_text(v: &Value) -> String {
    let servers = v["servers"].as_array().cloned().unwrap_or_default();
    let mut t = if servers.is_empty() {
        "Aucun serveur MCP déclaré.".to_string()
    } else {
        let mut t = String::from("**Serveurs MCP**\n\n");
        for srv in &servers {
            let state = srv["state"].as_str().unwrap_or("?");
            let label = match state {
                "configured" => "démarre au premier appel",
                "ready" => "prêt",
                "degraded" => "dégradé",
                "connecting" => "connexion",
                "failed" => "en panne",
                "disabled" => "désactivé",
                "auth_required" => "autorisation requise",
                other => other,
            };
            t.push_str(&format!(
                "{} `{}` · {} outil(s) · {label}\n",
                mcp_state_icon(state),
                srv["name"].as_str().unwrap_or("?"),
                shown(&srv["tools"])
            ));
            if let Some(e) = srv["last_error"].as_str().filter(|_| state != "ready") {
                t.push_str(&format!(
                    "   ↳ {}\n",
                    e.chars().take(200).collect::<String>()
                ));
            }
        }
        t
    };
    for bad in v["invalid"].as_array().cloned().unwrap_or_default() {
        t.push_str(&format!(
            "\n⚠️ `{}` : {}",
            bad["file"].as_str().unwrap_or("?"),
            bad["error"].as_str().unwrap_or("?")
        ));
    }
    t.push_str("\n\nDétail : `/mcp <serveur>` ; `/mcp restart|logs|test <serveur>`");
    t
}

/// `/mcp <serveur>` : état, outils, dernière erreur.
fn mcp_show_text(v: &Value) -> String {
    let st = &v["status"];
    let name = st["name"].as_str().unwrap_or("?");
    let state = st["state"].as_str().unwrap_or("?");
    let mut t = format!(
        "{} **{name}** · {} · {} outil(s) · {} appel(s), {} erreur(s)\n",
        mcp_state_icon(state),
        state,
        shown(&st["tool_count"]),
        shown(&st["calls"]),
        shown(&st["errors"])
    );
    if let Some(p) = st["protocol"].as_str() {
        t.push_str(&format!(
            "Protocole {p} · transport {}\n",
            st["transport"].as_str().unwrap_or("?")
        ));
    }
    if let Some(e) = st["last_error"].as_str() {
        t.push_str(&format!(
            "Dernière erreur : {}\n",
            e.chars().take(300).collect::<String>()
        ));
    }
    let tools = v["tools"].as_array().cloned().unwrap_or_default();
    if !tools.is_empty() {
        t.push('\n');
        for tool in tools.iter().take(25) {
            t.push_str(&format!(
                "- `{}` ({})\n",
                tool["title"].as_str().unwrap_or("?"),
                tool["risk"].as_str().unwrap_or("?")
            ));
        }
        if tools.len() > 25 {
            t.push_str(&format!("… et {} autres\n", tools.len() - 25));
        }
    }
    t
}

/// Alias et routage, tels que `model.list` les décrit./// Alias et routage, tels que `model.list` les décrit.
fn routing_text(v: &Value) -> String {
    let mut t = String::from("**Alias**\n\n");
    for a in v["aliases"].as_array().cloned().unwrap_or_default() {
        t.push_str(&format!(
            "- `{}` → `{}`\n",
            a["alias"].as_str().unwrap_or("?"),
            a["model"].as_str().unwrap_or("?")
        ));
    }
    let r = &v["routing"];
    if r.is_object() {
        let step = |k: &str| {
            format!(
                "`{}` (`{}`)",
                r[k]["alias"].as_str().unwrap_or("?"),
                r[k]["model"].as_str().unwrap_or("?")
            )
        };
        t.push_str("\n**Routage**\n\n");
        if r["classifier"].as_bool().unwrap_or(false) {
            t.push_str(&format!(
                "Adaptatif (classifieur `{}`) :\n- simple → {}\n- ordinaire → {}\n- difficile → {}\n",
                r["classifier_model"].as_str().unwrap_or("?"),
                step("low"),
                step("medium"),
                step("high"),
            ));
            t.push_str("Tout sur `main` : `/model auto off`\n");
        } else {
            t.push_str(&format!(
                "Fixe : tout passe par {} (adaptatif : `/model auto on`)\n",
                step("default")
            ));
        }
        if let Some(fb) = r["fallback"].as_object().filter(|f| !f.is_empty()) {
            let chains: Vec<String> = fb
                .iter()
                .map(|(from, to)| {
                    let to: Vec<&str> = to
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                        .unwrap_or_default();
                    format!("`{from}` → `{}`", to.join("`, `"))
                })
                .collect();
            t.push_str(&format!("Replis sur panne : {}\n", chains.join(" ; ")));
        }
    }
    t
}

/// Rend une valeur RPC en Markdown lisible dans une conversation.
pub fn render_value(v: &Value) -> String {
    fn scalar(v: &Value) -> String {
        match v {
            Value::String(s) => {
                let s: String = s.chars().take(120).collect();
                s
            }
            Value::Null => "—".into(),
            other => {
                let s = other.to_string();
                s.chars().take(120).collect()
            }
        }
    }
    let out = match v {
        Value::Array(items) if items.is_empty() => "(vide)".to_string(),
        Value::Array(items) => {
            let mut s = String::new();
            for it in items.iter().take(40) {
                match it {
                    Value::Object(o) => {
                        let line = o
                            .iter()
                            .filter(|(_, v)| !v.is_null() && !v.is_object() && !v.is_array())
                            .take(4)
                            .map(|(k, v)| format!("{k} : {}", scalar(v)))
                            .collect::<Vec<_>>()
                            .join(" · ");
                        s.push_str(&format!("- {line}\n"));
                    }
                    other => s.push_str(&format!("- {}\n", scalar(other))),
                }
            }
            if items.len() > 40 {
                s.push_str(&format!("… et {} de plus\n", items.len() - 40));
            }
            s
        }
        Value::Object(o) => {
            let mut s = String::new();
            for (k, v) in o {
                match v {
                    Value::Object(_) | Value::Array(_) => {
                        let compact = v.to_string();
                        let compact: String = compact.chars().take(200).collect();
                        s.push_str(&format!("**{k}** : `{compact}`\n"));
                    }
                    other => s.push_str(&format!("**{k}** : {}\n", scalar(other))),
                }
            }
            s
        }
        other => scalar(other),
    };
    out.chars().take(3_500).collect()
}

/// Clé du nom d'un sujet Telegram, lu dans les messages du sujet (issue #119).
pub fn topic_name_key(chat_id: i64, topic_id: i64) -> String {
    format!("tg.topic_name.{chat_id}.{topic_id}")
}

/// Titre d'un groupe autorisé, pour nommer où livre une planification (#124).
pub fn chat_title_key(chat_id: i64) -> String {
    format!("tg.chat_title.{chat_id}")
}

/// Titre du groupe d'un message (un chat privé n'en a pas).
fn chat_title_of(update: &Value) -> Option<(i64, String)> {
    let chat = &update.get("message")?["chat"];
    let title = chat["title"].as_str().filter(|t| !t.trim().is_empty())?;
    Some((chat["id"].as_i64()?, title.to_string()))
}

/// Nom du sujet d'un message de forum : à sa création, à son renommage, ou dans le
/// message de création auquel chaque message du sujet répond.
fn topic_name_of(update: &Value) -> Option<(i64, i64, String)> {
    let msg = update.get("message")?;
    let chat = msg["chat"]["id"].as_i64()?;
    let topic = msg["message_thread_id"].as_i64()?;
    let name = msg["forum_topic_created"]["name"]
        .as_str()
        .or(msg["forum_topic_edited"]["name"].as_str())
        .or(msg["reply_to_message"]["forum_topic_created"]["name"].as_str())?;
    Some((chat, topic, name.to_string()))
}

/// Conversations refusées récemment, les plus récentes d'abord (issue #113).
pub const SEEN_CHATS_KEY: &str = "telegram.seen_chats";
const SEEN_CHATS_MAX: usize = 20;

/// Une conversation refusée : son identifiant, son type, son titre et la dernière fois.
pub async fn record_seen_chat(s: &crate::runtime::Services, chat_id: i64, kind: &str, title: &str) {
    let mut seen = seen_chats(s).await;
    seen.retain(|c| c["id"].as_i64() != Some(chat_id));
    seen.insert(
        0,
        json!({"id": chat_id, "type": kind, "title": title, "last_seen": s.clock.now_rfc3339()}),
    );
    seen.truncate(SEEN_CHATS_MAX);
    let v = serde_json::to_string(&seen).unwrap_or_default();
    let _ = s
        .store
        .write(move |tx| penelope_store::kv_set(tx, SEEN_CHATS_KEY, &v))
        .await;
}

/// Conversations refusées récemment.
pub async fn seen_chats(s: &crate::runtime::Services) -> Vec<Value> {
    s.store
        .read(|c| penelope_store::kv_get(c, SEEN_CHATS_KEY))
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
