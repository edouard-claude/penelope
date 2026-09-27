//! Réponse : accumulation des événements `response.*`.

use super::*;

/// Accumule les événements `response.*` d'un flux Responses.
pub struct ResponsesAccumulator {
    /// Modèle réellement servi, lu dans l'en-tête `openai-model`.
    served_model: String,
    started: bool,
    saw_tool_call: bool,
    completed: bool,
    finish: Option<FinishReason>,
    quota: Arc<Mutex<Quota>>,
    sink: Option<Arc<dyn QuotaSink>>,
}

impl ResponsesAccumulator {
    pub fn new(served_model: String, quota: Arc<Mutex<Quota>>) -> Self {
        ResponsesAccumulator {
            served_model,
            started: false,
            saw_tool_call: false,
            completed: false,
            finish: None,
            quota: quota.clone(),
            sink: None,
        }
    }

    /// Publie les jauges lues en cours de flux (`codex.rate_limits`).
    pub fn with_quota_sink(mut self, sink: Option<Arc<dyn QuotaSink>>) -> Self {
        self.sink = sink;
        self
    }

    /// Accumulateur nu, pour les tests et les appels sans jauge partagée.
    pub fn plain() -> Self {
        Self::new(String::new(), Arc::new(Mutex::new(Quota::default())))
    }

    fn start(&mut self, v: &Value, out: &mut Vec<StreamChunk>) {
        if self.started {
            return;
        }
        self.started = true;
        let resp = v.get("response");
        // Le modèle réellement servi est celui de l'en-tête `openai-model` ; celui de
        // l'événement ne vient qu'à défaut (issue #142, repli côté serveur).
        let model = if self.served_model.is_empty() {
            resp.and_then(|r| r.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string()
        } else {
            self.served_model.clone()
        };
        out.push(StreamChunk::Started {
            id: resp
                .and_then(|r| r.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string(),
            model,
        });
    }
}

impl EventAccumulator for ResponsesAccumulator {
    fn push_payload(&mut self, data: &str) -> Vec<StreamChunk> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return vec![];
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        let mut out: Vec<StreamChunk> = Vec::new();
        let text_of = |key: &str| {
            v.get(key)
                .and_then(|d| d.as_str())
                .filter(|d| !d.is_empty())
                .map(String::from)
        };
        match kind {
            "response.created" => self.start(&v, &mut out),
            "response.output_text.delta" => {
                self.start(&v, &mut out);
                if let Some(text) = text_of("delta") {
                    out.push(StreamChunk::Delta { text });
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.start(&v, &mut out);
                if let Some(text) = text_of("delta") {
                    out.push(StreamChunk::Reasoning { text });
                }
            }
            // Un appel de fonction arrive **complet** ici : Codex ignore les deltas
            // d'arguments, et l'identifiant est celui du serveur (issue #54).
            "response.output_item.done" => {
                self.start(&v, &mut out);
                let Some(item) = v.get("item") else {
                    return out;
                };
                match item
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                {
                    "function_call" => {
                        let name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|i| i.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if !name.is_empty() && !call_id.is_empty() {
                            self.saw_tool_call = true;
                            out.push(StreamChunk::ToolCall(ToolCall {
                                id: call_id,
                                name,
                                arguments: crate::sse::parse_arguments(
                                    item.get("arguments")
                                        .and_then(|a| a.as_str())
                                        .unwrap_or("{}"),
                                ),
                            }));
                        }
                    }
                    // Le raisonnement chiffré repart tel quel au tour suivant.
                    "reasoning" if item.get("encrypted_content").is_some() => {
                        out.push(StreamChunk::ReasoningDetails(json!([item])));
                    }
                    _ => {}
                }
            }
            "response.completed" => {
                self.start(&v, &mut out);
                self.completed = true;
                if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                    out.push(StreamChunk::Usage(parse_responses_usage(u)));
                }
                let finish = self.finish.unwrap_or(if self.saw_tool_call {
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
                });
                out.push(StreamChunk::Done { finish });
            }
            "response.incomplete" => {
                self.start(&v, &mut out);
                self.completed = true;
                let reason = v
                    .get("response")
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or_default()
                    .to_string();
                if reason == "max_output_tokens" {
                    if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                        out.push(StreamChunk::Usage(parse_responses_usage(u)));
                    }
                    out.push(StreamChunk::Done {
                        finish: FinishReason::Length,
                    });
                } else {
                    out.push(StreamChunk::Error {
                        message: format!("réponse incomplète ({reason})"),
                        retryable: true,
                        error_type: (!reason.is_empty()).then_some(reason),
                    });
                }
            }
            "response.failed" => {
                self.completed = true;
                let err = v
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .or_else(|| v.get("error"));
                let error_type = err
                    .and_then(|e| e.get("code").or_else(|| e.get("type")))
                    .and_then(|c| c.as_str())
                    .map(String::from);
                let message = err
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("le backend Codex a abandonné la réponse")
                    .to_string();
                let retryable = matches!(
                    error_type.as_deref(),
                    Some("rate_limit_exceeded" | "slow_down" | "server_error" | "overloaded")
                );
                out.push(StreamChunk::Error {
                    message,
                    retryable,
                    error_type,
                });
            }
            "codex.rate_limits" => {
                let q = quota_from_event(&v, now_ms());
                if !q.is_empty() {
                    if let Ok(mut slot) = self.quota.lock() {
                        *slot = q.clone();
                    }
                    if let Some(sink) = &self.sink {
                        sink.record(q);
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Le backend clôt sur `response.completed`. Une fermeture avant est une coupure, pas
    /// une fin : la dire permet au tour de relancer plutôt que de garder un texte tronqué.
    fn on_eof(&mut self) -> Vec<StreamChunk> {
        if self.completed {
            return vec![];
        }
        vec![StreamChunk::Error {
            message: "flux Codex fermé sans `response.completed`".into(),
            retryable: true,
            error_type: None,
        }]
    }
}

/// Usage d'un `response.completed`.
pub fn parse_responses_usage(u: &Value) -> Usage {
    let get = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Usage {
        prompt: get("input_tokens"),
        completion: get("output_tokens"),
        cached: u
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        cache_write: get("cache_write_tokens"),
        reasoning: u
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        // L'abonnement ne facture pas l'appel : le coût est connu, il vaut zéro.
        cost_usd: Some(0.0),
    }
}
