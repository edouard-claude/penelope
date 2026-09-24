//! Un tour, itération par itération.

use super::*;

impl AgentLoop {
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
}
