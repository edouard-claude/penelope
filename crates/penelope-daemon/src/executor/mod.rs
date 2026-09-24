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

    async fn dispatch(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;

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

        self.native(name, args, cancel).await
    }
}

mod args;
mod defs;
mod meta;
mod precheck;
mod tools;

use args::{
    FS_LIST_INLINE_CHARS, b_arg, new_workflow_plan, render_listing, str_arg, summarise_listing,
    u_arg, with_session_labels,
};
use defs::native_info;
pub use defs::{chat_tool_defs, render_mcp_result, tool_defs};
pub(crate) use defs::{shell_override, wants_network};
pub(crate) use precheck::{call_arguments, effective_arguments};

#[cfg(test)]
mod tests;
