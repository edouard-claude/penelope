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
}

/// Accès aux serveurs MCP vivants.
#[async_trait::async_trait]
pub trait McpGateway: Send + Sync {
    /// Appelle un outil par son nom qualifié `mcp__<serveur>__<outil>`.
    async fn call_tool(&self, qualified: &str, args: &Value) -> Result<Value, String>;
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
    async fn start_workflow(
        &self,
        id: &str,
        params: Value,
        session_id: &str,
        origin: &Origin,
    ) -> Result<Value, String>;
    async fn spawn_sub_agent(
        &self,
        session_id: &str,
        prompt: &str,
        model: Option<&str>,
        tools: Vec<String>,
    ) -> Result<Value, String>;
    async fn generate_image(&self, prompt: &str, size: Option<&str>) -> Result<Value, String>;
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
    pub http: reqwest::Client,
    pub locks: Arc<penelope_tools::fs::FileLocks>,
    pub messenger: Option<Arc<dyn Messenger>>,
    pub mcp: Option<Arc<dyn McpGateway>>,
    pub orchestrator: Option<Arc<dyn Orchestrator>>,
    pub admin: Option<Arc<dyn crate::selfknow::Admin>>,
}

/// Workspaces autorisés : configuration, sinon `{data}/workspace`.
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
    v.into_iter()
        .map(|p| penelope_platform::sandbox::normalise(&p))
        .collect()
}

impl NativeToolExecutor {
    pub fn new(services: Arc<Services>, env: ToolEnv) -> Self {
        NativeToolExecutor {
            services,
            env,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .unwrap_or_default(),
            locks: Arc::new(penelope_tools::fs::FileLocks::new()),
            messenger: None,
            mcp: None,
            orchestrator: None,
            admin: None,
        }
    }

    fn workspace(&self) -> PathBuf {
        self.env
            .workspaces
            .first()
            .cloned()
            .unwrap_or_else(std::env::temp_dir)
    }

    fn path_arg(&self, args: &Value, key: &str) -> ToolResult<PathBuf> {
        let raw = str_arg(args, key)?;
        penelope_tools::fs::resolve(&raw, &self.env.workspaces)
    }

    fn cwd_arg(&self, args: &Value) -> ToolResult<PathBuf> {
        match args.get("cwd").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => penelope_tools::fs::resolve(c, &self.env.workspaces),
            _ => Ok(self.workspace()),
        }
    }

    async fn dispatch(&self, name: &str, args: &Value) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        // Les méta-outils MCP n'ont pas de spécification native.
        match name {
            "tool_search" => return self.tool_search(args).await,
            "tool_describe" => return self.tool_describe(args).await,
            "tool_call" => return self.tool_call(args).await,
            _ => {}
        }
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
                    Some(p) if !p.is_empty() => {
                        penelope_tools::fs::resolve(p, &self.env.workspaces)?
                    }
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
                let profile = penelope_tools::shell::profile_for(
                    &cfg.sandbox.default_profile,
                    &cwd,
                    cfg.sandbox.shell_network,
                );
                let shell = shell_override(&cfg.tools.shell);
                let out = penelope_tools::shell::exec(
                    &s.platform.processes,
                    Some(&profile),
                    &command,
                    Some(&cwd),
                    timeout,
                    cfg.tools.max_output_bytes,
                    shell,
                )
                .await?;
                let mut o = ToolOutcome::ok(out.to_json());
                if out.exit_code != 0 {
                    o.text = format!(
                        "Code de sortie {}.\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        out.exit_code, out.stdout, out.stderr
                    );
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
                penelope_tools::git::clone(
                    &str_arg(args, "url")?,
                    &dest,
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
                let warnings: Vec<String> =
                    penelope_kernel::coherence::contradictions(&s.config.config())
                        .into_iter()
                        .filter(|c| c.concerns(&path))
                        .map(|c| c.message)
                        .collect();
                json!({
                    "path": path,
                    "value": value,
                    "generation": generation,
                    "applied": "à chaud, dès le prochain appel",
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
                // Le déclencheur revient dans cette session, par ce canal.
                if let Some(o) = target.as_object_mut() {
                    o.entry("session_id").or_insert(json!(self.env.session_id));
                    o.entry("origin").or_insert(self.env.origin.to_value());
                }
                let sch = s
                    .schedules
                    .create(
                        kind,
                        args.get("spec").cloned().unwrap_or(Value::Null),
                        target,
                        args.get("dedup").cloned().unwrap_or(Value::Null),
                    )
                    .await
                    .map_err(ToolError::Invalid)?;
                serde_json::to_value(sch).unwrap_or_default()
            }
            "schedule_list" => serde_json::to_value(s.schedules.list().await?).unwrap_or_default(),
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
                let texte = str_arg(args, "texte")?;
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
                        .map(|(k, score)| json!({"name": k.name, "description": k.description, "score": score}))
                        .collect::<Vec<_>>()
                )
            }
            "skill_load" => {
                let name = str_arg(args, "name")?;
                let sk = s
                    .skills
                    .get(&name)
                    .ok_or_else(|| ToolError::Invalid(format!("skill `{name}` introuvable")))?;
                json!({"name": sk.name, "allowed_tools": sk.allowed_tools, "content": sk.body})
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
                s.skills.reload(None, &root, None).await?;
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
            "workflow_start" => {
                let o = self
                    .orchestrator
                    .as_ref()
                    .ok_or_else(|| ToolError::Other("moteur de workflows indisponible".into()))?;
                o.start_workflow(
                    &str_arg(args, "id")?,
                    args.get("params").cloned().unwrap_or(json!({})),
                    &self.env.session_id,
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
                let w = penelope_workflow::Workflow::from_json(&raw)
                    .map_err(|e| ToolError::Invalid(format!("JSON invalide : {e}")))?;
                let known =
                    crate::runtime::workflow_known_with(&cfg, &s.mcp_tools, &s.workflows).await;
                let dir = s.platform.dirs.workflows();
                let path = s
                    .workflows
                    .write(&dir, &w, &known)
                    .map_err(ToolError::Invalid)?;
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
                )
                .await
                .map_err(ToolError::Other)?
            }
            "session_metadata" => {
                let op = penelope_kernel::session::MetadataOp::parse(&str_arg(args, "op")?)
                    .ok_or_else(|| ToolError::Invalid("op inconnue".into()))?;
                s.sessions
                    .metadata(
                        &self.env.session_id,
                        op,
                        &str_arg(args, "key")?,
                        args.get("entry").cloned().unwrap_or(Value::Null),
                    )
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
                let mut state: Value = crate::workflow::kv_get(s, &key)
                    .await
                    .map_err(|e| ToolError::Other(e.to_string()))?
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_else(|| json!({}));
                if name == "return_value" {
                    state["result"] = args.get("result").cloned().unwrap_or(Value::Null);
                    state["content"] = args.get("content").cloned().unwrap_or(Value::Null);
                } else {
                    state["done"] = json!(true);
                }
                crate::workflow::kv_set(s, &key, &state.to_string())
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
        let hits = self
            .services
            .mcp_tools
            .search_hybrid(
                &q,
                vector.as_deref(),
                args.get("server").and_then(|v| v.as_str()),
                u_arg(args, "limit").unwrap_or(10),
            )
            .await?;
        if hits.is_empty() {
            return Ok(ToolOutcome::ok(json!({
                "résultats": [],
                "remarque": "aucun outil MCP ne correspond ; vérifier les serveurs avec /mcp",
            })));
        }
        Ok(ToolOutcome::ok(json!(
            hits.iter().map(|h| h.tool.short()).collect::<Vec<_>>()
        )))
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
        let v = self.services.mcp_tools.describe(&names).await?;
        Ok(ToolOutcome::ok(json!(v)))
    }

    async fn tool_call(&self, args: &Value) -> ToolResult<ToolOutcome> {
        let name = str_arg(args, "name")?;
        let inner = args.get("args").cloned().unwrap_or(json!({}));
        self.mcp_call(&name, &inner).await
    }

    async fn mcp_call(&self, qualified: &str, args: &Value) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        s.mcp_tools
            .validate_args(qualified, args)
            .await
            .map_err(|e| ToolError::Invalid(e.to_string()))?;
        s.mcp_tools.mark_for_promotion(&[qualified.to_string()]);
        let gw = self
            .mcp
            .as_ref()
            .ok_or_else(|| ToolError::Other("aucun serveur MCP n'est démarré".into()))?;
        let v = gw
            .call_tool(qualified, args)
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

#[async_trait::async_trait]
impl ToolExecutor for NativeToolExecutor {
    async fn execute(&self, name: &str, args: &Value) -> Result<ToolOutcome, ToolError> {
        self.dispatch(name, args).await
    }

    async fn describe_call(&self, name: &str, args: &Value) -> CallInfo {
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
                mcp_info(q).await
            }
            n if n.starts_with("mcp__") => mcp_info(n.to_string()).await,
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
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;

    async fn executor() -> (tempfile::TempDir, NativeToolExecutor) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = ToolEnv {
            session_id: "s1".into(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: vec![penelope_platform::sandbox::normalise(&ws)],
            in_workflow: false,
            turn_model: None,
        };
        (dir, NativeToolExecutor::new(s, env))
    }

    /// Issue #8 : une page HTML arrive en texte lisible, le brut reste en artefact ; une
    /// longue liste de fichiers part en artefact avec un résumé.
    #[tokio::test]
    async fn web_pages_and_long_listings_stay_small_in_context() {
        let (dir, x) = executor().await;
        let page = format!(
            "<!doctype html><html><head><title>Guide</title><script>{}</script></head>\
             <body><h1>Jetons</h1><p>Un jeton par <a href=\"/cles\">clé</a>.</p></body></html>",
            "var x = 1;".repeat(2_000)
        );
        // Réponse telle que `penelope_tools::http::fetch` la rend (le réseau local est
        // refusé par l'outil lui-même).
        let fetched = x
            .fetched_page(
                json!({
                    "url": "https://docs.exemple.fr/guide",
                    "status": 200,
                    "contentType": "text/html; charset=utf-8",
                    "bytes": page.len(),
                    "truncated": false,
                    "body": page,
                }),
                "https://docs.exemple.fr/guide",
            )
            .await
            .unwrap();
        assert!(fetched.text.contains("# Jetons"), "{}", fetched.text);
        assert!(
            fetched.text.contains("[clé](https://docs.exemple.fr/cles)"),
            "{}",
            fetched.text
        );
        assert!(fetched.text.len() < 1_000, "{}", fetched.text.len());
        assert!(!fetched.text.contains("var x"), "{}", fetched.text);
        let raw_id = fetched.value["raw_artifact"].as_str().unwrap();
        let (raw, _, _) = x
            .services
            .context
            .history
            .read_artifact(raw_id, 0, 100_000)
            .await
            .unwrap()
            .unwrap();
        assert!(raw.contains("var x = 1;"));

        let ws = dir.path().join("ws");
        for d in 0..12 {
            let sub = ws.join(format!("module_{d:02}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..60 {
                std::fs::write(sub.join(format!("fichier_numero_{f:03}.rs")), "x").unwrap();
            }
        }
        let listed = x
            .execute(
                "fs_list",
                &json!({"path": ".", "recursive": true, "max_entries": 2000}),
            )
            .await
            .unwrap();
        assert_eq!(listed.value["entries"], 732);
        assert!(listed.text.chars().count() < 6_000, "{}", listed.text.len());
        assert!(listed.text.contains("- module_00/ 61"), "{}", listed.text);
        assert!(listed.text.contains("artifact_read(\""), "{}", listed.text);

        let small = x
            .execute("fs_list", &json!({"path": "module_03"}))
            .await
            .unwrap();
        assert!(
            small.text.contains("fichier_numero_007.rs  1 o"),
            "{}",
            small.text
        );
        assert!(!small.text.contains("artifact_read"));
    }

    /// Issue #1 : une conversation d'une session fermée se retrouve depuis une autre
    /// session, par mots-clés en `all` comme par une question en phrase.
    #[tokio::test]
    async fn past_sessions_are_found_with_their_title_and_date() {
        let (_d, x) = executor().await;
        let s = x.services.clone();
        let old = s
            .sessions
            .create(
                penelope_kernel::session::SessionKind::Chat,
                Some("Refonte du site Zéphyr".into()),
            )
            .await
            .unwrap();
        for text in [
            "Pour le projet Zéphyr, on garde la maquette verte.",
            "Le client Zéphyr veut une livraison en octobre.",
        ] {
            s.context
                .history
                .append(
                    old.id.as_str(),
                    &penelope_llm::types::ChatMessage::user(text),
                    12,
                    0,
                    false,
                    None,
                )
                .await
                .unwrap();
        }
        s.sessions
            .set_state(old.id.as_str(), "closed")
            .await
            .unwrap();

        let here = x
            .execute("history_grep", &json!({"query": "Zéphyr"}))
            .await
            .unwrap();
        assert!(
            here.value["note"].as_str().unwrap().contains("all"),
            "{}",
            here.value
        );

        let all = x
            .execute(
                "history_grep",
                &json!({"query": "Zéphyr maquette", "scope": "all"}),
            )
            .await
            .unwrap();
        let hits = all.value.as_array().unwrap();
        assert_eq!(hits.len(), 1, "{}", all.value);
        assert_eq!(hits[0]["session_titre"], "Refonte du site Zéphyr");
        assert_eq!(hits[0]["session_date"].as_str().unwrap().len(), 10);

        let asked = x
            .execute(
                "history_expand_query",
                &json!({"question": "On avait parlé d'un projet Zéphyr dans une session précédente ?"}),
            )
            .await
            .unwrap();
        assert_eq!(asked.value["mots"], json!(["projet", "zéphyr"]));
        let extraits = asked.value["extraits"].as_array().unwrap();
        assert_eq!(extraits.len(), 2, "{}", asked.value);
        assert_eq!(
            extraits[0]["matched"].as_array().unwrap().len(),
            2,
            "le passage qui a les deux mots d'abord"
        );
        assert_eq!(extraits[0]["session_titre"], "Refonte du site Zéphyr");
    }

    #[tokio::test]
    async fn a_dated_intent_is_redirected_to_a_schedule() {
        let (_d, x) = executor().await;
        let e = x
            .execute(
                "intent_create",
                &json!({"texte": "rappelle-moi vendredi d'appeler Paul"}),
            )
            .await
            .unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains("schedule_create") && msg.contains("once"),
            "{msg}"
        );
        let ok = x
            .execute(
                "intent_create",
                &json!({"texte": "quand on reparle du déploiement, rappelle-moi le changelog"}),
            )
            .await
            .unwrap();
        assert!(ok.value["id"].as_str().unwrap().starts_with("i_"));
    }

    #[tokio::test]
    async fn files_are_written_edited_and_read_inside_the_workspace() {
        let (_d, x) = executor().await;
        x.execute(
            "fs_write",
            &json!({"path":"notes/a.txt","content":"bonjour\nmonde\n"}),
        )
        .await
        .unwrap();
        x.execute(
            "fs_edit",
            &json!({"path":"notes/a.txt","old":"monde","new":"Pénélope"}),
        )
        .await
        .unwrap();
        let r = x
            .execute("fs_read", &json!({"path":"notes/a.txt"}))
            .await
            .unwrap();
        assert!(r.text.contains("Pénélope"), "{}", r.text);
        assert!(r.eager, "une lecture est un résultat volatil");

        let e = x
            .execute("fs_read", &json!({"path":"/etc/passwd"}))
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::Denied(_)), "{e}");
    }

    #[tokio::test]
    async fn invalid_arguments_are_rejected_before_running() {
        let (_d, x) = executor().await;
        let e = x
            .execute("fs_write", &json!({"path": "a.txt"}))
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::Invalid(_)), "{e}");
    }

    #[tokio::test]
    async fn memory_tools_write_through_the_vault() {
        let (_d, x) = executor().await;
        let r = x
            .execute(
                "mem_remember",
                &json!({"niveau":"profil","texte":"Préférer le tutoiement"}),
            )
            .await
            .unwrap();
        let uid = r.value["uid"].as_str().unwrap().to_string();
        let found = x
            .execute("mem_search", &json!({"query":"tutoiement"}))
            .await
            .unwrap();
        assert!(found.text.contains(&uid), "{}", found.text);
        let gone = x.execute("mem_forget", &json!({"uid": uid})).await.unwrap();
        assert_eq!(gone.value["forgotten"], true);
    }

    #[tokio::test]
    async fn workflow_only_tools_are_refused_in_chat() {
        let (_d, x) = executor().await;
        let e = x.execute("step_done", &json!({})).await.unwrap_err();
        assert!(matches!(e, ToolError::Denied(_)), "{e}");
    }

    #[tokio::test]
    async fn mcp_meta_tools_are_read_only_but_calls_carry_the_target_risk() {
        let (_d, x) = executor().await;
        let d = penelope_mcp::protocol::ToolDescriptor::parse(&json!({
            "name": "delete_repo", "description": "Supprime un dépôt",
            "inputSchema": {"type":"object"},
            "annotations": {"destructiveHint": true, "readOnlyHint": false}
        }))
        .unwrap();
        x.services
            .mcp_tools
            .replace_server_tools(
                "forge",
                vec![penelope_mcp::registry::RegisteredTool::from_descriptor(
                    "forge", &d,
                )],
                "2026-09-16T10:00:00Z",
            )
            .await
            .unwrap();

        let search = x.describe_call("tool_search", &json!({"query":"x"})).await;
        assert_eq!(search.risk, RiskClass::Read);

        let call = x
            .describe_call(
                "tool_call",
                &json!({"name":"mcp__forge__delete_repo","args":{}}),
            )
            .await;
        assert_eq!(call.effective_name, "mcp__forge__delete_repo");
        assert_eq!(call.risk, RiskClass::Destructive);

        // Sans passerelle MCP, l'appel échoue proprement.
        let e = x
            .execute(
                "tool_call",
                &json!({"name":"mcp__forge__delete_repo","args":{}}),
            )
            .await
            .unwrap_err();
        assert!(e.to_string().contains("MCP"), "{e}");
    }

    #[test]
    fn tool_definitions_hide_workflow_only_tools_in_chat() {
        let chat = tool_defs(false, false);
        assert!(chat.iter().any(|t| t.name == "fs_read"));
        assert!(!chat.iter().any(|t| t.name == "step_done"));
        assert!(!chat.iter().any(|t| t.name == "tool_search"));
        let wf = tool_defs(true, true);
        assert!(wf.iter().any(|t| t.name == "step_done"));
        assert!(wf.iter().any(|t| t.name == "tool_call"));
    }

    #[test]
    fn mcp_results_render_their_text_blocks() {
        let v =
            json!({"content":[{"type":"text","text":"ligne 1"},{"type":"text","text":"ligne 2"}]});
        assert_eq!(render_mcp_result(&v), "ligne 1\nligne 2\n");
        let v = json!({"content":[], "structuredContent":{"total": 3}});
        assert!(render_mcp_result(&v).contains("\"total\": 3"));
    }
}
