//! Un tour, itération par itération.

use super::attempts::{call_record, of_response};
use super::*;
use penelope_app::attempts::{Attempt, AttemptCause};
use penelope_kernel::journal::CallRecord;

impl AgentLoop {
    /// Les itérations d'un tour, entre ses bornes (`turn_log`).
    pub(super) async fn run_steps(
        &self,
        spec: &TurnSpec,
        prefix: Option<&penelope_app::conversation::PromptPrefix>,
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
        // Les tentatives sans réponse du tour, hors historique (#206).
        let attempts = Attempts::default();
        // Un dépassement de fenêtre prouvé a droit à une compaction, pas davantage.
        let mut overflow_compacted = false;
        // Messages du propriétaire arrivés pendant le tour (§3.4).
        let steering = Steering::new(self.inbox.as_deref());

        for iteration in 0..self.max_iterations {
            if spec.cancel.is_cancelled() {
                // Les appels que ce tour vient de demander ne partiront pas (§3.4) ; au
                // premier passage, ceux du transcript appartiennent à une reprise.
                if iteration > 0 {
                    self.close_pending(conv, sink, NOT_RUN_STOPPED).await?;
                }
                return Ok(TurnOutcome::Cancelled);
            }

            // 1. Appels en attente : premier passage ou reprise, même chemin.
            match self
                .resolve_pending(spec, conv, execute, sink, &mut detector, &steering)
                .await?
            {
                Pending::Stop(outcome) => return Ok(outcome),
                Pending::Loop {
                    report,
                    tool,
                    last_result,
                } => {
                    return self
                        .answer_after_loop(
                            spec,
                            conv,
                            report,
                            &tool,
                            last_result.as_deref(),
                            &attempts,
                        )
                        .await;
                }
                Pending::Nothing | Pending::Resolved => {}
            }
            // 2. Gardes, dans l'ordre fixe : arrêt demandé, budget vérifié **avant** chaque
            // appel et pas après coup, puis le tour déjà long, reprises après approbation
            // comprises : plafond d'appels et point de contrôle de coût (issue #19).
            let cx = TurnContext {
                agent: self,
                spec,
                sink,
                iteration,
                cost,
            };
            if let Some(stop) = run_guards(&default_chain(), &cx).await? {
                return Ok(stop);
            }

            // 3. Appel du modèle, avec repli sur panne transitoire. Ce qui est arrivé
            // pendant le tour est réclamé et écrit d'abord : la lecture n'a pas d'effet.
            let steers = steering.claim(Checkpoint::BeforeModelCall).await?;
            steering.record(s, spec, conv, &steers).await?;
            let mut messages = steering.with_merge_note(conv.request_messages().await?);
            // Le tour précédent a été arrêté pendant ses outils : le modèle le sait, par
            // une note après le dernier message utilisateur, hors du préfixe (§3.4).
            if let Some(note) = interruption_note(&messages) {
                note.apply(&mut messages);
            }
            if spec.cancel.is_cancelled() {
                return Ok(TurnOutcome::Cancelled);
            }
            // Relance vue du modèle seulement : la consigne est dans le `conv.attempt` de la
            // réponse vide, pas dans l'historique ; le pliage l'ajoute de la même façon.
            if let Some(prompt) = attempts.retry_prompt() {
                messages.push(ChatMessage::user(prompt));
            }
            attempts.at_step(iteration + 1);
            // Empreinte et fournisseur amont collant : le cache de préfixe reste chaud et
            // un raté est expliqué (issue #17).
            let previous = s.budget.previous_call(&spec.session_id).await?;
            let pinned = sticky_upstream(previous.as_ref(), &spec.model_id, s.clock.now_ms());
            let fingerprint = Fingerprint::of(&messages, &spec.tools);
            let response = match self
                .call_model(spec, messages, sink, pinned, None, &attempts)
                .await?
            {
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
            if let Some(prefix) = prefix
                && let Err(e) = s.snapshots.record(&fingerprint.system_hash, prefix).await
            {
                tracing::warn!(error = %e, "instantané du prompt non enregistré");
            }
            self.record_usage(spec, previous.as_ref(), &fingerprint, &response)
                .await?;

            // Ce que l'appel dit de lui-même, pour le journal (T5).
            let prov = Provenance {
                turn: spec.turn_id.clone(),
                step: iteration + 1,
                call: Some(Box::new(CallRecord {
                    system_hash: Some(fingerprint.system_hash.clone()),
                    tools_hash: Some(fingerprint.tools_hash.clone()),
                    request_hash: fingerprint.request_hash(),
                    interrupted: response.finish == FinishReason::Cancelled,
                    ..call_record(&response)
                })),
                ..Default::default()
            };
            if response.finish == FinishReason::Cancelled {
                // Ce qui a été écrit avant l'arrêt reste dans le transcript.
                if !response.message.text().is_empty() {
                    conv.record_as(&response.message, false, &prov).await?;
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
                if let Some(failed) = self
                    .empty_answer(spec, &response, empty_retry, &attempts)
                    .await?
                {
                    return Ok(failed);
                }
                empty_retry = true;
                continue;
            }

            conv.record_as(&response.message, false, &prov).await?;
            // Une réponse écrite clôt la relance : le pliage efface la consigne au même
            // `conv.assistant`.
            attempts.set_retry_prompt(None);

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
                            penelope_kernel::budget::usd(turn_cost)
                        ));
                    }
                }
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

    /// L'appel entre au budget, avec son empreinte et la cause d'un raté de cache.
    async fn record_usage(
        &self,
        spec: &TurnSpec,
        previous: Option<&penelope_kernel::budget::PreviousCall>,
        fingerprint: &Fingerprint,
        response: &penelope_llm::types::ChatResponse,
    ) -> anyhow::Result<()> {
        let s = &self.services;
        let miss = miss_cause(
            previous,
            &Observed {
                fingerprint,
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
                s.snapshots
                    .prefix_cause(
                        previous.and_then(|p| p.system_hash.as_deref()),
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
        Ok(())
    }

    /// Ni texte ni appel d'outil : une relance au plus, puis une erreur qui dit pourquoi.
    /// `None` : relancer.
    async fn empty_answer(
        &self,
        spec: &TurnSpec,
        response: &penelope_llm::types::ChatResponse,
        empty_retry: bool,
        attempts: &Attempts,
    ) -> anyhow::Result<Option<TurnOutcome>> {
        let s = &self.services;
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
                TurnEventKind::EmptyAnswer
                    .draft(json!({
                        "model": response.model,
                        "upstream": response.upstream,
                        "generation_id": response.id,
                        "finish": format!("{:?}", response.finish),
                        "native_finish": response.native_finish,
                        "completion_tokens": response.usage.completion,
                        "reasoning_tokens": response.usage.reasoning,
                        "retried": empty_retry,
                    }))
                    .session(&spec.session_id),
            )
            .await?;
        let retry = !empty_retry && !reasoning_ate_budget;
        attempts
            .record(
                s,
                spec,
                Attempt {
                    retry_prompt: retry.then(|| EMPTY_RETRY_PROMPT.to_string()),
                    ..of_response(AttemptCause::EmptyAnswer, response)
                },
            )
            .await;
        if retry {
            attempts.set_retry_prompt(Some(EMPTY_RETRY_PROMPT));
            return Ok(None);
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
        Ok(Some(TurnOutcome::Failed {
            error: format!(
                "le modèle `{}`{upstream} n'a pas répondu : {cause}. Essayer un autre \
                 modèle (`/model main openrouter:<identifiant>`) ; `/models` montre le \
                 routage en vigueur",
                response.model
            ),
        }))
    }
}
