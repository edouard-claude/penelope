//! Exécution réelle des outils natifs et des méta-outils MCP (§8.9, §11).
//!
//! Le harnais a déjà fait son travail quand on arrive ici : liste blanche, détecteur de
//! boucles, politique, approbation, ledger. Ce module ne fait qu'exécuter, en validant
//! les arguments contre le schéma de l'outil et en restant dans les workspaces.

use crate::agent::{CallInfo, ToolExecutor};
use crate::bus::Origin;
use crate::runtime::Services;
use penelope_kernel::risk::RiskClass;
use penelope_tools::{ToolError, ToolOutcome, ToolResult};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Envoi d'un message au propriétaire, par le canal d'origine du tour.
#[async_trait::async_trait]
pub trait Messenger: Send + Sync {
    async fn send_text(&self, origin: &Origin, markdown: &str) -> Result<(), String>;
    async fn send_file(
        &self,
        origin: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String>;
    /// Carte d'approbation avec ses boutons, sur les canaux qui savent l'afficher.
    async fn send_approval(&self, _origin: &Origin, _approval_id: &str) -> Result<(), String> {
        Ok(())
    }
    /// Présente le plan et son gate dans le canal d'origine.
    async fn send_plan_card(
        &self,
        origin: &Origin,
        session: &str,
        draft: &penelope_workflow::plan::PlanDraft,
    ) -> Result<(), String> {
        let _ = session;
        self.send_text(
            origin,
            &format!("Plan v{} : {}", draft.plan.version(), draft.plan.goal()),
        )
        .await
    }
    /// Question d'une étape `user` : un bouton par choix. Sans boutons, le texte dit
    /// comment répondre en ligne de commande.
    #[allow(clippy::too_many_arguments)]
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
        let _ = (visit, wants_input);
        let mut text = markdown.to_string();
        if !choices.is_empty() {
            text.push_str(&format!(
                "\n\nRépondre : `penelope wf control {run_id} answer --choice <{}>`",
                choices.join("|")
            ));
        }
        if form.is_some() {
            text.push_str(" `--input '<objet JSON conforme au formulaire>'`");
        }
        self.send_text(origin, &text).await
    }
    /// Carte mise à jour sur place (progression d'un run) ; à défaut, un nouveau message.
    async fn upsert_card(&self, origin: &Origin, key: &str, markdown: &str) -> Result<(), String> {
        let _ = key;
        self.send_text(origin, markdown).await
    }
    /// Message écrit par une session pendant son tour. Seule la session au focus du chat
    /// écrit : une session en arrière-plan le met de côté (issue #10).
    async fn send_session_text(
        &self,
        session_id: &str,
        origin: &Origin,
        markdown: &str,
    ) -> Result<(), String> {
        let _ = session_id;
        self.send_text(origin, markdown).await
    }
    /// Fichier envoyé par une session pendant son tour, même règle.
    async fn send_session_file(
        &self,
        session_id: &str,
        origin: &Origin,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let _ = session_id;
        self.send_file(origin, path, caption).await
    }
    /// Message vocal OGG/Opus d'une session (issue #41).
    async fn send_session_voice(
        &self,
        session_id: &str,
        origin: &Origin,
        path: &Path,
        duration_s: u32,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let _ = (session_id, origin, path, duration_s, caption);
        Err("ce canal n'envoie pas de message vocal".into())
    }
}

/// Accès aux serveurs MCP vivants.
#[async_trait::async_trait]
pub trait McpGateway: Send + Sync {
    /// Appelle un outil par son nom qualifié `mcp__<serveur>__<outil>`.
    /// `from` : la conversation qui appelle — une élicitation du serveur y revient
    /// plutôt que d'atterrir dans le chat privé (issue #143).
    async fn call_tool(
        &self,
        qualified: &str,
        args: &Value,
        from: crate::elicitation::Destination,
    ) -> Result<Value, String>;
    /// Une ligne par serveur prêt, pour la tuile T1.
    async fn server_lines(&self) -> Vec<String>;
    /// Politique imposée à un outil par la déclaration de son serveur (`tool_policy`).
    async fn tool_policy(&self, _qualified: &str) -> Option<String> {
        None
    }
    /// Outils exposés directement au modèle (serveurs `eager_schemas`).
    async fn eager_tools(&self) -> Vec<penelope_llm::ToolDef> {
        Vec::new()
    }
}

/// Capacités qui dépendent du moteur de workflows et des sous-agents.
#[async_trait::async_trait]
pub trait Orchestrator: Send + Sync {
    /// `brief` : résumé de la conversation qui décide du lancement (issue #35).
    async fn start_workflow(
        &self,
        id: &str,
        params: Value,
        brief: Option<&str>,
        origin: &Origin,
    ) -> Result<Value, String>;
    /// `cancel` : le jeton du tour parent. Le sous-agent en reçoit un enfant, pour que
    /// `/stop` l'arrête aussi (issue #57).
    /// `origin` : l'origine du tour parent — le sous-agent en hérite le périmètre du
    /// fournisseur (issue #142).
    async fn spawn_sub_agent(
        &self,
        session_id: &str,
        prompt: &str,
        model: Option<&str>,
        tools: Vec<String>,
        origin: &Origin,
        cancel: &penelope_llm::CancelToken,
    ) -> Result<Value, String>;
    async fn generate_image(&self, prompt: &str, size: Option<&str>) -> Result<Value, String>;
    /// Question au modèle de vision sur une image (issue #125).
    async fn inspect_image(
        &self,
        session_id: &str,
        path: &Path,
        task: crate::vision::Task,
        question: &str,
    ) -> Result<Value, String> {
        let _ = (session_id, path, task, question);
        Err("modèle de vision indisponible ici".into())
    }
    /// Contrôle d'un run (`pause`, `resume`, `cancel`, `retry-step`, `skip-step`, `goto:<étape>`).
    async fn control_run(&self, run_id: &str, op: &str) -> Result<Value, String> {
        let _ = (run_id, op);
        Err("moteur de workflows indisponible".into())
    }
    /// Vecteur d'une requête de recherche, en temps borné ; `None` : recherche lexicale.
    async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        let _ = text;
        None
    }
}

/// Contexte d'un appel.
#[derive(Clone)]
pub struct ToolEnv {
    pub session_id: String,
    pub run_id: Option<String>,
    pub origin: Origin,
    pub workspaces: Vec<PathBuf>,
    pub in_workflow: bool,
    /// Modèle qui répond au tour, pour `self_status`.
    pub turn_model: Option<crate::selfknow::TurnModel>,
}

/// L'exécuteur du daemon.
pub struct NativeToolExecutor {
    pub services: Arc<Services>,
    pub env: ToolEnv,
    /// Racines de configuration présentes à la construction du tour. Elles sont remplacées
    /// par la génération vivante à chaque appel ; les racines propres au contexte (workdir
    /// d'un workflow, par exemple) restent stables.
    configured_workspaces_at_start: Vec<PathBuf>,
    pub http: reqwest::Client,
    pub locks: Arc<penelope_tools::fs::FileLocks>,
    pub messenger: Option<Arc<dyn Messenger>>,
    pub mcp: Option<Arc<dyn McpGateway>>,
    pub orchestrator: Option<Arc<dyn Orchestrator>>,
    pub admin: Option<Arc<dyn crate::selfknow::Admin>>,
}

/// Chemins dont la lecture est refusée aux commandes sous bac à sable (issue #68).
pub fn denied_reads(s: &Services) -> Vec<PathBuf> {
    s.config
        .config()
        .sandbox
        .deny_read
        .iter()
        .map(|d| s.platform.dirs.expand(d))
        .collect()
}

/// Statuts d'un critère de workflow ; `completed` et `passed` le cochent (issue #137).
pub const CRITERION_STATUSES: [&str; 4] = ["pending", "completed", "passed", "failed"];

/// Contrat des critères (`session_metadata`, clé `criteria`, issue #137) : chaque critère a
/// un `id` unique et un `text` (ou `label`), un `status` parmi [`CRITERION_STATUSES`]
/// (`pending` s'il manque) ; on coche par `update` sur un `id` existant. Une entrée hors
/// contrat est refusée avec ce qu'il faut, au lieu d'être écrite pour rien.
fn criteria_entry(
    op: penelope_kernel::session::MetadataOp,
    entry: Value,
    current: &Value,
) -> Result<Value, String> {
    use penelope_kernel::session::MetadataOp;
    let known: Vec<String> = current
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let vocabulary = CRITERION_STATUSES.join(", ");
    let status_ok = |c: &Value| -> Result<(), String> {
        match c.get("status") {
            None => Ok(()),
            Some(Value::String(st)) if CRITERION_STATUSES.contains(&st.as_str()) => Ok(()),
            Some(other) => Err(format!(
                "`status` {other} hors vocabulaire : {vocabulary} (`completed` ou `passed` \
                 cochent un critère)"
            )),
        }
    };
    let criterion = |c: Value| -> Result<Value, String> {
        let mut c = c;
        let o = c
            .as_object_mut()
            .ok_or("un critère est un objet {id, text, status}")?;
        let id_ok = o
            .get("id")
            .and_then(|i| i.as_str())
            .is_some_and(|i| !i.trim().is_empty());
        if !id_ok {
            return Err("chaque critère a un `id` (chaîne non vide)".into());
        }
        if !o.contains_key("text")
            && let Some(label) = o.get("label").cloned()
        {
            o.insert("text".into(), label);
        }
        if !o.get("text").is_some_and(|t| t.is_string()) {
            return Err("chaque critère a un `text` qui dit ce qu'il faut obtenir".into());
        }
        o.entry("status").or_insert_with(|| json!("pending"));
        status_ok(&c)?;
        Ok(c)
    };
    match op {
        MetadataOp::Set => {
            let items = entry
                .as_array()
                .cloned()
                .ok_or("`set` sur `criteria` attend la liste [{id, text, status}]")?;
            let mut seen = std::collections::BTreeSet::new();
            let mut out = Vec::new();
            for c in items {
                let c = criterion(c)?;
                let id = c["id"].as_str().unwrap_or_default().to_string();
                if !seen.insert(id.clone()) {
                    return Err(format!("`id` en double : `{id}`"));
                }
                out.push(c);
            }
            Ok(Value::Array(out))
        }
        MetadataOp::Append => {
            let c = criterion(entry)?;
            let id = c["id"].as_str().unwrap_or_default();
            if known.iter().any(|k| k == id) {
                return Err(format!(
                    "le critère `{id}` existe déjà : `update` pour le modifier"
                ));
            }
            Ok(c)
        }
        MetadataOp::Update => {
            let id = entry.get("id").and_then(|i| i.as_str()).unwrap_or_default();
            if !known.iter().any(|k| k == id) {
                return Err(format!(
                    "`update` sur `criteria` vise un critère par son `id` ; critères connus : {}. \
                     Pour cocher : entry={{\"id\": \"<id>\", \"status\": \"completed\"}}",
                    if known.is_empty() {
                        "aucun".to_string()
                    } else {
                        known.join(", ")
                    }
                ));
            }
            status_ok(&entry)?;
            Ok(entry)
        }
        MetadataOp::Remove => Ok(entry),
    }
}

/// Résultat dont le texte vient des serveurs MCP (descriptions, schémas) : la valeur reste
/// structurée, le texte montré au modèle est encadré comme non fiable (#92).
fn untrusted_listing(source: &str, value: Value) -> ToolOutcome {
    let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
    ToolOutcome {
        text: penelope_observe::injection::wrap_untrusted(source, &pretty),
        value,
        is_error: false,
        eager: false,
    }
}

/// Workspaces autorisés : configuration, sinon `{data}/workspace`.
fn canonical_workspace(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| penelope_platform::sandbox::normalise(path))
}

pub fn default_workspaces(s: &Services) -> Vec<PathBuf> {
    let cfg = s.config.config();
    let mut v: Vec<PathBuf> = cfg
        .sandbox
        .workspaces
        .iter()
        .map(|w| s.platform.dirs.expand(w))
        .collect();
    if v.is_empty() {
        let ws = s.platform.dirs.data().join("workspace");
        let _ = std::fs::create_dir_all(&ws);
        v.push(ws);
    }
    v.iter().map(|p| canonical_workspace(p)).collect()
}

/// Moment promis par `config_set`. La configuration est publiée immédiatement, mais les
/// choix collants d'un tour ne sont pas recalculés au milieu de celui-ci (#163).
fn config_application_time(path: &str) -> (&'static str, Option<&'static str>) {
    if penelope_kernel::config::restart_allowed(path) {
        return (
            "au redémarrage",
            Some("ce réglage ne modifie pas le processus déjà démarré"),
        );
    }
    if path == "owner.language"
        || path == "models.roles.chat_default"
        || path.starts_with("models.routing.")
    {
        return (
            "au prochain tour",
            Some("les outils de ce tour gardent l'ancienne valeur"),
        );
    }
    ("à chaud, dès le prochain appel", None)
}

impl NativeToolExecutor {
    /// Conversation à qui rendre une élicitation née de cet appel (issue #143) : celle
    /// du tour, quand il vient de Telegram ; sinon rien, et le canal choisit son repli.
    fn elicitation_destination(&self) -> crate::elicitation::Destination {
        let (chat_id, topic_id) = match self.env.origin.telegram_chat() {
            Some((c, t)) => (Some(c), t),
            None => (None, None),
        };
        crate::elicitation::Destination {
            session_id: Some(self.env.session_id.clone()),
            chat_id,
            topic_id,
        }
    }

    pub fn new(services: Arc<Services>, mut env: ToolEnv) -> Self {
        env.workspaces = env
            .workspaces
            .iter()
            .map(|root| canonical_workspace(root))
            .collect();
        let configured_workspaces_at_start = default_workspaces(&services);
        NativeToolExecutor {
            services,
            env,
            configured_workspaces_at_start,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                // Aucune redirection suivie par le client : `http::fetch` les suit lui-même pour
                // revérifier chaque saut (liste blanche, adresses privées), sinon la
                // vérification ne s'exécute jamais (issue #64).
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            locks: Arc::new(penelope_tools::fs::FileLocks::new()),
            messenger: None,
            mcp: None,
            orchestrator: None,
            admin: None,
        }
    }

    /// Racines réellement autorisées pour cet appel : les racines propres au contexte restent
    /// en tête, celles issues de `sandbox.workspaces` suivent la configuration vivante (#163).
    fn workspaces(&self) -> Vec<PathBuf> {
        let follows_config = self
            .env
            .workspaces
            .iter()
            .any(|root| self.configured_workspaces_at_start.contains(root));
        if !follows_config {
            // Un sous-agent peut recevoir une liste volontairement restreinte : ne jamais
            // l'élargir avec les workspaces généraux de la configuration.
            return self.env.workspaces.clone();
        }
        let mut roots: Vec<PathBuf> = self
            .env
            .workspaces
            .iter()
            .filter(|root| !self.configured_workspaces_at_start.contains(root))
            .cloned()
            .collect();
        for root in default_workspaces(&self.services) {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    fn workspace(&self) -> PathBuf {
        self.workspaces()
            .into_iter()
            .next()
            .unwrap_or_else(std::env::temp_dir)
    }

    fn path_arg(&self, args: &Value, key: &str) -> ToolResult<PathBuf> {
        let raw = str_arg(args, key)?;
        penelope_tools::fs::resolve(&raw, &self.workspaces())
    }

    fn cwd_arg(&self, args: &Value) -> ToolResult<PathBuf> {
        match args.get("cwd").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => penelope_tools::fs::resolve(c, &self.workspaces()),
            _ => Ok(self.workspace()),
        }
    }

    #[allow(clippy::too_many_lines)] // gel 0.17 : table de dispatch des outils natifs, lot G (executor/tools/*.rs)
    async fn dispatch(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        // Les méta-outils n'ont pas de spécification native.
        match name {
            "tool_search" => return self.tool_search(args).await,
            "tool_describe" => return self.tool_describe(args).await,
            "tool_call" => return Box::pin(self.tool_call(args, cancel)).await,
            _ => {}
        }
        // Outil à la demande utilisé : il reste dans la liste de la session (#104).
        crate::tools_on_demand::touch(s, &self.env.session_id, name).await;
        if name.starts_with("mcp__") {
            // Outil promu dans l'ensemble collant : appel direct.
            return self.mcp_call(name, args).await;
        }

        let spec =
            penelope_tools::tool_spec(name).ok_or_else(|| ToolError::Unknown(name.into()))?;
        if spec.workflow_only && !self.env.in_workflow {
            return Err(ToolError::Denied(format!(
                "`{name}` n'est disponible que dans un run de workflow"
            )));
        }
        penelope_tools::validate_args(name, args)?;

        let v = match name {
            // -------------------------------------------------------- fichiers
            "fs_read" => {
                let p = self.path_arg(args, "path")?;
                let offset = u_arg(args, "offset").unwrap_or(0);
                let limit = u_arg(args, "limit").unwrap_or(400);
                return Ok(ToolOutcome::ok(penelope_tools::fs::read(&p, offset, limit)?).eager());
            }
            "fs_list" => {
                let p = self.path_arg(args, "path")?;
                let rec = b_arg(args, "recursive").unwrap_or(false);
                let max = u_arg(args, "max_entries").unwrap_or(500);
                let v = penelope_tools::fs::list(&p, rec, max)?;
                let listing = render_listing(&p, &v);
                let mut o = ToolOutcome::ok(v.clone()).eager();
                // Une longue liste part en artefact : le modèle garde un résumé (issue #8).
                o.text = if listing.chars().count() > FS_LIST_INLINE_CHARS {
                    let art = s
                        .context
                        .history
                        .put_artifact(
                            Some(&self.env.session_id),
                            self.env.run_id.as_deref(),
                            "listing",
                            None,
                            &listing,
                        )
                        .await?;
                    summarise_listing(&p, &v, max, &listing, &art.id)
                } else {
                    listing
                };
                return Ok(o);
            }
            "fs_search" => {
                let root = match args.get("path").and_then(|v| v.as_str()) {
                    Some(p) if !p.is_empty() => penelope_tools::fs::resolve(p, &self.workspaces())?,
                    _ => self.workspace(),
                };
                let pattern = str_arg(args, "pattern")?;
                let glob = args.get("glob").and_then(|v| v.as_str());
                let max = u_arg(args, "max_results").unwrap_or(100);
                return Ok(ToolOutcome::ok(penelope_tools::fs::search(
                    &root, &pattern, glob, max,
                )?)
                .eager());
            }
            "fs_write" => {
                let p = self.path_arg(args, "path")?;
                let lock = self.locks.for_path(&p);
                let _g = lock.lock().await;
                penelope_tools::fs::write(&p, &str_arg(args, "content")?)?
            }
            "fs_edit" => {
                let p = self.path_arg(args, "path")?;
                let lock = self.locks.for_path(&p);
                let _g = lock.lock().await;
                penelope_tools::fs::edit(
                    &p,
                    &str_arg(args, "old")?,
                    &str_arg(args, "new")?,
                    b_arg(args, "replace_all").unwrap_or(false),
                )?
            }

            // -------------------------------------------------------- shell
            "shell_exec" => {
                let command = str_arg(args, "command")?;
                let cwd = self.cwd_arg(args)?;
                let default_timeout =
                    penelope_kernel::config::parse_duration(&cfg.tools.shell_timeout)
                        .unwrap_or(std::time::Duration::from_secs(120));
                let timeout = u_arg(args, "timeout_ms")
                    .map(|ms| std::time::Duration::from_millis(ms as u64))
                    .unwrap_or(default_timeout)
                    .min(std::time::Duration::from_secs(1800));
                // Réseau accordé par appel (#106) : la carte d'approbation l'a dit.
                let network = cfg.sandbox.shell_network || b_arg(args, "network").unwrap_or(false);
                let profile = penelope_tools::shell::profile_with_denied_reads(
                    &cfg.sandbox.default_profile,
                    &cwd,
                    network,
                    &denied_reads(s),
                );
                let shell = shell_override(&cfg.tools.shell);
                let out = penelope_tools::shell::exec(
                    &s.platform.processes,
                    &command,
                    penelope_tools::shell::ExecOptions {
                        profile: Some(&profile),
                        cwd: Some(&cwd),
                        timeout,
                        max_output_bytes: cfg.tools.max_output_bytes,
                        shell,
                        cancel: Some(cancel),
                    },
                )
                .await?;
                let offline = !network
                    && cfg.sandbox.default_profile != "full"
                    && penelope_tools::shell::looks_like_network_failure(
                        &command,
                        out.exit_code,
                        &out.stdout,
                        &out.stderr,
                    );
                let mut value = out.to_json();
                if offline {
                    value["network"] = json!(false);
                    value["note"] = json!(penelope_tools::shell::NETWORK_OFF_NOTE);
                }
                let mut o = ToolOutcome::ok(value);
                // Suites de tests et longues sorties en échec : résumé et échecs pour le
                // modèle, sortie complète en artefact (issue #32).
                let full = args.get("output").and_then(|v| v.as_str()) == Some("full");
                if !full
                    && let Some(body) = penelope_tools::test_output::digest(
                        &command,
                        out.exit_code,
                        &out.stdout,
                        &out.stderr,
                    )
                {
                    let log = format!(
                        "$ {command}\nCode de sortie {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        out.exit_code, out.stdout, out.stderr
                    );
                    let art = s
                        .context
                        .history
                        .put_artifact(
                            Some(&self.env.session_id),
                            self.env.run_id.as_deref(),
                            "log",
                            Some("shell.log"),
                            &log,
                        )
                        .await?;
                    o.text = format!(
                        "{body}\n\nSortie complète : artifact_read(\"{}\"){}",
                        art.id,
                        if out.truncated {
                            " (déjà tronquée au plafond de sortie)"
                        } else {
                            ""
                        }
                    );
                    if let Some(hint) = crate::tool_jobs::background_hint(&cfg, name, args) {
                        o.text.push_str(&hint);
                    }
                    return Ok(o.eager());
                }
                if out.exit_code != 0 {
                    o.text = format!(
                        "Code de sortie {}.\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        out.exit_code, out.stdout, out.stderr
                    );
                    if offline {
                        o.text =
                            format!("{}\n\n{}", penelope_tools::shell::NETWORK_OFF_NOTE, o.text);
                    }
                }
                // Un délai demandé au-delà de `tools.background_after` a immobilisé le
                // tour : la prochaine fois, proposer l'arrière-plan (issue #204).
                if let Some(hint) = crate::tool_jobs::background_hint(&cfg, name, args) {
                    o.text.push_str(&hint);
                }
                return Ok(o.eager());
            }

            // -------------------------------------------------------- git
            "git_status" => penelope_tools::git::status(&self.cwd_arg(args)?).await?,
            "git_diff" => {
                penelope_tools::git::diff(
                    &self.cwd_arg(args)?,
                    args.get("against").and_then(|v| v.as_str()),
                    b_arg(args, "staged").unwrap_or(false),
                )
                .await?
            }
            "git_branch" => {
                penelope_tools::git::branch(
                    &self.cwd_arg(args)?,
                    &str_arg(args, "name")?,
                    b_arg(args, "create").unwrap_or(true),
                )
                .await?
            }
            "git_commit" => {
                penelope_tools::git::commit(
                    &self.cwd_arg(args)?,
                    &str_arg(args, "message")?,
                    b_arg(args, "all").unwrap_or(true),
                )
                .await?
            }
            "git_clone" => {
                let dest = self.path_arg(args, "dest")?;
                penelope_tools::git::clone_in(
                    &str_arg(args, "url")?,
                    &dest,
                    &self.workspaces(),
                    u_arg(args, "depth").map(|d| d as u32),
                )
                .await?
            }
            "git_push" => {
                penelope_tools::git::push(
                    &self.cwd_arg(args)?,
                    args.get("remote")
                        .and_then(|v| v.as_str())
                        .unwrap_or("origin"),
                    &str_arg(args, "branch")?,
                )
                .await?
            }

            // -------------------------------------------------------- réseau
            "http_fetch" => {
                let headers: Vec<(String, String)> = args
                    .get("headers")
                    .and_then(|h| h.as_object())
                    .map(|o| {
                        o.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let v = penelope_tools::http::fetch(
                    &self.http,
                    &str_arg(args, "url")?,
                    args.get("method").and_then(|v| v.as_str()).unwrap_or("GET"),
                    &headers,
                    args.get("body").and_then(|v| v.as_str()),
                    &cfg.tools.http_allowlist,
                    u_arg(args, "max_bytes").unwrap_or(512 * 1024),
                    cfg.tools.http_block_private_ips,
                )
                .await?;
                return self
                    .fetched_page(v, &str_arg(args, "url")?)
                    .await
                    .map(ToolOutcome::eager);
            }

            // -------------------------------------------------------- soi-même
            "self_status" => {
                let section = args
                    .get("section")
                    .and_then(|v| v.as_str())
                    .unwrap_or("all")
                    .to_string();
                crate::selfknow::status(
                    s,
                    &self.env.session_id,
                    self.env.turn_model.as_ref(),
                    self.admin.as_deref(),
                    &section,
                )
                .await
                .map_err(|e| ToolError::Io(e.to_string()))?
            }
            "self_docs" => crate::selfdocs::tool(args).map_err(ToolError::Invalid)?,
            "config_set" => {
                let path = str_arg(args, "path")?;
                if let Some(why) = crate::selfknow::forbidden_path(&path) {
                    return Err(ToolError::Denied(why));
                }
                let raw = str_arg(args, "value")?;
                let value = crate::selfknow::parse_scalar(&raw);
                let admin = self.admin.as_ref().ok_or_else(|| {
                    ToolError::Denied("configuration non modifiable depuis ce contexte".into())
                })?;
                let generation = admin
                    .set_config(&path, value.clone())
                    .await
                    .map_err(ToolError::Invalid)?;
                let mut warnings: Vec<String> =
                    penelope_kernel::coherence::contradictions(&s.config.config())
                        .into_iter()
                        .filter(|c| c.concerns(&path))
                        .map(|c| c.message)
                        .collect();
                let applied_value = if path == "sandbox.workspaces" {
                    let stored = s.config.config().sandbox.workspaces.clone();
                    let requested: Vec<&str> = match &value {
                        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
                        Value::String(item) => vec![item],
                        _ => Vec::new(),
                    };
                    for (asked, actual) in requested.iter().zip(&stored) {
                        if asked != actual {
                            warnings.push(format!(
                                "workspace `{asked}` enregistré sous sa forme réelle `{actual}`"
                            ));
                        }
                    }
                    for root in &stored {
                        if !s.platform.dirs.expand(root).exists() {
                            warnings.push(format!("workspace `{root}` n'existe pas encore"));
                        }
                    }
                    json!(stored)
                } else {
                    value
                };
                let (applied, note) = config_application_time(&path);
                json!({
                    "path": path,
                    "value": applied_value,
                    "generation": generation,
                    "applied": applied,
                    "remarque": note,
                    "avertissements": warnings,
                })
            }
            "time_now" => {
                let tz = args
                    .get("timezone")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&cfg.owner.timezone)
                    .to_string();
                let utc =
                    chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default();
                match tz.parse::<chrono_tz::Tz>() {
                    Ok(t) => {
                        let local = utc.with_timezone(&t);
                        json!({"iso": local.to_rfc3339(), "timezone": tz,
                               "lisible": local.format("%A %d %B %Y, %H:%M").to_string()})
                    }
                    Err(_) => {
                        return Err(ToolError::Invalid(format!("fuseau inconnu : {tz}")));
                    }
                }
            }

            // -------------------------------------------------------- planification
            "schedule_create" => {
                let kind = penelope_workflow::TriggerKind::parse(&str_arg(args, "kind")?)
                    .ok_or_else(|| ToolError::Invalid("kind inconnu".into()))?;
                let mut target = args.get("target").cloned().unwrap_or(Value::Null);
                // Le déclencheur répond par ce canal ; la session d'origine n'est qu'une
                // référence, chaque exécution ouvre la sienne (issue #39).
                if let Some(o) = target.as_object_mut() {
                    if let Some(sid) = o.remove("session_id") {
                        o.entry("origin_session").or_insert(sid);
                    }
                    o.entry("origin_session")
                        .or_insert(json!(self.env.session_id));
                    o.entry("origin").or_insert(self.env.origin.to_value());
                }
                let spec = args.get("spec").cloned().unwrap_or(Value::Null);
                crate::scheduler::create(
                    s,
                    kind,
                    spec,
                    target,
                    args.get("dedup").cloned().unwrap_or(Value::Null),
                )
                .await
                .map_err(ToolError::Invalid)?
            }
            "schedule_list" => json!(
                crate::scheduler::listing(s)
                    .await
                    .map_err(|e| ToolError::Other(e.to_string()))?
            ),
            "schedule_move" => {
                let id = str_arg(args, "id")?;
                let (chat_id, topic_id) = match str_arg(args, "to")?.as_str() {
                    "private" => (s.config.config().owner.telegram_user_id, None),
                    "here" => self.env.origin.telegram_chat().ok_or_else(|| {
                        ToolError::Invalid(
                            "`here` : cette conversation n'est pas Telegram, `private` ou \
                             `penelope schedule move`"
                                .into(),
                        )
                    })?,
                    other => {
                        return Err(ToolError::Invalid(format!(
                            "`to` : `here` ou `private`, pas `{other}`"
                        )));
                    }
                };
                let to = crate::scheduler::retarget(s, &id, chat_id, topic_id)
                    .await
                    .map_err(ToolError::Invalid)?;
                json!({"id": id, "destination": to})
            }
            "schedule_delete" => {
                s.schedules
                    .set_state(&str_arg(args, "id")?, "deleted")
                    .await?;
                json!({"deleted": true})
            }

            // -------------------------------------------------------- messages
            "send_message" => {
                let m = self
                    .messenger
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("aucun canal de message disponible".into()))?;
                m.send_session_text(
                    &self.env.session_id,
                    &self.env.origin,
                    &str_arg(args, "text")?,
                )
                .await
                .map_err(ToolError::Network)?;
                json!({"sent": true})
            }
            "send_voice" => {
                let admin = self
                    .admin
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("vocal indisponible hors du daemon".into()))?;
                admin
                    .send_voice(&self.env.session_id, &self.env.origin, args)
                    .await
                    .map_err(ToolError::Invalid)?
            }
            "send_file" => {
                let p = self.path_arg(args, "path")?;
                let m = self
                    .messenger
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("aucun canal de message disponible".into()))?;
                m.send_session_file(
                    &self.env.session_id,
                    &self.env.origin,
                    &p,
                    args.get("caption").and_then(|v| v.as_str()),
                )
                .await
                .map_err(ToolError::Network)?;
                json!({"sent": true, "path": p})
            }

            // -------------------------------------------------------- mémoire
            "mem_search" => {
                let filter = penelope_memory::SearchFilter {
                    level: args
                        .get("level")
                        .and_then(|v| v.as_str())
                        .and_then(penelope_memory::Level::parse),
                    projet: args
                        .get("projet")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    include_episodic: b_arg(args, "include_episodic").unwrap_or(false),
                    // Recherche explicite : les documents ingérés en font partie, encadrés.
                    include_untrusted: true,
                    slug: args.get("slug").and_then(|v| v.as_str()).map(String::from),
                    limit: u_arg(args, "limit").unwrap_or(10),
                    ..Default::default()
                };
                let query = str_arg(args, "query")?;
                let vector = match &self.orchestrator {
                    Some(o) => o.embed_query(&query).await,
                    None => None,
                };
                let hits = s.memory.search(&query, vector, &filter, &[]).await?;
                // Rien trouvé : le périmètre et ce qui en sort, jamais un silence (issue #15).
                if hits.is_empty() {
                    return Ok(ToolOutcome::ok(
                        crate::vault_inventory::empty_search_note(s).await,
                    ));
                }
                // Retour d'usage (issues #37 et #105) : trouvé par une recherche, donc
                // servi ; utile si la réponse d'une conversation le reprend.
                let in_chat = matches!(
                    s.sessions.get(&self.env.session_id).await,
                    Ok(Some(ref x)) if x.kind == penelope_kernel::session::SessionKind::Chat
                );
                let uids: Vec<String> = hits.iter().map(|h| h.entry.uid.clone()).collect();
                crate::usage_feedback::served(s, &self.env.session_id, in_chat, &uids, &query)
                    .await;
                let mut out = Vec::new();
                for h in &hits {
                    let untrusted = h.entry.etype == penelope_memory::ingest::SOURCE_ETYPE
                        && s.memory.origin_of(&h.entry.uid).await?
                            != Some(penelope_memory::Origin::Owner);
                    let texte = if untrusted {
                        penelope_memory::provenance::frame_untrusted(&h.entry.text, &h.entry.file)
                    } else {
                        h.entry.text.clone()
                    };
                    out.push(json!({
                        "uid": h.entry.uid, "texte": texte,
                        "niveau": h.entry.level.as_str(), "fichier": h.entry.file,
                        "type": h.entry.etype,
                        "score": (h.score * 1000.0).round() / 1000.0,
                    }));
                }
                json!(out)
            }
            "mem_neighbors" => crate::concepts::neighbors(s, &str_arg(args, "slug")?)
                .await
                .map_err(|e| ToolError::Other(e.to_string()))?,
            "mem_get" => {
                let mut entries = match args.get("uid").and_then(|v| v.as_str()) {
                    Some(uid) => s.memory.get(uid).await?.into_iter().collect(),
                    None => s.memory.by_slug(&str_arg(args, "slug")?).await?,
                };
                // Un document ingéré se lit par passages, encadrés s'ils ne sont pas fiables.
                const MAX_PASSAGES: usize = 40;
                let total = entries.len();
                entries.truncate(MAX_PASSAGES);
                for e in entries.iter_mut() {
                    if e.etype == penelope_memory::ingest::SOURCE_ETYPE
                        && s.memory.origin_of(&e.uid).await? != Some(penelope_memory::Origin::Owner)
                    {
                        e.text = penelope_memory::provenance::frame_untrusted(&e.text, &e.file);
                    }
                }
                if args.get("uid").is_some() {
                    serde_json::to_value(entries.into_iter().next()).unwrap_or_default()
                } else {
                    json!({
                        "entrees": entries,
                        "total": total,
                        "tronque": total > MAX_PASSAGES,
                    })
                }
            }
            "mem_note" => {
                let ctype = penelope_memory::CandidateType::parse(&str_arg(args, "type")?)
                    .ok_or_else(|| ToolError::Invalid("type inconnu".into()))?;
                // Un secret part dans le magasin, la note n'en garde que la référence
                // (issue #37).
                let (texte, _) = crate::secret_shelf::shelve(s, &str_arg(args, "texte")?)
                    .map_err(ToolError::Denied)?;
                crate::vault_ops::write_filter(&texte).map_err(ToolError::Denied)?;
                // Une règle dictée par le propriétaire compte comme la sienne, à condition
                // d'en citer l'extrait mot pour mot (issue #24).
                let citation = args.get("citation").and_then(|v| v.as_str());
                let from_owner = match citation {
                    Some(c) => self.cited_by_owner(c).await?,
                    None => false,
                };
                let origin = if from_owner {
                    penelope_memory::Origin::Owner
                } else {
                    penelope_memory::Origin::Agent
                };
                let mut c = penelope_memory::Candidate::new(
                    ctype,
                    &texte,
                    origin,
                    "interactive",
                    &s.clock.now_rfc3339(),
                )
                .in_session(&self.env.session_id);
                if let Some(i) = u_arg(args, "importance") {
                    c = c.with_importance(i.clamp(1, 10) as u8);
                }
                if let Some(q) = args.get("quand").and_then(|v| v.as_str()) {
                    let w = penelope_memory::When::parse(q).map_err(ToolError::Invalid)?;
                    c = c.with_when(w);
                }
                let n = s
                    .candidates
                    .record(vec![c], cfg.memory.review_max_candidates.max(1))
                    .await?;
                json!({
                    "noted": n == 1,
                    "origine": origin.as_str(),
                    "remarque": match (citation.is_some(), from_owner) {
                        (_, true) => "citation vérifiée : consolidé comme une règle du propriétaire au prochain rêve",
                        (true, false) => "citation introuvable mot pour mot dans les messages du propriétaire de ce tour : la règle lui sera demandée",
                        (false, false) => "consolidé lors du prochain rêve",
                    },
                })
            }
            "mem_remember" => {
                let level = match str_arg(args, "niveau")?.as_str() {
                    "profil" => penelope_memory::Level::Profil,
                    "coeur" => penelope_memory::Level::Coeur,
                    "projet" => penelope_memory::Level::Projet,
                    _ => penelope_memory::Level::Cure,
                };
                let vault = crate::conversation::vault_dir(s);
                let uid = crate::vault_ops::remember(
                    s,
                    &vault,
                    level,
                    &str_arg(args, "texte")?,
                    &self.env.session_id,
                )
                .await
                .map_err(ToolError::Denied)?;
                json!({"uid": uid, "niveau": level.as_str()})
            }
            "mem_forget" => {
                let vault = crate::conversation::vault_dir(s);
                let done = crate::vault_ops::forget(s, &vault, &str_arg(args, "uid")?)
                    .await
                    .map_err(ToolError::Io)?;
                json!({"forgotten": done})
            }
            "intent_create" => {
                let texte = str_arg(args, "texte")?;
                if let penelope_memory::intents::IntentKind::Temporal(when) =
                    penelope_memory::intents::classify_intent(&texte)
                {
                    return Err(ToolError::Invalid(format!(
                        "intention datée (« {when} ») : c'est un rappel, pas une intention. \
                         Utiliser `schedule_create` avec kind `cron`, spec `{{\"expr\": \"<minute> \
                         <heure> <jour> <mois> *\", \"once\": true}}` (sans `once` s'il se répète) \
                         et target `{{\"type\": \"notify\", \"template\": \"⏰ …\"}}` ; `time_now` \
                         donne la date du jour"
                    )));
                }
                let mut triggers: Vec<String> = args
                    .get("declencheurs")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if triggers.is_empty() {
                    triggers = penelope_memory::intents::extract_triggers(&texte);
                }
                let ms = |d: &str| {
                    penelope_kernel::config::parse_duration(d)
                        .map(|x| x.as_millis() as i64)
                        .ok()
                };
                let i = s
                    .intents
                    .create(
                        &texte,
                        triggers,
                        None,
                        ms(&cfg.memory.intents.cooldown).unwrap_or(86_400_000),
                        cfg.memory.intents.fire_budget,
                        ms(&cfg.memory.intents.expiry),
                    )
                    .await?;
                serde_json::to_value(i).unwrap_or_default()
            }
            "intent_list" => serde_json::to_value(s.intents.all().await?).unwrap_or_default(),
            "intent_cancel" => json!({"cancelled": s.intents.cancel(&str_arg(args, "id")?).await?}),

            // -------------------------------------------------------- historique
            "history_grep" => {
                let scope = args
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("session");
                let session = (scope != "all").then_some(self.env.session_id.as_str());
                let hits = s
                    .context
                    .history
                    .grep(&str_arg(args, "query")?, session, 20)
                    .await?;
                let mut hits = serde_json::to_value(hits).unwrap_or_default();
                with_session_labels(s, &mut hits).await;
                if scope != "all" && hits.as_array().is_some_and(|h| h.is_empty()) {
                    json!({
                        "extraits": hits,
                        "note": "rien dans cette session : `scope: \"all\"` cherche dans les \
                                 sessions précédentes",
                    })
                } else {
                    hits
                }
            }
            "history_describe" => {
                let m = s.context.lcm.describe(&str_arg(args, "node_id")?).await?;
                serde_json::to_value(m).unwrap_or_default()
            }
            "history_expand" => {
                let node = s
                    .context
                    .lcm
                    .get(&str_arg(args, "node_id")?)
                    .await?
                    .ok_or_else(|| ToolError::Invalid("nœud introuvable".into()))?;
                let (from, to) = (node.from_seq.unwrap_or(0), node.to_seq.unwrap_or(i64::MAX));
                let page = u_arg(args, "page").unwrap_or(0);
                let entries = s.context.history.load(&node.session_id, from).await?;
                let msgs: Vec<Value> = entries
                    .iter()
                    .filter(|e| e.seq <= to)
                    .skip(page * 20)
                    .take(20)
                    .map(|e| json!({"seq": e.seq, "role": e.message.role.as_str(), "texte": e.message.text()}))
                    .collect();
                json!({"node": node.id, "page": page, "messages": msgs})
            }
            "history_expand_query" => {
                let q = str_arg(args, "question")?;
                let terms = penelope_context::store::significant_terms(&q);
                let ranked = s.context.history.grep_terms(&terms, None, 20, 30).await?;
                let mut hits = serde_json::to_value(ranked).unwrap_or_default();
                with_session_labels(s, &mut hits).await;
                json!({"question": q, "mots": terms, "extraits": hits})
            }
            "artifact_read" => {
                let cursor = u_arg(args, "cursor").unwrap_or(0) as u64;
                let v = s
                    .context
                    .history
                    .read_artifact(&str_arg(args, "id")?, cursor, 16_000)
                    .await?;
                serde_json::to_value(v).unwrap_or_default()
            }

            // -------------------------------------------------------- skills
            "skill_search" => {
                let hits = s
                    .skills
                    .search(&str_arg(args, "query")?, u_arg(args, "limit").unwrap_or(5));
                json!(
                    hits.iter()
                        .map(|(k, score)| {
                            // Une skill dont un binaire requis manque est annoncée telle
                            // quelle (issue #156) : elle reste listée — c'est au
                            // propriétaire d'installer, jamais à Pénélope (#146) — mais
                            // le modèle sait avant de l'appliquer qu'elle échouera.
                            let mut o = json!({
                                "name": k.name, "description": k.description, "score": score
                            });
                            let missing = crate::machine::missing_binaries(&k.requires);
                            if !missing.is_empty() {
                                o["binaires_manquants"] = json!(missing);
                            }
                            o
                        })
                        .collect::<Vec<_>>()
                )
            }
            "skill_load" => {
                let name = str_arg(args, "name")?;
                let sk = s
                    .skills
                    .get(&name)
                    .ok_or_else(|| ToolError::Invalid(format!("skill `{name}` introuvable")))?;
                // Un corps venu d'ailleurs parle en outils Claude Code et en chemins
                // relatifs : la table de correspondance et le dossier absolu sont posés
                // devant, sans toucher au fichier (issue #146).
                let content = match penelope_skills::install::portage_note(&sk) {
                    Some(note) => format!("{note}{}", sk.body),
                    None => sk.body.clone(),
                };
                let mut out = json!({
                    "name": sk.name, "allowed_tools": sk.allowed_tools,
                    "requires": sk.requires, "content": content
                });
                // Dit avant l'application, pas au premier échec de commande (issue #156).
                let missing = crate::machine::missing_binaries(&sk.requires);
                if !missing.is_empty() {
                    out["binaires_manquants"] = json!(missing);
                    out["remarque"] = json!(format!(
                        "Binaire(s) absent(s) de cette machine : {}. Les étapes qui les \
                         appellent échoueront ; dis-le au propriétaire plutôt que de \
                         contourner.",
                        missing.join(", ")
                    ));
                }
                out
            }
            "skill_propose" | "skill_patch" => {
                let skill_name = str_arg(args, "name")?;
                let existing = s.skills.get(&skill_name);
                let proposal = penelope_skills::SkillProposal {
                    name: skill_name.clone(),
                    description: args
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| existing.as_ref().map(|k| k.description.clone()))
                        .unwrap_or_default(),
                    body: str_arg(args, "body")?,
                    allowed_tools: args
                        .get("allowed_tools")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .or_else(|| existing.as_ref().map(|k| k.allowed_tools.clone()))
                        .unwrap_or_default(),
                    kind: if name == "skill_patch" {
                        "patch"
                    } else {
                        "create"
                    }
                    .into(),
                    diff: String::new(),
                    rationale: String::new(),
                };
                proposal.validate().map_err(ToolError::Invalid)?;
                let root = s.platform.dirs.skills();
                let path = penelope_skills::write_skill(&root, &proposal).map_err(ToolError::Io)?;
                crate::runtime::reload_skills(s)
                    .await
                    .map_err(|e| ToolError::Io(e.to_string()))?;
                json!({"written": path, "name": proposal.name})
            }

            // -------------------------------------------------------- workflows
            "workflow_list" => {
                json!(
                s.workflows
                    .all()
                    .iter()
                    .map(|e| json!({"id": e.workflow.metadata.id, "name": e.workflow.metadata.name,
                                    "description": e.workflow.metadata.description}))
                    .collect::<Vec<_>>()
            )
            }
            "workflow_describe" => {
                let id = str_arg(args, "id")?;
                let w = s
                    .workflows
                    .get(&id)
                    .ok_or_else(|| ToolError::Invalid(format!("workflow `{id}` introuvable")))?;
                json!({"definition": w, "graphe": w.render_graph()})
            }
            "workflow_plan" => {
                use penelope_workflow::plan::{PlanStep, PlanStore};
                let id = str_arg(args, "id")?;
                s.workflows
                    .get(&id)
                    .ok_or_else(|| ToolError::Invalid(format!("workflow `{id}` introuvable")))?;
                let plans = PlanStore::new(s.store.clone());
                let existing = plans.get(&self.env.session_id).await?;
                let restore = args.get("restore_version").and_then(Value::as_u64);
                let goal = args.get("goal").and_then(Value::as_str);
                let steps = args.get("steps");
                let draft = if let Some(mut old) = existing {
                    if old.plan.can_execute()
                        && args.get("expected_version").is_none()
                        && goal.is_some()
                        && steps.is_some()
                    {
                        let new = new_workflow_plan(&id, args)?;
                        plans.start_next(&self.env.session_id, &old, &new).await?;
                        new
                    } else {
                        if old.workflow_id != id {
                            return Err(ToolError::Invalid(format!(
                                "la session prépare déjà le workflow `{}`",
                                old.workflow_id
                            )));
                        }
                        if restore.is_some() || goal.is_some() || steps.is_some() {
                            let previous = old.clone();
                            let expected = args
                                .get("expected_version")
                                .and_then(Value::as_u64)
                                .ok_or_else(|| {
                                    ToolError::Invalid(
                                        "expected_version requis pour réviser".into(),
                                    )
                                })?;
                            if let Some(version) = restore {
                                old.restore(expected, version)
                                    .map_err(|e| ToolError::Invalid(e.to_string()))?;
                            } else {
                                let goal =
                                    goal.ok_or_else(|| ToolError::Invalid("goal requis".into()))?;
                                let steps: Vec<PlanStep> =
                                    serde_json::from_value(steps.cloned().ok_or_else(|| {
                                        ToolError::Invalid("steps requis".into())
                                    })?)
                                    .map_err(|e| ToolError::Invalid(e.to_string()))?;
                                old.revise(expected, goal, steps)
                                    .map_err(|e| ToolError::Invalid(e.to_string()))?;
                            }
                            old.params = args.get("params").cloned().unwrap_or(old.params);
                            old.brief = args
                                .get("brief")
                                .and_then(Value::as_str)
                                .map(String::from)
                                .or(old.brief);
                            plans.replace(&self.env.session_id, &previous, &old).await?;
                        }
                        old
                    }
                } else {
                    let new = new_workflow_plan(&id, args)?;
                    plans.create(&self.env.session_id, &new).await?;
                    new
                };
                if let Some(messenger) = &self.messenger {
                    messenger
                        .send_plan_card(&self.env.origin, &self.env.session_id, &draft)
                        .await
                        .map_err(ToolError::Other)?;
                }
                serde_json::to_value(&draft).unwrap_or_default()
            }
            "workflow_start" => {
                if matches!(self.env.origin, Origin::Telegram { .. }) && !self.env.in_workflow {
                    return Err(ToolError::Invalid(
                        "propose d'abord un plan avec workflow_plan ; seul le propriétaire peut valider « vas-y »".into(),
                    ));
                }
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("moteur de workflows indisponible".into()))?;
                o.start_workflow(
                    &str_arg(args, "id")?,
                    args.get("params").cloned().unwrap_or(json!({})),
                    args.get("brief").and_then(|v| v.as_str()),
                    &self.env.origin,
                )
                .await
                .map_err(ToolError::Other)?
            }
            "workflow_status" => serde_json::to_value(s.runs.get(&str_arg(args, "run_id")?).await?)
                .unwrap_or_default(),
            "workflow_control" => {
                let op = str_arg(args, "op")?;
                let run_id = str_arg(args, "run_id")?;
                match &self.orchestrator {
                    Some(o) => o
                        .control_run(&run_id, &op)
                        .await
                        .map_err(ToolError::Other)?,
                    None => {
                        let control = penelope_workflow::Control::parse(&op).ok_or_else(|| {
                            ToolError::Invalid(format!("opération inconnue : {op}"))
                        })?;
                        let st = s.runs.control(&run_id, &control).await?;
                        json!({"state": st.as_str()})
                    }
                }
            }
            "workflow_author" => {
                let draft = args.get("draft").cloned().unwrap_or(Value::Null);
                let raw = match &draft {
                    Value::String(t) => t.clone(),
                    other => other.to_string(),
                };
                // Un brouillon refusé renvoie à la section de la documentation (issue #34).
                let with_doc = |e: String| {
                    let (heading, link) = crate::selfdocs::workflow_doc_for(&e);
                    ToolError::Invalid(format!(
                        "{e}\nDocumentation : « {heading} », {link} (lire avec `self_docs` \
                         action `read`, file `docs/workflows.md`, section « {heading} »)."
                    ))
                };
                let w = penelope_workflow::Workflow::from_json(&raw)
                    .map_err(|e| with_doc(format!("JSON invalide : {e}")))?;
                let known =
                    crate::runtime::workflow_known_with(&cfg, &s.mcp_tools, &s.workflows).await;
                let dir = s.platform.dirs.workflows();
                let path = s.workflows.write(&dir, &w, &known).map_err(with_doc)?;
                s.workflows
                    .load_dir(&dir, penelope_workflow::registry::Scope::User, &known);
                json!({"written": path, "id": w.metadata.id})
            }
            "sub_agent_spawn" => {
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("sous-agents indisponibles".into()))?;
                o.spawn_sub_agent(
                    &self.env.session_id,
                    &str_arg(args, "prompt")?,
                    args.get("model").and_then(|v| v.as_str()),
                    args.get("tools")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    &self.env.origin,
                    cancel,
                )
                .await
                .map_err(ToolError::Other)?
            }
            // ------------------------------------------------- jobs d'outils (#204)
            "job_status" | "job_wait" | "job_cancel" | "job_list" => {
                crate::tool_jobs::tool(s, &self.env.session_id, name, args).await?
            }
            "session_notes" => crate::session_notes::tool(s, &self.env.session_id, args)
                .await
                .map_err(ToolError::Invalid)?,
            "session_metadata" => {
                let op = penelope_kernel::session::MetadataOp::parse(&str_arg(args, "op")?)
                    .ok_or_else(|| ToolError::Invalid("op inconnue".into()))?;
                let key = str_arg(args, "key")?;
                let mut entry = args.get("entry").cloned().unwrap_or(Value::Null);
                if key == "criteria" {
                    let current = s
                        .sessions
                        .get(&self.env.session_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|x| x.metadata["criteria"].clone())
                        .unwrap_or(Value::Null);
                    entry = criteria_entry(op, entry, &current).map_err(ToolError::Invalid)?;
                }
                s.sessions
                    .metadata(&self.env.session_id, op, &key, entry)
                    .await
                    .map_err(|e| ToolError::Other(e.to_string()))?
            }
            "ask_user" => {
                let q = str_arg(args, "question")?;
                let choices: Vec<String> = args
                    .get("choices")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut text = format!("❓ {q}");
                if !choices.is_empty() {
                    text.push_str("\n\n");
                    for c in &choices {
                        text.push_str(&format!("- {c}\n"));
                    }
                }
                if let Some(m) = &self.messenger {
                    m.send_session_text(&self.env.session_id, &self.env.origin, &text)
                        .await
                        .map_err(ToolError::Network)?;
                }
                json!({"asked": true, "remarque": "la réponse arrivera comme un nouveau message"})
            }
            "step_done" | "return_value" if self.env.in_workflow => {
                let run = self
                    .env
                    .run_id
                    .clone()
                    .ok_or_else(|| ToolError::Denied("aucun run en cours".into()))?;
                let key = crate::workflow::step_done_key(&run);
                let stored = s.kv_get(&key).await;
                let mut state: Value = stored
                    .map_err(|e| ToolError::Other(e.to_string()))?
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_else(|| json!({}));
                if name == "return_value" {
                    state["result"] = args.get("result").cloned().unwrap_or(Value::Null);
                    state["content"] = args.get("content").cloned().unwrap_or(Value::Null);
                } else {
                    state["done"] = json!(true);
                }
                s.kv_set(&key, &state.to_string())
                    .await
                    .map_err(|e| ToolError::Other(e.to_string()))?;
                if name == "step_done" {
                    json!({"ok": true, "remarque": "étape terminée : termine ta réponse"})
                } else {
                    json!({"ok": true})
                }
            }
            "step_done" | "return_value" => {
                return Err(ToolError::Denied(format!(
                    "`{name}` n'a de sens que dans une étape de workflow"
                )));
            }
            "image_inspect" => {
                let task =
                    crate::vision::Task::parse(&str_arg(args, "mode")?).ok_or_else(|| {
                        ToolError::Invalid("`mode` : describe, read ou locate".into())
                    })?;
                // Une photo reçue vit dans `{data}/media/photos`, hors des workspaces.
                let mut roots = self.workspaces();
                roots.push(penelope_platform::sandbox::normalise(
                    &s.platform.dirs.data().join("media").join("photos"),
                ));
                let path = penelope_tools::fs::resolve(&str_arg(args, "path")?, &roots)?;
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("modèle de vision indisponible".into()))?;
                let v = o
                    .inspect_image(
                        &self.env.session_id,
                        &path,
                        task,
                        args.get("question")
                            .and_then(|q| q.as_str())
                            .unwrap_or_default(),
                    )
                    .await
                    .map_err(ToolError::Other)?;
                // Ce que le modèle de vision a lu dans l'image est une donnée (§13.3).
                return Ok(untrusted_listing("image", v));
            }
            "image_generate" => {
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("génération d'image indisponible".into()))?;
                let v = o
                    .generate_image(
                        &str_arg(args, "prompt")?,
                        args.get("size").and_then(|v| v.as_str()),
                    )
                    .await
                    .map_err(ToolError::Other)?;
                // Les images partent aussitôt vers le propriétaire (§10.4).
                if let Some(m) = &self.messenger {
                    for f in v["files"].as_array().cloned().unwrap_or_default() {
                        if let Some(path) = f.as_str() {
                            let _ = m
                                .send_session_file(
                                    &self.env.session_id,
                                    &self.env.origin,
                                    Path::new(path),
                                    None,
                                )
                                .await;
                        }
                    }
                }
                v
            }
            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// Réponse de `http_fetch` telle que le modèle la lit : une page HTML devient du texte
    /// lisible et le brut reste relisible en artefact (issue #8) ; le tout encadré comme
    /// donnée non fiable (§13.3).
    async fn fetched_page(&self, mut v: Value, url: &str) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let content_type = v["contentType"].as_str().unwrap_or_default().to_string();
        let body = v["body"].as_str().unwrap_or_default().to_string();
        if penelope_tools::html::looks_like_html(&content_type, &body) {
            let base = v["url"].as_str().and_then(|u| url::Url::parse(u).ok());
            let text = penelope_tools::html::to_text(&body, base.as_ref());
            let raw = s
                .context
                .history
                .put_artifact(
                    Some(&self.env.session_id),
                    self.env.run_id.as_deref(),
                    "html",
                    None,
                    &body,
                )
                .await?;
            v["body"] = json!(text);
            v["format"] = json!("texte extrait du HTML");
            v["raw_artifact"] = json!(raw.id);
        }
        // Une forge dont le client est connecté : le dire au lieu de laisser le modèle
        // réapprendre à chaque sujet (issue #156). La lecture a déjà eu lieu, rien n'est
        // bloqué — c'est une remarque, comme celle du réseau coupé (#106).
        let hint = match crate::machine::cached(s).await {
            Some(inv) => crate::machine::forge_hint(&inv, url),
            None => None,
        };
        if let Some(h) = &hint {
            v["remarque"] = json!(h);
        }
        let mut shown = penelope_tools::render(&v);
        if let Some(id) = v["raw_artifact"].as_str() {
            shown.push_str(&format!("\n\n[HTML d'origine : artifact_read(\"{id}\")]"));
        }
        let mut o = ToolOutcome::ok(v);
        o.text = penelope_observe::injection::wrap_untrusted(&format!("http_fetch {url}"), &shown);
        Ok(o)
    }

    /// Vrai si `citation` figure mot pour mot (espaces et casse près) dans un message du
    /// propriétaire du tour en cours : messages utilisateur depuis la dernière réponse
    /// finale, hors déclencheurs, relances et contenus transférés.
    async fn cited_by_owner(&self, citation: &str) -> ToolResult<bool> {
        let norm = |t: &str| {
            t.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let needle = norm(citation);
        if needle.chars().count() < 8 || matches!(self.env.origin, Origin::Internal { .. }) {
            return Ok(false);
        }
        let s = &self.services;
        let sid = self.env.session_id.clone();
        let last: i64 = s
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1",
                    [sid],
                    |r| r.get(0),
                )?)
            })
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let entries = s
            .context
            .history
            .load(&self.env.session_id, (last - 60).max(0))
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        for e in entries.iter().rev() {
            let m = &e.message;
            match m.role {
                penelope_llm::types::Role::Assistant if m.tool_calls.is_empty() => break,
                penelope_llm::types::Role::User => {
                    let text = m.text();
                    if text.starts_with("[déclencheur planifié]")
                        || text.starts_with("[relance]")
                        || text.contains("<<<DONNÉES NON FIABLES")
                    {
                        continue;
                    }
                    if norm(&text).contains(&needle) {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        Ok(false)
    }

    async fn tool_search(&self, args: &Value) -> ToolResult<ToolOutcome> {
        let q = str_arg(args, "query")?;
        let vector = match &self.orchestrator {
            Some(o) => o.embed_query(&q).await,
            None => None,
        };
        let limit = u_arg(args, "limit").unwrap_or(10);
        let server = args.get("server").and_then(|v| v.as_str());
        // Outils natifs à la demande d'abord (#104) : leur description est la nôtre.
        let natives: Vec<Value> = if server.is_none() || server == Some("natif") {
            penelope_tools::search_on_demand(&q, limit)
                .into_iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "server": "natif",
                        "description": t.description.chars().take(200).collect::<String>(),
                        "risk": t.risk.as_str(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        let hits = self
            .services
            .mcp_tools
            .search_hybrid(&q, vector.as_deref(), server, limit)
            .await?;
        if hits.is_empty() && natives.is_empty() {
            return Ok(ToolOutcome::ok(json!({
                "résultats": [],
                "remarque": "aucun outil ne correspond ; les serveurs MCP : /mcp",
            })));
        }
        if hits.is_empty() {
            return Ok(ToolOutcome::ok(json!(natives)));
        }
        // Descriptions écrites par les serveurs : encadrées comme tout contenu observé,
        // avec l'alerte du détecteur local s'il y voit une consigne (#92).
        let mut all = natives;
        all.extend(hits.iter().map(|h| h.tool.short()));
        Ok(untrusted_listing("mcp tool_search", json!(all)))
    }

    async fn tool_describe(&self, args: &Value) -> ToolResult<ToolOutcome> {
        let names: Vec<String> = args
            .get("names")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() {
            return Err(ToolError::Invalid("`names` est vide".into()));
        }
        // Natifs : schéma du catalogue, et l'outil décrit rejoint la liste de la session.
        let (natives, mcp): (Vec<String>, Vec<String>) = names
            .into_iter()
            .partition(|n| penelope_tools::tool_spec(n).is_some());
        let mut described: Vec<Value> = Vec::new();
        for n in &natives {
            if let Some(t) = penelope_tools::tool_spec(n) {
                crate::tools_on_demand::touch(&self.services, &self.env.session_id, n).await;
                described.push(json!({
                    "name": t.name,
                    "server": "natif",
                    "description": t.description,
                    "inputSchema": t.schema,
                    "risk": t.risk.as_str(),
                }));
            }
        }
        if mcp.is_empty() {
            return Ok(ToolOutcome::ok(json!(described)));
        }
        let v = self.services.mcp_tools.describe(&mcp).await?;
        described.extend(v);
        Ok(untrusted_listing("mcp tool_describe", json!(described)))
    }

    /// `tool_call` : un outil natif (à la demande ou non) part par le même chemin qu'un
    /// appel direct ; sinon, un outil MCP.
    async fn tool_call(
        &self,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let name = str_arg(args, "name")?;
        let inner = call_arguments(args)?;
        if penelope_tools::tool_spec(&name).is_some() {
            return self.dispatch(&name, &inner, cancel).await;
        }
        self.mcp_call(&name, &inner).await
    }

    async fn mcp_call(&self, qualified: &str, args: &Value) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        // L'intention est pour la carte, pas pour le serveur (#116).
        let args = &crate::agent::without_intention(args);
        // Refus local, sans aller au serveur : `explain` y joint son schéma (#110).
        s.mcp_tools
            .validate_args(qualified, args)
            .await
            .map_err(|e| match e {
                penelope_mcp::McpError::UnknownTool(q) => ToolError::Unknown(q),
                penelope_mcp::McpError::InvalidArguments { reason, .. } => {
                    ToolError::Invalid(reason)
                }
                other => ToolError::Invalid(other.to_string()),
            })?;
        s.mcp_tools.mark_for_promotion(&[qualified.to_string()]);
        let gw = self
            .mcp
            .as_ref()
            .ok_or_else(|| ToolError::Other("aucun serveur MCP n'est démarré".into()))?;
        let v = gw
            .call_tool(qualified, args, self.elicitation_destination())
            .await
            .map_err(ToolError::Other)?;
        let is_error = v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false);
        let text = penelope_observe::injection::wrap_untrusted(qualified, &render_mcp_result(&v));
        Ok(ToolOutcome {
            value: v,
            is_error,
            text,
            eager: true,
        })
    }
}

impl NativeToolExecutor {
    /// Exécute, et rend une erreur d'arguments ou de nom avec de quoi se corriger.
    async fn run(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> Result<ToolOutcome, ToolError> {
        let started = std::time::Instant::now();
        let result = match self.dispatch(name, args, cancel).await {
            Err(e) => Err(self.explain(name, args, e).await),
            Ok(o) => {
                // Un secret lu (fichier, sortie de commande, page) devient une valeur
                // connue : recopié plus tard dans une commande ou un message, il est
                // masqué partout où la rédaction passe (issue #134). Rien n'est modifié
                // de ce qui s'exécute.
                penelope_observe::redact::learn_secrets(&o.text);
                Ok(o)
            }
        };
        let (tool, effective_args) = if name == "tool_call" {
            (
                args.get("name").and_then(Value::as_str).unwrap_or(name),
                effective_arguments(name, args),
            )
        } else {
            (name, args.clone())
        };
        let (ok, output) = match &result {
            Ok(outcome) => (!outcome.is_error, outcome.value.clone()),
            Err(error) => (false, json!({"error": error.to_string()})),
        };
        let payload = json!({
            "tool": tool,
            "args": crate::runtime_events::bounded_redacted(&crate::agent::without_intention(&effective_args)),
            "result": crate::runtime_events::bounded_redacted(&output),
            "ok": ok,
            "duration_ms": started.elapsed().as_millis() as u64,
            "cost_usd_estimated": if tool.starts_with("mcp__") { Value::Null } else { json!(0.0) },
        });
        let mut event = penelope_kernel::event::EventDraft::new("runtime.tool", payload)
            .session(&self.env.session_id);
        if let Some(run_id) = &self.env.run_id {
            event = event.run(run_id);
        }
        if let Err(error) = self.services.events.append(event).await {
            tracing::warn!(%error, tool, "événement d'outil non enregistré");
        }
        result
    }

    /// Arguments d'un appel, validés sans rien exécuter (issue #117) : balisage laissé
    /// par le modèle, outil connu, schéma natif ou MCP.
    async fn validate_call(&self, name: &str, args: &Value) -> ToolResult<()> {
        if let Some((path, marker)) = penelope_tools::call_markup(args) {
            let field = if path.is_empty() {
                "arguments".to_string()
            } else {
                format!("`{path}`")
            };
            return Err(ToolError::Invalid(format!(
                "la valeur de {field} contient le balisage d'appel d'outil du modèle \
                 (`{marker}`) : l'appel est mal formé, ce n'est pas une valeur ; renvoie les \
                 arguments en JSON structuré, selon leur type"
            )));
        }
        let (target, inner) = if name == "tool_call" {
            (
                args.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                call_arguments(args)?,
            )
        } else {
            (name.to_string(), args.clone())
        };
        match target.as_str() {
            "tool_search" | "tool_describe" | "tool_call" => Ok(()),
            "git_clone" => {
                penelope_tools::validate_args(&target, &inner)?;
                penelope_tools::git::normalize_clone_url(&str_arg(&inner, "url")?)?;
                Ok(())
            }
            t if penelope_tools::tool_spec(t).is_some() => penelope_tools::validate_args(t, &inner),
            t if t.starts_with("mcp__") || name == "tool_call" => self
                .services
                .mcp_tools
                .validate_args(t, &crate::agent::without_intention(&inner))
                .await
                .map_err(|e| match e {
                    penelope_mcp::McpError::UnknownTool(q) => ToolError::Unknown(q),
                    penelope_mcp::McpError::InvalidArguments { reason, .. } => {
                        ToolError::Invalid(reason)
                    }
                    other => ToolError::Invalid(other.to_string()),
                }),
            t => Err(ToolError::Unknown(t.to_string())),
        }
    }

    /// Schéma d'arguments d'un outil natif ou MCP.
    async fn schema_of(&self, tool: &str) -> Option<Value> {
        if let Some(t) = penelope_tools::tool_spec(tool) {
            return Some(t.schema);
        }
        self.services
            .mcp_tools
            .get(tool)
            .await
            .ok()
            .flatten()
            .map(|t| t.input_schema)
    }

    /// Issue #110 : une erreur d'arguments porte les paramètres attendus (outil natif ou
    /// MCP, borné) ; un `tool_call` arrivé sans arguments le dit comme tel ; un nom inconnu
    /// rend les noms proches. Le filet reste la garde de boucle, qui compare les appels.
    async fn explain(&self, name: &str, args: &Value, e: ToolError) -> ToolError {
        let via_call = name == "tool_call";
        let target = if via_call {
            args.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        } else {
            name.to_string()
        };
        // `args` vide et pas de repli en chaîne : les arguments ont pu se perdre en route.
        let lost = via_call
            && call_arguments(args)
                .ok()
                .and_then(|v| v.as_object().map(|o| o.is_empty()))
                .unwrap_or(false);
        let lost_note = |reason: String, schema: &Value| -> String {
            if !lost {
                return reason;
            }
            let required: Vec<String> = schema["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(|r| format!("`{r}`"))
                        .collect()
                })
                .unwrap_or_default();
            if required.is_empty() {
                return reason;
            }
            format!(
                "`tool_call` est arrivé avec `args` vide alors que {} est requis. Si tu \
                 l'avais rempli, les arguments ont été perdus en route : renvoie-les dans \
                 `args_json`, en chaîne JSON (par exemple \"{{\\\"{}\\\": …}}\") ; sinon, \
                 ajoute-les ({reason})",
                required.join(", "),
                required[0].trim_matches('`')
            )
        };
        match e {
            ToolError::Invalid(reason) if !target.is_empty() => match self.schema_of(&target).await
            {
                Some(schema) => ToolError::BadArguments {
                    reason: lost_note(reason, &schema),
                    expected: penelope_tools::expected_args(
                        &schema,
                        penelope_tools::EXPECTED_ARGS_MAX_CHARS,
                    ),
                    tool: target,
                },
                None => ToolError::Invalid(reason),
            },
            ToolError::BadArguments {
                tool,
                reason,
                expected,
            } => {
                let reason = match self.schema_of(&tool).await {
                    Some(schema) => lost_note(reason, &schema),
                    None => reason,
                };
                ToolError::BadArguments {
                    tool,
                    reason,
                    expected,
                }
            }
            ToolError::Unknown(n) | ToolError::NoSuchTool { name: n, .. } => {
                let mut names: Vec<String> = penelope_tools::all_tools()
                    .iter()
                    .map(|t| t.name.to_string())
                    .collect();
                names.extend(self.services.mcp_tools.names().await.unwrap_or_default());
                ToolError::NoSuchTool {
                    close: penelope_tools::close_names(&n, names.iter().map(String::as_str), 5),
                    name: n,
                }
            }
            other => other,
        }
    }
}

/// Arguments d'un `tool_call` : l'objet `args`, ou `args_json` quand l'objet arrive vide
/// (certains fournisseurs vident un objet sans propriétés déclarées, issue #110).
pub(crate) fn call_arguments(args: &Value) -> ToolResult<Value> {
    let object = args.get("args").filter(|v| !v.is_null());
    if let Some(v) = object.filter(|v| v.as_object().is_some_and(|o| !o.is_empty())) {
        return Ok(v.clone());
    }
    // Des arguments rendus en chaîne, dans `args_json` ou à la place de l'objet.
    let raw = args
        .get("args_json")
        .and_then(|v| v.as_str())
        .or_else(|| object.and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty());
    if let Some(raw) = raw {
        return serde_json::from_str::<Value>(raw)
            .ok()
            .filter(|v| v.is_object())
            .ok_or_else(|| ToolError::Invalid("`args_json` n'est pas un objet JSON".into()));
    }
    Ok(json!({}))
}

/// Arguments sur lesquels portent la politique et la carte d'approbation : ceux de
/// l'outil visé par un `tool_call`, ceux de l'appel sinon.
pub(crate) fn effective_arguments(tool: &str, args: &Value) -> Value {
    if tool == "tool_call" {
        call_arguments(args).unwrap_or_else(|_| json!({}))
    } else {
        args.clone()
    }
}

/// `cd <répertoire> && <commande>` rendu en `{command: <commande>, cwd: <répertoire>}`
/// quand le répertoire est dans un workspace et que l'appel ne donne pas déjà son `cwd`
/// (issue #123) : la commande se classe, s'approuve et se règle pour ce qu'elle est. Hors
/// des workspaces, la ligne reste composée, donc demandée.
pub(crate) fn lift_cd(args: &Value, workspaces: &[PathBuf]) -> Option<Value> {
    let obj = args.as_object()?;
    if obj
        .get("cwd")
        .and_then(|v| v.as_str())
        .is_some_and(|c| !c.trim().is_empty())
    {
        return None;
    }
    let (dir, rest) = penelope_tools::shell::split_cd_prefix(obj.get("command")?.as_str()?)?;
    let cwd = penelope_tools::fs::resolve(&dir, workspaces).ok()?;
    let mut out = obj.clone();
    out.insert("command".into(), json!(rest));
    out.insert("cwd".into(), json!(cwd.to_string_lossy()));
    Some(Value::Object(out))
}

#[async_trait::async_trait]
impl ToolExecutor for NativeToolExecutor {
    fn policy_workspace(&self) -> Option<PathBuf> {
        self.workspaces().into_iter().next()
    }

    /// Même session, même origine, mêmes branchements : ce que le job exécutera hors du
    /// tour est l'exécuteur du tour, sans son emprunt (issue #204).
    fn detached(&self) -> Option<Arc<dyn ToolExecutor + Send + Sync>> {
        let mut copy = NativeToolExecutor::new(self.services.clone(), self.env.clone());
        copy.locks = self.locks.clone();
        copy.messenger = self.messenger.clone();
        copy.mcp = self.mcp.clone();
        copy.orchestrator = self.orchestrator.clone();
        copy.admin = self.admin.clone();
        Some(Arc::new(copy))
    }

    fn normalise_call(&self, name: &str, args: &Value) -> Option<Value> {
        let workspaces = self.workspaces();
        match name {
            "shell_exec" => lift_cd(args, &workspaces),
            // Par `tool_call`, l'appel interne ; `args_json` cède la place à l'objet.
            "tool_call" if args.get("name").and_then(|v| v.as_str()) == Some("shell_exec") => {
                let inner = lift_cd(&call_arguments(args).ok()?, &workspaces)?;
                let mut out = args.as_object()?.clone();
                out.remove("args_json");
                out.insert("args".into(), inner);
                Some(Value::Object(out))
            }
            _ => None,
        }
    }

    async fn precheck(&self, name: &str, args: &Value) -> Result<(), ToolError> {
        match self.validate_call(name, args).await {
            Ok(()) => Ok(()),
            Err(e) => Err(self.explain(name, args, e).await),
        }
    }

    async fn execute(&self, name: &str, args: &Value) -> Result<ToolOutcome, ToolError> {
        self.run(name, args, &penelope_llm::CancelToken::new())
            .await
    }

    async fn execute_cancellable(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> Result<ToolOutcome, ToolError> {
        self.run(name, args, cancel).await
    }

    async fn describe_call(&self, name: &str, args: &Value) -> CallInfo {
        let shell_network = self.services.config.config().sandbox.shell_network;
        let s = &self.services;
        let mcp_info = |q: String| async move {
            let risk = match s.mcp_tools.get(&q).await {
                Ok(Some(t)) => t.risk,
                _ => RiskClass::Unknown,
            };
            let policy = match &self.mcp {
                Some(gw) => gw
                    .tool_policy(&q)
                    .await
                    .and_then(|p| penelope_kernel::risk::PolicyDecision::parse(&p)),
                None => None,
            };
            CallInfo {
                idempotent: risk == RiskClass::Read,
                effective_name: q,
                risk,
                policy,
            }
        };
        match name {
            "tool_search" | "tool_describe" => CallInfo {
                effective_name: name.to_string(),
                risk: RiskClass::Read,
                idempotent: true,
                policy: None,
            },
            "tool_call" => {
                let q = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool_call")
                    .to_string();
                // Un natif par `tool_call` garde sa classe de risque et sa politique : la
                // même carte qu'un appel direct (#104).
                if penelope_tools::tool_spec(&q).is_some() {
                    let inner = effective_arguments("tool_call", args);
                    return native_info(&q, &inner, shell_network);
                }
                mcp_info(q).await
            }
            n if n.starts_with("mcp__") => mcp_info(n.to_string()).await,
            _ => native_info(name, args, shell_network),
        }
    }
}

/// Vrai quand un appel `shell_exec` demande le réseau (issue #106).
pub(crate) fn wants_network(tool: &str, args: &Value) -> bool {
    tool == "shell_exec" && args.get("network").and_then(|v| v.as_bool()) == Some(true)
}

/// Risque et nom effectif d'un appel d'outil natif. `shell_network` : le réseau est
/// ouvert à toutes les commandes par la configuration.
fn native_info(name: &str, args: &Value, shell_network: bool) -> CallInfo {
    match name {
        // Réseau demandé pour une commande : une action externe, approuvée comme telle.
        "shell_exec" if wants_network(name, args) && !shell_network => CallInfo {
            effective_name: name.to_string(),
            risk: RiskClass::External,
            idempotent: false,
            policy: None,
        },
        // Une lecture simple (`ls`, `cat`, `grep`, `git status`…) est une lecture : elle ne
        // demande rien, comme `fs_read` (issue #111).
        "shell_exec"
            if !wants_network(name, args)
                && args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .is_some_and(penelope_tools::shell::is_read_command) =>
        {
            CallInfo {
                effective_name: name.to_string(),
                risk: RiskClass::Read,
                idempotent: true,
                policy: None,
            }
        }
        "config_set" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            CallInfo {
                effective_name: name.to_string(),
                risk: if crate::selfknow::sensitive_path(path) {
                    RiskClass::Destructive
                } else {
                    RiskClass::Write
                },
                idempotent: true,
                policy: None,
            }
        }
        _ => CallInfo {
            effective_name: name.to_string(),
            risk: penelope_tools::effective_risk(name, &Default::default()),
            idempotent: penelope_tools::tool_spec(name)
                .map(|t| t.idempotent)
                .unwrap_or(false),
            policy: None,
        },
    }
}

/// Outils d'un tour de conversation (issue #104) : le noyau, les outils à la demande que
/// la session a découverts, et les méta-outils qui mènent aux autres. Trié : la liste ne
/// change qu'avec ce que la session découvre ou oublie.
pub fn chat_tool_defs(discovered: &[String]) -> Vec<penelope_llm::ToolDef> {
    let mut specs = penelope_tools::core_exposed();
    for d in discovered {
        if let Some(t) = penelope_tools::tool_spec(d)
            && !specs.iter().any(|s| s.name == t.name)
        {
            specs.push(t);
        }
    }
    specs.sort_by(|a, b| a.name.cmp(b.name));
    let mut v: Vec<penelope_llm::ToolDef> = specs
        .into_iter()
        .map(|t| penelope_llm::ToolDef::new(t.name, t.description, t.schema))
        .collect();
    for (name, desc, schema) in penelope_mcp::registry::ToolRegistry::meta_tools() {
        v.push(penelope_llm::ToolDef::new(name, desc, schema));
    }
    v
}

/// Définitions d'outils offertes au modèle pour une session.
pub fn tool_defs(in_workflow: bool, with_mcp: bool) -> Vec<penelope_llm::ToolDef> {
    let mut v: Vec<penelope_llm::ToolDef> = penelope_tools::all_tools()
        .into_iter()
        .filter(|t| in_workflow || !t.workflow_only)
        .map(|t| penelope_llm::ToolDef::new(t.name, t.description, t.schema))
        .collect();
    if with_mcp {
        for (name, desc, schema) in penelope_mcp::registry::ToolRegistry::meta_tools() {
            v.push(penelope_llm::ToolDef::new(name, desc, schema));
        }
    }
    v
}

/// Rendu d'un résultat d'outil MCP : texte des blocs, sinon contenu structuré.
pub fn render_mcp_result(v: &Value) -> String {
    let mut out = String::new();
    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                out.push_str(t);
                out.push('\n');
            } else if let Some(uri) = b.get("uri").and_then(|u| u.as_str()) {
                out.push_str(&format!("[ressource {uri}]\n"));
            }
        }
    }
    if out.trim().is_empty() {
        if let Some(sc) = v.get("structuredContent") {
            return serde_json::to_string_pretty(sc).unwrap_or_default();
        }
        return serde_json::to_string_pretty(v).unwrap_or_default();
    }
    out
}

pub(crate) fn shell_override(raw: &str) -> Option<(String, Vec<String>)> {
    let mut parts = raw.split_whitespace();
    let program = parts.next()?.to_string();
    let mut args: Vec<String> = parts.map(String::from).collect();
    if args.is_empty() {
        args.push("-c".into());
    }
    Some((program, args))
}

fn new_workflow_plan(id: &str, args: &Value) -> ToolResult<penelope_workflow::plan::PlanDraft> {
    use penelope_workflow::plan::{Plan, PlanDraft, PlanStep};
    let goal = str_arg(args, "goal")?;
    let steps: Vec<PlanStep> = serde_json::from_value(
        args.get("steps")
            .cloned()
            .ok_or_else(|| ToolError::Invalid("steps requis".into()))?,
    )
    .map_err(|e| ToolError::Invalid(e.to_string()))?;
    let plan = Plan::new(goal, steps).map_err(|e| ToolError::Invalid(e.to_string()))?;
    Ok(PlanDraft {
        workflow_id: id.into(),
        params: args.get("params").cloned().unwrap_or(json!({})),
        brief: args.get("brief").and_then(Value::as_str).map(String::from),
        plan,
    })
}

fn str_arg(args: &Value, key: &str) -> ToolResult<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| ToolError::Invalid(format!("`{key}` manquant")))
}

fn u_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

fn b_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

/// Au-delà, la liste d'un `fs_list` part en artefact avec un résumé (~4 k tokens).
const FS_LIST_INLINE_CHARS: usize = 16_000;

fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} Gio", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} Mio", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} Kio", b as f64 / (1u64 << 10) as f64),
        b => format!("{b} o"),
    }
}

/// `fs_list` en lignes compactes, chemins relatifs à la racine.
fn render_listing(root: &Path, v: &Value) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let mut out = format!("{} ({} entrées)\n", root.display(), items.len());
    for it in &items {
        let path = it["path"].as_str().unwrap_or_default();
        let rel = Path::new(path)
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.to_string());
        if it["dir"].as_bool().unwrap_or(false) {
            out.push_str(&format!("{rel}/\n"));
        } else {
            let size = size_label(it["bytes"].as_u64().unwrap_or(0));
            out.push_str(&format!("{rel}  {size}\n"));
        }
    }
    out
}

/// Résumé d'une longue liste : décompte par dossier de premier niveau et aperçu.
fn summarise_listing(root: &Path, v: &Value, max: usize, listing: &str, artifact: &str) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let dirs = items
        .iter()
        .filter(|i| i["dir"].as_bool().unwrap_or(false))
        .count();
    let mut per_top: std::collections::BTreeMap<String, usize> = Default::default();
    for it in &items {
        let path = Path::new(it["path"].as_str().unwrap_or_default());
        if let Ok(rel) = path.strip_prefix(root)
            && let Some(first) = rel.components().next()
        {
            let is_leaf_file =
                rel.components().count() == 1 && !it["dir"].as_bool().unwrap_or(false);
            let key = if is_leaf_file {
                "(racine)".to_string()
            } else {
                format!("{}/", first.as_os_str().to_string_lossy())
            };
            *per_top.entry(key).or_default() += 1;
        }
    }
    let mut top: Vec<(String, usize)> = per_top.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = format!(
        "{} : {} entrées ({dirs} dossiers, {} fichiers){}.\nPar dossier de premier niveau :\n",
        root.display(),
        items.len(),
        items.len() - dirs,
        if items.len() >= max {
            format!(", liste arrêtée à max_entries = {max}")
        } else {
            String::new()
        }
    );
    for (name, n) in top.iter().take(30) {
        out.push_str(&format!("- {name} {n}\n"));
    }
    if top.len() > 30 {
        out.push_str(&format!("- … {} autres\n", top.len() - 30));
    }
    out.push_str("Aperçu :\n");
    for line in listing.lines().skip(1).take(40) {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!(
        "Liste complète : artifact_read(\"{artifact}\"). Pour moins de bruit, lister un \
         sous-dossier ou passer par fs_search."
    ));
    out
}

/// Ajoute à chaque extrait le titre et la date de sa session.
async fn with_session_labels(s: &Services, hits: &mut Value) {
    let Some(items) = hits.as_array_mut() else {
        return;
    };
    let mut known: std::collections::BTreeMap<String, (String, String)> = Default::default();
    for h in items.iter_mut() {
        let Some(sid) = h["session_id"].as_str().map(String::from) else {
            continue;
        };
        if !known.contains_key(&sid) {
            let label = match s.sessions.get(&sid).await {
                Ok(Some(sess)) => (
                    sess.title
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| "(sans titre)".into()),
                    sess.last_activity
                        .unwrap_or(sess.created_at)
                        .chars()
                        .take(10)
                        .collect(),
                ),
                _ => ("(session inconnue)".into(), String::new()),
            };
            known.insert(sid.clone(), label);
        }
        if let Some((title, date)) = known.get(&sid) {
            h["session_titre"] = json!(title);
            h["session_date"] = json!(date);
        }
    }
}

#[cfg(test)]
mod tests;
