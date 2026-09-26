//! Cycle de vie d'une vie du processus : démarrage (services, configuration patchée,
//! fournisseur scripté ou enregistreur, serveur MCP simulé, reprise), redémarrage, et
//! fermeture attendue plutôt que supposée (références relâchées, checkpoint de l'écrivain
//! SQLite fini) ; nouvel essai borné quand la base est encore verrouillée.

use super::super::{Mode, ScriptLine, SeedRoot};
use super::{
    Gateway, HEARTBEAT, Harness, Life, RETRY_STEP, SHUTDOWN_WAIT, descriptor, lock, outcome_json,
    workspace_of,
};
use anyhow::Context as _;
use penelope_app::bus::Origin;
use penelope_app::engine::{SessionModels, TurnIntake};
use penelope_app::services::Services;
use penelope_daemon::runner;
use penelope_daemon::runtime::Daemon;
use penelope_executor::executor::McpGateway;
use penelope_kernel::clock::SharedClock;
use penelope_kernel::error::KernelError;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::provider::{CancelToken, ChunkStream};
use penelope_llm::types::{
    ChatRequest, FinishReason, LlmError, LlmErrorKind, StreamChunk, ToolCall, Transcription,
};
use penelope_llm::{ModelInfo, Provider, ProviderSet};
use penelope_mcp::registry::RegisteredTool;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

type Shared<T> = super::Shared<T>;

impl Harness<'_> {
    /// Démarre une vie ; si la base est encore verrouillée par la fermeture de la vie
    /// précédente, réessaie par pas de 50 ms, cinq secondes au plus, en le disant.
    pub(super) async fn boot(&mut self, first: bool) -> anyhow::Result<Value> {
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        let mut attempt = 1u32;
        loop {
            match self.boot_once(first).await {
                Ok(report) => {
                    if attempt > 1 {
                        eprintln!(
                            "scénario {} : base rouverte à la tentative {attempt}",
                            self.spec.name
                        );
                    }
                    return Ok(report);
                }
                Err(e) if is_locked(&e) && Instant::now() < deadline => {
                    eprintln!(
                        "scénario {} : base encore verrouillée à la tentative {attempt} ({e:#})",
                        self.spec.name
                    );
                    if let Some(life) = self.life.take() {
                        shut_down(life, &self.spec.name).await;
                    }
                    if let Some(db) = self.db.clone() {
                        wait_for_wal(&db, deadline).await;
                    }
                    attempt += 1;
                    tokio::time::sleep(RETRY_STEP).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Une vie : services, daemon, configuration patchée, fournisseur, serveur MCP
    /// simulé, reprise. La première vie sème aussi les fichiers et ouvre la session.
    async fn boot_once(&mut self, first: bool) -> anyhow::Result<Value> {
        let shared: SharedClock = self.clock.clone();
        let services = Arc::new(
            Services::for_tests(self.root.path().to_path_buf(), shared)
                .await
                .context("services de test")?,
        );
        self.db = Some(services.store.path().to_path_buf());
        let daemon = Arc::new(Daemon::from_services(services.clone()));
        // L'orchestrateur, comme au démarrage du superviseur : planification, workflows,
        // sous-agents, images et embeddings passent par lui.
        daemon
            .hooks
            .set_orchestrator(Arc::new(penelope_daemon::workflow::orchestrator_of(
                &daemon,
            )));
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
        let vault = penelope_app::helpers::vault_dir(services);
        let skills = services.platform.dirs.skills();
        for f in &self.spec.files {
            let root = match f.root {
                SeedRoot::Workspace => &workspace,
                SeedRoot::Vault => &vault,
                SeedRoot::Skills => &skills,
            };
            let path = root.join(&f.path);
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
        services: &Arc<Services>,
    ) -> anyhow::Result<Option<Arc<Gateway>>> {
        if !self.spec.mcp_servers.is_empty() {
            anyhow::ensure!(
                self.spec.mcp_tools.is_empty(),
                "`[[mcp_tools]]` (passerelle simulée) et `[[mcp_servers]]` (superviseur) \
                 s'excluent"
            );
            super::rpc::install_supervisor(daemon, services, &self.spec.mcp_servers).await;
            return Ok(None);
        }
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
            release: tokio::sync::Notify::new(),
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

    /// Nouvelle vie sur le même répertoire : reprise, puis les tours en attente sont joués.
    pub(super) async fn restart(&mut self) -> anyhow::Result<Value> {
        if let Some(life) = self.life.take() {
            shut_down(life, &self.spec.name).await;
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
}

/// Fin d'une vie, attendue plutôt que supposée. Les tâches encore vivantes (battement de
/// bail tué avec le tour, travaux bloquants) relâchent leurs références au daemon et aux
/// services au prochain passage de l'ordonnanceur ; puis l'écrivain SQLite, qui ne
/// s'arrête qu'avec la dernière copie du `Store`, finit son checkpoint de fermeture et le
/// journal `-wal` retombe à zéro. Sans cette attente, la vie suivante rouvrait la base
/// pendant ce checkpoint et son premier `write` échouait avec `database is locked`
/// (runner macOS de la CI, run 35946713239). Bornée à `SHUTDOWN_WAIT` ; les attentes
/// sont dites sur la sortie d'erreur, jamais écrites dans le monde.
pub(super) async fn shut_down(life: Life, name: &str) {
    let Life {
        services,
        daemon,
        gateway,
    } = life;
    drop(gateway);
    // L'orchestrateur tient une copie du daemon : la relâcher, sinon la vie ne finit pas.
    if let Ok(mut slot) = daemon.hooks.orchestrator.write() {
        *slot = None;
    }
    let db = services.store.path().to_path_buf();
    let deadline = Instant::now() + SHUTDOWN_WAIT;
    let mut polls = 0u32;
    loop {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        // Une copie du daemon (la nôtre) et deux des services (la nôtre, celle du daemon).
        let (d, s) = (Arc::strong_count(&daemon), Arc::strong_count(&services));
        if d == 1 && s == 2 {
            break;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "scénario {name} : des références survivent à la vie ({} sur le daemon, {} sur \
                 les services) ; fermeture sans les attendre",
                d - 1,
                s - 2
            );
            break;
        }
        polls += 1;
        tokio::time::sleep(RETRY_STEP).await;
    }
    drop(daemon);
    drop(services);
    let checks = wait_for_wal(&db, deadline).await;
    // Une attente ou deux sont l'ordinaire du checkpoint ; au-delà, c'est à lire.
    if polls > 1 || checks > 1 {
        eprintln!(
            "scénario {name} : fermeture de la vie attendue ({polls} attente(s) de références, \
             {checks} attente(s) du checkpoint)"
        );
    }
}

/// Attend que le journal WAL soit vide ou absent : l'écrivain a fini son checkpoint de
/// fermeture (`wal_checkpoint(TRUNCATE)`). Rend le nombre d'attentes.
async fn wait_for_wal(db: &Path, deadline: Instant) -> u32 {
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    let mut waits = 0;
    while wal_len(&wal) > 0 && Instant::now() < deadline {
        waits += 1;
        tokio::time::sleep(RETRY_STEP).await;
    }
    waits
}

fn wal_len(wal: &Path) -> u64 {
    std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
}

/// Une erreur SQLite de verrou (`database is locked`, code 5), la seule qu'un nouvel
/// essai de démarrage puisse résoudre.
fn is_locked(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}").to_lowercase();
    text.contains("database is locked") || text.contains("database table is locked")
}

/// Pose `value` à `chemin.pointé` dans un arbre JSON, en créant les objets manquants.
pub(super) fn set_path(tree: &mut Value, path: &str, value: Value) -> Result<(), String> {
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
pub(super) fn fold(
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
