//! Résolution des appels en attente : décisions, puis exécution, dans l'ordre.

use super::*;

pub(super) mod decide;
pub(super) mod policy;

use decide::{
    CallContext, DescribedCall, GuardStop, Refusal, Suspension, call_chain, run_call_guards,
};
use policy::PolicyStage;
pub use policy::{ApprovalMode, declared_allow, local_draft_allow};

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

/// Ce que la décision d'un appel ajoute à la liste.
enum Decided {
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
}

impl AgentLoop {
    /// Résout les appels d'outils sans résultat à la fin du transcript.
    pub(super) async fn resolve_pending(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        execute: &(dyn ToolExecutor + Send + Sync),
        sink: &dyn TurnSink,
        detector: &mut LoopDetector,
        steering: &Steering<'_>,
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
        let mut cx = CallContext {
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
            let described = DescribedCall { call, info };
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
        }
        conv.admit_tool_results(recorded).await?;
        Ok(Pending::Resolved)
    }

    /// Décide d'un appel, sans rien exécuter : gardes, politique, carte d'approbation.
    async fn decide_call(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        sink: &dyn TurnSink,
        cx: &mut CallContext<'_>,
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
            Some(GuardStop::Approved) => {
                return Ok(Decided::Step(Step::Execute {
                    call: described.call,
                    info: described.info,
                    parallel: false,
                }));
            }
        }
        let DescribedCall { call, info } = described;

        // 4. Politique et approbation, sur les arguments de l'outil visé : par
        // `tool_call`, ceux de l'appel interne (#110), sinon une règle
        // « Toujours » couvrirait l'outil entier.
        let effective_args = crate::effective_arguments(&call.name, &call.arguments);
        let policy_workspace = execute.policy_workspace();
        let verdict =
            PolicyStage::evaluate(s, spec, policy_workspace.as_deref(), &info, &effective_args)
                .await?;

        match verdict.decision {
            PolicyDecision::Deny => {
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
                return Ok(Decided::Stop(Terminal::Stop(
                    TurnOutcome::AwaitingApproval {
                        approval_id: approval.id.0,
                    },
                )));
            }
            PolicyDecision::Auto => {}
        }
        // Lecture pure autorisée d'office : elle peut partir avec ses voisines.
        let parallel =
            info.risk == RiskClass::Read && PARALLEL_SAFE.contains(&info.effective_name.as_str());
        Ok(Decided::Step(Step::Execute {
            call,
            info,
            parallel,
        }))
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
                TurnEventKind::ToolResult
                    .draft(json!({
                        "tool": info.effective_name,
                        "ok": !outcome.is_error,
                        // Forme de la ligne, pour mesurer la consigne « une commande par
                        // appel » (issue #150). La commande elle-même n'est pas journalée
                        // ici : seule sa forme l'est.
                        "shape": line_shape(&info.effective_name, &call.arguments),
                    }))
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
