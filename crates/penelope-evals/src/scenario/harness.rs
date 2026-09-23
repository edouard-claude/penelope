//! Rejeu d'un scénario sur des services de test : vies successives (un `restart` détruit
//! et reconstruit les services sur le même répertoire, comme `resilience::boot`),
//! étapes, fournisseur scripté ou enregistreur, serveur MCP simulé, relevé du monde.
//!
//! Tout passe par l'API publique du daemon (`Services::for_tests`, `Daemon`,
//! `runner::process`, `compaction::compact`, `session_ops`, `purge`) : le scénario
//! survit aux déplacements internes de la V1.

use super::normalise::Normaliser;
use super::{Crash, McpTool, Mode, Scenario, ScriptLine, Spec, Step, world};
use anyhow::Context as _;
use penelope_daemon::agent::TurnOutcome;
use penelope_daemon::bus::Origin;
use penelope_daemon::elicitation::Destination;
use penelope_daemon::executor::{McpGateway, default_workspaces};
use penelope_daemon::{Daemon, Services, compaction, purge, runner, session_ops};
use penelope_kernel::budget::UsageRecord;
use penelope_kernel::clock::{Clock, SharedClock, TestClock};
use penelope_kernel::config::parse_duration;
use penelope_kernel::error::KernelError;
use penelope_kernel::turn::Turn;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::provider::{CancelToken, ChunkStream};
use penelope_llm::types::{
    ChatMessage, ChatRequest, FinishReason, LlmError, LlmErrorKind, StreamChunk, ToolCall, ToolDef,
    Transcription,
};
use penelope_llm::{ModelInfo, Provider, ProviderSet};
use penelope_mcp::protocol::ToolDescriptor;
use penelope_mcp::registry::{RegisteredTool, qualified_name};
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

/// Ce qu'un rejeu produit.
pub struct Run {
    /// Le monde après le run, normalisé (`expected.jsonl`).
    pub expected: Vec<Value>,
    /// Les requêtes vues par le modèle, rédigées et normalisées (`surface.jsonl`).
    pub surface: Vec<Value>,
    /// Le script capté sur le vrai fournisseur (mode enregistreur seulement).
    pub recorded: Option<Vec<ScriptLine>>,
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
    life: Option<Life>,
    session: String,
    outcomes: Vec<Value>,
    crashed: bool,
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
        life: None,
        session: String::new(),
        outcomes: Vec::new(),
        crashed: false,
    };
    h.boot(true).await?;
    h.run_steps().await?;
    anyhow::ensure!(
        !h.crashed,
        "le scénario finit sur un crash : une étape `restart` doit le suivre"
    );
    let dumped = {
        let life = h.life.as_ref().context("services absents")?;
        let workspace = workspace_of(&life.services);
        world::dump(&life.services, &workspace).await?
    };

    // Les jetons sont numérotés dans l'ordre du monde (sessions par création, tours par
    // mise en file, effets par appel…), pas dans celui de leur première mention.
    let mut n = Normaliser::new(start_ms, h.root.path());
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

    /// Démarre une vie : services, daemon, configuration patchée, fournisseur, serveur
    /// MCP simulé, reprise. La première vie sème aussi les fichiers et ouvre la session.
    async fn boot(&mut self, first: bool) -> anyhow::Result<Value> {
        let shared: SharedClock = self.clock.clone();
        let services = Arc::new(
            Services::for_tests(self.root.path().to_path_buf(), shared)
                .await
                .context("services de test")?,
        );
        let daemon = Arc::new(Daemon::from_services(services.clone()));
        self.apply_config(&daemon)?;
        daemon.set_provider_override(self.provider(&services)?);
        let gateway = self.install_mcp(&daemon, &services).await?;
        if first {
            self.seed_files(&services)?;
            self.session = daemon
                .chat_session_for(&Origin::Cli)
                .await
                .context("session de départ")?;
            if let Some(alias) = &self.spec.pin_model {
                daemon
                    .pin_model(&self.session, Some(alias))
                    .await
                    .context("alias épinglé")?;
            }
        }
        let report = daemon.recover().await.context("reprise au démarrage")?;
        self.life = Some(Life {
            services,
            daemon,
            gateway,
        });
        Ok(serde_json::to_value(report)?)
    }

    /// Patchs `"chemin.pointé" = valeur`, par un aller-retour JSON de la configuration.
    fn apply_config(&self, daemon: &Daemon) -> anyhow::Result<()> {
        if self.spec.config.is_empty() {
            return Ok(());
        }
        let patches: Vec<(String, Value)> = self
            .spec
            .config
            .iter()
            .map(|(k, v)| Ok((k.clone(), serde_json::to_value(v)?)))
            .collect::<anyhow::Result<_>>()?;
        daemon
            .publish_config("scenario", |c| {
                let mut tree = serde_json::to_value(&*c)
                    .map_err(|e| KernelError::config(format!("configuration : {e}")))?;
                for (path, value) in &patches {
                    set_path(&mut tree, path, value.clone())
                        .map_err(|e| KernelError::config(format!("patch `{path}` : {e}")))?;
                }
                *c = serde_json::from_value(tree)
                    .map_err(|e| KernelError::config(format!("configuration patchée : {e}")))?;
                Ok(patches.iter().map(|(k, _)| k.clone()).collect())
            })
            .context("configuration du scénario")?;
        Ok(())
    }

    fn seed_files(&self, services: &Services) -> anyhow::Result<()> {
        let workspace = workspace_of(services);
        for f in &self.spec.files {
            let path = workspace.join(&f.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, f.content.repeat(f.repeat.max(1)))
                .with_context(|| format!("fichier semé {}", path.display()))?;
        }
        Ok(())
    }

    /// Le fournisseur de la vie : mock qui rejoue le script, ou enregistreur autour du
    /// vrai fournisseur (`RECORD_SCENARIO`, `OPENROUTER_API_KEY`).
    fn provider(&self, services: &Services) -> anyhow::Result<Arc<dyn Provider>> {
        if self.mode == Mode::Record {
            let key = std::env::var("OPENROUTER_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty())
                .context("RECORD_SCENARIO demande OPENROUTER_API_KEY")?;
            services
                .platform
                .secrets
                .set("openrouter_api_key", &key)
                .map_err(|e| anyhow::anyhow!("secret : {e}"))?;
            let set = penelope_llm::build_providers(
                &services.config.config(),
                services.platform.secrets.as_ref(),
                services.catalog.clone(),
                None,
            )
            .map_err(|e| anyhow::anyhow!("fournisseurs : {e}"))?;
            return Ok(Arc::new(Recorder::new(
                set,
                self.recorded.clone(),
                self.seen.clone(),
            )));
        }
        let mock = MockProvider::new();
        let (script, seen) = (self.script.clone(), self.seen.clone());
        mock.set_responder(Some(Arc::new(move |req: &ChatRequest| {
            lock(&seen).push(req.clone());
            match lock(&script).pop_front() {
                Some(line) => line.to_scripted(),
                None => Scripted::Error(
                    LlmErrorKind::Other,
                    "script épuisé : model.jsonl prévoit moins d'appels que le scénario n'en fait"
                        .into(),
                ),
            }
        })));
        Ok(Arc::new(mock))
    }

    /// Inscrit les outils simulés au registre (ils y survivent à un redémarrage, comme
    /// ceux d'un vrai serveur) et branche la passerelle.
    async fn install_mcp(
        &self,
        daemon: &Daemon,
        services: &Services,
    ) -> anyhow::Result<Option<Arc<Gateway>>> {
        if self.spec.mcp_tools.is_empty() {
            return Ok(None);
        }
        let now = services.clock.now_rfc3339();
        let mut by_server: BTreeMap<String, Vec<RegisteredTool>> = BTreeMap::new();
        for t in &self.spec.mcp_tools {
            by_server
                .entry(t.server.clone())
                .or_default()
                .push(RegisteredTool::from_descriptor(&t.server, &descriptor(t)));
        }
        for (server, tools) in by_server {
            services
                .mcp_tools
                .replace_server_tools(&server, tools, &now)
                .await
                .with_context(|| format!("registre MCP, serveur {server}"))?;
        }
        let gateway = Arc::new(Gateway {
            tools: self.spec.mcp_tools.clone(),
            block: AtomicBool::new(false),
            called: tokio::sync::Notify::new(),
        });
        let mut slot = daemon
            .hooks
            .mcp
            .write()
            .map_err(|_| anyhow::anyhow!("branchement MCP verrouillé"))?;
        *slot = Some(gateway.clone() as Arc<dyn McpGateway>);
        drop(slot);
        Ok(Some(gateway))
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
                Step::Message { text, crash } => self.message(text, *crash).await,
                Step::Enqueue { text } => self.enqueue(text).await,
                Step::Command { command } => self.command(command).await,
                Step::AdvanceClock { by } => {
                    let d = parse_duration(by).with_context(|| format!("durée `{by}`"))?;
                    self.clock.advance_ms(d.as_millis() as i64);
                    Ok(json!({"clock": self.clock.now_rfc3339()}))
                }
                Step::Restart => self.restart().await,
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

    async fn message(&mut self, text: &str, crash: Option<Crash>) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        d.enqueue_message(&self.session, text, &Origin::Cli, None)
            .await?
            .context("tour non créé")?;
        let turn = self.claim().await?.context("aucun tour à réclamer")?;
        let merged = turn.merged_messages.len();
        match crash {
            None => {
                let out = runner::process(&d, turn, HEARTBEAT).await;
                let mut v = outcome_json(&out);
                if merged > 0 {
                    v["merged"] = json!(merged);
                }
                Ok(v)
            }
            Some(Crash::DuringTool) => {
                self.crash_during_tool(turn).await?;
                Ok(json!({"outcome": "crash", "detail": "processus mort pendant l'appel d'outil"}))
            }
        }
    }

    /// Le tour part dans une tâche ; dès que l'outil simulé est appelé (réponse du modèle
    /// écrite, effet en vol, résultat absent), la tâche est tuée et les services détruits :
    /// c'est le processus qui meurt entre la réponse et le résultat d'outil.
    async fn crash_during_tool(&mut self, turn: Turn) -> anyhow::Result<()> {
        let life = self.life.take().context("processus déjà mort")?;
        let gateway = life
            .gateway
            .clone()
            .context("`crash = \"during_tool\"` demande un outil MCP simulé (`[[mcp_tools]]`)")?;
        gateway.block.store(true, Ordering::SeqCst);
        let d = life.daemon.clone();
        let task = tokio::spawn(async move { runner::process(&d, turn, HEARTBEAT).await });
        tokio::time::timeout(CRASH_WAIT, gateway.called.notified())
            .await
            .context("l'outil simulé n'a pas été appelé : rien à interrompre")?;
        task.abort();
        let _ = task.await;
        drop(life);
        self.crashed = true;
        Ok(())
    }

    async fn enqueue(&mut self, text: &str) -> anyhow::Result<Value> {
        self.daemon()?
            .enqueue_message(&self.session, text, &Origin::Cli, None)
            .await?
            .context("tour non créé")?;
        Ok(json!({"outcome": "queued"}))
    }

    async fn command(&mut self, command: &str) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        let (name, args) = command
            .trim()
            .split_once(' ')
            .map(|(n, a)| (n, a.trim()))
            .unwrap_or((command.trim(), ""));
        match name {
            "/compact" => {
                let r = compaction::compact(&d, &self.session, compaction::Trigger::Manual, None)
                    .await?;
                Ok(json!({
                    "text": compaction::report_text(&r),
                    "published": r.published,
                    "messages": r.messages,
                    "remaining_batches": r.remaining_batches,
                    "deferred": r.deferred,
                    "skipped": r.skipped,
                }))
            }
            "/fork" => {
                let title = (!args.is_empty()).then(|| args.to_string());
                let v = session_ops::fork(&d, &self.session, title).await?;
                self.session = v["session"]
                    .as_str()
                    .context("fork sans identifiant")?
                    .to_string();
                Ok(v)
            }
            "/rewind" => {
                let turns: usize = if args.is_empty() { 1 } else { args.parse()? };
                session_ops::rewind(&d, &self.session, turns).await
            }
            "/purge" => purge::session(&d, &self.session, "scénario").await,
            other => anyhow::bail!(
                "commande inconnue `{other}` : /compact, /fork [titre], /rewind [n], /purge"
            ),
        }
    }

    /// Nouvelle vie sur le même répertoire : reprise, puis les tours en attente sont joués.
    async fn restart(&mut self) -> anyhow::Result<Value> {
        if let Some(life) = self.life.take() {
            drop(life);
        }
        self.crashed = false;
        let recovered = self.boot(false).await?;
        let d = self.daemon()?;
        let mut drained = Vec::new();
        while let Some(turn) = self.claim().await? {
            drained.push(outcome_json(&runner::process(&d, turn, HEARTBEAT).await));
        }
        Ok(json!({"recovered": recovered, "drained": drained}))
    }

    async fn approve(&mut self) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        let s = self.services()?;
        let pending = s.approvals.pending(50).await?;
        let approval = pending
            .iter()
            .find(|a| a.session_id.as_deref() == Some(self.session.as_str()))
            .context("aucune demande en attente pour la session")?;
        let id = approval.id.0.clone();
        let decision = penelope_hitl::Decision::approve_once("cli");
        let resumed = penelope_daemon::agent::decide_approval(&s, &id, &decision).await?;
        d.enqueue_resume(&self.session, &id, &Origin::Cli)
            .await?
            .context("reprise non créée")?;
        let turn = self.claim().await?.context("aucune reprise à réclamer")?;
        let mut v = outcome_json(&runner::process(&d, turn, HEARTBEAT).await);
        v["approval"] = json!(id);
        v["resumable"] = json!(resumed);
        Ok(v)
    }

    async fn usage(&mut self, prompt: u64) -> anyhow::Result<Value> {
        let s = self.services()?;
        let model = s
            .config
            .config()
            .alias_model("main")
            .context("alias `main`")?
            .to_string();
        s.budget
            .record(UsageRecord {
                session_id: Some(self.session.clone()),
                model: model.clone(),
                provider: "mock".into(),
                role: Some("chat".into()),
                prompt,
                ..Default::default()
            })
            .await?;
        Ok(json!({"model": model, "prompt": prompt}))
    }

    async fn seed(
        &mut self,
        exchanges: usize,
        user: &str,
        assistant: &str,
        filler: &str,
        tokens: u64,
    ) -> anyhow::Result<Value> {
        let s = self.services()?;
        let fill = |template: &str, i: usize| {
            template
                .replace("{i}", &i.to_string())
                .replace("{filler}", filler)
        };
        for i in 1..=exchanges {
            s.context
                .history
                .append(
                    &self.session,
                    &ChatMessage::user(fill(user, i)),
                    tokens,
                    0,
                    false,
                    None,
                )
                .await?;
            s.context
                .history
                .append(
                    &self.session,
                    &ChatMessage::assistant(fill(assistant, i)),
                    tokens,
                    0,
                    false,
                    None,
                )
                .await?;
        }
        Ok(json!({"messages": exchanges * 2, "tokens_each": tokens}))
    }
}

/// Pose `value` à `chemin.pointé` dans un arbre JSON, en créant les objets manquants.
fn set_path(tree: &mut Value, path: &str, value: Value) -> Result<(), String> {
    let mut node = tree;
    let parts: Vec<&str> = path.split('.').collect();
    let (last, dirs) = parts.split_last().ok_or("chemin vide")?;
    for p in dirs {
        let obj = node
            .as_object_mut()
            .ok_or_else(|| format!("`{p}` n'est pas une table"))?;
        node = obj.entry(p.to_string()).or_insert_with(|| json!({}));
    }
    node.as_object_mut()
        .ok_or_else(|| format!("`{last}` : parent non objet"))?
        .insert(last.to_string(), value);
    Ok(())
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

/// Serveur MCP simulé : rend le texte déclaré, ou, en mode crash, signale l'appel et ne
/// répond jamais.
struct Gateway {
    tools: Vec<McpTool>,
    block: AtomicBool,
    called: tokio::sync::Notify,
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
            std::future::pending::<()>().await;
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

/// Fournisseur enregistreur (`RECORD_SCENARIO`) : enveloppe le vrai jeu de fournisseurs
/// et transcrit chaque réponse en une ligne de `model.jsonl`. Implémenté, pas encore
/// exercé contre un vrai modèle.
struct Recorder {
    set: ProviderSet,
    name: String,
    lines: Shared<Vec<ScriptLine>>,
    seen: Shared<Vec<ChatRequest>>,
}

impl Recorder {
    fn new(
        set: ProviderSet,
        lines: Shared<Vec<ScriptLine>>,
        seen: Shared<Vec<ChatRequest>>,
    ) -> Self {
        let name = if set.openrouter.is_some() {
            "openrouter"
        } else {
            "openai_compat"
        };
        Recorder {
            set,
            name: name.into(),
            lines,
            seen,
        }
    }

    fn inner(&self, model: &str) -> penelope_llm::types::Result<Arc<dyn Provider>> {
        self.set.get(model).ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::UnknownModel,
                format!("aucun fournisseur pour `{model}`"),
            )
        })
    }

    fn any(&self) -> penelope_llm::types::Result<Arc<dyn Provider>> {
        if let Some(p) = &self.set.openrouter {
            return Ok(p.clone() as Arc<dyn Provider>);
        }
        if let Some(p) = &self.set.compat {
            return Ok(p.clone() as Arc<dyn Provider>);
        }
        Err(LlmError::new(
            LlmErrorKind::Other,
            "aucun fournisseur configuré",
        ))
    }
}

/// Transcrit un flux consommé en ligne de script.
fn fold(
    text: String,
    calls: Vec<ToolCall>,
    images: Vec<String>,
    usage: Option<penelope_llm::types::Usage>,
    finish: Option<FinishReason>,
    error: Option<String>,
) -> ScriptLine {
    if let Some(message) = error {
        return ScriptLine::MidStreamError { text, message };
    }
    if !calls.is_empty() {
        return ScriptLine::ToolCalls { text, calls };
    }
    if !images.is_empty() {
        return ScriptLine::Images { text, urls: images };
    }
    let usage = usage.unwrap_or_default();
    if finish == Some(FinishReason::Length) {
        if text.trim().is_empty() && usage.reasoning > 0 {
            return ScriptLine::ReasonedOnly {
                completion: usage.completion,
                reasoning: usage.reasoning,
            };
        }
        return ScriptLine::Written {
            text,
            completion: usage.completion,
            cut: true,
        };
    }
    ScriptLine::Text(text)
}

#[async_trait::async_trait]
impl Provider for Recorder {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: CancelToken,
    ) -> penelope_llm::types::Result<ChunkStream> {
        lock(&self.seen).push(req.clone());
        let inner = self.inner(&req.model)?;
        let mut stream = match inner.chat_stream(req, cancel).await {
            Ok(s) => s,
            Err(e) => {
                lock(&self.lines).push(if e.kind == LlmErrorKind::ContextLength {
                    ScriptLine::ContextOverflow
                } else {
                    ScriptLine::Error {
                        kind: format!("{:?}", e.kind).to_lowercase(),
                        message: e.message.clone(),
                    }
                });
                return Err(e);
            }
        };
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let lines = self.lines.clone();
        tokio::spawn(async move {
            let mut text = String::new();
            let mut calls = Vec::new();
            let mut images = Vec::new();
            let mut usage = None;
            let mut finish = None;
            let mut error = None;
            while let Some(chunk) = stream.recv().await {
                match &chunk {
                    StreamChunk::Delta { text: t } => text.push_str(t),
                    StreamChunk::ToolCall(c) => calls.push(c.clone()),
                    StreamChunk::Image { url } => images.push(url.clone()),
                    StreamChunk::Usage(u) => usage = Some(*u),
                    StreamChunk::Done { finish: f } => finish = Some(*f),
                    StreamChunk::Error { message, .. } => error = Some(message.clone()),
                    _ => {}
                }
                if tx.send(chunk).await.is_err() {
                    break;
                }
            }
            lock(&lines).push(fold(text, calls, images, usage, finish, error));
        });
        Ok(rx)
    }

    async fn fetch_models(&self) -> penelope_llm::types::Result<Vec<ModelInfo>> {
        self.any()?.fetch_models().await
    }

    async fn embed(
        &self,
        model: &str,
        inputs: &[String],
    ) -> penelope_llm::types::Result<Vec<Vec<f32>>> {
        self.inner(model)?.embed(model, inputs).await
    }

    async fn transcribe(
        &self,
        model: &str,
        audio: Vec<u8>,
        filename: &str,
        language: Option<&str>,
    ) -> penelope_llm::types::Result<Transcription> {
        self.inner(model)?
            .transcribe(model, audio, filename, language)
            .await
    }

    async fn speak(
        &self,
        model: &str,
        input: &str,
        voice: &str,
        format: &str,
    ) -> penelope_llm::types::Result<Vec<u8>> {
        self.inner(model)?.speak(model, input, voice, format).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_path_creates_missing_tables() {
        let mut tree = json!({"context": {"tail_ratio": 0.1}});
        set_path(&mut tree, "context.large_payload_tokens", json!(300)).unwrap();
        set_path(&mut tree, "nouveau.sous.cle", json!("x")).unwrap();
        assert_eq!(tree["context"]["large_payload_tokens"], 300);
        assert_eq!(tree["context"]["tail_ratio"], 0.1);
        assert_eq!(tree["nouveau"]["sous"]["cle"], "x");
        assert!(set_path(&mut tree, "context.tail_ratio.x", json!(1)).is_err());
    }

    #[test]
    fn folding_a_stream_gives_the_script_vocabulary() {
        assert_eq!(
            fold(
                "ok".into(),
                vec![],
                vec![],
                None,
                Some(FinishReason::Stop),
                None
            ),
            ScriptLine::Text("ok".into())
        );
        assert!(matches!(
            fold(
                String::new(),
                vec![],
                vec![],
                None,
                None,
                Some("coupé".into())
            ),
            ScriptLine::MidStreamError { .. }
        ));
        let cut = fold(
            "long".into(),
            vec![],
            vec![],
            Some(penelope_llm::types::Usage {
                completion: 9,
                ..Default::default()
            }),
            Some(FinishReason::Length),
            None,
        );
        assert!(matches!(
            cut,
            ScriptLine::Written {
                cut: true,
                completion: 9,
                ..
            }
        ));
    }
}
