//! Ports du daemon : ce qu'un module reçoit au lieu de `&Daemon` (épopée #208, lot D,
//! `design/v1/decoupage-daemon.md` §2.2).

use crate::bus::Origin;
use penelope_kernel::clock::SharedClock;
use penelope_kernel::event::EventLog;
use penelope_llm::Provider;
use penelope_mcp::config::ServerConfig;
use penelope_mcp::supervisor::ServerStatus;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LockResult, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Providers des modèles, construits à la demande (`Daemon::provider_for` jusqu'ici).
#[async_trait::async_trait]
pub trait ProviderSource: Send + Sync {
    /// Provider d'un modèle ; l'erreur est lisible par le propriétaire.
    async fn provider_for(&self, model_id: &str) -> Result<Arc<dyn Provider>, String>;
    /// Provider imposé (tests, suites sans réseau), s'il y en a un.
    fn provider_override_active(&self) -> Option<Arc<dyn Provider>>;
}

/// Administration des serveurs MCP (`mcp.*`, `doctor`, cartes Telegram, import Hermes),
/// au lieu du superviseur concret : état, déclarations, cycle de vie, prompts.
#[async_trait::async_trait]
pub trait McpAdmin: McpGateway {
    async fn statuses(&self) -> Vec<ServerStatus>;
    /// Déclarations invalides lors du dernier chargement : (fichier, erreur).
    fn invalid(&self) -> Vec<(String, String)>;
    /// Répertoire des déclarations (`mcp.d`).
    fn dir(&self) -> &Path;
    async fn show(&self, name: &str) -> Result<Value, String>;
    async fn prompts(&self, name: &str) -> Result<Vec<Value>, String>;
    async fn get_prompt(&self, name: &str, prompt: &str, args: Value) -> Result<Value, String>;
    async fn logs(&self, name: &str, n: usize) -> Result<Vec<String>, String>;
    async fn restart(&self, name: &str) -> Result<ServerStatus, String>;
    async fn test(&self, cfg: &ServerConfig) -> Value;
    async fn config_of(&self, name: &str) -> Option<ServerConfig>;
    async fn add(&self, cfg: ServerConfig, replace: bool) -> Result<ReloadReport, String>;
    async fn edit(&self, name: &str, patch: &Value) -> Result<ReloadReport, String>;
    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<ReloadReport, String>;
    async fn remove(&self, name: &str) -> Result<ReloadReport, String>;
    async fn reload(&self) -> ReloadReport;
    /// État d'une tâche MCP et, une fois terminée, son résultat.
    async fn task_status(&self, server: &str, task_ref: &str) -> Result<Value, String>;
    /// Messages pour le propriétaire, en attente d'envoi.
    fn take_notices(&self) -> Vec<String>;
}

/// Branchement posé après le démarrage (canal de message, MCP, orchestration) : un
/// module le reçoit explicitement et le lit au moment de s'en servir. Les boucles de fond
/// partent avant Telegram et MCP (`supervisor.rs`) : une valeur lue à leur lancement
/// serait vide pour toujours.
pub struct Slot<T: ?Sized>(Arc<RwLock<Option<Arc<T>>>>);

impl<T: ?Sized> Slot<T> {
    /// Valeur branchée à cet instant.
    pub fn get(&self) -> Option<Arc<T>> {
        self.0.read().ok().and_then(|g| g.clone())
    }
    pub fn set(&self, value: Option<Arc<T>>) {
        if let Ok(mut g) = self.0.write() {
            *g = value;
        }
    }
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, Option<Arc<T>>>> {
        self.0.read()
    }
    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, Option<Arc<T>>>> {
        self.0.write()
    }
}

impl<T: ?Sized> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Slot(self.0.clone())
    }
}

impl<T: ?Sized> Default for Slot<T> {
    fn default() -> Self {
        Slot(Arc::new(RwLock::new(None)))
    }
}

/// Ce qu'une boucle de fond surveillée reçoit (`tasks::spawn_supervised`) : le registre
/// des boucles, le signal d'arrêt, l'horloge et le journal d'audit.
#[derive(Clone)]
pub struct Supervision {
    pub tasks: Arc<crate::tasks::Tasks>,
    pub handle: Handle,
    pub clock: SharedClock,
    pub events: EventLog,
}

/// Un arrêt demandé : qui (`signal`, `cli`, `telegram`, `mise à jour`, `rpc`), pourquoi,
/// et s'il doit être suivi d'un redémarrage. Écrit au journal au moment de la demande et
/// relu au démarrage suivant (issue #225).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stop {
    pub by: String,
    pub why: String,
    pub restart: bool,
}

impl Stop {
    pub fn new(by: &str, why: impl Into<String>, restart: bool) -> Stop {
        Stop {
            by: by.to_string(),
            why: why.into(),
            restart,
        }
    }

    /// Méthodes RPC `shutdown` et `restart` : l'appelant se nomme par `by` et `why` ;
    /// à défaut, la socket seule est connue.
    pub fn from_params(p: &Value, restart: bool) -> Stop {
        let text = |k: &str| p[k].as_str().filter(|s| !s.trim().is_empty());
        let method = if restart { "restart" } else { "shutdown" };
        Stop::new(
            text("by").unwrap_or("rpc"),
            text("why").map_or_else(|| format!("méthode `{method}`"), str::to_string),
            restart,
        )
    }
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.restart {
            "redémarrage"
        } else {
            "arrêt"
        };
        write!(f, "{what} demandé par {} ({})", self.by, self.why)
    }
}

/// Poignée de contrôle du daemon : signal d'arrêt et de redémarrage, compteurs.
#[derive(Clone)]
pub struct Handle {
    shutdown: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<Stop>>>,
    started_at_ms: i64,
    turns_done: Arc<AtomicU64>,
}

impl Handle {
    pub fn new(started_at_ms: i64) -> Handle {
        Handle {
            shutdown: Arc::new(AtomicBool::new(false)),
            restart: Arc::new(AtomicBool::new(false)),
            reason: Arc::new(Mutex::new(None)),
            started_at_ms,
            turns_done: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
    pub fn request_restart(&self) {
        self.restart.store(true, Ordering::SeqCst);
        self.shutdown.store(true, Ordering::SeqCst);
    }
    /// Arrêt (ou redémarrage) avec son origine, journalisée tout de suite : la première
    /// demande est celle qui reste, sauf un redémarrage qui suit un simple arrêt, pour
    /// que l'origine gardée dise la même chose que `wants_restart` (issue #225).
    pub fn stop(&self, stop: Stop) {
        tracing::info!(
            par = %stop.by,
            motif = %stop.why,
            redemarrage = stop.restart,
            "arrêt demandé"
        );
        {
            let mut reason = self.reason.lock().unwrap_or_else(|p| p.into_inner());
            if reason.as_ref().is_none_or(|r| stop.restart && !r.restart) {
                *reason = Some(stop.clone());
            }
        }
        if stop.restart {
            self.request_restart();
        } else {
            self.shutdown();
        }
    }
    /// L'origine de l'arrêt en cours, s'il a été demandé par `stop`.
    pub fn stop_reason(&self) -> Option<Stop> {
        self.reason
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
    pub fn wants_restart(&self) -> bool {
        self.restart.load(Ordering::SeqCst)
    }
    pub fn uptime_s(&self, now_ms: i64) -> u64 {
        ((now_ms - self.started_at_ms).max(0) / 1000) as u64
    }
    pub fn turns_done(&self) -> u64 {
        self.turns_done.load(Ordering::SeqCst)
    }
    pub fn record_turn(&self) {
        self.turns_done.fetch_add(1, Ordering::SeqCst);
    }
}

/// Ce qu'un rechargement a changé.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct ReloadReport {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub invalid: Vec<(String, String)>,
}

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
        self.send_text(origin, &question_text(markdown, run_id, choices, form))
            .await
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

/// Question d'une étape `user` en texte seul, pour les canaux sans boutons.
pub fn question_text(
    markdown: &str,
    run_id: &str,
    choices: &[String],
    form: Option<&Value>,
) -> String {
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
    text
}

/// Accès aux serveurs MCP vivants.
#[async_trait::async_trait]
pub trait McpGateway: Send + Sync {
    /// Appelle un outil par son nom qualifié `mcp__<serveur>__<outil>`.
    /// `from` : la conversation qui appelle — une élicitation du serveur y revient
    /// plutôt que d'atterrir dans le chat privé (issue #143). Un bloc `image` ou `audio`
    /// du résultat garde son `data` en base64 à côté d'un `text` qui le mentionne :
    /// l'appelant qui le garde le pose sur disque avant tout transcript (issue #304).
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
    /// Outils promus (ensemble collant, `apply_promotions`) des serveurs actifs sans
    /// `eager_schemas` : exposés d'office aux conversations à partir de leur frontière
    /// suivante (§8.9, #236).
    async fn promoted_tools(&self) -> Vec<penelope_llm::ToolDef> {
        Vec::new()
    }
    /// Abonne Pénélope aux mises à jour d'une ressource (`resources/subscribe`, #293).
    /// Idempotent sur une connexion vivante ; après une reconnexion, l'abonnement est
    /// reposé par l'appel suivant. `Ok(false)` : le serveur ne déclare pas
    /// `resources.subscribe`, l'appelant sonde à la place. `Err` : serveur injoignable.
    async fn subscribe_resource(&self, server: &str, uri: &str) -> Result<bool, String> {
        let _ = (server, uri);
        Err("abonnement aux ressources MCP indisponible ici".into())
    }
    /// Lit une ressource (`resources/read`) : `{"contents": [{uri, mimeType, text|blob}]}`.
    async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, String> {
        let _ = (server, uri);
        Err("lecture de ressource MCP indisponible ici".into())
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
    /// Crée une planification ; une planification active identique est signalée dans la
    /// réponse (issue #39).
    async fn schedule_create(
        &self,
        kind: penelope_workflow::TriggerKind,
        spec: Value,
        target: Value,
        dedup: Value,
    ) -> Result<Value, String> {
        let _ = (kind, spec, target, dedup);
        Err(SCHEDULER_MISSING.into())
    }
    /// Planifications avec leur destination (issue #124).
    async fn schedule_list(&self) -> Result<Vec<Value>, String> {
        Err(SCHEDULER_MISSING.into())
    }
    /// Déplace une planification vers la conversation `to`, sans la recréer (issue
    /// #124). Renvoie la nouvelle destination, en mots.
    async fn schedule_move(&self, id: &str, to: &Origin) -> Result<String, String> {
        let _ = (id, to);
        Err(SCHEDULER_MISSING.into())
    }
    /// Supprime une planification (état `deleted`) ; un identifiant inconnu est une
    /// erreur (#223).
    async fn schedule_delete(&self, id: &str) -> Result<(), String> {
        let _ = id;
        Err(SCHEDULER_MISSING.into())
    }
}

/// Réponse des méthodes de planification quand aucun ordonnanceur n'est branché.
const SCHEDULER_MISSING: &str = "planificateur indisponible ici";

/// Accès du daemon dont les outils ont besoin pour parler de lui.
#[async_trait::async_trait]
pub trait Admin: Send + Sync {
    fn uptime_s(&self) -> u64;
    /// Écrit un réglage (chemin pointé) et le publie à chaud. Renvoie la génération.
    async fn set_config(&self, path: &str, value: Value) -> Result<u64, String>;
    /// Serveurs MCP : état, outils, dernière erreur.
    async fn mcp_servers(&self) -> Value {
        Value::Null
    }
    /// Recherche mémoire : hybride ou lexicale seule, vecteurs calculés (issue #11).
    async fn memory_search(&self) -> Value {
        Value::Null
    }
    /// Mémoire résidente du processus, en mégaoctets.
    fn rss_mb(&self) -> Option<f64> {
        None
    }
    /// Fournisseur `codex` : compte, plan, jauges du plan (issue #142).
    async fn codex_view(&self) -> Value {
        Value::Null
    }
    /// Taille du contexte d'une session (issue #18) : dernier prompt, cache, seuils,
    /// fenêtre du modèle.
    async fn context_view(
        &self,
        session_id: &str,
        model_id: Option<&str>,
    ) -> anyhow::Result<Value> {
        let _ = (session_id, model_id);
        Ok(Value::Null)
    }
    /// Outil `send_voice` : synthèse, conversion et envoi, repli en texte (issue #41).
    /// État des sauvegardes (issue #42).
    async fn backup_status(&self) -> Result<Value, String> {
        Err("sauvegardes indisponibles".into())
    }

    async fn send_voice(
        &self,
        session_id: &str,
        origin: &Origin,
        args: &Value,
    ) -> Result<Value, String> {
        let _ = (session_id, origin, args);
        Err("vocal indisponible hors du daemon".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #225 : la première origine reste, sauf un redémarrage demandé après un simple arrêt ;
    /// sans `by` ni `why`, la socket seule est nommée.
    #[test]
    fn the_stop_reason_keeps_the_first_request_unless_a_restart_follows() {
        let h = Handle::new(0);
        assert_eq!(h.stop_reason(), None);
        h.stop(Stop::from_params(&serde_json::json!({}), false));
        assert_eq!(
            h.stop_reason(),
            Some(Stop::new("rpc", "méthode `shutdown`", false))
        );
        h.stop(Stop::new("signal", "SIGTERM", false));
        assert_eq!(h.stop_reason().unwrap().by, "rpc");
        let tg = serde_json::json!({"by": "telegram", "why": "/restart, confirmé"});
        h.stop(Stop::from_params(&tg, true));
        let r = h.stop_reason().unwrap();
        assert!(h.is_shutting_down() && h.wants_restart());
        assert_eq!(
            r.to_string(),
            "redémarrage demandé par telegram (/restart, confirmé)"
        );
        h.stop(Stop::new("mise à jour", "version 9.9.9 installée", true));
        assert_eq!(h.stop_reason(), Some(r));
    }
}
