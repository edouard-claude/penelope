//! Boucle d'agent : un tour, de bout en bout (§3.3, §4.2, §9).
//!
//! Invariants :
//! - tout effet non `readOnly` est **planifié dans le ledger avant** exécution ;
//! - un outil qui exige une approbation suspend **ce tour seulement**, et l'appel reste
//!   dans le transcript : la reprise le retrouve par son identifiant ;
//! - le détecteur de boucles arrête le tour plutôt que de laisser tourner ;
//! - une erreur d'outil est renvoyée au modèle, pas au harnais.
//!
//! Une itération commence **toujours** par résoudre les appels d'outils en attente à la
//! fin du transcript, puis appelle le modèle. Un premier passage et une reprise après
//! approbation suivent donc exactement le même chemin.

use crate::runtime::Services;
use penelope_hitl::{ApprovalKind, ApprovalState, Decision};
use penelope_kernel::effects::{EffectKind, EffectSpec, Planned};
use penelope_kernel::event::EventDraft;
use penelope_kernel::risk::{PolicyDecision, PolicyWindow, RiskClass};
use penelope_llm::provider::{CancelToken, Provider, collect_stream_observed};
use penelope_llm::types::*;
use penelope_tools::{LoopDetector, LoopVerdict, ToolOutcome};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// Issue d'un tour.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// Réponse finale produite.
    Answered {
        text: String,
        iterations: u32,
        cost_usd: f64,
    },
    /// Le tour attend une approbation : il reprendra après décision.
    AwaitingApproval {
        approval_id: String,
    },
    /// Arrêté par le détecteur de boucles (issue #31) : une réponse sans outil qui cite
    /// l'erreur réelle, 2 ou 3 suites à proposer en boutons, et le rapport technique,
    /// gardé pour les événements et les journaux.
    LoopAborted {
        report: String,
        answer: String,
        choices: Vec<String>,
    },
    /// Annulé (bouton stop, `/stop`, annulation du run).
    Cancelled,
    /// Budget épuisé : périmètre (`jour`, `session`, `run`), dépense et plafond.
    BudgetExceeded {
        scope: String,
        spent_usd: f64,
        limit_usd: f64,
    },
    Failed {
        error: String,
    },
}

/// Ce que le tour montre pendant qu'il se déroule.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    /// Fragment de réponse en cours d'écriture.
    Delta(String),
    /// Fragment de raisonnement (affiché seulement si l'interface le demande).
    Reasoning(String),
    ToolCall {
        name: String,
        args: Value,
    },
    ToolResult {
        name: String,
        ok: bool,
        preview: String,
    },
    /// Une approbation est demandée : c'est au canal de la présenter.
    Approval {
        id: String,
        tool: String,
        risk: RiskClass,
        arguments: Value,
        reason: String,
        double: bool,
    },
    /// Le modèle réellement utilisé, après routage et repli éventuel.
    Model {
        model_id: String,
    },
}

/// Destinataire des événements d'un tour. Synchrone : un canal derrière suffit.
pub trait TurnSink: Send + Sync {
    fn emit(&self, event: TurnEvent);
}

/// Sink qui ignore tout.
pub struct NullSink;

impl TurnSink for NullSink {
    fn emit(&self, _event: TurnEvent) {}
}

/// Sink qui garde tout, pour les tests et la collecte.
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<TurnEvent>>,
}

impl RecordingSink {
    pub fn events(&self) -> Vec<TurnEvent> {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl TurnSink for RecordingSink {
    fn emit(&self, event: TurnEvent) {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event);
    }
}

/// Le transcript sur lequel travaille un tour.
#[async_trait::async_trait]
pub trait Conversation: Send + Sync {
    /// Messages à envoyer au modèle pour la prochaine itération (projection complète,
    /// prompt système compris).
    async fn request_messages(&self) -> anyhow::Result<Vec<ChatMessage>>;
    /// Ajoute un message au transcript.
    async fn record(&self, message: &ChatMessage, eager: bool) -> anyhow::Result<()>;
    /// Queue du transcript, sans prompt système : sert à retrouver les appels en attente.
    async fn tail(&self) -> anyhow::Result<Vec<ChatMessage>>;
    /// Compacte tout de suite après un dépassement de fenêtre prouvé par le provider.
    /// Vrai si des messages ont été résumés : la requête peut être reconstruite.
    async fn compact_for_overflow(&self) -> anyhow::Result<bool> {
        Ok(false)
    }
    /// Applique le budget d'admission (§5.4 niveau 1) aux `count` derniers résultats
    /// d'outils **ensemble** : cinq résultats de 20 k tokens ne passent pas parce
    /// qu'aucun ne dépasse le seuil à lui seul (issue #52).
    async fn admit_tool_results(&self, _count: usize) -> anyhow::Result<()> {
        Ok(())
    }
    /// Le préfixe stable du prompt système, avec sa découpe en tuiles quand elle est
    /// connue : l'audit du prompt (issue #205) l'enregistre sous son empreinte.
    fn prompt_prefix(&self) -> Option<crate::prompt_snapshot::PromptPrefix> {
        None
    }
}

/// Compaction à la demande d'une session (§5.4 : une tentative bornée sur dépassement).
#[async_trait::async_trait]
pub trait Compactor: Send + Sync {
    async fn compact_now(&self, session_id: &str) -> anyhow::Result<bool>;
}

/// Échec d'un appel au modèle, déjà formulé pour l'utilisateur.
pub(crate) struct CallFailure {
    pub message: String,
    /// Le provider a prouvé que la requête dépasse la fenêtre du modèle.
    pub context_length: bool,
}

impl CallFailure {
    fn from_llm(e: &LlmError) -> Self {
        CallFailure {
            message: humanise_llm_error(e),
            context_length: e.kind == LlmErrorKind::ContextLength,
        }
    }

    fn plain(message: impl Into<String>) -> Self {
        CallFailure {
            message: message.into(),
            context_length: false,
        }
    }
}

/// Message d'un plafond atteint : la clé à relever est celle du périmètre atteint, et
/// ce qui ne débloque rien est dit (issue #4).
pub fn budget_exceeded_text(scope: &str, spent_usd: f64, limit_usd: f64) -> String {
    let (label, key) = match scope {
        "session" => ("de la session", "budget.session_usd"),
        "run" => ("du run", "budget.run_usd"),
        _ => ("du jour", "budget.daily_usd"),
    };
    let raised = (limit_usd * 2.0).max(spent_usd + 1.0).ceil();
    let mut out = format!(
        "💸 Budget {label} atteint : {spent_usd:.2} $ dépensés pour un plafond de \
         {limit_usd:.2} $. Tour suspendu.\n\n"
    );
    match scope {
        "session" => out.push_str(&format!(
            "`/new` repart de zéro dans une nouvelle session. Pour continuer celle-ci, relever \
             le plafond : `penelope config set {key} {raised}`."
        )),
        "run" => out.push_str(&format!(
            "Pour laisser le run continuer, relever le plafond : `penelope config set {key} \
             {raised}`."
        )),
        _ => out.push_str(&format!(
            "La dépense du jour repart de zéro à minuit. D'ici là, relever le plafond : \
             `penelope config set {key} {raised}`."
        )),
    }
    out.push_str(
        "\n`/compact` allège le contexte des prochains tours mais ne rembourse pas ce qui est \
         déjà dépensé.",
    );
    out
}

/// Transcript en mémoire : sous-agents, tests, appels ponctuels.
pub struct MemoryConversation {
    system: String,
    messages: Mutex<Vec<ChatMessage>>,
}

impl MemoryConversation {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        MemoryConversation {
            system: system.into(),
            messages: Mutex::new(vec![ChatMessage::user(user.into())]),
        }
    }

    pub fn messages(&self) -> Vec<ChatMessage> {
        self.messages
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

#[async_trait::async_trait]
impl Conversation for MemoryConversation {
    async fn request_messages(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let mut v = vec![ChatMessage::system(self.system.clone())];
        v.extend(self.messages());
        Ok(v)
    }

    async fn record(&self, message: &ChatMessage, _eager: bool) -> anyhow::Result<()> {
        self.messages
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(message.clone());
        Ok(())
    }

    async fn tail(&self) -> anyhow::Result<Vec<ChatMessage>> {
        Ok(self.messages())
    }

    fn prompt_prefix(&self) -> Option<crate::prompt_snapshot::PromptPrefix> {
        Some(crate::prompt_snapshot::PromptPrefix::plain(&self.system))
    }
}

/// Ce qu'il faut savoir d'un appel avant de l'autoriser.
#[derive(Debug, Clone, PartialEq)]
pub struct CallInfo {
    /// Nom sur lequel portent la politique et le ledger : pour `tool_call`, c'est
    /// l'outil MCP visé, pas le méta-outil.
    pub effective_name: String,
    pub risk: RiskClass,
    pub idempotent: bool,
    /// Politique imposée par la déclaration du serveur MCP (`tool_policy`).
    pub policy: Option<PolicyDecision>,
}

/// Exécution concrète d'un outil, fournie par le daemon (ou simulée en test).
#[async_trait::async_trait]
pub trait ToolExecutor {
    async fn execute(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError>;

    /// Exécute en écoutant l'arrêt demandé : `/stop` pendant un `shell_exec` ou un
    /// sous-agent doit les interrompre, pas attendre leur délai (issue #57).
    async fn execute_cancellable(
        &self,
        name: &str,
        args: &Value,
        cancel: &CancelToken,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        let _ = cancel;
        self.execute(name, args).await
    }

    /// Vérifie un appel **avant** toute demande d'approbation : un appel qui ne pourrait
    /// pas aboutir ne coûte pas une carte au propriétaire (issue #117). Par défaut : rien
    /// à vérifier.
    async fn precheck(&self, name: &str, args: &Value) -> Result<(), penelope_tools::ToolError> {
        let _ = (name, args);
        Ok(())
    }

    /// Forme canonique d'un appel, avant toute décision : ce que la garde de boucle, la
    /// politique, la carte et l'exécution voient (issue #123). `None` : l'appel tel quel.
    fn normalise_call(&self, name: &str, args: &Value) -> Option<Value> {
        let _ = (name, args);
        None
    }

    /// Racine pour les motifs de chemins relatifs des règles d'approbation.
    fn policy_workspace(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Risque et nom effectif d'un appel. Par défaut : le catalogue natif.
    async fn describe_call(&self, name: &str, args: &Value) -> CallInfo {
        let _ = args;
        CallInfo {
            effective_name: name.to_string(),
            risk: penelope_tools::effective_risk(name, &Default::default()),
            idempotent: penelope_tools::tool_spec(name)
                .map(|s| s.idempotent)
                .unwrap_or(false),
            policy: None,
        }
    }
}

/// Paramètres d'un tour.
#[derive(Clone)]
pub struct TurnSpec {
    pub session_id: String,
    pub run_id: Option<String>,
    /// Tour d'origine, pour attribuer les coûts à la requête du propriétaire.
    pub turn_id: Option<String>,
    pub model_id: String,
    /// Modèles de repli, dans l'ordre, si le principal est en panne (§10.3 point 5).
    pub fallback_models: Vec<String>,
    pub tools: Vec<ToolDef>,
    /// Outils autorisés (liste blanche d'étape ou de skill) ; vide = tous.
    pub allowed_tools: Vec<String>,
    pub cancel: CancelToken,
}

/// Forme historique d'une requête : prompt système et message utilisateur en mémoire.
pub struct TurnRequest {
    pub session_id: String,
    pub run_id: Option<String>,
    pub user_text: String,
    pub model_id: String,
    pub tools: Vec<ToolDef>,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    pub cancel: CancelToken,
}

/// Un exécuteur de tour.
pub struct AgentLoop {
    pub services: Arc<Services>,
    pub provider: Arc<dyn Provider>,
    pub max_iterations: u32,
}

/// Résolution des appels en attente.
/// Lectures lancées ensemble, au plus (issue #85).
const PARALLEL_READS: usize = 4;

/// Lectures **pures**, sans effet sur la conversation, le run ou le propriétaire : elles
/// seules partent en parallèle. `return_value` puis `step_done`, `ask_user`, `send_voice`
/// ou `skill_load` sont aussi de classe `read`, mais leur ordre compte (#85).
const PARALLEL_SAFE: &[&str] = &[
    "fs_read",
    "fs_list",
    "fs_search",
    "git_status",
    "git_diff",
    "time_now",
    "schedule_list",
    "mem_search",
    "mem_get",
    "mem_neighbors",
    "intent_list",
    "history_grep",
    "history_describe",
    "history_expand",
    "history_expand_query",
    "artifact_read",
    "skill_search",
    "workflow_list",
    "workflow_describe",
    "workflow_status",
    "self_status",
    "self_docs",
    "tool_search",
    "tool_describe",
];

/// Suite d'un appel, décidée avant toute exécution.
enum Step {
    /// Résultat connu sans exécuter : refus, avertissement du harnais.
    Record(ToolCall, String),
    /// À exécuter ; `parallel` : lecture autorisée d'office, qui peut partir avec ses
    /// voisines.
    Execute {
        call: ToolCall,
        info: CallInfo,
        parallel: bool,
    },
}

/// Ce qui arrête la liste des appels après l'exécution de ceux qui précèdent.
enum Terminal {
    Stop(TurnOutcome),
    Loop {
        call: ToolCall,
        tool: String,
        message: String,
    },
}

enum Pending {
    Nothing,
    Resolved,
    Stop(TurnOutcome),
    /// Boucle arrêtée : le tour répond encore, sans outil.
    Loop {
        report: String,
        tool: String,
        last_result: Option<String>,
    },
}

/// Note laissée dans la conversation quand le détecteur arrête une boucle : le tour suivant
/// voit que l'approche a échoué.
pub const LOOP_STOP_NOTE: &str =
    "[boucle arrêtée par le harnais : cette approche a échoué, ne pas la réessayer telle quelle]";

/// Suites proposées quand le modèle n'en donne pas.
const LOOP_DEFAULT_CHOICES: [&str; 3] = [
    "Chercher autrement",
    "Je te précise (compte, dossier, dates)",
    "Laisser tomber",
];

/// Dernier résultat réel d'un appel identique (même outil, mêmes arguments) dans le
/// transcript, avertissements du harnais exclus.
pub fn last_result_of(tail: &[ChatMessage], tool: &str, args: &Value) -> Option<String> {
    let ids: BTreeSet<&str> = tail
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.tool_calls.iter())
        .filter(|c| c.name == tool && &c.arguments == args)
        .map(|c| c.id.as_str())
        .collect();
    tail.iter()
        .rev()
        .filter(|m| m.role == Role::Tool)
        .filter(|m| m.tool_call_id.as_deref().is_some_and(|id| ids.contains(id)))
        .map(|m| m.text())
        .find(|t| !t.starts_with("[avertissement du harnais]") && !t.contains(LOOP_STOP_NOTE))
}

/// Sépare la réponse de la ligne `CHOIX : a | b | c` qui la termine.
pub fn split_choices(text: &str) -> (String, Vec<String>) {
    let mut lines: Vec<&str> = text.trim_end().lines().collect();
    let Some(pos) = lines.iter().rposition(|l| {
        l.trim()
            .trim_start_matches(['*', '_', '-', ' '])
            .to_lowercase()
            .starts_with("choix")
    }) else {
        return (text.trim().to_string(), Vec::new());
    };
    let line = lines.remove(pos);
    let choices: Vec<String> = line
        .split_once(':')
        .map(|(_, rest)| rest)
        .unwrap_or_default()
        .split('|')
        .map(|c| {
            c.trim()
                .trim_matches(['*', '_', '`', '«', '»', '"'])
                .trim()
                .to_string()
        })
        .filter(|c| !c.is_empty())
        .map(|c| c.chars().take(60).collect())
        .take(3)
        .collect();
    (lines.join("\n").trim().to_string(), choices)
}

/// Appels au modèle par tour : au-delà, le tour s'arrête ; « Continuer » en redonne
/// autant sur le même transcript.
pub const TURN_CALLS: u32 = 24;

/// Début du message d'un tour arrêté par son plafond d'appels (pas une erreur) : Telegram
/// le reconnaît pour proposer « Continuer » plutôt que « Réessayer » (issue #139).
pub const CALLS_EXHAUSTED: &str = "le tour n'a pas convergé";

impl AgentLoop {
    pub fn new(services: Arc<Services>, provider: Arc<dyn Provider>) -> Self {
        AgentLoop {
            services,
            provider,
            max_iterations: TURN_CALLS,
        }
    }

    /// Exécute un tour sur un transcript en mémoire.
    pub async fn run(
        &self,
        req: TurnRequest,
        execute: &(dyn ToolExecutor + Send + Sync),
    ) -> anyhow::Result<TurnOutcome> {
        let conv = MemoryConversation::new(req.system_prompt, req.user_text);
        let spec = TurnSpec {
            session_id: req.session_id,
            run_id: req.run_id,
            turn_id: None,
            model_id: req.model_id,
            fallback_models: Vec::new(),
            tools: req.tools,
            allowed_tools: req.allowed_tools,
            cancel: req.cancel,
        };
        self.run_conversation(&spec, &conv, execute, &NullSink)
            .await
    }

    /// Exécute (ou reprend) un tour sur un transcript quelconque.
    #[allow(clippy::too_many_lines)] // gel 0.17 : boucle d'agent, découpée au lot G (agent/loop.rs)
    pub async fn run_conversation(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        execute: &(dyn ToolExecutor + Send + Sync),
        sink: &dyn TurnSink,
    ) -> anyhow::Result<TurnOutcome> {
        let s = &self.services;
        let cfg = s.config.config();
        let mut detector = LoopDetector::new(cfg.tools.loop_detector_repeats);
        let mut cost = 0.0f64;
        // Une réponse vide a droit à une seule relance, puis devient une erreur explicite.
        let mut empty_retry = false;
        // Un dépassement de fenêtre prouvé a droit à une compaction, pas davantage.
        let mut overflow_compacted = false;

        // Ce que le modèle va lire est nommé dès l'ouverture du tour : la chaîne d'audit
        // référence le prompt système par son empreinte, et l'instantané la résout
        // (issue #205).
        let prefix = conv.prompt_prefix();
        s.events
            .append(
                EventDraft::new(
                    "turn.started",
                    json!({
                        "model": spec.model_id,
                        "system_hash": prefix.as_ref().map(|p| p.hash()),
                        "tools_hash": crate::cache_audit::Fingerprint::tools_hash_of(&spec.tools),
                    }),
                )
                .session(&spec.session_id),
            )
            .await?;

        for iteration in 0..self.max_iterations {
            if spec.cancel.is_cancelled() {
                return Ok(TurnOutcome::Cancelled);
            }

            // 1. Appels en attente : premier passage ou reprise, même chemin.
            match self
                .resolve_pending(spec, conv, execute, sink, &mut detector)
                .await?
            {
                Pending::Stop(outcome) => return Ok(outcome),
                Pending::Loop {
                    report,
                    tool,
                    last_result,
                } => {
                    return self
                        .answer_after_loop(spec, conv, report, &tool, last_result.as_deref())
                        .await;
                }
                Pending::Nothing | Pending::Resolved => {}
            }
            if spec.cancel.is_cancelled() {
                return Ok(TurnOutcome::Cancelled);
            }

            // 2. Budget : vérifié **avant** chaque appel, pas après coup.
            let statuses = s
                .budget
                .status(&cfg.budget, Some(&spec.session_id), spec.run_id.as_deref())
                .await?;
            if let Some(exceeded) = statuses.iter().find(|b| b.exceeded) {
                let stop = TurnOutcome::BudgetExceeded {
                    scope: exceeded.scope.as_str().to_string(),
                    spent_usd: exceeded.spent_usd,
                    limit_usd: exceeded.limit_usd,
                };
                // Une carte « continuer ? » par plafond atteint (issue #32) : une demande
                // déjà tranchée sans relèvement arrête le tour, une demande en attente est
                // réutilisée plutôt que dupliquée.
                let call_id = format!(
                    "budget:{}:{}",
                    exceeded.scope.as_str(),
                    (exceeded.limit_usd * 100.0).round() as i64
                );
                let approval_id = match s
                    .approvals
                    .find_for_call(&spec.session_id, &call_id)
                    .await?
                {
                    Some(a) if a.state == ApprovalState::Pending => a.id.0.clone(),
                    Some(_) => return Ok(stop),
                    None => {
                        s.approvals
                            .create(
                                ApprovalKind::BudgetExceeded,
                                exceeded.scope.as_str(),
                                RiskClass::Unknown,
                                json!({
                                    "budget": true,
                                    "scope": exceeded.scope.as_str(),
                                    "spent": exceeded.spent_usd,
                                    "limit": exceeded.limit_usd,
                                    "call_id": call_id,
                                    "turn_id": spec.turn_id,
                                    "run_id": spec.run_id,
                                }),
                                vec!["+5 $".into(), "+20 $".into(), "Arrêter".into()],
                                Some(&spec.session_id),
                                spec.run_id.as_deref(),
                                false,
                            )
                            .await?
                            .id
                            .0
                    }
                };
                // Session ouverte par le propriétaire : le tour se suspend et reprend là où
                // il s'est arrêté si le plafond est relevé. Le jour et les runs s'arrêtent.
                let owner_session = exceeded.scope == penelope_kernel::budget::BudgetScope::Session
                    && spec.run_id.is_none()
                    && s.sessions
                        .get(&spec.session_id)
                        .await?
                        .is_some_and(|x| x.kind == penelope_kernel::session::SessionKind::Chat);
                if owner_session {
                    return Ok(TurnOutcome::AwaitingApproval { approval_id });
                }
                return Ok(stop);
            }

            // 2 bis. Tour déjà long, reprises après approbation comprises : plafond d'appels
            // et point de contrôle de coût (issue #19).
            if let Some(stop) = self.turn_limits(spec, sink, iteration, cost).await? {
                return Ok(stop);
            }

            // 3. Appel du modèle, avec repli sur panne transitoire.
            let mut messages = conv.request_messages().await?;
            if spec.cancel.is_cancelled() {
                return Ok(TurnOutcome::Cancelled);
            }
            if empty_retry {
                // Relance vue du modèle seulement : rien n'est écrit dans l'historique.
                messages.push(ChatMessage::user(
                    "(Relance automatique : ta réponse précédente était vide. Réponds \
                     maintenant, en texte, au dernier message.)",
                ));
            }
            // Empreinte et fournisseur amont collant : le cache de préfixe reste chaud et
            // un raté est expliqué (issue #17).
            let previous = crate::cache_audit::previous_call(s, &spec.session_id).await?;
            let pinned = crate::cache_audit::sticky_upstream(
                previous.as_ref(),
                &spec.model_id,
                s.clock.now_ms(),
            );
            let fingerprint = crate::cache_audit::Fingerprint::of(&messages, &spec.tools);
            let response = match self.call_model(spec, messages, sink, pinned, None).await? {
                Ok(r) => r,
                Err(failure) if failure.context_length && !overflow_compacted => {
                    overflow_compacted = true;
                    match conv.compact_for_overflow().await {
                        Ok(true) => {
                            tracing::info!(
                                session = %spec.session_id,
                                "fenêtre dépassée : historique compacté, nouvel essai"
                            );
                            continue;
                        }
                        Ok(false) => {
                            return Ok(TurnOutcome::Failed {
                                error: failure.message,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(
                                session = %spec.session_id,
                                error = %e,
                                "compaction sur dépassement impossible"
                            );
                            return Ok(TurnOutcome::Failed {
                                error: failure.message,
                            });
                        }
                    }
                }
                Err(failure) => {
                    return Ok(TurnOutcome::Failed {
                        error: failure.message,
                    });
                }
            };

            cost += response.cost_usd;
            // Le prompt système rendu devient une ligne, adressée par l'empreinte déjà
            // calculée (issue #205). L'écriture suit l'appel : elle n'est pas dans la
            // latence du premier jeton, et son échec ne coûte que le diagnostic.
            if let Some(prefix) = &prefix
                && let Err(e) =
                    crate::prompt_snapshot::record(s, &fingerprint.system_hash, prefix).await
            {
                tracing::warn!(error = %e, "instantané du prompt non enregistré");
            }
            let miss = crate::cache_audit::miss_cause(
                previous.as_ref(),
                &crate::cache_audit::Observed {
                    fingerprint: &fingerprint,
                    model: &response.model,
                    upstream: response.upstream.as_deref(),
                    prompt: response.usage.prompt,
                    cached: response.usage.cached,
                    now_ms: s.clock.now_ms(),
                },
            );
            // « Le préfixe a changé » ne suffit pas : dire laquelle des tuiles a bougé.
            let miss = match miss {
                Some("prefixe") => Some(
                    crate::prompt_snapshot::prefix_cause(
                        s,
                        previous.as_ref().and_then(|p| p.system_hash.as_deref()),
                        &fingerprint.system_hash,
                    )
                    .await,
                ),
                other => other.map(String::from),
            };
            s.budget
                .record(penelope_kernel::budget::UsageRecord {
                    msg_count: Some(fingerprint.chain.len() as i64),
                    request_hash: fingerprint.request_hash(),
                    system_hash: Some(fingerprint.system_hash.clone()),
                    tools_hash: Some(fingerprint.tools_hash.clone()),
                    miss_cause: miss,
                    session_id: Some(spec.session_id.clone()),
                    run_id: spec.run_id.clone(),
                    turn_id: spec.turn_id.clone(),
                    model: response.model.clone(),
                    provider: response.provider.clone(),
                    role: Some("chat".into()),
                    generation_id: (!response.id.is_empty()).then(|| response.id.clone()),
                    upstream: response.upstream.clone(),
                    finish: Some(format!("{:?}", response.finish).to_lowercase()),
                    prompt: response.usage.prompt,
                    completion: response.usage.completion,
                    cached: response.usage.cached,
                    cache_write: response.usage.cache_write,
                    reasoning: response.usage.reasoning,
                    cost_usd: response.cost_usd,
                    estimated: response.cost_estimated,
                    ..Default::default()
                })
                .await?;

            if response.finish == FinishReason::Cancelled {
                // Ce qui a été écrit avant l'arrêt reste dans le transcript.
                if !response.message.text().is_empty() {
                    conv.record(&response.message, false).await?;
                }
                return Ok(TurnOutcome::Cancelled);
            }

            let mut response = response;
            // Un refus explicite est une réponse : on la montre telle quelle.
            if response.message.tool_calls.is_empty()
                && response.message.text().trim().is_empty()
                && let Some(refusal) = response.refusal.clone().filter(|r| !r.trim().is_empty())
            {
                response.message.content = vec![penelope_llm::types::Content::text(refusal)];
            }

            // Ni texte ni appel d'outil : on ne livre jamais une réponse vide en silence.
            if response.message.tool_calls.is_empty() && response.message.text().trim().is_empty() {
                // Budget de sortie mangé par le raisonnement : relancer ne changerait rien.
                let reasoning_ate_budget = response.finish == FinishReason::Length
                    && response.usage.reasoning > 0
                    && response
                        .usage
                        .completion
                        .saturating_sub(response.usage.reasoning)
                        <= 2;
                tracing::warn!(
                    session = %spec.session_id,
                    model = %response.model,
                    upstream = ?response.upstream,
                    finish = ?response.finish,
                    native_finish = ?response.native_finish,
                    prompt_tokens = response.usage.prompt,
                    completion_tokens = response.usage.completion,
                    reasoning_tokens = response.usage.reasoning,
                    reasoning_chars = response.reasoning.chars().count(),
                    retried = empty_retry,
                    "réponse vide du modèle"
                );
                s.events
                    .append(
                        EventDraft::new(
                            "turn.empty_answer",
                            json!({
                                "model": response.model,
                                "upstream": response.upstream,
                                "generation_id": response.id,
                                "finish": format!("{:?}", response.finish),
                                "native_finish": response.native_finish,
                                "completion_tokens": response.usage.completion,
                                "reasoning_tokens": response.usage.reasoning,
                                "retried": empty_retry,
                            }),
                        )
                        .session(&spec.session_id),
                    )
                    .await?;
                if !empty_retry && !reasoning_ate_budget {
                    empty_retry = true;
                    continue;
                }
                let upstream = response
                    .upstream
                    .as_deref()
                    .map(|u| format!(" chez {u}"))
                    .unwrap_or_default();
                let cause = if reasoning_ate_budget {
                    format!(
                        "le raisonnement a consommé tout le budget de sortie ({} tokens sur {})",
                        response.usage.reasoning, response.usage.completion
                    )
                } else {
                    format!(
                        "aucun texte, deux fois (fin : {:?}{} ; {} tokens produits dont {} de \
                         raisonnement)",
                        response.finish,
                        response
                            .native_finish
                            .as_deref()
                            .map(|n| format!(", amont : {n}"))
                            .unwrap_or_default(),
                        response.usage.completion,
                        response.usage.reasoning
                    )
                };
                return Ok(TurnOutcome::Failed {
                    error: format!(
                        "le modèle `{}`{upstream} n'a pas répondu : {cause}. Essayer un autre \
                         modèle (`/model main openrouter:<identifiant>`) ; `/models` montre le \
                         routage en vigueur",
                        response.model
                    ),
                });
            }

            conv.record(&response.message, false).await?;

            // 4. Pas d'appel d'outil : c'est la réponse finale.
            if response.message.tool_calls.is_empty() {
                let mut text = response.message.text();
                // Un tour coûteux le dit, sans que la mention entre dans l'historique.
                if let Some(turn_id) = &spec.turn_id
                    && cfg.budget.show_turn_cost_usd > 0.0
                {
                    let (calls, turn_cost) = s.budget.turn_totals(turn_id).await?;
                    if turn_cost >= cfg.budget.show_turn_cost_usd {
                        text.push_str(&format!(
                            "\n\n_Coût de ce tour : {} ({calls} appels au modèle)._",
                            crate::budget_alert::usd(turn_cost)
                        ));
                    }
                }
                s.events
                    .append(
                        EventDraft::new(
                            "turn.finished",
                            json!({"iterations": iteration + 1, "cost_usd": cost}),
                        )
                        .session(&spec.session_id),
                    )
                    .await?;
                return Ok(TurnOutcome::Answered {
                    text,
                    iterations: iteration + 1,
                    cost_usd: cost,
                });
            }
            // Sinon, l'itération suivante résout les appels qui viennent d'être écrits.
        }

        Ok(TurnOutcome::Failed {
            error: format!("{CALLS_EXHAUSTED} en {} itérations", self.max_iterations),
        })
    }

    /// Appelle le modèle en diffusant les fragments ; essaie les replis sur panne.
    ///
    /// Chez OpenRouter, les replis partent dans la requête (`models`) : OpenRouter bascule
    /// lui-même avant le premier jeton, y compris quand la panne survient après le 200.
    /// Ailleurs, ils sont essayés ici, sur les erreurs d'avant flux.
    #[allow(clippy::too_many_lines)] // gel 0.17 : appel du modèle et replis, lot G (agent/model_call.rs)
    async fn call_model(
        &self,
        spec: &TurnSpec,
        messages: Vec<ChatMessage>,
        sink: &dyn TurnSink,
        pinned_upstream: Option<String>,
        tool_choice: Option<ToolChoice>,
    ) -> anyhow::Result<Result<ChatResponse, CallFailure>> {
        let s = &self.services;
        let server_side_fallback = self.provider.name() == "openrouter";
        let mut candidates = vec![spec.model_id.clone()];
        if !server_side_fallback {
            candidates.extend(
                spec.fallback_models
                    .iter()
                    .filter(|m| **m != spec.model_id)
                    .cloned(),
            );
        }
        let mut last_error = CallFailure::plain("aucun modèle n'a répondu");
        // Erreur d'avant flux : quelques nouvelles tentatives, attente doublée à chaque
        // fois, avant de changer de modèle (issue #50).
        let max_retries = s.config.config().providers.openrouter.request_retries;
        let mut retries: u32 = 0;
        let mut waited_secs: u64 = 0;
        // Erreur en cours de flux avant tout texte : un nouvel essai, puis les replis
        // (côté client, même avec OpenRouter dont le repli ne joue qu'avant le flux).
        let mut stream_retried = false;
        let mut client_fallbacks = !server_side_fallback;

        let mut attempt = 0;
        while attempt < candidates.len() {
            let model_id = candidates[attempt].clone();
            let request = ChatRequest {
                model: model_id.clone(),
                messages: fit_modalities(&messages, &s.catalog, &model_id),
                tools: spec.tools.clone(),
                tool_choice: match (&tool_choice, spec.tools.is_empty()) {
                    (_, true) => None,
                    (Some(forced), false) => Some(*forced),
                    (None, false) => Some(ToolChoice::Auto),
                },
                stream: true,
                session_id: Some(spec.session_id.clone()),
                fallback_models: if server_side_fallback {
                    spec.fallback_models.clone()
                } else {
                    Vec::new()
                },
                // Collant pour le modèle principal seulement : un repli change de fournisseur.
                pinned_upstream: (attempt == 0).then(|| pinned_upstream.clone()).flatten(),
                ..Default::default()
            };

            // Machine d'état des appels LLM (§4.3). Les trois clés de l'empreinte sont
            // calculées sur le corps réellement envoyé : c'est par elles que la requête
            // se retrouve, le corps n'étant jamais recopié (issue #205).
            let llm_id = format!("q_{}", penelope_kernel::ids::Ulid::new());
            let body = serde_json::to_value(&request).unwrap_or(Value::Null);
            let keys =
                crate::cache_audit::Fingerprint::of(&request.messages, &request.tools).keys();
            s.llm_state
                .plan(
                    penelope_llm::PlannedCall {
                        id: &llm_id,
                        session_id: Some(&spec.session_id),
                        run_id: spec.run_id.as_deref(),
                        model: &model_id,
                        provider: self.provider.name(),
                        keys,
                    },
                    &body,
                )
                .await?;
            s.llm_state.dispatching(&llm_id).await?;

            let stream = match self
                .provider
                .chat_stream(request, spec.cancel.clone())
                .await
            {
                Ok(st) => st,
                Err(e) => {
                    s.llm_state
                        .failed(&llm_id, &e.to_string(), e.maybe_billed)
                        .await?;
                    last_error = CallFailure::from_llm(&e);
                    let retryable = penelope_llm::Router::should_fallback(&e);
                    // Incident passager (5xx, délai de connexion, limite de débit) : on
                    // rappelle le même modèle après 1 s, 2 s, 4 s, ou après le
                    // `Retry-After` s'il est court. Un incident de deux secondes ne fait
                    // plus échouer le tour (issue #50).
                    // Tant qu'un autre modèle reste à essayer, une seule attente : le
                    // repli coûte moins cher qu'une attente de plus. Sur le dernier
                    // candidat, tout le budget de tentatives sert.
                    let others_left = attempt + 1 < candidates.len()
                        || (!client_fallbacks
                            && spec.fallback_models.iter().any(|m| !candidates.contains(m)));
                    let budget = if others_left {
                        max_retries.min(1)
                    } else {
                        max_retries
                    };
                    if retryable && retries < budget && !spec.cancel.is_cancelled() {
                        let secs = e
                            .retry_after
                            .filter(|s| *s <= RETRY_AFTER_MAX_SECS)
                            .unwrap_or_else(|| retry_backoff_secs(retries));
                        retries += 1;
                        waited_secs += secs;
                        tracing::warn!(
                            model = %model_id, error = %e, secs, attempt = retries,
                            "erreur avant le flux : nouvel essai"
                        );
                        s.events
                            .append(
                                EventDraft::new(
                                    "llm.retried",
                                    json!({
                                        "model": model_id,
                                        "attempt": retries,
                                        "wait_s": secs,
                                        "error": e.to_string(),
                                    }),
                                )
                                .session(&spec.session_id),
                            )
                            .await?;
                        if !sleep_unless_cancelled(&spec.cancel, secs).await {
                            return Ok(Err(CallFailure::plain("arrêt demandé")));
                        }
                        continue;
                    }
                    // Le fournisseur lui-même est injoignable : son repli côté serveur ne
                    // joue pas, on prend la main avec les replis d'alias.
                    if retryable && !client_fallbacks {
                        client_fallbacks = true;
                        for m in &spec.fallback_models {
                            if !candidates.contains(m) {
                                candidates.push(m.clone());
                            }
                        }
                    }
                    let more = attempt + 1 < candidates.len();
                    if more && retryable {
                        tracing::warn!(model = %model_id, error = %e, "repli sur le modèle suivant");
                        retries = 0;
                        attempt += 1;
                        continue;
                    }
                    return Ok(Err(with_attempts(last_error, retries, waited_secs)));
                }
            };
            s.llm_state.response_started(&llm_id).await?;
            sink.emit(TurnEvent::Model {
                model_id: model_id.clone(),
            });

            // Ce qui est déjà parti vers l'utilisateur : texte de réponse ou appel d'outil.
            let shown = std::sync::atomic::AtomicBool::new(false);
            let observe = |chunk: &StreamChunk| match chunk {
                StreamChunk::Delta { text } => {
                    if !text.is_empty() {
                        shown.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    sink.emit(TurnEvent::Delta(text.clone()))
                }
                StreamChunk::Reasoning { text } => sink.emit(TurnEvent::Reasoning(text.clone())),
                StreamChunk::ToolCall(_) => shown.store(true, std::sync::atomic::Ordering::SeqCst),
                _ => {}
            };
            match collect_stream_observed(
                stream,
                &model_id,
                self.provider.name(),
                &s.catalog,
                &observe,
            )
            .await
            {
                Ok(r) => {
                    s.llm_state.completed(&llm_id).await?;
                    let requested = penelope_llm::catalog::strip_provider(&model_id);
                    if !r.model.is_empty() && r.model != requested {
                        // Repli fait par OpenRouter : on le dit, rien n'est silencieux.
                        tracing::warn!(
                            session = %spec.session_id,
                            requested = %requested,
                            served = %r.model,
                            "réponse servie par un modèle de repli"
                        );
                        s.events
                            .append(
                                EventDraft::new(
                                    "llm.fallback_used",
                                    json!({"requested": requested, "served": r.model}),
                                )
                                .session(&spec.session_id),
                            )
                            .await?;
                        sink.emit(TurnEvent::Model {
                            model_id: r.model.clone(),
                        });
                    }
                    return Ok(Ok(r));
                }
                Err(e) => {
                    s.llm_state
                        .failed(&llm_id, &e.to_string(), e.maybe_billed)
                        .await?;
                    let shown = shown.load(std::sync::atomic::Ordering::SeqCst);
                    if shown {
                        // Des fragments sont déjà partis : pas de repli silencieux, on le dit.
                        let mut failure = CallFailure::from_llm(&e);
                        failure.message = format!(
                            "{}\n\nLa réponse a été coupée en cours d'écriture : le début \
                             affiché est incomplet.",
                            failure.message
                        );
                        return Ok(Err(failure));
                    }
                    last_error = CallFailure::from_llm(&e);
                    if !penelope_llm::Router::should_fallback(&e) || spec.cancel.is_cancelled() {
                        return Ok(Err(last_error));
                    }
                    if !stream_retried {
                        stream_retried = true;
                        let secs = e
                            .retry_after
                            .filter(|s| *s <= RETRY_AFTER_MAX_SECS)
                            .unwrap_or(STREAM_RETRY_SECS);
                        tracing::warn!(
                            model = %model_id,
                            error = %e,
                            secs,
                            "flux interrompu avant tout texte : nouvel essai"
                        );
                        if !sleep_unless_cancelled(&spec.cancel, secs).await {
                            return Ok(Err(CallFailure::plain("arrêt demandé")));
                        }
                        continue;
                    }
                    if !client_fallbacks {
                        client_fallbacks = true;
                        for m in &spec.fallback_models {
                            if !candidates.contains(m) {
                                candidates.push(m.clone());
                            }
                        }
                    }
                    if attempt + 1 < candidates.len() {
                        tracing::warn!(
                            model = %model_id,
                            error = %e,
                            "flux interrompu avant tout texte : repli sur le modèle suivant"
                        );
                        attempt += 1;
                        continue;
                    }
                    return Ok(Err(last_error));
                }
            }
        }
        Ok(Err(last_error))
    }

    /// Réponse après une boucle arrêtée (issue #31) : un appel sans outil explique ce qui a
    /// été tenté et l'erreur exacte, et propose des suites ; à défaut, un repli lisible qui
    /// cite l'erreur réelle. La réponse est gardée dans la conversation.
    async fn answer_after_loop(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        report: String,
        tool: &str,
        last_result: Option<&str>,
    ) -> anyhow::Result<TurnOutcome> {
        let s = &self.services;
        let mut messages = conv.request_messages().await?;
        if spec.cancel.is_cancelled() {
            return Ok(TurnOutcome::Cancelled);
        }
        messages.push(ChatMessage::user(format!(
            "(Message du harnais, pas du propriétaire.) Tu as appelé `{tool}` en boucle avec \
             les mêmes arguments : les outils sont arrêtés pour ce tour. Réponds maintenant au \
             propriétaire, sans outil, en quelques lignes : ce que tu as essayé, l'erreur exacte \
             renvoyée par l'outil (cite-la telle quelle), et ce que tu as déjà obtenu s'il y a \
             quelque chose. Termine par une seule ligne `CHOIX : <suite 1> | <suite 2> | <suite \
             3>`, deux ou trois suites courtes qu'il pourra choisir d'un clic (par exemple \
             Chercher autrement, Je te précise le compte ou les dates, Laisser tomber)."
        )));
        let text = match self
            .call_model(spec, messages, &NullSink, None, Some(ToolChoice::None))
            .await?
        {
            Ok(r) => {
                let _ = s
                    .budget
                    .record(penelope_kernel::budget::UsageRecord {
                        session_id: Some(spec.session_id.clone()),
                        run_id: spec.run_id.clone(),
                        turn_id: spec.turn_id.clone(),
                        model: r.model.clone(),
                        provider: r.provider.clone(),
                        role: Some("chat".into()),
                        generation_id: (!r.id.is_empty()).then(|| r.id.clone()),
                        prompt: r.usage.prompt,
                        completion: r.usage.completion,
                        cached: r.usage.cached,
                        reasoning: r.usage.reasoning,
                        cost_usd: r.cost_usd,
                        estimated: r.cost_estimated,
                        ..Default::default()
                    })
                    .await;
                r.message.text()
            }
            Err(failure) => {
                tracing::warn!(error = %failure.message, "réponse après boucle impossible");
                String::new()
            }
        };
        let (mut answer, mut choices) = split_choices(&text);
        if answer.trim().is_empty() {
            answer = format!(
                "Je me suis arrêtée : j'appelais `{tool}` en boucle sans avancer.\n\nErreur \
                 renvoyée par l'outil : {}\n\nComment veux-tu continuer ?",
                last_result
                    .map(|r| r.chars().take(800).collect::<String>())
                    .unwrap_or_else(|| "aucun résultat exploitable".into())
            );
        }
        if choices.len() < 2 {
            choices = LOOP_DEFAULT_CHOICES.iter().map(|c| c.to_string()).collect();
        }
        conv.record(&ChatMessage::assistant(&answer), true).await?;
        Ok(TurnOutcome::LoopAborted {
            report,
            answer,
            choices,
        })
    }

    /// Résout les appels d'outils sans résultat à la fin du transcript.
    #[allow(clippy::too_many_lines)] // gel 0.17 : lot G (agent/pending.rs)
    async fn resolve_pending(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        execute: &(dyn ToolExecutor + Send + Sync),
        sink: &dyn TurnSink,
        detector: &mut LoopDetector,
    ) -> anyhow::Result<Pending> {
        let s = &self.services;
        let cfg = s.config.config();
        let tail = conv.tail().await?;
        let pending = pending_calls(&tail);
        if pending.is_empty() {
            return Ok(Pending::Nothing);
        }
        let nudge = self.delegation_nudge(spec).await?;

        // 1. Décisions, dans l'ordre des appels : liste blanche, décision déjà prise,
        //    boucles, politique. Rien n'est exécuté ici ; un appel qui demande une
        //    approbation arrête la liste (ceux d'avant partent quand même).
        let mut steps: Vec<Step> = Vec::new();
        let mut terminal: Option<Terminal> = None;
        for mut call in pending {
            // `cd <workspace> && grep …` devient `grep …` dans ce répertoire (#123).
            if let Some(args) = execute.normalise_call(&call.name, &call.arguments) {
                call.arguments = args;
            }
            let info = execute.describe_call(&call.name, &call.arguments).await;

            // Liste blanche de l'étape ou de la skill.
            if !penelope_tools::is_allowed(&call.name, &spec.allowed_tools) {
                let text = format!("Refusé : `{}` n'est pas autorisé ici.", call.name);
                steps.push(Step::Record(call, text));
                continue;
            }

            // Une décision a-t-elle déjà été prise pour cet appel précis ?
            let prior = s
                .approvals
                .find_for_call(&spec.session_id, &call.id)
                .await?;
            let decided = match &prior {
                Some(a) => match a.state {
                    ApprovalState::Pending => {
                        terminal = Some(Terminal::Stop(TurnOutcome::AwaitingApproval {
                            approval_id: a.id.0.clone(),
                        }));
                        break;
                    }
                    ApprovalState::Approved => Some(true),
                    _ => Some(false),
                },
                None => None,
            };

            let mut parallel = false;
            match decided {
                Some(false) => {
                    let a = prior.as_ref().expect("décision sans demande");
                    let why = match a.state {
                        ApprovalState::Expired => "la demande a expiré sans réponse".to_string(),
                        _ => a
                            .reason
                            .clone()
                            .map(|r| format!("le propriétaire a refusé : {r}"))
                            .unwrap_or_else(|| "le propriétaire a refusé".to_string()),
                    };
                    steps.push(Step::Record(
                        call,
                        format!("Non exécuté : {why}. Propose une autre approche ou demande."),
                    ));
                    continue;
                }
                Some(true) => {
                    // Approuvé : on exécute, sans redemander.
                    let _ = detector.observe(&call.name, &call.arguments);
                }
                None => {
                    // Détecteur de boucles, sans l'intention : la reformuler ne change
                    // pas l'appel (#116).
                    match detector.observe(&call.name, &without_intention(&call.arguments)) {
                        LoopVerdict::Ok => {}
                        LoopVerdict::Warn(m) => {
                            steps.push(Step::Record(
                                call,
                                format!("[avertissement du harnais] {m}"),
                            ));
                            continue;
                        }
                        LoopVerdict::Abort(m) => {
                            terminal = Some(Terminal::Loop {
                                call,
                                tool: info.effective_name.clone(),
                                message: m,
                            });
                            break;
                        }
                    }

                    // Arguments vérifiés avant toute carte : un appel invalide revient au
                    // modèle avec les paramètres attendus, et compte pour la garde de
                    // boucle (issue #117).
                    if let Err(e) = execute.precheck(&call.name, &call.arguments).await {
                        let mut text = e.for_model();
                        match detector.observe_invalid(&info.effective_name) {
                            LoopVerdict::Ok => {}
                            LoopVerdict::Warn(m) => {
                                text.push_str(&format!("\n\n[avertissement du harnais] {m}"));
                            }
                            LoopVerdict::Abort(m) => {
                                terminal = Some(Terminal::Loop {
                                    call,
                                    tool: info.effective_name.clone(),
                                    message: m,
                                });
                                break;
                            }
                        }
                        steps.push(Step::Record(call, text));
                        continue;
                    }

                    // 4. Politique et approbation, sur les arguments de l'outil visé : par
                    // `tool_call`, ceux de l'appel interne (#110), sinon une règle
                    // « Toujours » couvrirait l'outil entier.
                    let effective_args =
                        crate::executor::effective_arguments(&call.name, &call.arguments);
                    let policy_workspace = execute.policy_workspace();
                    let mut verdict = s
                        .policies
                        .evaluate_in(
                            &cfg.mcp.policy,
                            &info.effective_name,
                            server_of(&info.effective_name).as_deref(),
                            &effective_args,
                            info.risk,
                            spec.run_id.as_deref(),
                            Some(&spec.session_id),
                            policy_workspace.as_deref(),
                        )
                        .await?;
                    // La déclaration du serveur MCP peut imposer sa politique à un outil :
                    // un refus l'emporte toujours, le reste cède à une règle du propriétaire.
                    if let Some(forced) = info.policy
                        && (forced == PolicyDecision::Deny || verdict.rule_id.is_none())
                    {
                        verdict.decision = forced;
                        verdict.reason = format!(
                            "politique de la déclaration du serveur : `{}`",
                            forced.as_str()
                        );
                    }
                    // Autorisation déclarée d'avance, puis mode de la session (#111).
                    if verdict.rule_id.is_none()
                        && verdict.decision != PolicyDecision::Deny
                        && let Some(why) = crate::approval_mode::local_draft_allow(
                            &info.effective_name,
                        )
                        .or_else(|| {
                            crate::approval_mode::declared_allow(
                                &cfg,
                                &info.effective_name,
                                &effective_args,
                            )
                        })
                    {
                        verdict.decision = PolicyDecision::Auto;
                        verdict.reason = why;
                    }
                    match crate::approval_mode::of_session(s, &spec.session_id).await {
                        crate::approval_mode::ApprovalMode::Ask
                            if verdict.decision == PolicyDecision::Auto
                                && (info.effective_name == "shell_exec"
                                    || info.risk != RiskClass::Read) =>
                        {
                            verdict.decision = PolicyDecision::Ask;
                            verdict.reason = "mode « demander tout » de la session".into();
                        }
                        crate::approval_mode::ApprovalMode::Auto
                            if verdict.decision == PolicyDecision::Ask
                                && verdict.rule_id.is_none()
                                && info.policy.is_none()
                                && info.risk != RiskClass::Destructive
                                && !(info.effective_name == "shell_exec"
                                    && effective_args
                                        .get("command")
                                        .and_then(|c| c.as_str())
                                        .is_none_or(penelope_tools::shell::may_destroy)) =>
                        {
                            verdict.decision = PolicyDecision::Auto;
                            verdict.reason =
                                "mode « tout sauf le destructif » de la session".into();
                        }
                        _ => {}
                    }
                    // Réseau demandé par une commande : la carte le dit en toutes lettres.
                    if crate::executor::wants_network(&info.effective_name, &effective_args)
                        && info.risk == RiskClass::External
                    {
                        verdict.reason = format!(
                            "accès réseau demandé pour cette commande ({})",
                            verdict.reason
                        );
                    }
                    // Une règle « toujours » posée pour `config_set` vaut pour les réglages
                    // ordinaires, jamais pour le bac à sable, les providers ou Telegram.
                    if info.effective_name == "config_set"
                        && info.risk == RiskClass::Destructive
                        && verdict.decision != PolicyDecision::Deny
                    {
                        verdict.decision = PolicyDecision::AskTwice;
                        verdict.reason =
                            "réglage sensible : double confirmation à chaque fois".into();
                    }

                    match verdict.decision {
                        PolicyDecision::Deny => {
                            steps.push(Step::Record(
                                call,
                                format!("Refusé par la politique : {}", verdict.reason),
                            ));
                            continue;
                        }
                        PolicyDecision::Ask | PolicyDecision::AskTwice => {
                            let double = verdict.decision == PolicyDecision::AskTwice;
                            let arguments = penelope_observe::redact_json(&effective_args);
                            // Ce que Pénélope cherche à faire, en tête de la carte : sa
                            // phrase, sinon le message du propriétaire qui a lancé le tour,
                            // jamais la raison de la politique (issue #116).
                            let why = call_intention(&call.arguments)
                                .or(call_intention(&effective_args))
                                .map(|w| (w, "agent"))
                                .or(turn_goal(conv).await.map(|g| (g, "tour")));
                            let approval = s
                                .approvals
                                .create(
                                    ApprovalKind::ToolCall,
                                    &info.effective_name,
                                    info.risk,
                                    json!({
                                        "tool": info.effective_name,
                                        "arguments": arguments,
                                        "reason": verdict.reason,
                                        "why": why.as_ref().map(|(w, _)| w),
                                        "why_from": why.as_ref().map(|(_, f)| f),
                                        "double": double,
                                        "call_id": call.id,
                                        "turn_id": spec.turn_id,
                                    }),
                                    vec![
                                        "Autoriser".into(),
                                        "Pour cette session".into(),
                                        "Toujours".into(),
                                        "Refuser".into(),
                                    ],
                                    Some(&spec.session_id),
                                    spec.run_id.as_deref(),
                                    false,
                                )
                                .await?;
                            sink.emit(TurnEvent::Approval {
                                id: approval.id.0.clone(),
                                tool: info.effective_name.clone(),
                                risk: info.risk,
                                arguments,
                                reason: verdict.reason.clone(),
                                double,
                            });
                            terminal = Some(Terminal::Stop(TurnOutcome::AwaitingApproval {
                                approval_id: approval.id.0,
                            }));
                            break;
                        }
                        PolicyDecision::Auto => {}
                    }
                    // Lecture pure autorisée d'office : elle peut partir avec ses voisines.
                    parallel = info.risk == RiskClass::Read
                        && PARALLEL_SAFE.contains(&info.effective_name.as_str());
                }
            }
            steps.push(Step::Execute {
                call,
                info,
                parallel,
            });
        }

        // 2. Exécution et résultats, dans l'ordre des appels. Des lectures consécutives
        //    partent ensemble, par quatre ; toute autre chose attend qu'elles aient fini et
        //    forme une barrière (issue #85).
        let mut recorded = 0usize;
        let mut nudge = nudge;
        let mut steps = steps.into_iter().peekable();
        while let Some(step) = steps.next() {
            if spec.cancel.is_cancelled() {
                return Ok(Pending::Stop(TurnOutcome::Cancelled));
            }
            match step {
                Step::Record(call, text) => {
                    self.record_result(conv, sink, &call, false, text, false)
                        .await?;
                    recorded += 1;
                }
                Step::Execute {
                    call,
                    info,
                    parallel: true,
                } => {
                    let mut batch = vec![(call, info)];
                    while let Some(Step::Execute { parallel: true, .. }) = steps.peek() {
                        if let Some(Step::Execute { call, info, .. }) = steps.next() {
                            batch.push((call, info));
                        }
                    }
                    for (call, _) in &batch {
                        sink.emit(TurnEvent::ToolCall {
                            name: call.name.clone(),
                            args: penelope_observe::redact_json(&call.arguments),
                        });
                    }
                    let mut outcomes: Vec<anyhow::Result<ToolOutcome>> = Vec::new();
                    for chunk in batch.chunks(PARALLEL_READS) {
                        let mut running = Vec::with_capacity(chunk.len());
                        for (call, info) in chunk {
                            running.push(self.run_effect(spec, execute, call, info));
                        }
                        outcomes.extend(futures::future::join_all(running).await);
                    }
                    for ((call, info), outcome) in batch.iter().zip(outcomes) {
                        self.finish_call(spec, conv, sink, call, info, outcome?, &mut nudge)
                            .await?;
                        recorded += 1;
                    }
                }
                Step::Execute { call, info, .. } => {
                    sink.emit(TurnEvent::ToolCall {
                        name: call.name.clone(),
                        args: penelope_observe::redact_json(&call.arguments),
                    });
                    let outcome = self.run_effect(spec, execute, &call, &info).await?;
                    self.finish_call(spec, conv, sink, &call, &info, outcome, &mut nudge)
                        .await?;
                    recorded += 1;
                }
            }
        }

        match terminal {
            None => {}
            Some(Terminal::Stop(outcome)) => return Ok(Pending::Stop(outcome)),
            Some(Terminal::Loop {
                call,
                tool,
                message,
            }) => {
                let report = format!("{message}\n\n{}", detector.report());
                s.events
                    .append(
                        EventDraft::new("turn.loop_aborted", json!({"report": report}))
                            .session(&spec.session_id),
                    )
                    .await?;
                tracing::warn!(session = %spec.session_id, %report, "boucle d'outil arrêtée");
                // L'échec reste dans la conversation (issue #31) : le résultat réel, puis
                // la note d'arrêt, pour que le tour suivant ne recommence pas.
                let last = last_result_of(&conv.tail().await?, &call.name, &call.arguments);
                let body = match &last {
                    Some(r) => format!(
                        "Dernier résultat réel de l'outil :\n{}",
                        r.chars().take(1_500).collect::<String>()
                    ),
                    None => "Aucun résultat obtenu.".to_string(),
                };
                self.record_result(
                    conv,
                    sink,
                    &call,
                    false,
                    format!("{body}\n\n{LOOP_STOP_NOTE}"),
                    false,
                )
                .await?;
                for rest in pending_calls(&conv.tail().await?) {
                    self.record_result(
                        conv,
                        sink,
                        &rest,
                        false,
                        "Non exécuté : tour arrêté par le détecteur de boucles.".into(),
                        false,
                    )
                    .await?;
                }
                return Ok(Pending::Loop {
                    report,
                    tool,
                    last_result: last,
                });
            }
        }
        conv.admit_tool_results(recorded).await?;
        Ok(Pending::Resolved)
    }

    /// Ledger d'effets **avant** exécution, puis l'outil (§4.2) : jamais ré-exécuté s'il
    /// est déjà fait ; un effet incertain attend la décision du propriétaire.
    async fn run_effect(
        &self,
        spec: &TurnSpec,
        execute: &(dyn ToolExecutor + Send + Sync),
        call: &ToolCall,
        info: &CallInfo,
    ) -> anyhow::Result<ToolOutcome> {
        let s = &self.services;
        let spec_effect = EffectSpec::new(
            effect_kind(&info.effective_name),
            info.effective_name.clone(),
            call.arguments.clone(),
        )
        .session(&spec.session_id)
        .step(&call.id)
        .idempotent(info.idempotent);
        let spec_effect = match &spec.run_id {
            Some(r) => spec_effect.run(r),
            None => spec_effect,
        };

        Ok(match s.effects.plan(spec_effect).await? {
            // Rejoué depuis le ledger : **jamais** ré-exécuté.
            Planned::Replayed(v) => ToolOutcome::ok(v),
            Planned::NeedsDecision(id) => ToolOutcome {
                value: json!({"effect": id.as_str(), "state": "unknown"}),
                is_error: true,
                text: format!(
                    "Cet appel a peut-être déjà eu lieu (effet {id}). Une décision du \
                     propriétaire est requise avant de relancer."
                ),
                eager: false,
            },
            Planned::InFlight(_) => ToolOutcome {
                value: json!({"state": "in_flight"}),
                is_error: true,
                text: "Appel déjà en cours.".into(),
                eager: false,
            },
            Planned::Fresh(id) => {
                s.effects.dispatching(&id).await?;
                let result = execute
                    .execute_cancellable(&call.name, &call.arguments, &spec.cancel)
                    .await;
                match &result {
                    Ok(o) if !o.is_error => s.effects.complete(&id, o.value.clone()).await?,
                    Ok(o) => s.effects.fail(&id, o.text.clone()).await?,
                    Err(e) => s.effects.fail(&id, e.to_string()).await?,
                }
                match result {
                    Ok(o) => o,
                    Err(e) => ToolOutcome::error(&e),
                }
            }
        })
    }

    /// Journalise le résultat d'un appel et l'enregistre dans la conversation.
    #[allow(clippy::too_many_arguments)]
    async fn finish_call(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        sink: &dyn TurnSink,
        call: &ToolCall,
        info: &CallInfo,
        outcome: ToolOutcome,
        nudge: &mut Option<String>,
    ) -> anyhow::Result<()> {
        penelope_observe::metrics::counter_inc(
            "penelope_tool_calls_total",
            &[
                ("tool", info.effective_name.as_str()),
                ("ok", if outcome.is_error { "false" } else { "true" }),
            ],
            1.0,
        );
        self.services
            .events
            .append(
                EventDraft::new(
                    "tool.result",
                    json!({
                        "tool": info.effective_name,
                        "ok": !outcome.is_error,
                        // Forme de la ligne, pour mesurer la consigne « une commande par
                        // appel » (issue #150). La commande elle-même n'est pas journalée
                        // ici : seule sa forme l'est.
                        "shape": line_shape(&info.effective_name, &call.arguments),
                    }),
                )
                .session(&spec.session_id),
            )
            .await?;
        let mut text = outcome.text.clone();
        if let Some(n) = nudge.take() {
            text.push_str(&n);
        }
        self.record_result(conv, sink, call, !outcome.is_error, text, outcome.eager)
            .await
    }

    /// Plafond d'appels et point de contrôle de coût d'un tour, comptés sur tout le tour
    /// (reprises après approbation comprises) : `Some` arrête ou suspend le tour.
    async fn turn_limits(
        &self,
        spec: &TurnSpec,
        sink: &dyn TurnSink,
        iteration: u32,
        cost: f64,
    ) -> anyhow::Result<Option<TurnOutcome>> {
        let s = &self.services;
        let cfg = s.config.config();
        let Some(turn_id) = &spec.turn_id else {
            return Ok(None);
        };
        let (calls, turn_cost) = s.budget.turn_totals(turn_id).await?;
        if calls >= self.max_iterations as i64 {
            return Ok(Some(TurnOutcome::Failed {
                error: format!(
                    "{CALLS_EXHAUSTED} en {} appels au modèle (reprises après \
                     approbation comprises)",
                    self.max_iterations
                ),
            }));
        }
        let step = cfg.budget.turn_checkpoint_usd;
        // Un run de workflow a son propre plafond (`budget.run_usd`).
        if step <= 0.0 || spec.run_id.is_some() || turn_cost < step {
            return Ok(None);
        }
        let level = (turn_cost / step).floor() as i64;
        let call_id = format!("checkpoint:{turn_id}:{level}");
        let usd = crate::budget_alert::usd;
        let prior = s
            .approvals
            .find_for_call(&spec.session_id, &call_id)
            .await?;
        match prior.map(|a| (a.state, a.id.0)) {
            Some((ApprovalState::Approved, _)) => Ok(None),
            Some((ApprovalState::Pending, id)) => {
                Ok(Some(TurnOutcome::AwaitingApproval { approval_id: id }))
            }
            Some(_) => Ok(Some(TurnOutcome::Answered {
                text: format!(
                    "⏹ Tour arrêté à ta demande après {} ({calls} appels au modèle).",
                    usd(turn_cost)
                ),
                iterations: iteration,
                cost_usd: cost,
            })),
            None => {
                let reason = format!(
                    "Ce tour a coûté {} ({calls} appels au modèle), je continue ?",
                    usd(turn_cost)
                );
                let arguments = json!({"cost_usd": turn_cost, "calls": calls});
                let approval = s
                    .approvals
                    .create(
                        ApprovalKind::BudgetExceeded,
                        "tour",
                        RiskClass::Unknown,
                        json!({
                            "checkpoint": true,
                            "call_id": call_id,
                            "turn_id": turn_id,
                            "arguments": arguments,
                            "reason": reason,
                        }),
                        vec!["Continuer".into(), "Arrêter".into()],
                        Some(&spec.session_id),
                        None,
                        false,
                    )
                    .await?;
                sink.emit(TurnEvent::Approval {
                    id: approval.id.0.clone(),
                    tool: "tour".into(),
                    risk: RiskClass::Unknown,
                    arguments,
                    reason,
                    double: false,
                });
                Ok(Some(TurnOutcome::AwaitingApproval {
                    approval_id: approval.id.0,
                }))
            }
        }
    }

    /// Tous les `delegate_after_calls` appels au modèle d'un tour, le résultat d'outil
    /// suivant rappelle de regrouper les commandes ou de déléguer (issue #19).
    async fn delegation_nudge(&self, spec: &TurnSpec) -> anyhow::Result<Option<String>> {
        let s = &self.services;
        let every = s.config.config().budget.delegate_after_calls as i64;
        let Some(turn_id) = &spec.turn_id else {
            return Ok(None);
        };
        if every == 0 {
            return Ok(None);
        }
        let (calls, turn_cost) = s.budget.turn_totals(turn_id).await?;
        if calls == 0 || calls % every != 0 {
            return Ok(None);
        }
        Ok(Some(format!(
            "\n\n[Harnais : {calls} appels au modèle dans ce tour ({}), chacun renvoie tout le \
             contexte. Regroupe les commandes restantes dans un seul `shell_exec`, ou confie la \
             suite à `sub_agent_spawn`, qui ne rend que sa conclusion. Mets aussi à jour tes \
             notes de travail (`session_notes` : plan, décisions, prochaine étape).]",
            crate::budget_alert::usd(turn_cost)
        )))
    }

    async fn record_result(
        &self,
        conv: &dyn Conversation,
        sink: &dyn TurnSink,
        call: &ToolCall,
        ok: bool,
        text: String,
        eager: bool,
    ) -> anyhow::Result<()> {
        let preview: String = text.chars().take(200).collect();
        conv.record(&ChatMessage::tool_result(&call.id, &call.name, text), eager)
            .await?;
        sink.emit(TurnEvent::ToolResult {
            name: call.name.clone(),
            ok,
            preview,
        });
        Ok(())
    }

    /// Tranche une approbation et crée la règle éventuelle (§9.2). Ne relance pas le
    /// tour : c'est au canal de remettre un tour `resume` en file.
    pub async fn decide_approval(
        &self,
        approval_id: &str,
        decision: &Decision,
    ) -> anyhow::Result<bool> {
        decide_approval(&self.services, approval_id, decision).await
    }

    /// Ancien nom, conservé pour les appelants existants.
    pub async fn resume_after_approval(
        &self,
        approval_id: &str,
        decision: &Decision,
    ) -> anyhow::Result<bool> {
        self.decide_approval(approval_id, decision).await
    }
}

/// Phrase d'intention d'un appel (`pourquoi`), s'il en porte une (issue #116).
pub(crate) fn call_intention(args: &Value) -> Option<String> {
    args.get(penelope_tools::WHY_FIELD)
        .and_then(|v| v.as_str())
        .map(|w| w.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().take(200).collect())
}

/// But du tour, à défaut d'intention : le dernier message du propriétaire, raccourci.
async fn turn_goal(conv: &dyn Conversation) -> Option<String> {
    let tail = conv.tail().await.ok()?;
    let said = tail
        .iter()
        .rev()
        .find(|m| m.role == Role::User)?
        .text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if said.is_empty() {
        return None;
    }
    let short: String = said.chars().take(160).collect();
    let short = if short.chars().count() < said.chars().count() {
        format!("{short}…")
    } else {
        short
    };
    Some(format!("Pour ta demande : « {short} »"))
}

/// Arguments sans l'intention : elle ne change pas l'appel, ni pour la garde de boucle ni
/// pour le serveur qui l'exécute.
pub(crate) fn without_intention(args: &Value) -> Value {
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        o.remove(penelope_tools::WHY_FIELD);
    }
    a
}

/// Forme d'une ligne `shell_exec`, pour la mesure (issue #150) : `simple` (une commande),
/// `liste` (des `&&` nommables), `composee` (le reste, qui ne peut porter aucune règle).
/// `None` pour tout autre outil.
fn line_shape(tool: &str, args: &Value) -> Option<&'static str> {
    if tool != "shell_exec" {
        return None;
    }
    let command = args.get("command").and_then(|v| v.as_str())?;
    Some(match penelope_hitl::cmdline::list(command) {
        Some(l) if l.steps.len() > 1 => "liste",
        Some(_) => "simple",
        None => "composee",
    })
}

/// Vrai quand un « Toujours » sur cet appel n'écrira aucune règle : une commande composée
/// n'a pas de famille, et une règle sur `shell_exec` entier n'existe pas (issue #111). La
/// carte et le CLI le disent **avant** le clic, plutôt que de laisser croire au contraire
/// (issue #141).
pub fn always_creates_no_rule(subject: &str, args: Option<&Value>) -> bool {
    args.is_some() && subject == "shell_exec" && arg_patterns(subject, args).is_empty()
}

/// Nombre de familles qu'un seul clic peut autoriser. Au-delà, la carte ne peut plus les
/// nommer toutes d'un coup de lecture : le propriétaire autoriserait sans savoir quoi.
pub const MAX_FAMILIES_PER_CLICK: usize = 3;

/// Motifs de règles d'un appel : un par famille (issue #150). Une ligne simple en rend un,
/// une liste `a && b && c` en rend un par famille distincte, et une ligne composée aucun.
///
/// Les étapes de lecture pure (`ls -la tmp/x*`) n'en demandent pas : elles passent déjà
/// sans carte (#111), et une règle sur `ls` ne borne rien.
pub(crate) fn arg_patterns(tool: &str, args: Option<&Value>) -> Vec<Value> {
    use penelope_hitl::policy::CMD_PREFIX_OP;
    if tool != "shell_exec" {
        return arg_pattern(tool, args).into_iter().collect();
    }
    let Some(args) = args else {
        return Vec::new();
    };
    let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    // `&&` seulement : `;`, `||`, une substitution ou une redirection laissent la ligne
    // composée, donc sans famille — c'est le choix de #67, inchangé.
    let Some(list) = penelope_hitl::cmdline::list(command) else {
        return Vec::new();
    };
    let network = crate::executor::wants_network(tool, args);
    let mut out: Vec<Value> = Vec::new();
    for step in &list.steps {
        // Une lecture, ou un `cd` qui prépare la suite, n'a besoin d'aucune règle.
        if penelope_hitl::cmdline::needs_no_rule(step) {
            continue;
        }
        // Une étape sans famille lisible rend la ligne entière incouvrable : mieux vaut
        // aucune règle qu'une règle qui n'en couvre qu'une partie.
        let Some(head) = family_of(step) else {
            return Vec::new();
        };
        let pattern = if network {
            json!({"command": {CMD_PREFIX_OP: head}, "network": true})
        } else {
            json!({"command": {CMD_PREFIX_OP: head}})
        };
        if !out.contains(&pattern) {
            out.push(pattern);
        }
    }
    // Trois familles d'un clic au plus : la carte doit pouvoir les nommer toutes.
    if out.len() > MAX_FAMILIES_PER_CLICK {
        return Vec::new();
    }
    out
}

/// Famille d'une étape : son programme, ou ses deux premiers mots pour les commandes qui
/// portent une sous-commande. `None` quand elle ne se relit pas telle qu'écrite.
fn family_of(step: &penelope_hitl::cmdline::Pipeline) -> Option<String> {
    const TWO_WORDS: &[&str] = &[
        "cargo", "git", "gh", "npm", "pnpm", "yarn", "make", "docker", "kubectl", "brew",
        "python3", "uv", "go",
    ];
    let head_words: Vec<String> = match step.head.words.as_slice() {
        [first, second, ..] if TWO_WORDS.contains(&first.as_str()) => {
            vec![first.clone(), second.clone()]
        }
        [first, ..] => vec![first.clone()],
        [] => return None,
    };
    let head = head_words.join(" ");
    // La famille doit se relire comme elle a été écrite, sinon la règle créée ne
    // couvrirait jamais la commande dont elle vient (régression de #111) : un mot qui
    // porte une espace ou un guillemet ne fait pas une famille.
    if penelope_hitl::cmdline::family(&head).is_none_or(|w| w != head_words) {
        return None;
    }
    Some(head)
}

/// Motif d'arguments d'une règle « toujours », dérivé de l'appel : ce qui borne
/// l'autorisation à ce que le propriétaire a vraiment vu (issue #67). `None` : la règle
/// couvre l'outil (outils MCP, outils sans argument significatif).
pub(crate) fn arg_pattern(tool: &str, args: Option<&Value>) -> Option<Value> {
    use penelope_hitl::policy::{CMD_PREFIX_OP, ORIGIN_OP, PATH_PREFIX_OP};
    let args = args?;
    let str_of = |k: &str| args.get(k).and_then(|v| v.as_str()).map(String::from);
    let prefix = |k: &str, op: &str, v: String| Some(json!({k: {op: v}}));
    match tool {
        // Famille de commandes : `cargo test …`, `git log …`, `ls …`.
        "shell_exec" => {
            let command = str_of("command")?;
            // Une commande composée (`cd /x && ls`) n'a pas de famille : une règle sur
            // `cd` ne s'appliquerait jamais (issue #111). Pas de motif, donc pas de règle.
            // Le découpage est celui de `cmdline` : un `&` entre guillemets (URL de
            // requête) n'enchaîne rien, `VAR=x cmd` a pour famille `cmd`, et un tube vers
            // une lecture pure (`… | jq`) celle de sa première étape (issue #141).
            let line = penelope_hitl::cmdline::pipeline(&command)?;
            let network = crate::executor::wants_network(tool, args);
            let head = family_of(&line)?;
            // Le réseau accordé l'est à la famille de commandes, jamais au shell (#106) :
            // « Toujours » sur `git push` avec réseau ne donne rien à `curl`.
            if network {
                return Some(json!({"command": {CMD_PREFIX_OP: head}, "network": true}));
            }
            prefix("command", CMD_PREFIX_OP, head)
        }
        // Répertoire du fichier : un « toujours » sur `src/a.rs` vaut pour `src/`.
        "fs_write" | "fs_edit" => {
            let path = str_of("path")?;
            let dir = match path.rfind('/') {
                Some(i) => path[..=i].to_string(),
                None => String::new(),
            };
            prefix("path", PATH_PREFIX_OP, dir)
        }
        "git_push" => {
            let mut m = serde_json::Map::new();
            for k in ["remote", "branch"] {
                if let Some(v) = str_of(k) {
                    m.insert(k.into(), json!(v));
                }
            }
            (!m.is_empty()).then(|| Value::Object(m))
        }
        "git_clone" => {
            let source = str_of("url")?;
            let url = penelope_tools::git::normalize_clone_url(&source).ok()?;
            if url.starts_with("file://") {
                return None;
            }
            let origin = if let Some((scheme, rest)) = url.split_once("://") {
                let host = rest.split('/').next()?.to_ascii_lowercase();
                format!("{}://{host}", scheme.to_ascii_lowercase())
            } else {
                let (user_host, _) = url.split_once(':')?;
                let (_, host) = user_host.split_once('@')?;
                format!("ssh://{}", host.to_ascii_lowercase())
            };
            prefix("url", ORIGIN_OP, origin)
        }
        // Hôte visé, schéma compris.
        "http_fetch" => {
            let url = str_of("url")?;
            let host = url
                .split_once("://")
                .map(|(scheme, rest)| {
                    format!("{scheme}://{}", rest.split('/').next().unwrap_or_default())
                })
                .unwrap_or(url);
            prefix("url", ORIGIN_OP, host)
        }
        "config_set" => str_of("path").map(|p| json!({"path": p})),
        _ => None,
    }
}

#[cfg(test)]
mod clone_policy_tests;

/// Choix d'une demande `effect_unknown` (#83) : l'appel a eu lieu (vérifié par le
/// propriétaire), il faut le relancer, ou il reste tel quel.
pub const EFFECT_DONE: &str = "C'est fait";
pub const EFFECT_RETRY: &str = "Relancer";
pub const EFFECT_IGNORE: &str = "Ignorer";

/// Tranche un effet incertain : la décision passe au ledger, **jamais** dans une règle.
/// La transition est faite avant que le canal ne remette le tour en file : la reprise
/// trouve l'effet `completed` (rejoué), `planned` (relancé) ou la demande refusée.
async fn decide_uncertain_effect(
    s: &Services,
    a: &penelope_hitl::ApprovalRequest,
    decision: &Decision,
) -> anyhow::Result<bool> {
    use penelope_kernel::effects::UnknownDecision;
    use penelope_kernel::ids::EffectId;
    let ledger = match (decision.approved, decision.choice.as_str()) {
        (true, EFFECT_DONE) => UnknownDecision::MarkCompleted(json!({
            "statut": "fait",
            "note": "Le propriétaire a vérifié : cet appel avait bien eu lieu avant l'arrêt \
                     du daemon. Il n'est pas relancé.",
        })),
        (true, EFFECT_RETRY) => UnknownDecision::Retry,
        (false, _) => UnknownDecision::Ignore,
        (true, other) => anyhow::bail!(
            "effet incertain : choisir « {EFFECT_DONE} », « {EFFECT_RETRY} » ou « \
             {EFFECT_IGNORE} » (reçu « {other} ») ; en ligne de commande, `penelope approve \
             <id> --effect done|retry` ou `penelope deny <id>`"
        ),
    };
    let effect_id = a.payload["effect_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("demande {} sans effet", a.id.0))?
        .to_string();
    // Pas de règle, quelle que soit la fenêtre demandée.
    let recorded = Decision {
        window: PolicyWindow::Once,
        reason: decision.reason.clone().or_else(|| {
            (!decision.approved)
                .then(|| "effet incertain laissé tel quel, sans relance".to_string())
        }),
        ..decision.clone()
    };
    match s.approvals.decide(a.id.as_str(), &recorded).await {
        Ok(_) => {
            s.effects
                .resolve_unknown(&EffectId(effect_id.clone()), ledger)
                .await?;
            s.events
                .append(EventDraft::new(
                    "approval.decided",
                    json!({
                        "id": a.id.as_str(),
                        "approved": decision.approved,
                        "via": decision.via,
                        "window": "once",
                        "effect": effect_id,
                        "choice": decision.choice,
                    }),
                ))
                .await?;
            Ok(decision.approved)
        }
        Err(penelope_hitl::HitlError::AlreadyDecided { .. }) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Tranche une approbation : la première décision gagne, une fenêtre crée une règle.
pub async fn decide_approval(
    s: &Services,
    approval_id: &str,
    decision: &Decision,
) -> anyhow::Result<bool> {
    if let Some(a) = s.approvals.get(approval_id).await?
        && a.kind == penelope_hitl::ApprovalKind::EffectUnknown
    {
        return decide_uncertain_effect(s, &a, decision).await;
    }
    match s.approvals.decide(approval_id, decision).await {
        Ok(a) => {
            // Une règle ne naît que d'un appel d'outil dont on a vu les arguments : une
            // demande sans arguments (budget, effet incertain…) n'ouvre jamais une
            // autorisation sur l'outil entier (#83, régression de #67).
            let rule_allowed = a.payload.get("arguments").is_some()
                && !matches!(
                    a.kind,
                    penelope_hitl::ApprovalKind::BudgetExceeded
                        | penelope_hitl::ApprovalKind::EffectUnknown
                );
            // « Toujours », « pour ce run », « pour cette session » : règle visible et
            // révocable dans `/policies`.
            let window_ref = match decision.window {
                PolicyWindow::Run => a.run_id.clone().or_else(|| a.session_id.clone()),
                PolicyWindow::Session => a.session_id.clone(),
                _ => None,
            };
            if decision.approved && decision.window != PolicyWindow::Once && rule_allowed {
                let window = if decision.window == PolicyWindow::Run && a.run_id.is_none() {
                    PolicyWindow::Session
                } else {
                    decision.window
                };
                // Un « toujours » est borné au contexte de l'appel (famille de commandes,
                // répertoire, remote), pas à l'outil entier (issue #67). Une liste
                // `a && b` en demande une par famille (issue #150).
                let patterns = arg_patterns(&a.subject, a.payload.get("arguments"));
                // Une commande sans famille (composée) : autorisée cette fois, jamais le
                // shell entier (issue #111).
                if patterns.is_empty() && matches!(a.subject.as_str(), "shell_exec" | "git_clone") {
                    tracing::info!(
                        approval = approval_id,
                        "appel sans motif sûr : autorisé une fois, sans règle"
                    );
                    s.approvals.note_rules(approval_id, 0).await?;
                } else {
                    // Sans motif (outils MCP), la règle porte sur l'outil : une seule.
                    let patterns: Vec<Option<Value>> = if patterns.is_empty() {
                        vec![None]
                    } else {
                        patterns.into_iter().map(Some).collect()
                    };
                    let created = patterns.len();
                    for pattern in patterns {
                        s.policies
                            .create_rule(
                                penelope_hitl::RuleScope::Tool,
                                Some(&a.subject),
                                server_of(&a.subject).as_deref(),
                                pattern,
                                PolicyDecision::Auto,
                                window,
                                window_ref.as_deref(),
                            )
                            .await?;
                    }
                    s.approvals.note_rules(approval_id, created).await?;
                }
            } else if !decision.approved && decision.window.creates_rule() && rule_allowed {
                let pattern = if a.subject == "git_clone" {
                    arg_pattern(&a.subject, a.payload.get("arguments"))
                } else {
                    None
                };
                if a.subject != "git_clone" || pattern.is_some() {
                    s.policies
                        .create_rule(
                            penelope_hitl::RuleScope::Tool,
                            Some(&a.subject),
                            server_of(&a.subject).as_deref(),
                            pattern,
                            PolicyDecision::Deny,
                            decision.window,
                            None,
                        )
                        .await?;
                }
            }
            s.events
                .append(EventDraft::new(
                    "approval.decided",
                    json!({
                        "id": approval_id,
                        "approved": decision.approved,
                        "via": decision.via,
                        "window": decision.window.as_str(),
                    }),
                ))
                .await?;
            Ok(decision.approved)
        }
        // Déjà tranché par l'autre canal : la première décision gagne.
        Err(penelope_hitl::HitlError::AlreadyDecided { .. }) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Appels du dernier message assistant qui n'ont pas encore de résultat.
///
/// Si un message utilisateur est arrivé depuis, les appels sont abandonnés : la
/// conversation a repris ailleurs, et ils ne doivent pas bloquer la nouvelle demande.
pub fn pending_calls(tail: &[ChatMessage]) -> Vec<ToolCall> {
    let Some(idx) = tail
        .iter()
        .rposition(|m| m.role == Role::Assistant && !m.tool_calls.is_empty())
    else {
        return Vec::new();
    };
    let after = &tail[idx + 1..];
    if after.iter().any(|m| m.role != Role::Tool) {
        return Vec::new();
    }
    let answered: BTreeSet<&str> = after
        .iter()
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    tail[idx]
        .tool_calls
        .iter()
        .filter(|c| !answered.contains(c.id.as_str()))
        .cloned()
        .collect()
}

/// Message d'erreur lisible pour le propriétaire.
/// Attente maximale honorée pour un `Retry-After`.
const RETRY_AFTER_MAX_SECS: u64 = 20;
/// Attente avant de relancer un flux coupé sans `Retry-After`.
const STREAM_RETRY_SECS: u64 = 2;

/// Attente avant la n-ième nouvelle tentative d'avant flux : 1 s, 2 s, 4 s… (issue #50).
fn retry_backoff_secs(done: u32) -> u64 {
    1u64 << done.min(4)
}

/// Dit combien de fois on a essayé et combien de temps on a attendu : un échec après
/// trois délais de connexion ne se lit pas comme un échec immédiat (issue #50).
fn with_attempts(mut failure: CallFailure, retries: u32, waited_secs: u64) -> CallFailure {
    if retries > 0 {
        failure.message = format!(
            "{}\n\n{} tentatives, {waited_secs} s d'attente entre elles.",
            failure.message,
            retries + 1
        );
    }
    failure
}

/// Dort `secs` secondes, sauf annulation. Vrai si l'attente est allée au bout.
async fn sleep_unless_cancelled(cancel: &CancelToken, secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if cancel.is_cancelled() {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    !cancel.is_cancelled()
}

/// Un modèle qui ne lit pas les images reçoit une mention à leur place : une photo
/// envoyée plus tôt ne doit pas faire échouer la conversation après un changement de
/// modèle ou un repli.
pub(crate) fn fit_modalities(
    messages: &[ChatMessage],
    catalog: &penelope_llm::catalog::Catalog,
    model_id: &str,
) -> Vec<ChatMessage> {
    let blind = catalog
        .get(penelope_llm::catalog::strip_provider(model_id))
        .map(|i| !i.accepts_images())
        .unwrap_or(false);
    let has_images = messages.iter().any(|m| {
        m.content
            .iter()
            .any(|c| matches!(c, Content::ImageUrl { .. }))
    });
    if !blind || !has_images {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            m.content = m
                .content
                .into_iter()
                .map(|c| match c {
                    Content::ImageUrl { .. } => {
                        Content::text("[image non transmise : ce modèle ne lit pas les images]")
                    }
                    other => other,
                })
                .collect();
            m
        })
        .collect()
}

fn humanise_llm_error(e: &LlmError) -> String {
    let msg = &e.message;
    match e.kind {
        LlmErrorKind::Auth => format!(
            "le provider refuse la clé ou la demande ({msg}). Vérifier la clé : \
             `penelope secret set openrouter_api_key`"
        ),
        LlmErrorKind::UnknownModel => format!(
            "modèle inconnu du provider ({msg}). Changer de modèle : \
             `penelope model set main openrouter:<identifiant>`"
        ),
        LlmErrorKind::PaymentRequired => format!(
            "crédits OpenRouter épuisés ou plafond de la clé atteint ({msg}). \
             Recharger : https://openrouter.ai/settings/credits"
        ),
        LlmErrorKind::RateLimited => match e.retry_after {
            Some(s) => format!("limite de débit du provider ({msg}), réessayer dans {s} s"),
            None => format!("limite de débit du provider ({msg}), réessayer dans un instant"),
        },
        LlmErrorKind::ContentFilter => {
            format!("le provider a refusé la demande (filtre de contenu : {msg})")
        }
        LlmErrorKind::ContextLength => format!(
            "la conversation dépasse la fenêtre du modèle ({msg}). `/compact` résume les \
             anciens échanges, `/new` repart d'une session vide"
        ),
        LlmErrorKind::Transient => format!("provider indisponible pour l'instant ({msg})"),
        _ => e.to_string(),
    }
}

pub(crate) fn server_of(tool: &str) -> Option<String> {
    tool.strip_prefix("mcp__")
        .and_then(|rest| rest.split("__").next())
        .map(String::from)
}

pub(crate) fn effect_kind(tool: &str) -> EffectKind {
    if tool.starts_with("mcp__") {
        return EffectKind::Mcp;
    }
    match tool {
        "shell_exec" => EffectKind::Shell,
        "http_fetch" => EffectKind::Http,
        t if t.starts_with("git_") => EffectKind::Git,
        t if t.starts_with("fs_") => EffectKind::Fs,
        "send_message" | "send_file" => EffectKind::Telegram,
        _ => EffectKind::Tool,
    }
}

#[cfg(test)]
mod tests;
