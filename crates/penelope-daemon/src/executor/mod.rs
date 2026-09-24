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

pub use penelope_app::ports::{McpGateway, Messenger, Orchestrator, question_text};

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

pub use penelope_app::helpers::{canonical_workspace, default_workspaces, denied_reads};

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
pub(crate) use defs::shell_override;
pub use defs::{chat_tool_defs, render_mcp_result, tool_defs, wants_network};
pub(crate) use precheck::{call_arguments, effective_arguments};

#[cfg(test)]
mod tests;
