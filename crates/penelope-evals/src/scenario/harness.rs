//! Rejeu d'un scénario sur des services de test : vies successives (un `restart` détruit
//! et reconstruit les services sur le même répertoire, comme `resilience::boot`),
//! étapes, fournisseur scripté ou enregistreur, serveur MCP simulé, relevé du monde.
//!
//! Tout passe par l'API publique du daemon (`Services::for_tests`, `Daemon`,
//! `runner::process`, `compaction::compact`, `session_ops`, `purge`) : le scénario
//! survit aux déplacements internes de la V1.

mod journal;
mod lifecycle;
mod rpc;
mod messenger;
mod steps;
mod telegram;
#[cfg(test)]
mod tests;
mod visible;

use self::lifecycle::shut_down;
pub use self::visible::Visible;
use super::normalise::Normaliser;
use super::{McpTool, Mode, Scenario, ScriptLine, Spec, Step, world};
use anyhow::Context as _;
use penelope_agent::TurnOutcome;
use penelope_app::elicitation::Destination;
use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_executor::executor::{McpGateway, default_workspaces};
use penelope_kernel::clock::{Clock, TestClock};
use penelope_kernel::config::parse_duration;
use penelope_kernel::turn::Turn;
use penelope_llm::types::{ChatMessage, ChatRequest, ToolDef};
use penelope_mcp::protocol::ToolDescriptor;
use penelope_mcp::registry::qualified_name;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Battement de bail pendant un tour : au-delà de tout tour de scénario.
const HEARTBEAT: Duration = Duration::from_secs(60);
/// Attente de l'appel d'outil simulé avant de faire mourir le processus.
const CRASH_WAIT: Duration = Duration::from_secs(30);
/// Attente bornée de la fermeture complète d'une vie (références relâchées, checkpoint de
/// fermeture de l'écrivain SQLite fini) et de la réouverture d'une base encore
/// verrouillée, par pas de `RETRY_STEP`.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
const RETRY_STEP: Duration = Duration::from_millis(50);

/// Ce qu'un rejeu produit.
pub struct Run {
    /// Le monde après le run, normalisé (`expected.jsonl`).
    pub expected: Vec<Value>,
    /// Les requêtes vues par le modèle, rédigées et normalisées (`surface.jsonl`).
    pub surface: Vec<Value>,
    /// Le script capté sur le vrai fournisseur (mode enregistreur seulement).
    pub recorded: Option<Vec<ScriptLine>>,
    /// Ce que les contrôles du journal ont vu (épopée #208, T22).
    pub audit: Audit,
}

/// Ce que les contrôles du journal, à la fin du run, ont vu : les critères d'acceptation
/// vérifient ainsi qu'ils n'ont pas réussi à vide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audit {
    /// Requêtes comparées à leur pliage, remplacements admis dans un tour.
    pub visible: Visible,
    /// Lignes de cache que `history reindex` a redonnées à l'identique.
    pub reindexed: usize,
}

type Shared<T> = Arc<Mutex<T>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Une vie du processus : services et daemon sur le répertoire du scénario.
struct Life {
    services: Arc<Services>,
    daemon: Arc<Daemon>,
    gateway: Option<Arc<Gateway>>,
}

struct Harness<'a> {
    spec: &'a Spec,
    mode: Mode,
    root: tempfile::TempDir,
    clock: Arc<TestClock>,
    script: Shared<VecDeque<ScriptLine>>,
    seen: Shared<Vec<ChatRequest>>,
    recorded: Shared<Vec<ScriptLine>>,
    /// Envois du canal simulé, toutes vies confondues (`messenger = true`).
    sent: Shared<Vec<Value>>,
    life: Option<Life>,
    /// Chemin de la base, connu dès la première vie : la fermeture s'y vérifie.
    db: Option<PathBuf>,
    session: String,
    /// Réponses RPC gardées par `bind`, lues par `$nom.chemin`.
    bound: BTreeMap<String, Value>,
    outcomes: Vec<Value>,
    crashed: bool,
    telegram: telegram::Chat,
}

pub async fn run(scenario: &Scenario, mode: Mode) -> anyhow::Result<Run> {
    let spec = &scenario.spec;
    let clock = Arc::new(match &spec.clock {
        Some(raw) => TestClock::new(
            chrono::DateTime::parse_from_rfc3339(raw)
                .with_context(|| format!("horloge de départ `{raw}`"))?
                .timestamp_millis(),
        ),
        None => TestClock::default(),
    });
    let start_ms = clock.now_ms();
    let mut h = Harness {
        spec,
        mode,
        root: tempfile::tempdir().context("répertoire temporaire")?,
        clock,
        script: Arc::new(Mutex::new(scenario.script.iter().cloned().collect())),
        seen: Arc::new(Mutex::new(Vec::new())),
        recorded: Arc::new(Mutex::new(Vec::new())),
        sent: Arc::new(Mutex::new(Vec::new())),
        life: None,
        db: None,
        session: String::new(),
        bound: BTreeMap::new(),
        outcomes: Vec::new(),
        crashed: false,
        telegram: telegram::Chat::default(),
    };
    h.boot(true).await?;
    h.run_steps().await?;
    anyhow::ensure!(
        !h.crashed,
        "le scénario finit sur un crash : une étape `restart` doit le suivre"
    );
    let (dumped, audit) = {
        let life = h.life.as_ref().context("services absents")?;
        let workspace = workspace_of(&life.services);
        let mut dumped = world::dump(&life.services, &workspace).await?;
        dumped.extend(rpc::observe(&life.services, &spec.observe).await?);
        dumped.extend(lock(&h.sent).iter().cloned());
        let seen = lock(&h.seen).clone();
        let visible = visible::check(&life.services, &seen, &spec.name).await?;
        let reindexed = journal::check(&life.services, &spec.name).await?;
        (dumped, Audit { visible, reindexed })
    };
    if let Some(life) = h.life.take() {
        shut_down(life, &spec.name).await;
    }

    // Les jetons sont numérotés dans l'ordre du monde (sessions par création, tours par
    // mise en file, effets par appel…), pas dans celui de leur première mention.
    let mut n = Normaliser::new(start_ms, h.root.path()).with_masks(&spec.masks)?;
    for line in &dumped {
        let key = if line["type"] == "summary" {
            "node"
        } else {
            "id"
        };
        if let Some(id) = line[key].as_str() {
            n.text(id);
        }
    }
    let outcomes = std::mem::take(&mut h.outcomes);
    let expected: Vec<Value> = outcomes
        .into_iter()
        .chain(dumped)
        .map(|v| n.value(v))
        .collect();
    let surface: Vec<Value> = lock(&h.seen)
        .iter()
        .enumerate()
        .map(|(i, req)| n.value(penelope_observe::redact_json(&surface_line(i + 1, req))))
        .collect();
    let recorded = (mode == Mode::Record).then(|| lock(&h.recorded).clone());
    Ok(Run {
        expected,
        surface,
        recorded,
        audit,
    })
}

fn workspace_of(services: &Services) -> PathBuf {
    default_workspaces(services)
        .into_iter()
        .next()
        .unwrap_or_else(|| services.platform.dirs.data().join("workspace"))
}

impl Harness<'_> {
    fn life(&self) -> anyhow::Result<&Life> {
        self.life
            .as_ref()
            .context("le processus est mort : `restart` attendu")
    }

    fn daemon(&self) -> anyhow::Result<Arc<Daemon>> {
        Ok(self.life()?.daemon.clone())
    }

    fn services(&self) -> anyhow::Result<Arc<Services>> {
        Ok(self.life()?.services.clone())
    }

    async fn claim(&self) -> anyhow::Result<Option<Turn>> {
        Ok(self.services()?.turns.claim("scenario").await?)
    }

    fn outcome(&mut self, step: usize, label: &str, detail: Value) {
        let mut line = json!({"type": "outcome", "step": step, "input": label});
        if let (Some(o), Some(d)) = (line.as_object_mut(), detail.as_object()) {
            for (k, v) in d {
                o.insert(k.clone(), v.clone());
            }
        }
        self.outcomes.push(line);
    }

    async fn run_steps(&mut self) -> anyhow::Result<()> {
        let spec = self.spec;
        for (i, step) in spec.steps.iter().enumerate() {
            let n = i + 1;
            let label = step.label();
            if self.crashed && !matches!(step, Step::Restart) {
                anyhow::bail!(
                    "étape {n} ({label}) : après un crash, l'étape suivante est `restart`"
                );
            }
            let detail = match step {
                Step::Message { text, crash, steer } => {
                    self.message(text, *crash, steer.as_deref()).await
                }
                Step::Enqueue { text } => self.enqueue(text).await,
                Step::Command { command } => self.command(command).await,
                Step::Telegram { text, click } => {
                    self.telegram(text.as_deref(), click.as_deref()).await
                }
                Step::AdvanceClock { by } => {
                    let d = parse_duration(by).with_context(|| format!("durée `{by}`"))?;
                    self.clock.advance_ms(d.as_millis() as i64);
                    Ok(json!({"clock": self.clock.now_rfc3339()}))
                }
                Step::Restart => self.restart().await,
                Step::Rpc {
                    method,
                    params,
                    bind,
                    pick,
                    mask,
                    lines,
                    error,
                    during,
                } => {
                    self.rpc(rpc::Call {
                        method,
                        params,
                        bind: bind.as_deref(),
                        pick,
                        mask,
                        lines,
                        error: *error,
                        during: during.as_deref(),
                    })
                    .await
                }
                Step::Approve => self.approve().await,
                Step::Usage { prompt } => self.usage(*prompt).await,
                Step::Seed {
                    exchanges,
                    user,
                    assistant,
                    filler,
                    repeat,
                    tokens,
                } => {
                    let filler = filler.repeat(*repeat);
                    self.seed(*exchanges, user, assistant, &filler, *tokens)
                        .await
                }
            }
            .with_context(|| format!("étape {n} ({label})"))?;
            self.outcome(n, &label, detail);
        }
        Ok(())
    }
}

fn outcome_json(o: &TurnOutcome) -> Value {
    match o {
        TurnOutcome::Answered {
            text, iterations, ..
        } => json!({"outcome": "answered", "text": text, "iterations": iterations}),
        TurnOutcome::AwaitingApproval { approval_id } => {
            json!({"outcome": "awaiting_approval", "approval": approval_id})
        }
        TurnOutcome::LoopAborted {
            answer, choices, ..
        } => json!({"outcome": "loop_aborted", "answer": answer, "choices": choices}),
        TurnOutcome::Cancelled => json!({"outcome": "cancelled"}),
        TurnOutcome::BudgetExceeded { scope, .. } => {
            json!({"outcome": "budget_exceeded", "scope": scope})
        }
        TurnOutcome::Failed { error } => json!({"outcome": "failed", "error": error}),
    }
}

/// Une requête vue par le modèle, réduite à ce qui compte : modèle, outils (noms),
/// réglages, messages (rôle, texte, appels d'outils).
fn surface_line(call: usize, req: &ChatRequest) -> Value {
    json!({
        "call": call,
        "model": req.model,
        "tool_choice": req.tool_choice,
        "tools": req.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        "max_tokens": req.max_tokens,
        "reasoning_effort": req.reasoning_effort,
        "structured_output": req.response_format.is_some(),
        "session": req.session_id,
        "fallback_models": req.fallback_models,
        "pinned_upstream": req.pinned_upstream,
        "messages": req.messages.iter().map(message_line).collect::<Vec<_>>(),
    })
}

fn message_line(m: &ChatMessage) -> Value {
    let mut line = json!({"role": m.role.as_str(), "text": m.text()});
    let images = m.content.iter().filter(|c| c.as_text().is_none()).count();
    if images > 0 {
        line["images"] = json!(images);
    }
    if !m.tool_calls.is_empty() {
        line["tool_calls"] = m
            .tool_calls
            .iter()
            .map(|c| json!({"id": c.id, "name": c.name, "arguments": c.arguments}))
            .collect();
    }
    if let Some(id) = &m.tool_call_id {
        line["tool_call_id"] = json!(id);
    }
    if let Some(name) = &m.name {
        line["name"] = json!(name);
    }
    if m.cache_marker {
        line["cache_marker"] = json!(true);
    }
    line
}

fn descriptor(t: &McpTool) -> ToolDescriptor {
    ToolDescriptor {
        name: t.name.clone(),
        title: None,
        description: t.description.clone(),
        input_schema: schema(t),
        output_schema: None,
        annotations: json!({"readOnlyHint": t.read_only}),
        icons: None,
    }
}

fn schema(t: &McpTool) -> Value {
    let props: serde_json::Map<String, Value> = t
        .params
        .iter()
        .map(|p| (p.clone(), json!({"type": "string"})))
        .collect();
    json!({"type": "object", "properties": props})
}

/// Serveur MCP simulé : rend le texte déclaré ; en mode bloqué, signale l'appel et ne
/// répond qu'une fois relâché (jamais, pour un crash).
struct Gateway {
    tools: Vec<McpTool>,
    block: AtomicBool,
    called: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl McpGateway for Gateway {
    async fn call_tool(
        &self,
        qualified: &str,
        _args: &Value,
        _from: Destination,
    ) -> Result<Value, String> {
        let tool = self
            .tools
            .iter()
            .find(|t| qualified_name(&t.server, &t.name) == qualified)
            .ok_or_else(|| format!("outil simulé inconnu : {qualified}"))?;
        if self.block.load(Ordering::SeqCst) {
            self.called.notify_one();
            self.release.notified().await;
        }
        Ok(json!({"content": [{"type": "text", "text": tool.result}]}))
    }

    async fn server_lines(&self) -> Vec<String> {
        let mut by_server: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for t in &self.tools {
            by_server.entry(&t.server).or_default().push(&t.name);
        }
        by_server
            .into_iter()
            .map(|(server, names)| format!("{server} (simulé) : {}", names.join(", ")))
            .collect()
    }

    async fn eager_tools(&self) -> Vec<ToolDef> {
        self.tools
            .iter()
            .map(|t| {
                ToolDef::new(
                    qualified_name(&t.server, &t.name),
                    t.description.clone(),
                    schema(t),
                )
            })
            .collect()
    }
}
