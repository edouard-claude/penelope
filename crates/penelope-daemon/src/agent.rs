//! Boucle d'agent : un tour, de bout en bout (§3.3, §4.2, §9).
//!
//! Invariants :
//! - tout effet non `readOnly` est **planifié dans le ledger avant** exécution ;
//! - un outil qui exige une approbation suspend **ce run seulement** ;
//! - le détecteur de boucles arrête le tour plutôt que de laisser tourner ;
//! - une erreur d'outil est renvoyée au modèle, pas au harnais.

use crate::runtime::Services;
use penelope_hitl::{ApprovalKind, Decision};
use penelope_kernel::effects::{EffectKind, EffectSpec, Planned};
use penelope_kernel::event::EventDraft;
use penelope_kernel::risk::{PolicyDecision, RiskClass};
use penelope_llm::provider::{CancelToken, Provider, collect_stream};
use penelope_llm::types::*;
use penelope_tools::{LoopDetector, LoopVerdict, ToolOutcome};
use serde_json::{Value, json};
use std::sync::Arc;

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

/// Un exécuteur de tour.
pub struct AgentLoop {
    pub services: Arc<Services>,
    pub provider: Arc<dyn Provider>,
    pub max_iterations: u32,
}

/// Ce dont un tour a besoin pour démarrer.
pub struct TurnRequest {
    pub session_id: String,
    pub run_id: Option<String>,
    pub user_text: String,
    pub model_id: String,
    pub tools: Vec<ToolDef>,
    pub system_prompt: String,
    /// Outils autorisés (liste blanche d'étape ou de skill) ; vide = tous.
    pub allowed_tools: Vec<String>,
    pub cancel: CancelToken,
}

impl AgentLoop {
    pub fn new(services: Arc<Services>, provider: Arc<dyn Provider>) -> Self {
        AgentLoop {
            services,
            provider,
            max_iterations: 24,
        }
    }

    /// Exécute un tour complet.
    pub async fn run(
        &self,
        req: TurnRequest,
        execute: &(dyn ToolExecutor + Send + Sync),
    ) -> anyhow::Result<TurnOutcome> {
        let s = &self.services;
        let cfg = s.config.config();
        let mut detector = LoopDetector::new(cfg.tools.loop_detector_repeats);
        let mut messages = vec![
            ChatMessage::system(req.system_prompt.clone()),
            ChatMessage::user(req.user_text.clone()),
        ];
        let mut cost = 0.0f64;

        s.events
            .append(
                EventDraft::new("turn.started", json!({"model": req.model_id}))
                    .session(&req.session_id),
            )
            .await?;

        for iteration in 0..self.max_iterations {
            if req.cancel.is_cancelled() {
                return Ok(TurnOutcome::Cancelled);
            }

            // Budget : vérifié **avant** chaque appel, pas après coup.
            let statuses = s
                .budget
                .status(&cfg.budget, Some(&req.session_id), req.run_id.as_deref())
                .await?;
            if let Some(exceeded) = statuses.iter().find(|b| b.exceeded) {
                let a = s
                    .approvals
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
                        Some(&req.session_id),
                        req.run_id.as_deref(),
                        false,
                    )
                    .await?;
                let _ = a;
                return Ok(TurnOutcome::BudgetExceeded {
                    scope: exceeded.scope.as_str().to_string(),
                });
            }

            let request = ChatRequest {
                model: req.model_id.clone(),
                messages: messages.clone(),
                tools: req.tools.clone(),
                tool_choice: Some(ToolChoice::Auto),
                stream: true,
                ..Default::default()
            };

            // Machine d'état des appels LLM (§4.3).
            let llm_id = format!("q_{}", penelope_kernel::ids::Ulid::new());
            let body = serde_json::to_value(&request).unwrap_or(Value::Null);
            s.llm_state
                .plan(
                    &llm_id,
                    Some(&req.session_id),
                    req.run_id.as_deref(),
                    &req.model_id,
                    self.provider.name(),
                    &body,
                )
                .await?;
            s.llm_state.dispatching(&llm_id).await?;

            let stream = match self.provider.chat_stream(request, req.cancel.clone()).await {
                Ok(st) => st,
                Err(e) => {
                    s.llm_state
                        .failed(&llm_id, &e.to_string(), e.maybe_billed)
                        .await?;
                    return Ok(TurnOutcome::Failed {
                        error: e.to_string(),
                    });
                }
            };
            s.llm_state.response_started(&llm_id).await?;

            let response =
                match collect_stream(stream, &req.model_id, self.provider.name(), &s.catalog).await
                {
                    Ok(r) => r,
                    Err(e) => {
                        s.llm_state
                            .failed(&llm_id, &e.to_string(), e.maybe_billed)
                            .await?;
                        return Ok(TurnOutcome::Failed {
                            error: e.to_string(),
                        });
                    }
                };
            s.llm_state.completed(&llm_id).await?;

            cost += response.cost_usd;
            s.budget
                .record(penelope_kernel::budget::UsageRecord {
                    session_id: Some(req.session_id.clone()),
                    run_id: req.run_id.clone(),
                    model: response.model.clone(),
                    provider: response.provider.clone(),
                    prompt: response.usage.prompt,
                    completion: response.usage.completion,
                    cached: response.usage.cached,
                    reasoning: response.usage.reasoning,
                    cost_usd: response.cost_usd,
                    estimated: response.cost_estimated,
                    ..Default::default()
                })
                .await?;

            if response.finish == FinishReason::Cancelled {
                return Ok(TurnOutcome::Cancelled);
            }

            // Pas d'appel d'outil : c'est la réponse finale.
            if response.message.tool_calls.is_empty() {
                let text = response.message.text();
                s.events
                    .append(
                        EventDraft::new(
                            "turn.finished",
                            json!({"iterations": iteration + 1, "cost_usd": cost}),
                        )
                        .session(&req.session_id),
                    )
                    .await?;
                return Ok(TurnOutcome::Answered {
                    text,
                    iterations: iteration + 1,
                    cost_usd: cost,
                });
            }

            messages.push(response.message.clone());

            for call in &response.message.tool_calls {
                // 1. Liste blanche de l'étape ou de la skill.
                if !penelope_tools::is_allowed(&call.name, &req.allowed_tools) {
                    messages.push(ChatMessage::tool_result(
                        &call.id,
                        &call.name,
                        format!("Refusé : `{}` n'est pas autorisé ici.", call.name),
                    ));
                    continue;
                }

                // 2. Détecteur de boucles.
                match detector.observe(&call.name, &call.arguments) {
                    LoopVerdict::Ok => {}
                    LoopVerdict::Warn(m) => {
                        messages.push(ChatMessage::tool_result(
                            &call.id,
                            &call.name,
                            format!("[avertissement du harnais] {m}"),
                        ));
                        continue;
                    }
                    LoopVerdict::Abort(m) => {
                        let report = format!("{m}\n\n{}", detector.report());
                        s.events
                            .append(
                                EventDraft::new("turn.loop_aborted", json!({"report": report}))
                                    .session(&req.session_id),
                            )
                            .await?;
                        return Ok(TurnOutcome::LoopAborted { report });
                    }
                }

                // 3. Politique et approbation.
                let risk = penelope_tools::effective_risk(&call.name, &Default::default());
                let verdict = s
                    .policies
                    .evaluate(
                        &cfg.mcp.policy,
                        &call.name,
                        server_of(&call.name).as_deref(),
                        &call.arguments,
                        risk,
                        req.run_id.as_deref(),
                        Some(&req.session_id),
                    )
                    .await?;

                match verdict.decision {
                    PolicyDecision::Deny => {
                        messages.push(ChatMessage::tool_result(
                            &call.id,
                            &call.name,
                            format!("Refusé par la politique : {}", verdict.reason),
                        ));
                        continue;
                    }
                    PolicyDecision::Ask | PolicyDecision::AskTwice => {
                        let approval = s
                            .approvals
                            .create(
                                ApprovalKind::ToolCall,
                                &call.name,
                                risk,
                                json!({
                                    "tool": call.name,
                                    "arguments": penelope_observe::redact_json(&call.arguments),
                                    "reason": verdict.reason,
                                    "double": verdict.decision == PolicyDecision::AskTwice,
                                }),
                                vec![
                                    "Autoriser".into(),
                                    "Pour ce run".into(),
                                    "Toujours".into(),
                                    "Refuser".into(),
                                ],
                                Some(&req.session_id),
                                req.run_id.as_deref(),
                                false,
                            )
                            .await?;
                        return Ok(TurnOutcome::AwaitingApproval {
                            approval_id: approval.id.0,
                        });
                    }
                    PolicyDecision::Auto => {}
                }

                // 4. Ledger d'effets **avant** exécution.
                let spec = EffectSpec::new(
                    effect_kind(&call.name),
                    call.name.clone(),
                    call.arguments.clone(),
                )
                .session(&req.session_id)
                .idempotent(
                    penelope_tools::tool_spec(&call.name)
                        .map(|s| s.idempotent)
                        .unwrap_or(false),
                );
                let spec = match &req.run_id {
                    Some(r) => spec.run(r),
                    None => spec,
                };

                let outcome = match s.effects.plan(spec).await? {
                    Planned::Replayed(v) => {
                        // Rejoué depuis le ledger : **jamais** ré-exécuté.
                        ToolOutcome::ok(v)
                    }
                    Planned::NeedsDecision(id) => {
                        messages.push(ChatMessage::tool_result(
                            &call.id,
                            &call.name,
                            format!(
                                "Cet appel a peut-être déjà eu lieu (effet {id}). \
                                 Une décision du propriétaire est requise avant de relancer."
                            ),
                        ));
                        continue;
                    }
                    Planned::InFlight(_) => {
                        messages.push(ChatMessage::tool_result(
                            &call.id,
                            &call.name,
                            "Appel déjà en cours.",
                        ));
                        continue;
                    }
                    Planned::Fresh(id) => {
                        s.effects.dispatching(&id).await?;
                        let result = execute.execute(&call.name, &call.arguments).await;
                        match &result {
                            Ok(o) if !o.is_error => {
                                s.effects.complete(&id, o.value.clone()).await?;
                            }
                            Ok(o) => {
                                s.effects.fail(&id, o.text.clone()).await?;
                            }
                            Err(e) => {
                                s.effects.fail(&id, e.to_string()).await?;
                            }
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
                            json!({
                                "tool": call.name,
                                "ok": !outcome.is_error,
                            }),
                        )
                        .session(&req.session_id),
                    )
                    .await?;

                messages.push(ChatMessage::tool_result(
                    &call.id,
                    &call.name,
                    outcome.text.clone(),
                ));
            }
        }

        Ok(TurnOutcome::Failed {
            error: format!(
                "le tour n'a pas convergé en {} itérations",
                self.max_iterations
            ),
        })
    }

    /// Reprend un tour après une décision d'approbation (§9.2).
    pub async fn resume_after_approval(
        &self,
        approval_id: &str,
        decision: &Decision,
    ) -> anyhow::Result<bool> {
        let s = &self.services;
        let approved = s.approvals.decide(approval_id, decision).await;
        match approved {
            Ok(a) => {
                // Une fenêtre « toujours » crée une règle visible et révocable.
                if decision.window.creates_rule() {
                    let tool = a.subject.clone();
                    s.policies
                        .create_rule(
                            penelope_hitl::RuleScope::Tool,
                            Some(&tool),
                            server_of(&tool).as_deref(),
                            None,
                            if decision.approved {
                                PolicyDecision::Auto
                            } else {
                                PolicyDecision::Deny
                            },
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
}

/// Exécution concrète d'un outil, fournie par le daemon (ou simulée en test).
#[async_trait::async_trait]
pub trait ToolExecutor {
    async fn execute(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError>;
}

fn server_of(tool: &str) -> Option<String> {
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
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        match loop_.run(request(&sid), &exec).await.unwrap() {
            TurnOutcome::Answered {
                text, iterations, ..
            } => {
                assert_eq!(text, "voici la réponse");
                assert_eq!(iterations, 1);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(exec.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn read_tools_run_without_approval() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({"path":"a.rs"}),
            }],
        ));
        p.reply("j'ai lu le fichier");
        let sid = session(&s).await;
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &exec)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        assert_eq!(exec.calls.load(Ordering::SeqCst), 1);
    }

    /// §9 : un outil `write` suspend le tour et crée une demande d'approbation.
    #[tokio::test]
    async fn write_tools_suspend_the_turn_for_approval() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command":"cargo build"}),
            }],
        ));
        let sid = session(&s).await;
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &exec)
            .await
            .unwrap();
        let id = match out {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            exec.calls.load(Ordering::SeqCst),
            0,
            "aucun effet avant approbation"
        );
        let pending = s.approvals.pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id.0, id);
    }

    #[tokio::test]
    async fn always_decision_creates_a_revocable_rule() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command":"cargo build"}),
            }],
        ));
        let sid = session(&s).await;
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let id = match loop_.run(request(&sid), &exec).await.unwrap() {
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
    async fn a_second_decision_does_not_win() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command":"x"}),
            }],
        ));
        let sid = session(&s).await;
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let loop_ = AgentLoop::new(s.clone(), p.clone());
        let id = match loop_.run(request(&sid), &exec).await.unwrap() {
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

        // Premier tour : l'outil s'exécute.
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({"path":"a.rs"}),
            }],
        ));
        p.reply("lu");
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &exec)
            .await
            .unwrap();
        assert_eq!(exec.calls.load(Ordering::SeqCst), 1);

        // Second tour identique : l'effet est rejoué depuis le ledger.
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({"path":"a.rs"}),
            }],
        ));
        p.reply("relu");
        AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &exec)
            .await
            .unwrap();
        assert_eq!(
            exec.calls.load(Ordering::SeqCst),
            1,
            "aucune seconde exécution"
        );
    }

    #[tokio::test]
    async fn tool_errors_go_back_to_the_model() {
        let (_d, s, p) = setup().await;
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({"path":"a.rs"}),
            }],
        ));
        p.reply("je vois l'erreur, je change d'approche");
        let sid = session(&s).await;
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: true,
        };
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &exec)
            .await
            .unwrap();
        match out {
            TurnOutcome::Answered { text, .. } => assert!(text.contains("change d'approche")),
            other => panic!("le tour doit continuer malgré l'erreur : {other:?}"),
        }
        // L'effet est marqué en échec, pas complété.
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
        for _ in 0..6 {
            p.push(Scripted::ToolCalls(
                String::new(),
                vec![ToolCall {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    arguments: json!({"path":"a.rs"}),
                }],
            ));
        }
        let sid = session(&s).await;
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &exec)
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
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        assert_eq!(
            AgentLoop::new(s.clone(), p.clone())
                .run(req, &exec)
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
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(request(&sid), &exec)
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
            vec![ToolCall {
                id: "c1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command":"x"}),
            }],
        ));
        p.reply("compris");
        let sid = session(&s).await;
        let mut req = request(&sid);
        req.allowed_tools = vec!["fs_*".into()];
        let exec = CountingExecutor {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let out = AgentLoop::new(s.clone(), p.clone())
            .run(req, &exec)
            .await
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        assert_eq!(exec.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            s.approvals.pending(10).await.unwrap().len(),
            0,
            "un outil hors liste blanche ne demande même pas d'approbation"
        );
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
}
