//! Résolution des appels en attente : décisions, puis exécution, dans l'ordre.

use super::*;

pub(super) mod decide;
pub(super) mod judge;
pub(super) mod policy;
pub(super) mod sample;

pub use decide::{CallContext, CallId};
use decide::{
    DescribedCall, GuardContext, GuardStop, Refusal, Suspension, call_chain, nested_ask,
    run_call_guards,
};
use judge::JudgeStep;
use policy::PolicyStage;
pub use policy::{ApprovalMode, declared_allow, local_draft_allow};
use sample::{Issue, Seen};

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
    "env_explore",
    "tool_search",
    "tool_describe",
];

/// Suite d'un appel, décidée avant toute exécution.
pub(crate) enum Step {
    /// Résultat connu sans exécuter : refus, avertissement du harnais.
    Record(ToolCall, String),
    /// À exécuter ; `parallel` : lecture autorisée d'office, qui peut partir avec ses
    /// voisines ; `id` : l'identité de l'appel pour le ledger et le jeu de décisions.
    Execute {
        call: ToolCall,
        info: CallInfo,
        parallel: bool,
        id: CallId,
    },
}

/// Ce qui arrête la liste des appels après l'exécution de ceux qui précèdent.
pub(crate) enum Terminal {
    Stop(TurnOutcome),
    Loop {
        call: ToolCall,
        tool: String,
        message: String,
    },
    /// Refus répété de l'exécuteur (#302) : `result` revient à l'appel, `answer` au
    /// propriétaire.
    Halt {
        call: ToolCall,
        result: String,
        answer: String,
    },
}

/// Ce que reçoivent les appels qui suivaient un refus répété (#302).
const NOT_RUN_HALTED: &str = "Non exécuté : tour arrêté sur un refus répété de l'exécuteur.";

/// Ce que la décision d'un appel ajoute à la liste.
pub(crate) enum Decided {
    Step(Step),
    Stop(Terminal),
}

pub(super) enum Pending {
    Nothing,
    Resolved,
    Stop(TurnOutcome),
    /// Boucle arrêtée : le tour répond encore, sans outil.
    Loop {
        report: String,
        tool: String,
        last_result: Option<String>,
    },
    /// Refus répété de l'exécuteur (#302) : la réponse est déjà écrite.
    Halt {
        answer: String,
    },
}

impl AgentLoop {
    /// Résout les appels d'outils sans résultat à la fin du transcript.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn resolve_pending(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        execute: &(dyn ToolExecutor + Send + Sync),
        sink: &dyn TurnSink,
        detector: &mut LoopDetector,
        steering: &Steering<'_>,
        shown: &ToolImages,
    ) -> anyhow::Result<Pending> {
        let s = &self.services;
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
        let mut cx = GuardContext {
            agent: self,
            spec,
            execute,
            detector: &mut *detector,
        };
        for mut call in pending {
            // `cd <workspace> && grep …` devient `grep …` dans ce répertoire (#123).
            if let Some(args) = execute.normalise_call(&call.name, &call.arguments) {
                call.arguments = args;
            }
            let info = execute.describe_call(&call.name, &call.arguments).await;
            let identity = self.call_identity(spec, conv, &tail, &call).await?;
            let context = CallContext::root(&identity);
            let described = DescribedCall {
                call,
                info,
                context,
            };
            match self
                .decide_call(spec, conv, sink, &mut cx, described)
                .await?
            {
                Decided::Step(step) => steps.push(step),
                Decided::Stop(stop) => {
                    terminal = Some(stop);
                    break;
                }
            }
        }

        // 2. Exécution et résultats, dans l'ordre des appels. Des lectures consécutives
        //    partent ensemble, par quatre ; toute autre chose attend qu'elles aient fini et
        //    forme une barrière (issue #85).
        let mut recorded = 0usize;
        let mut nudge = nudge;
        let mut steps = steps.into_iter().peekable();
        while let Some(step) = steps.next() {
            // `/stop` : ce qui n'est pas parti ne part plus, et le dit (§3.4).
            if spec.cancel.is_cancelled() {
                let rest = std::iter::once(step).chain(steps);
                self.skip_steps(conv, sink, rest, NOT_RUN_STOPPED).await?;
                return Ok(Pending::Stop(TurnOutcome::Cancelled));
            }
            // Un message du propriétaire arrivé pendant le lot : l'appel en cours a fini,
            // ceux qui ne sont pas partis ne partent plus, le modèle est rappelé avec le
            // message (§3.4). Pas quand le lot finit sur une carte ou une boucle : le
            // message attend alors le tour suivant, comme avant.
            if terminal.is_none() && matches!(step, Step::Execute { .. }) {
                let steers = steering.claim(Checkpoint::BetweenCalls).await?;
                if !steers.is_empty() {
                    let rest = std::iter::once(step).chain(steps);
                    recorded += self
                        .skip_steps(conv, sink, rest, NOT_RUN_NEW_MESSAGE)
                        .await?;
                    conv.admit_tool_results(recorded).await?;
                    steering.record(s, spec, conv, &steers).await?;
                    return Ok(Pending::Resolved);
                }
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
                    id,
                } => {
                    let mut batch = vec![(call, info, id)];
                    while let Some(Step::Execute { parallel: true, .. }) = steps.peek() {
                        if let Some(Step::Execute { call, info, id, .. }) = steps.next() {
                            batch.push((call, info, id));
                        }
                    }
                    for (call, _, _) in &batch {
                        sink.emit(TurnEvent::ToolCall {
                            name: call.name.clone(),
                            args: penelope_observe::redact_json(&call.arguments),
                        });
                    }
                    let mut outcomes: Vec<anyhow::Result<ToolOutcome>> = Vec::new();
                    for chunk in batch.chunks(PARALLEL_READS) {
                        let mut running = Vec::with_capacity(chunk.len());
                        for (call, info, id) in chunk {
                            running.push(self.run_effect(spec, execute, call, info, id));
                        }
                        outcomes.extend(futures::future::join_all(running).await);
                    }
                    for ((call, info, id), outcome) in batch.iter().zip(outcomes) {
                        let outcome = outcome?;
                        shown.note(execute, &outcome.value);
                        self.finish_call(spec, conv, sink, call, info, id, outcome, &mut nudge)
                            .await?;
                        recorded += 1;
                    }
                }
                Step::Execute { call, info, id, .. } => {
                    sink.emit(TurnEvent::ToolCall {
                        name: call.name.clone(),
                        args: penelope_observe::redact_json(&call.arguments),
                    });
                    let outcome = self.run_effect(spec, execute, &call, &info, &id).await?;
                    shown.note(execute, &outcome.value);
                    self.finish_call(spec, conv, sink, &call, &info, &id, outcome, &mut nudge)
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
                        TurnEventKind::LoopAborted
                            .draft(json!({"report": report}))
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
                self.close_pending(
                    conv,
                    sink,
                    "Non exécuté : tour arrêté par le détecteur de boucles.",
                )
                .await?;
                return Ok(Pending::Loop {
                    report,
                    tool,
                    last_result: last,
                });
            }
            Some(Terminal::Halt {
                call,
                result,
                answer,
            }) => {
                // Refus répété de l'exécuteur (#302) : le résultat de l'appel entre dans
                // la conversation, les appels qui suivaient sont fermés, et la réponse au
                // propriétaire est l'état connu, sans autre appel au modèle.
                s.events
                    .append(
                        TurnEventKind::Halted
                            .draft(json!({"tool": call.name, "answer": answer}))
                            .session(&spec.session_id),
                    )
                    .await?;
                tracing::warn!(session = %spec.session_id, tool = %call.name, "tour arrêté sur un refus répété");
                self.record_result(conv, sink, &call, false, result, false)
                    .await?;
                self.close_pending(conv, sink, NOT_RUN_HALTED).await?;
                let prov = Provenance {
                    turn: spec.turn_id.clone(),
                    ..Default::default()
                };
                conv.record_as(&ChatMessage::assistant(&answer), true, &prov)
                    .await?;
                return Ok(Pending::Halt { answer });
            }
        }
        conv.admit_tool_results(recorded).await?;
        Ok(Pending::Resolved)
    }

    /// Décide d'un appel, sans rien exécuter : gardes, politique, carte d'approbation.
    pub(crate) async fn decide_call(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        sink: &dyn TurnSink,
        cx: &mut GuardContext<'_>,
        described: DescribedCall,
    ) -> anyhow::Result<Decided> {
        let s = &self.services;
        let execute = cx.execute;

        // Gardes, dans l'ordre : liste blanche, décision déjà prise, boucles,
        // arguments. Une décision déjà prise par le propriétaire est définitive.
        match run_call_guards(&call_chain(), &described, cx).await? {
            None => {}
            Some(GuardStop::Refuse(refusal)) => {
                return Ok(Decided::Step(Step::Record(described.call, refusal.text())));
            }
            Some(GuardStop::Suspend(Suspension::Prior { approval_id })) => {
                return Ok(Decided::Stop(Terminal::Stop(
                    TurnOutcome::AwaitingApproval { approval_id },
                )));
            }
            Some(GuardStop::LoopAbort(message)) => {
                return Ok(Decided::Stop(Terminal::Loop {
                    tool: described.info.effective_name.clone(),
                    call: described.call,
                    message,
                }));
            }
            Some(GuardStop::Halt { result, answer }) => {
                return Ok(Decided::Stop(Terminal::Halt {
                    call: described.call,
                    result,
                    answer,
                }));
            }
            Some(GuardStop::Approved) => {
                return Ok(Decided::Step(Step::Execute {
                    call: described.call,
                    info: described.info,
                    parallel: false,
                    id: described.context.call_id,
                }));
            }
        }
        let DescribedCall {
            call,
            info,
            context,
        } = described;

        // 4. Politique et approbation, sur les arguments de l'outil visé : par
        // `tool_call`, ceux de l'appel interne (#110), sinon une règle
        // « Toujours » couvrirait l'outil entier.
        let effective_args = crate::effective_arguments(&call.name, &call.arguments);
        let policy_workspace = execute.policy_workspace();
        let verdict =
            PolicyStage::evaluate(s, spec, policy_workspace.as_deref(), &info, &effective_args)
                .await?;

        if let Some(refusal) = nested_ask(&context, verdict.decision, &verdict.reason) {
            return Ok(Decided::Step(Step::Record(call, refusal.text())));
        }
        // 5. Juge (#203) : seulement sur une carte `shell_exec` sans motif possible. Il
        // enrichit la carte, ou la retire quand un contrôle déterministe le confirme.
        let stage = self
            .judge_stage(
                spec,
                policy_workspace.as_deref(),
                &info,
                &effective_args,
                &verdict,
            )
            .await?;
        // Ce que le jeu de décisions garde de l'appel (#233), quelle que soit l'issue.
        let seen = Seen {
            call_id: &context.call_id.0,
            workspace: policy_workspace.as_deref(),
            info: &info,
            args: &effective_args,
            verdict: &verdict,
            judge: stage.sample.as_ref(),
        };
        let judged = match stage.step {
            JudgeStep::Auto(v) => {
                tracing::info!(session = %spec.session_id, layer = ?v.layer, reason = %v.reason, "appel autorisé sans carte");
                self.sample_call(spec, &seen, Issue::Auto).await;
                return Ok(Decided::Step(Step::Execute {
                    call,
                    info,
                    parallel: false,
                    id: context.call_id,
                }));
            }
            JudgeStep::Card(judged) => judged,
        };
        match verdict.decision {
            PolicyDecision::Deny => {
                self.sample_call(spec, &seen, Issue::Denied).await;
                return Ok(Decided::Step(Step::Record(
                    call,
                    Refusal::Policy {
                        reason: verdict.reason,
                    }
                    .text(),
                )));
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
                            "call_id": context.call_id.0,
                            "turn_id": spec.turn_id,
                            "judged": judged,
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
                // Après la carte, hors de son chemin : l'issue viendra de sa décision.
                self.sample_call(spec, &seen, Issue::Card(approval.id.0.clone()))
                    .await;
                return Ok(Decided::Stop(Terminal::Stop(
                    TurnOutcome::AwaitingApproval {
                        approval_id: approval.id.0,
                    },
                )));
            }
            PolicyDecision::Auto => self.sample_call(spec, &seen, Issue::Auto).await,
        }
        // Lecture pure autorisée d'office : elle peut partir avec ses voisines.
        let parallel =
            info.risk == RiskClass::Read && PARALLEL_SAFE.contains(&info.effective_name.as_str());
        Ok(Decided::Step(Step::Execute {
            call,
            info,
            parallel,
            id: context.call_id,
        }))
    }

    /// Identité d'un appel pour la carte d'approbation, le ledger d'effets et le jeu de
    /// décisions : l'identifiant émis par le modèle, sauf s'il a déjà servi dans la queue
    /// de la session (#266). Il est alors ancré au message qui le porte, persistant et le
    /// même d'une reprise à l'autre, et l'écart est journalisé. Sans cela, un fournisseur
    /// qui numérote ses appels de façon constante faisait rejouer le second appel
    /// identique depuis le premier résultat, et couvrir un appel par l'approbation d'un
    /// autre. Le transcript garde l'identifiant émis : le résultat répond à l'appel tel
    /// que le fournisseur l'a nommé.
    async fn call_identity(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        tail: &[ChatMessage],
        call: &ToolCall,
    ) -> anyhow::Result<String> {
        if !crate::pending::seen_before(tail, &call.id) {
            return Ok(call.id.clone());
        }
        let anchor = conv
            .pending_anchor()
            .await?
            .ok_or_else(|| anyhow::anyhow!("appel en attente sans message qui le porte"))?;
        let identity = format!("{}@{anchor}", call.id);
        tracing::warn!(
            session = %spec.session_id, call_id = %call.id, %identity, tool = %call.name,
            "identifiant d'appel déjà vu dans la session : renuméroté"
        );
        self.services
            .events
            .append(
                TurnEventKind::CallIdReused
                    .draft(json!({
                        "call_id": call.id,
                        "identity": identity,
                        "tool": call.name,
                    }))
                    .session(&spec.session_id),
            )
            .await?;
        Ok(identity)
    }

    /// Clôt les appels qui ne partiront pas : un résultat pour chacun, pour que le
    /// transcript reste complet sans réparation à la projection (§3.4). Un refus déjà
    /// décidé garde son texte ; un appel à exécuter reçoit `why`.
    async fn skip_steps(
        &self,
        conv: &dyn Conversation,
        sink: &dyn TurnSink,
        steps: impl Iterator<Item = Step>,
        why: &str,
    ) -> anyhow::Result<usize> {
        let mut recorded = 0;
        for step in steps {
            let (call, text) = match step {
                Step::Record(call, text) => (call, text),
                Step::Execute { call, .. } => (call, why.to_string()),
            };
            self.record_result(conv, sink, &call, false, text, false)
                .await?;
            recorded += 1;
        }
        Ok(recorded)
    }

    /// Clôt les appels sans résultat en fin de transcript, chacun avec `why`.
    pub(super) async fn close_pending(
        &self,
        conv: &dyn Conversation,
        sink: &dyn TurnSink,
        why: &str,
    ) -> anyhow::Result<()> {
        for rest in pending_calls(&conv.tail().await?) {
            self.record_result(conv, sink, &rest, false, why.to_string(), false)
                .await?;
        }
        Ok(())
    }

    /// Ledger d'effets **avant** exécution, puis l'outil (§4.2) : jamais ré-exécuté s'il
    /// est déjà fait ; un effet incertain attend la décision du propriétaire.
    async fn run_effect(
        &self,
        spec: &TurnSpec,
        execute: &(dyn ToolExecutor + Send + Sync),
        call: &ToolCall,
        info: &CallInfo,
        id: &CallId,
    ) -> anyhow::Result<ToolOutcome> {
        let s = &self.services;
        let spec_effect = EffectSpec::new(
            effect_kind(&info.effective_name),
            info.effective_name.clone(),
            call.arguments.clone(),
        )
        .session(&spec.session_id)
        .step(&id.0)
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
                // L'appel qui demande l'arrière-plan sort du tour ici, et **seulement**
                // ici : l'effet est planifié avant, le job le passe `dispatching` puis le
                // clôt (§4.2, issue #204). Rien ne contourne le ledger.
                if let Some(outcome) = s
                    .jobs
                    .maybe_spawn(
                        execute,
                        JobRequest {
                            session_id: &spec.session_id,
                            run_id: spec.run_id.as_deref(),
                            turn_id: spec.turn_id.as_deref(),
                            call,
                            tool: &info.effective_name,
                            effect: &id,
                        },
                    )
                    .await?
                {
                    return Ok(outcome);
                }
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
        id: &CallId,
        outcome: ToolOutcome,
        nudge: &mut Option<String>,
    ) -> anyhow::Result<()> {
        self.sample_execution(spec, &id.0, info, &outcome).await;
        penelope_observe::metrics::counter_inc(
            "penelope_tool_calls_total",
            &[
                ("tool", info.effective_name.as_str()),
                ("ok", if outcome.is_error { "false" } else { "true" }),
            ],
            1.0,
        );
        let mut payload = json!({
            "tool": info.effective_name,
            "ok": !outcome.is_error,
            // Forme de la ligne, pour mesurer la consigne « une commande par appel »
            // (issue #150). La commande elle-même n'est pas journalée ici : seule sa
            // forme l'est.
            "shape": line_shape(&info.effective_name, &call.arguments),
        });
        // Le style du rappel de délégation que ce résultat porte (#291), pour la mesure.
        if nudge.is_some() {
            payload["nudge_style"] = json!(
                self.services
                    .config
                    .config()
                    .budget
                    .delegation_nudge_style
                    .as_str()
            );
        }
        self.services
            .events
            .append(
                TurnEventKind::ToolResult
                    .draft(payload)
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
        let prov = Provenance {
            ok: Some(ok),
            ..Default::default()
        };
        conv.record_as(
            &ChatMessage::tool_result(&call.id, &call.name, text),
            eager,
            &prov,
        )
        .await?;
        sink.emit(TurnEvent::ToolResult {
            name: call.name.clone(),
            ok,
            preview,
        });
        Ok(())
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

pub fn server_of(tool: &str) -> Option<String> {
    tool.strip_prefix("mcp__")
        .and_then(|rest| rest.split("__").next())
        .map(String::from)
}

pub fn effect_kind(tool: &str) -> EffectKind {
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
