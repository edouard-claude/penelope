//! Lecture d'un flux SSE de réponse et collecte en réponse complète.

use super::*;

pub(super) fn map_reqwest_error(e: reqwest::Error) -> LlmError {
    // Délai dépassé et connexion refusée sont tous deux transitoires : on réessaie.
    let kind = if e.is_timeout() || e.is_connect() {
        LlmErrorKind::Transient
    } else {
        LlmErrorKind::Other
    };
    LlmError::new(kind, e.to_string())
}

/// Intervalle de vérification de l'annulation pendant un flux silencieux.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Silence toléré par défaut pendant un flux (issue #51).
pub const DEFAULT_STREAM_IDLE: std::time::Duration = std::time::Duration::from_secs(120);

/// Fenêtre par défaut d'un modèle servi par un endpoint OpenAI-compatible.
pub const DEFAULT_LOCAL_WINDOW: u64 = 32_768;

/// Fenêtre d'un modèle local : ce que `GET /models` en dit (vLLM, llama.cpp, LM Studio),
/// sinon la valeur configurée (issue #53).
pub(super) fn local_window(m: &Value, configured: u64) -> u64 {
    for path in [
        "context_length",
        "max_model_len",
        "max_context_length",
        "n_ctx",
    ] {
        for value in [m.get(path), m.get("meta").and_then(|x| x.get(path))] {
            if let Some(n) = value.and_then(|v| v.as_u64()).filter(|n| *n > 0) {
                return n;
            }
        }
    }
    configured
}

/// Transforme une réponse HTTP en flux de fragments.
///
/// Les en-têtes sont déjà reçus à ce stade : l'appel passe en `response_started` (§4.3).
pub(crate) async fn stream_from_response(
    resp: reqwest::Response,
    cancel: CancelToken,
    provider: String,
    idle: std::time::Duration,
    mut acc: Box<dyn EventAccumulator>,
) -> Result<ChunkStream> {
    let status = resp.status().as_u16();
    if status >= 400 {
        // 429 et 503 peuvent porter `Retry-After` (secondes).
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = resp.text().await.unwrap_or_default();
        let mut e = LlmError::from_status(status, &body);
        e.retry_after = retry_after;
        // Un 5xx après envoi complet peut avoir été facturé.
        if status >= 500 {
            e = e.billed();
        }
        return Err(e);
    }

    let (tx, rx) = mpsc::channel::<StreamChunk>(64);
    tokio::spawn(async move {
        let mut decoder = SseDecoder::new();
        let mut bytes = resp.bytes_stream();
        let mut finished = false;
        let mut last_data = std::time::Instant::now();

        loop {
            // L'annulation est vérifiée même quand rien n'arrive (modèle qui réfléchit) :
            // couper la connexion arrête la génération, et sa facturation, chez les
            // providers qui le supportent.
            let next = match tokio::time::timeout(CANCEL_POLL, bytes.next()).await {
                Err(_) => {
                    if cancel.is_cancelled() {
                        let _ = tx
                            .send(StreamChunk::Done {
                                finish: FinishReason::Cancelled,
                            })
                            .await;
                        return;
                    }
                    // Fournisseur muet après ses en-têtes : on coupe et on laisse la
                    // relance jouer, plutôt que d'attendre le délai global (issue #51).
                    if !idle.is_zero() && last_data.elapsed() >= idle {
                        let _ = tx
                            .send(StreamChunk::Error {
                                message: format!(
                                    "flux muet ({provider}) : aucune donnée depuis {} s",
                                    idle.as_secs()
                                ),
                                retryable: true,
                                error_type: None,
                            })
                            .await;
                        return;
                    }
                    continue;
                }
                Ok(None) => break,
                Ok(Some(next)) => next,
            };
            // Tout octet reçu, commentaire SSE compris, prouve que le flux vit.
            last_data = std::time::Instant::now();
            if cancel.is_cancelled() {
                let _ = tx
                    .send(StreamChunk::Done {
                        finish: FinishReason::Cancelled,
                    })
                    .await;
                return;
            }
            let chunk = match next {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx
                        .send(StreamChunk::Error {
                            message: format!("flux interrompu ({provider}) : {e}"),
                            retryable: true,
                            error_type: None,
                        })
                        .await;
                    return;
                }
            };
            for payload in decoder.push(&chunk) {
                for out in acc.push_payload(&payload) {
                    if matches!(out, StreamChunk::Done { .. }) {
                        finished = true;
                    }
                    if tx.send(out).await.is_err() {
                        return; // le consommateur est parti
                    }
                }
            }
        }

        if !finished {
            // Flux coupé avant sa fin : à l'accumulateur de dire ce que ça vaut.
            for out in acc.on_eof() {
                if tx.send(out).await.is_err() {
                    return;
                }
            }
        }
    });

    Ok(rx)
}

/// Consomme un flux jusqu'au bout et reconstruit une réponse complète.
///
/// Utilisé par les appels non interactifs (classifieur, résumé, sub-agents).
pub async fn collect_stream(
    rx: ChunkStream,
    model: &str,
    provider: &str,
    catalog: &Catalog,
) -> Result<ChatResponse> {
    collect_stream_observed(rx, model, provider, catalog, &|_| {}).await
}

/// Comme [`collect_stream`], mais chaque fragment est aussi présenté à `observe` au
/// moment où il arrive : c'est ce qui permet d'afficher la réponse pendant qu'elle
/// s'écrit (brouillons Telegram, `penelope chat`) sans lire le flux deux fois.
pub async fn collect_stream_observed(
    mut rx: ChunkStream,
    model: &str,
    provider: &str,
    catalog: &Catalog,
    observe: &(dyn Fn(&StreamChunk) + Send + Sync),
) -> Result<ChatResponse> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut reasoning_parts: Vec<Value> = Vec::new();
    let mut refusal = String::new();
    let mut images: Vec<String> = Vec::new();
    let mut calls = Vec::new();
    let mut usage = Usage::default();
    let mut finish = FinishReason::Stop;
    let mut id = String::new();
    let mut actual_model = model.to_string();
    let mut upstream = None;
    let mut native_finish = None;

    while let Some(c) = rx.recv().await {
        observe(&c);
        match c {
            StreamChunk::Started { id: i, model: m } => {
                id = i;
                if !m.is_empty() {
                    actual_model = m;
                }
            }
            StreamChunk::Delta { text: t } => text.push_str(&t),
            StreamChunk::Reasoning { text: t } => reasoning.push_str(&t),
            StreamChunk::ReasoningDetails(v) => reasoning_parts.push(v),
            StreamChunk::Refusal { text: t } => refusal.push_str(&t),
            StreamChunk::Image { url } => images.push(url),
            StreamChunk::Meta {
                upstream: u,
                native_finish: n,
            } => {
                upstream = u.or(upstream);
                native_finish = n.or(native_finish);
            }
            StreamChunk::ToolCall(tc) => calls.push(tc),
            StreamChunk::Usage(u) => usage = u,
            StreamChunk::Done { finish: f } => finish = f,
            StreamChunk::Error {
                message,
                retryable,
                error_type,
            } => {
                return Err(LlmError::mid_stream(message, retryable, error_type));
            }
        }
    }

    // Le coût facturé par OpenRouter fait foi ; à défaut, estimation du catalogue.
    let (cost, cost_estimated) = match usage.cost_usd {
        Some(c) => (c, false),
        None => (
            catalog
                .get(&actual_model)
                .map(|i| i.cost(&usage))
                .unwrap_or(0.0),
            true,
        ),
    };
    let mut content = if text.is_empty() {
        Vec::new()
    } else {
        vec![Content::text(text)]
    };
    content.extend(
        images
            .into_iter()
            .map(|url| Content::ImageUrl { url, detail: None }),
    );
    let message = ChatMessage {
        role: Role::Assistant,
        content,
        tool_calls: calls,
        tool_call_id: None,
        name: None,
        cache_marker: false,
        reasoning: (!reasoning.is_empty()).then(|| reasoning.clone()),
        reasoning_details: crate::sse::merge_reasoning_details(&reasoning_parts),
    };

    Ok(ChatResponse {
        id,
        model: actual_model,
        provider: provider.to_string(),
        message,
        finish,
        usage,
        cost_usd: cost,
        cost_estimated,
        reasoning,
        upstream,
        native_finish,
        refusal: (!refusal.is_empty()).then_some(refusal),
    })
}
