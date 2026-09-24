//! Résolution des appels en attente : décisions, puis exécution, dans l'ordre.

use super::*;

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
    #[allow(clippy::too_many_lines)] // gel 0.17 : lot G (agent/pending.rs)
    pub(super) async fn resolve_pending(
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
                // L'appel qui demande l'arrière-plan sort du tour ici, et **seulement**
                // ici : l'effet est planifié avant, le job le passe `dispatching` puis le
                // clôt (§4.2, issue #204). Rien ne contourne le ledger.
                if let Some(outcome) = crate::tool_jobs::maybe_spawn(
                    s,
                    execute,
                    crate::tool_jobs::JobRequest {
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
