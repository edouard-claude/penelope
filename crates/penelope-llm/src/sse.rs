//! Décodage SSE et reconstitution des appels d'outils (§10.1).
//!
//! Le streaming est **obligatoire**. Les appels d'outils arrivent en fragments
//! (`index`, `id`, `function.name`, `function.arguments` par morceaux) : ils sont
//! réassemblés ici, et les arguments ne sont décodés qu'une fois le fragment terminé.

use crate::types::{FinishReason, StreamChunk, ToolCall, Usage};
use serde_json::Value;
use std::collections::BTreeMap;

/// Découpe un flux d'octets SSE en événements `data:`.
#[derive(Default)]
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ajoute des octets et renvoie les charges utiles `data:` complètes.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();

        // Un événement se termine par une ligne vide ; on tolère \n\n et \r\n\r\n.
        while let Some(end) = find_event_end(&self.buffer) {
            let raw: String = self.buffer[..end.0].to_string();
            self.buffer.drain(..end.1);
            let mut data = String::new();
            for line in raw.lines() {
                let line = line.trim_end_matches('\r');
                if let Some(rest) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest.trim_start());
                }
                // `event:`, `id:`, `retry:` et les commentaires `:` sont ignorés : le
                // protocole chat completions n'utilise que `data:`.
            }
            if !data.is_empty() {
                out.push(data);
            }
        }
        out
    }

    /// Reste non consommé (diagnostic d'un flux coupé).
    pub fn pending(&self) -> &str {
        &self.buffer
    }
}

fn find_event_end(s: &str) -> Option<(usize, usize)> {
    if let Some(i) = s.find("\r\n\r\n") {
        return Some((i, i + 4));
    }
    if let Some(i) = s.find("\n\n") {
        return Some((i, i + 2));
    }
    None
}

/// Accumule les fragments d'un flux chat completions.
#[derive(Default)]
pub struct StreamAccumulator {
    pub id: String,
    pub model: String,
    pub text: String,
    pub reasoning: String,
    pub finish: Option<FinishReason>,
    pub usage: Option<Usage>,
    started: bool,
    partial_calls: BTreeMap<i64, PartialCall>,
}

#[derive(Default, Clone)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Traite une charge utile `data:`. Renvoie les fragments à publier.
    pub fn push_payload(&mut self, data: &str) -> Vec<StreamChunk> {
        if data.trim() == "[DONE]" {
            let mut out = self.flush_tool_calls();
            out.push(StreamChunk::Done {
                finish: self.finish.unwrap_or(FinishReason::Stop),
            });
            return out;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return vec![];
        };

        // OpenRouter transmet parfois une erreur au milieu du flux.
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("erreur du provider")
                .to_string();
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            return vec![StreamChunk::Error {
                message: msg,
                retryable: code >= 500 || code == 429,
            }];
        }

        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            self.id = v
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            self.model = v
                .get("model")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            out.push(StreamChunk::Started {
                id: self.id.clone(),
                model: self.model.clone(),
            });
        }

        if let Some(u) = v.get("usage") {
            let usage = parse_usage(u);
            self.usage = Some(usage);
            out.push(StreamChunk::Usage(usage));
        }

        let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
            return out;
        };
        for choice in choices {
            if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                self.finish = Some(FinishReason::parse(fr));
            }
            let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) else {
                continue;
            };

            if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
                if !t.is_empty() {
                    self.text.push_str(t);
                    out.push(StreamChunk::Delta { text: t.into() });
                }
            }
            // Certains modèles exposent le raisonnement séparément.
            for key in ["reasoning", "reasoning_content"] {
                if let Some(t) = delta.get(key).and_then(|c| c.as_str()) {
                    if !t.is_empty() {
                        self.reasoning.push_str(t);
                        out.push(StreamChunk::Reasoning { text: t.into() });
                    }
                }
            }

            if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                for call in calls {
                    let idx = call.get("index").and_then(|i| i.as_i64()).unwrap_or(0);
                    let entry = self.partial_calls.entry(idx).or_default();
                    if let Some(id) = call.get("id").and_then(|i| i.as_str()) {
                        if !id.is_empty() {
                            entry.id = id.to_string();
                        }
                    }
                    if let Some(f) = call.get("function") {
                        if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                            if !n.is_empty() {
                                entry.name.push_str(n);
                            }
                        }
                        if let Some(a) = f.get("arguments").and_then(|a| a.as_str()) {
                            entry.args.push_str(a);
                        }
                    }
                }
            }
        }

        // Quand le modèle annonce `tool_calls`, les fragments sont complets.
        if self.finish == Some(FinishReason::ToolCalls) {
            out.extend(self.flush_tool_calls());
        }
        out
    }

    /// Publie les appels d'outils accumulés, une seule fois.
    fn flush_tool_calls(&mut self) -> Vec<StreamChunk> {
        let mut out = Vec::new();
        let calls = std::mem::take(&mut self.partial_calls);
        for (i, (_, c)) in calls.into_iter().enumerate() {
            if c.name.is_empty() {
                continue;
            }
            out.push(StreamChunk::ToolCall(ToolCall {
                id: if c.id.is_empty() {
                    format!("call_{i}")
                } else {
                    c.id
                },
                name: c.name,
                arguments: parse_arguments(&c.args),
            }));
        }
        out
    }

    pub fn has_pending_calls(&self) -> bool {
        !self.partial_calls.is_empty()
    }
}

/// Décode les arguments d'un appel d'outil. Un JSON invalide est conservé brut : le
/// harnais renvoie l'erreur au modèle pour qu'il se corrige plutôt que d'échouer le tour.
pub fn parse_arguments(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Object(Default::default());
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v) if v.is_object() => v,
        Ok(v) => serde_json::json!({ "__value": v }),
        Err(_) => match crate::emulation::extract_json(trimmed) {
            Some(v) if v.is_object() => v,
            _ => serde_json::json!({ "__raw": trimmed }),
        },
    }
}

fn parse_usage(u: &Value) -> Usage {
    let get = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let cached = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|x| x.as_u64())
        .or_else(|| u.get("cache_read_input_tokens").and_then(|x| x.as_u64()))
        .unwrap_or(0);
    let reasoning = u
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    Usage {
        prompt: get("prompt_tokens"),
        completion: get("completion_tokens"),
        cached,
        reasoning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_splits_events() {
        let mut d = SseDecoder::new();
        assert!(
            d.push(b"data: {\"a\":1}\n").is_empty(),
            "événement incomplet"
        );
        let out = d.push(b"\ndata: {\"b\":2}\n\n");
        assert_eq!(out, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);
    }

    #[test]
    fn decoder_handles_crlf_and_comments() {
        let mut d = SseDecoder::new();
        let out = d.push(b": ping\r\ndata: {\"x\":1}\r\n\r\n");
        assert_eq!(out, vec!["{\"x\":1}".to_string()]);
    }

    #[test]
    fn decoder_handles_split_utf8_across_chunks() {
        let mut d = SseDecoder::new();
        let out = d.push("data: {\"t\":\"é\"}\n\n".as_bytes());
        assert_eq!(out.len(), 1);
        assert!(out[0].contains('é'));
    }

    #[test]
    fn accumulates_text_deltas() {
        let mut a = StreamAccumulator::new();
        let c = a.push_payload(r#"{"id":"1","model":"m","choices":[{"delta":{"content":"bon"}}]}"#);
        assert!(matches!(c[0], StreamChunk::Started { .. }));
        assert!(matches!(&c[1], StreamChunk::Delta { text } if text == "bon"));
        a.push_payload(r#"{"choices":[{"delta":{"content":"jour"}}]}"#);
        assert_eq!(a.text, "bonjour");
    }

    #[test]
    fn reassembles_fragmented_tool_calls() {
        let mut a = StreamAccumulator::new();
        a.push_payload(
            r#"{"id":"1","model":"m","choices":[{"delta":{"tool_calls":[
               {"index":0,"id":"call_a","function":{"name":"fs_","arguments":"{\"pa"}}]}}]}"#,
        );
        a.push_payload(
            r#"{"choices":[{"delta":{"tool_calls":[
               {"index":0,"function":{"name":"read","arguments":"th\":\"a.rs\"}"}}]}}]}"#,
        );
        let out = a.push_payload(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#);
        let call = out
            .iter()
            .find_map(|c| match c {
                StreamChunk::ToolCall(t) => Some(t.clone()),
                _ => None,
            })
            .expect("appel d'outil reconstitué");
        assert_eq!(call.id, "call_a");
        assert_eq!(call.name, "fs_read");
        assert_eq!(call.arguments, serde_json::json!({"path":"a.rs"}));
    }

    #[test]
    fn two_parallel_tool_calls_are_kept_apart() {
        let mut a = StreamAccumulator::new();
        a.push_payload(
            r#"{"id":"1","model":"m","choices":[{"delta":{"tool_calls":[
               {"index":0,"id":"c0","function":{"name":"a","arguments":"{}"}},
               {"index":1,"id":"c1","function":{"name":"b","arguments":"{\"x\":1}"}}]}}]}"#,
        );
        let out = a.push_payload("[DONE]");
        let calls: Vec<_> = out
            .iter()
            .filter_map(|c| match c {
                StreamChunk::ToolCall(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].arguments, serde_json::json!({"x":1}));
    }

    #[test]
    fn tool_calls_are_emitted_once() {
        let mut a = StreamAccumulator::new();
        a.push_payload(
            r#"{"id":"1","model":"m","choices":[{"delta":{"tool_calls":[
               {"index":0,"id":"c0","function":{"name":"a","arguments":"{}"}}]},
               "finish_reason":"tool_calls"}]}"#,
        );
        let second = a.push_payload("[DONE]");
        assert!(
            !second.iter().any(|c| matches!(c, StreamChunk::ToolCall(_))),
            "un appel déjà publié ne doit pas l'être deux fois"
        );
    }

    #[test]
    fn invalid_arguments_are_kept_raw_for_self_correction() {
        assert_eq!(
            parse_arguments("{pas du json"),
            serde_json::json!({"__raw":"{pas du json"})
        );
        assert_eq!(parse_arguments(""), serde_json::json!({}));
    }

    #[test]
    fn usage_reads_cached_and_reasoning() {
        let mut a = StreamAccumulator::new();
        let out = a.push_payload(
            r#"{"id":"1","model":"m","usage":{"prompt_tokens":100,"completion_tokens":20,
               "prompt_tokens_details":{"cached_tokens":80},
               "completion_tokens_details":{"reasoning_tokens":7}},"choices":[]}"#,
        );
        let u = out
            .iter()
            .find_map(|c| match c {
                StreamChunk::Usage(u) => Some(*u),
                _ => None,
            })
            .unwrap();
        assert_eq!(u.prompt, 100);
        assert_eq!(u.cached, 80);
        assert_eq!(u.reasoning, 7);
    }

    #[test]
    fn mid_stream_error_is_surfaced() {
        let mut a = StreamAccumulator::new();
        let out = a.push_payload(r#"{"error":{"message":"surcharge","code":503}}"#);
        match &out[0] {
            StreamChunk::Error { message, retryable } => {
                assert_eq!(message, "surcharge");
                assert!(*retryable);
            }
            other => panic!("attendu une erreur, obtenu {other:?}"),
        }
    }
}
