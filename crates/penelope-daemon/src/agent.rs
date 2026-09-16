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
    /// Arrêté par le détecteur de boucles, avec rapport.
    LoopAborted {
        report: String,
    },
    /// Annulé (bouton stop, `/stop`, annulation du run).
    Cancelled,
    /// Budget épuisé.
    BudgetExceeded {
        scope: String,
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
                s.approvals
                    .create(
                        ApprovalKind::BudgetExceeded,
                        exceeded.scope.as_str(),
                        RiskClass::Unknown,
                        json!({
                            "scope": exceeded.scope.as_str(),
                            "spent": exceeded.spent_usd,
                            "limit": exceeded.limit_usd,
                        }),
                        vec!["+50 %".into(), "Arrêter".into()],
                        Some(&spec.session_id),
                        spec.run_id.as_deref(),
                        false,
                    )
                    .await?;
                return Ok(TurnOutcome::BudgetExceeded {
                    scope: exceeded.scope.as_str().to_string(),
                });
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
            let response = match self.call_model(spec, messages, sink).await? {
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
            s.budget
                .record(penelope_kernel::budget::UsageRecord {
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
                let text = response.message.text();
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
        let mut waited = false;

        let mut attempt = 0;
        while attempt < candidates.len() {
            let model_id = &candidates[attempt];
            let request = ChatRequest {
                model: model_id.clone(),
                messages: fit_modalities(&messages, &s.catalog, model_id),
                tools: spec.tools.clone(),
                tool_choice: if spec.tools.is_empty() {
                    None
                } else {
                    Some(ToolChoice::Auto)
                },
                stream: true,
                session_id: Some(spec.session_id.clone()),
                fallback_models: if server_side_fallback {
                    spec.fallback_models.clone()
                } else {
                    Vec::new()
                },
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
                    model_id,
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
                    // `Retry-After` court : une seule attente, puis le même modèle.
                    if let Some(secs) = e.retry_after.filter(|s| *s <= RETRY_AFTER_MAX_SECS)
                        && !waited
                        && penelope_llm::Router::should_fallback(&e)
                    {
                        waited = true;
                        tracing::warn!(model = %model_id, secs, "limite de débit : nouvel essai");
                        if !sleep_unless_cancelled(&spec.cancel, secs).await {
                            return Ok(Err(CallFailure::plain("arrêt demandé")));
                        }
                        continue;
                    }
                    let more = attempt + 1 < candidates.len();
                    if more && penelope_llm::Router::should_fallback(&e) {
                        tracing::warn!(model = %model_id, error = %e, "repli sur le modèle suivant");
                        attempt += 1;
                        continue;
                    }
                    return Ok(Err(last_error));
                }
            };
            s.llm_state.response_started(&llm_id).await?;
            sink.emit(TurnEvent::Model {
                model_id: model_id.clone(),
            });

            let observe = |chunk: &StreamChunk| match chunk {
                StreamChunk::Delta { text } => sink.emit(TurnEvent::Delta(text.clone())),
                StreamChunk::Reasoning { text } => sink.emit(TurnEvent::Reasoning(text.clone())),
                _ => {}
            };
            match collect_stream_observed(
                stream,
                model_id,
                self.provider.name(),
                &s.catalog,
                &observe,
            )
            .await
            {
                Ok(r) => {
                    s.llm_state.completed(&llm_id).await?;
                    let requested = penelope_llm::catalog::strip_provider(model_id);
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
                    // Des fragments sont peut-être déjà partis : pas de repli silencieux.
                    return Ok(Err(CallFailure::from_llm(&e)));
                }
            }
        }
        Ok(Err(last_error))
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
                            return Ok(Pending::Stop(TurnOutcome::LoopAborted { report }));
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
                    let result = execute.execute(&call.name, &call.arguments).await;
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
            self.record_result(
                conv,
                sink,
                &call,
                !outcome.is_error,
                outcome.text.clone(),
                outcome.eager,
            )
            .await?;
        }
        Ok(Pending::Resolved)
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
                s.policies
                    .create_rule(
                        penelope_hitl::RuleScope::Tool,
                        Some(&a.subject),
                        server_of(&a.subject).as_deref(),
                        None,
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

fn effect_kind(tool: &str) -> EffectKind {
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
            TurnOutcome::LoopAborted { report } => {
                assert!(report.contains("fs_read"));
                assert!(report.contains("Appels du tour"));
            }
            other => panic!("{other:?}"),
        }
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
        assert!(matches!(out, TurnOutcome::BudgetExceeded { .. }), "{out:?}");
        assert_eq!(p.call_count(), 0, "le modèle n'est pas appelé");
        assert_eq!(s.approvals.pending(10).await.unwrap().len(), 1);
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

    #[tokio::test]
    async fn a_transient_failure_falls_back_to_the_next_model() {
        let (_d, s, p) = setup().await;
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
        assert_eq!(models, vec!["mock/model", "mock/repli"]);
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
