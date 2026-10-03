//! Cycle de vie d'une vie du processus : démarrage (services, configuration patchée,
//! fournisseur scripté ou enregistreur, serveur MCP simulé, reprise), redémarrage, et
//! fermeture attendue plutôt que supposée (références relâchées, checkpoint de l'écrivain
//! SQLite fini) ; nouvel essai borné quand la base est encore verrouillée.

use super::super::{Mode, ScriptEntry, ScriptLine, SeedRoot};
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
        for id in &self.spec.vision_models {
            let bare = penelope_llm::catalog::strip_provider(id);
            let mut model = ModelInfo::minimal(bare, "openrouter", 128_000);
            model.input_modalities = vec!["text".into(), "image".into()];
            services.catalog.upsert(vec![model]);
        }
        let daemon = Arc::new(Daemon::from_services(services.clone()));
        // L'orchestrateur, comme au démarrage du superviseur : planification, workflows,
        // sous-agents, images et embeddings passent par lui.
        daemon
            .hooks
            .set_orchestrator(Arc::new(penelope_daemon::workflow::orchestrator_of(
                &daemon.core,
            )));
        self.apply_config(&daemon)?;
        if self.spec.messenger {
            let recorder = super::messenger::Recorder {
                sent: self.sent.clone(),
            };
            if let Ok(mut slot) = daemon.hooks.messenger.write() {
                *slot = Some(Arc::new(recorder));
            }
        }
        daemon.set_provider_override(self.provider(&services)?);
        let (gateway, mcp) = self.install_mcp(&daemon, &services).await?;
        if first {
            self.seed_files(&services)?;
            for (key, value) in &self.spec.kv {
                services
                    .kv_set(key, value)
                    .await
                    .context("`[kv]` du scénario")?;
            }
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
            mcp,
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
            .map(|(k, v)| {
                let v = serde_json::to_value(v)?;
                let v = match &self.http {
                    Some(h) => h.fill(v),
                    None => v,
                };
                Ok((k.clone(), v))
            })
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
        for f in &self.spec.files {
            let content = f.content.repeat(f.repeat.max(1));
            let content = match &self.http {
                Some(h) => content.replace("{{http}}", h.base()),
                None => content,
            };
            write_under(services, f.root, &f.path, &content)?;
        }
        for r in &self.spec.repos {
            seed_repo(&workspace_of(services), r)?;
        }
        Ok(())
    }

    /// Étape `file` : un fichier écrit entre deux étapes.
    pub(super) fn write_file(
        &self,
        root: SeedRoot,
        path: &str,
        content: &str,
    ) -> anyhow::Result<Value> {
        write_under(&*self.services()?, root, path, content)?;
        Ok(json!({"path": path, "bytes": content.len()}))
    }

    /// Le fournisseur de la vie : mock qui rejoue le script, ou enregistreur autour du
    /// vrai fournisseur (`RECORD_SCENARIO`, `OPENROUTER_API_KEY`). Les appels du modèle
    /// du rôle `trace` (#273) prennent leurs lignes dans la file `"role": "trace"`.
    fn provider(&self, services: &Services) -> anyhow::Result<Arc<dyn Provider>> {
        let trace_model = penelope_gateway_telegram::trace_role_model(&services.config.config());
        let role_of = move |model: &str| -> Option<String> {
            (trace_model.as_deref() == Some(model)).then(|| "trace".to_string())
        };
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
                Arc::new(role_of),
            )));
        }
        let mock = MockProvider::new();
        let (script, roles, seen) = (self.script.clone(), self.roles.clone(), self.seen.clone());
        let errors = self.script_errors.clone();
        mock.set_responder(Some(Arc::new(move |req: &ChatRequest| {
            lock(&seen).push(req.clone());
            let next = match role_of(&req.model) {
                Some(role) => lock(&roles)
                    .get_mut(&role)
                    .and_then(|q| q.pop_front())
                    .ok_or_else(|| {
                        format!(
                            "script du rôle `{role}` épuisé : model.jsonl prévoit moins de \
                             lignes `\"role\": \"{role}\"` que le scénario n'en appelle"
                        )
                    }),
                None => lock(&script).pop_front().ok_or_else(|| {
                    "script épuisé : model.jsonl prévoit moins d'appels que le scénario n'en fait"
                        .to_string()
                }),
            };
            match next {
                Ok(line) => match super::ids::resolve(&line, req) {
                    Ok(line) => line.to_scripted(),
                    Err(e) => {
                        lock(&errors).push(e.clone());
                        Scripted::Error(LlmErrorKind::Other, e)
                    }
                },
                Err(e) => Scripted::Error(LlmErrorKind::Other, e),
            }
        })));
        Ok(Arc::new(mock))
    }

    /// Inscrit les outils simulés au registre (ils y survivent à un redémarrage, comme
    /// ceux d'un vrai serveur) et branche la passerelle ; ou, avec `[[mcp_servers]]`, le
    /// vrai superviseur, rendu pour que son entretien suive l'horloge.
    async fn install_mcp(
        &self,
        daemon: &Daemon,
        services: &Arc<Services>,
    ) -> anyhow::Result<(
        Option<Arc<Gateway>>,
        Option<Arc<penelope_mcp_host::McpSupervisor>>,
    )> {
        if !self.spec.mcp_servers.is_empty() {
            anyhow::ensure!(
                self.spec.mcp_tools.is_empty(),
                "`[[mcp_tools]]` (passerelle simulée) et `[[mcp_servers]]` (superviseur) \
                 s'excluent"
            );
            let sup =
                super::rpc::install_supervisor(daemon, services, &self.spec.mcp_servers).await;
            return Ok((None, Some(sup)));
        }
        if self.spec.mcp_tools.is_empty() && self.spec.mcp_resources.is_empty() {
            return Ok((None, None));
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
            resources: self.spec.mcp_resources.clone(),
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
        Ok((Some(gateway), None))
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
/// sont dites sur la sortie d'erreur, jamais écrites dans le monde. Vrai quand toutes
/// les références ont été relâchées avant l'échéance.
pub(super) async fn shut_down(life: Life, name: &str) -> bool {
    let Life {
        services,
        daemon,
        gateway,
        mcp,
    } = life;
    drop(gateway);
    // Le superviseur tient une copie des services : la relâcher avant de les compter.
    drop(mcp);
    // L'orchestrateur tient une copie du daemon : la relâcher, sinon la vie ne finit pas.
    if let Ok(mut slot) = daemon.hooks.orchestrator.write() {
        *slot = None;
    }
    let db = services.store.path().to_path_buf();
    let deadline = Instant::now() + SHUTDOWN_WAIT;
    // D'abord le daemon, dont seule notre copie doit rester ; puis les services, dont
    // seule la nôtre doit rester une fois le daemon lâché. Compter les copies des
    // services tant que le daemon vit supposait qu'il n'en tient qu'une : le cœur et ses
    // `Providers` en tiennent deux, et chaque vie attendait `SHUTDOWN_WAIT` pour rien.
    let mut polls = 0u32;
    let daemon_left = wait_until(&mut polls, deadline, || Arc::strong_count(&daemon) == 1).await;
    drop(daemon);
    let services_left =
        wait_until(&mut polls, deadline, || Arc::strong_count(&services) == 1).await;
    if !(daemon_left && services_left) {
        eprintln!(
            "scénario {name} : des références survivent à la vie ({} sur les services, daemon \
             {}) ; fermeture sans les attendre",
            Arc::strong_count(&services) - 1,
            if daemon_left {
                "relâché"
            } else {
                "encore tenu"
            }
        );
    }
    drop(services);
    let checks = wait_for_wal(&db, deadline).await;
    // Une attente ou deux sont l'ordinaire du checkpoint ; au-delà, c'est à lire.
    if polls > 1 || checks > 1 {
        eprintln!(
            "scénario {name} : fermeture de la vie attendue ({polls} attente(s) de références, \
             {checks} attente(s) du checkpoint)"
        );
    }
    daemon_left && services_left
}

/// Attend `done`, en rendant la main à l'ordonnanceur entre deux regards : les tâches
/// encore vivantes relâchent leurs copies à leur prochain passage. Faux à l'échéance.
async fn wait_until(polls: &mut u32, deadline: Instant, done: impl Fn() -> bool) -> bool {
    loop {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        *polls += 1;
        tokio::time::sleep(RETRY_STEP).await;
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
/// Le rôle qu'un modèle sert dans le script (`trace`), `None` pour la conversation.
type RoleOf = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

struct Recorder {
    set: ProviderSet,
    name: String,
    lines: Shared<Vec<ScriptEntry>>,
    seen: Shared<Vec<ChatRequest>>,
    role_of: RoleOf,
}

impl Recorder {
    fn new(
        set: ProviderSet,
        lines: Shared<Vec<ScriptEntry>>,
        seen: Shared<Vec<ChatRequest>>,
        role_of: RoleOf,
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
            role_of,
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
        let role = (self.role_of)(&req.model);
        let inner = self.inner(&req.model)?;
        let mut stream = match inner.chat_stream(req, cancel).await {
            Ok(s) => s,
            Err(e) => {
                let line = if e.kind == LlmErrorKind::ContextLength {
                    ScriptLine::ContextOverflow
                } else {
                    ScriptLine::Error {
                        kind: format!("{:?}", e.kind).to_lowercase(),
                        message: e.message.clone(),
                    }
                };
                lock(&self.lines).push(ScriptEntry { role, line });
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
            lock(&lines).push(ScriptEntry {
                role,
                line: fold(text, calls, images, usage, finish, error),
            });
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

/// Écrit `content` sous la racine d'un fichier semé (workspace, vault, skills).
fn write_under(
    services: &Services,
    root: SeedRoot,
    path: &str,
    content: &str,
) -> anyhow::Result<()> {
    let root = match root {
        SeedRoot::Workspace => workspace_of(services),
        SeedRoot::Vault => penelope_app::helpers::vault_dir(services),
        SeedRoot::Skills => services.platform.dirs.skills(),
    };
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content).with_context(|| format!("fichier écrit {}", path.display()))
}

/// `git` pour un dépôt semé : sans la configuration de l'hôte, auteur et dates fixes.
fn seed_git(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Pénélope")
        .env("GIT_AUTHOR_EMAIL", "penelope@exemple.fr")
        .env("GIT_COMMITTER_NAME", "Pénélope")
        .env("GIT_COMMITTER_EMAIL", "penelope@exemple.fr")
        .env("GIT_AUTHOR_DATE", "2026-09-28T12:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2026-09-28T12:00:00+00:00")
        .output()
        .context("git")?;
    anyhow::ensure!(
        out.status.success(),
        "git {args:?} : {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Sème un dépôt `[[repos]]` : `base` vide poussée sur un remote nu, `branch` avec les
/// fichiers semés.
fn seed_repo(workspace: &Path, r: &crate::scenario::SeedRepo) -> anyhow::Result<()> {
    let dir = workspace.join(&r.path);
    let remote = workspace.join(format!("{}.origin.git", r.path));
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(&remote)?;
    seed_git(&remote, &["init", "-q", "--bare"])?;
    seed_git(&dir, &["init", "-q", "-b", &r.base])?;
    std::fs::write(dir.join(".git/info/exclude"), ".penelope/\n")?;
    seed_git(&dir, &["commit", "-q", "--allow-empty", "-m", "départ"])?;
    seed_git(
        &dir,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    )?;
    seed_git(&dir, &["push", "-q", "origin", &r.base])?;
    seed_git(&dir, &["checkout", "-q", "-b", &r.branch])?;
    seed_git(&dir, &["add", "."])?;
    seed_git(&dir, &["commit", "-q", "-m", "travail"])?;
    Ok(())
}
