//! Appel du modèle : flux, nouvelles tentatives, replis, erreurs lisibles.

use super::*;

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

impl AgentLoop {
    /// Appelle le modèle en diffusant les fragments ; essaie les replis sur panne.
    ///
    /// Chez OpenRouter, les replis partent dans la requête (`models`) : OpenRouter bascule
    /// lui-même avant le premier jeton, y compris quand la panne survient après le 200.
    /// Ailleurs, ils sont essayés ici, sur les erreurs d'avant flux.
    #[allow(clippy::too_many_lines)] // gel 0.17 : appel du modèle et replis, lot G (agent/model_call.rs)
    pub(super) async fn call_model(
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
