//! Décodage SSE et reconstitution des appels d'outils (§10.1).
//!
//! Le streaming est **obligatoire**. Les appels d'outils arrivent en fragments
//! (`index`, `id`, `function.name`, `function.arguments` par morceaux) : ils sont
//! réassemblés ici, et les arguments ne sont décodés qu'une fois le fragment terminé.

use crate::types::{FinishReason, StreamChunk, ToolCall, Usage, kind_for_error_type};
use serde_json::Value;
use std::collections::BTreeMap;

/// Découpe un flux d'octets SSE en événements `data:`.
///
/// Les paquets suivent les frontières du transport (TLS, TCP, proxy), jamais celles des
/// caractères : un `é` ou un emoji peut arriver en deux morceaux. Les octets d'un
/// caractère incomplet attendent donc le paquet suivant au lieu d'être décodés seuls, ce
/// qui les changerait en « � » dans la réponse et les arguments d'outils (issue #80).
#[derive(Default)]
pub struct SseDecoder {
    buffer: String,
    /// Début d'un caractère multi-octets coupé par le transport (trois octets au plus).
    partial: Vec<u8>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ajoute des octets et renvoie les charges utiles `data:` complètes.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let joined;
        let mut input: &[u8] = if self.partial.is_empty() {
            bytes
        } else {
            let mut v = std::mem::take(&mut self.partial);
            v.extend_from_slice(bytes);
            joined = v;
            &joined
        };
        loop {
            match std::str::from_utf8(input) {
                Ok(text) => {
                    self.buffer.push_str(text);
                    break;
                }
                Err(e) => {
                    let (valid, rest) = input.split_at(e.valid_up_to());
                    // Sûr : `valid_up_to` borne une tranche UTF-8 valide.
                    self.buffer
                        .push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        // Caractère incomplet en fin de paquet : il attend la suite.
                        None => {
                            self.partial = rest.to_vec();
                            break;
                        }
                        // Octets réellement invalides : remplacés, comme avant.
                        Some(n) => {
                            self.buffer.push(char::REPLACEMENT_CHARACTER);
                            input = &rest[n..];
                        }
                    }
                }
            }
        }
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

    /// Octets d'un caractère encore incomplet, en attente du paquet suivant.
    pub fn pending_bytes(&self) -> &[u8] {
        &self.partial
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
    /// Provider amont (`provider`) et raison d'arrêt brute (`native_finish_reason`).
    pub upstream: Option<String>,
    pub native_finish: Option<String>,
    /// Refus explicite du modèle (`delta.refusal`).
    pub refusal: String,
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

        // Erreur au milieu du flux : le 200 est déjà parti, l'erreur arrive en fragment,
        // au premier niveau, avec `finish_reason: "error"` dans `choices`.
        if let Some(err) = v.get("error") {
            let mut message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("erreur du provider")
                .to_string();
            if let Some(p) = v.get("provider").and_then(|p| p.as_str()) {
                message.push_str(&format!(" ({p})"));
            }
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            let error_type = err
                .get("metadata")
                .and_then(|m| m.get("error_type"))
                .and_then(|t| t.as_str())
                .map(String::from);
            let retryable = match error_type.as_deref().and_then(kind_for_error_type) {
                Some(kind) => kind.is_retryable(),
                None => code >= 500 || code == 429,
            };
            return vec![StreamChunk::Error {
                message,
                retryable,
                error_type,
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

        if let Some(p) = v.get("provider").and_then(|p| p.as_str())
            && !p.is_empty()
            && self.upstream.as_deref() != Some(p)
        {
            self.upstream = Some(p.to_string());
            out.push(self.meta());
        }

        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
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
            if let Some(n) = choice.get("native_finish_reason").and_then(|f| f.as_str())
                && !n.is_empty()
                && self.native_finish.as_deref() != Some(n)
            {
                self.native_finish = Some(n.to_string());
                out.push(self.meta());
            }
            let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) else {
                continue;
            };

            if let Some(t) = delta.get("content").and_then(|c| c.as_str())
                && !t.is_empty()
            {
                self.text.push_str(t);
                out.push(StreamChunk::Delta { text: t.into() });
            }
            // Certains modèles exposent le raisonnement séparément. OpenRouter envoie le
            // même texte en `reasoning` et en `reasoning_content` : on n'en lit qu'un.
            if let Some(t) = delta
                .get("reasoning")
                .and_then(|c| c.as_str())
                .or_else(|| delta.get("reasoning_content").and_then(|c| c.as_str()))
                && !t.is_empty()
            {
                self.reasoning.push_str(t);
                out.push(StreamChunk::Reasoning { text: t.into() });
            }
            if let Some(t) = delta.get("refusal").and_then(|r| r.as_str())
                && !t.is_empty()
            {
                self.refusal.push_str(t);
                out.push(StreamChunk::Refusal { text: t.into() });
            }
            // Génération d'image : `images: [{type: "image_url", image_url: {url}}]`.
            if let Some(images) = delta.get("images").and_then(|i| i.as_array()) {
                for img in images {
                    if let Some(url) = img
                        .get("image_url")
                        .and_then(|u| u.get("url"))
                        .and_then(|u| u.as_str())
                        .filter(|u| !u.is_empty())
                    {
                        out.push(StreamChunk::Image { url: url.into() });
                    }
                }
            }
            if let Some(details) = delta.get("reasoning_details")
                && details.as_array().map(|a| !a.is_empty()).unwrap_or(false)
            {
                out.push(StreamChunk::ReasoningDetails(details.clone()));
            }

            if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                for call in calls {
                    let idx = call.get("index").and_then(|i| i.as_i64()).unwrap_or(0);
                    let entry = self.partial_calls.entry(idx).or_default();
                    if let Some(id) = call.get("id").and_then(|i| i.as_str())
                        && !id.is_empty()
                    {
                        entry.id = id.to_string();
                    }
                    if let Some(f) = call.get("function") {
                        if let Some(n) = f.get("name").and_then(|n| n.as_str())
                            && !n.is_empty()
                        {
                            entry.name.push_str(n);
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

    fn meta(&self) -> StreamChunk {
        StreamChunk::Meta {
            upstream: self.upstream.clone(),
            native_finish: self.native_finish.clone(),
        }
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

/// Fusionne les fragments de `reasoning_details` reçus en streaming.
///
/// Les blocs arrivent morceau par morceau, repérés par `index` : les champs textuels
/// (`text`, `summary`, `data`) se concatènent, les autres (`type`, `id`, `format`,
/// `signature`) prennent la dernière valeur non nulle. L'ordre des blocs est conservé,
/// comme l'exige OpenRouter au renvoi.
pub fn merge_reasoning_details(parts: &[Value]) -> Option<Value> {
    let mut merged: BTreeMap<i64, serde_json::Map<String, Value>> = BTreeMap::new();
    let mut next_index = 0i64;
    for part in parts {
        let items: Vec<&Value> = match part {
            Value::Array(a) => a.iter().collect(),
            other => vec![other],
        };
        for item in items {
            let Some(obj) = item.as_object() else {
                continue;
            };
            let idx = obj
                .get("index")
                .and_then(|i| i.as_i64())
                .unwrap_or_else(|| {
                    next_index += 1;
                    next_index - 1
                });
            let slot = merged.entry(idx).or_default();
            for (k, v) in obj {
                match (k.as_str(), v) {
                    ("text" | "summary" | "data", Value::String(s)) => {
                        let current = slot
                            .get(k)
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        slot.insert(k.clone(), Value::String(current + s));
                    }
                    (_, Value::Null) => {
                        slot.entry(k.clone()).or_insert(Value::Null);
                    }
                    _ => {
                        slot.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }
    if merged.is_empty() {
        return None;
    }
    Some(Value::Array(
        merged.into_values().map(Value::Object).collect(),
    ))
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
        Err(_) => match crate::json_scan::extract_json(trimmed) {
            Some(v) if v.is_object() => v,
            _ => serde_json::json!({ "__raw": trimmed }),
        },
    }
}

fn parse_usage(u: &Value) -> Usage {
    let get = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let prompt_details = u.get("prompt_tokens_details");
    let cached = prompt_details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|x| x.as_u64())
        .or_else(|| u.get("cache_read_input_tokens").and_then(|x| x.as_u64()))
        .unwrap_or(0);
    let cache_write = prompt_details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|x| x.as_u64())
        .or_else(|| {
            u.get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
        })
        .unwrap_or(0);
    let reasoning = u
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    // `cost` est ce qu'OpenRouter débite. En BYOK, l'inférence est payée au provider
    // à part (`cost_details.upstream_inference_cost`) : on l'ajoute pour le vrai total.
    let cost_usd = u.get("cost").and_then(|c| c.as_f64()).map(|cost| {
        let byok = u.get("is_byok").and_then(|b| b.as_bool()).unwrap_or(false);
        let upstream = u
            .get("cost_details")
            .and_then(|d| d.get("upstream_inference_cost"))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        if byok { cost + upstream } else { cost }
    });
    Usage {
        prompt: get("prompt_tokens"),
        completion: get("completion_tokens"),
        cached,
        cache_write,
        reasoning,
        cost_usd,
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

    /// Décode un flux découpé aux frontières données, tous les événements à la suite.
    fn decode_in_pieces(stream: &[u8], cuts: &[usize]) -> Vec<String> {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        let mut from = 0;
        for &cut in cuts.iter().chain(std::iter::once(&stream.len())) {
            out.extend(d.push(&stream[from..cut]));
            from = cut;
        }
        assert!(d.pending_bytes().is_empty(), "octets restés en attente");
        out
    }

    /// #80 : un caractère multi-octets coupé entre deux paquets est reconstitué, à
    /// **chaque** offset d'octet possible.
    #[test]
    fn a_character_split_at_any_byte_is_rebuilt() {
        // é (2 octets), € (3), 🚀 (4), e + accent combinant (1 + 2).
        let text = "été € 🚀 e\u{301} fini";
        let stream = format!("data: {{\"t\":\"{text}\"}}\n\n");
        let expected = decode_in_pieces(stream.as_bytes(), &[]);
        assert!(expected[0].contains(text));
        for cut in 1..stream.len() {
            assert_eq!(
                decode_in_pieces(stream.as_bytes(), &[cut]),
                expected,
                "coupé à l'octet {cut}"
            );
        }
    }

    /// #80 : les arguments d'un appel d'outil, fragmentés par le fournisseur puis coupés
    /// par le transport, sont identiques à ceux d'un flux entier.
    #[test]
    fn tool_arguments_survive_transport_cuts() {
        let events = [
            r#"{"id":"1","model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"fs_write","arguments":"{\"path\":\"rapport-d"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"écembre.md\",\"content\":\"Été 🚀\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];
        let stream: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        let call_of = |payloads: Vec<String>| {
            let mut a = StreamAccumulator::new();
            payloads
                .iter()
                .flat_map(|p| a.push_payload(p))
                .find_map(|c| match c {
                    StreamChunk::ToolCall(t) => Some(t),
                    _ => None,
                })
                .expect("appel reconstitué")
        };
        let whole = call_of(decode_in_pieces(stream.as_bytes(), &[]));
        assert_eq!(
            whole.arguments,
            serde_json::json!({"path": "rapport-décembre.md", "content": "Été 🚀"})
        );
        for cut in 1..stream.len() {
            assert_eq!(
                call_of(decode_in_pieces(stream.as_bytes(), &[cut])).arguments,
                whole.arguments,
                "coupé à l'octet {cut}"
            );
        }
    }

    /// #80 : paquets de tailles aléatoires (graine fixe) sur un flux de plusieurs Kio.
    #[test]
    fn random_packet_sizes_give_the_same_text() {
        let mut stream = String::new();
        for i in 0..200 {
            stream.push_str(&format!(
                "data: {{\"n\":{i},\"t\":\"déjà vu, ça coûte 3 € 🚀 … ok\"}}\n\n"
            ));
        }
        let bytes = stream.as_bytes();
        let expected = decode_in_pieces(bytes, &[]);
        assert_eq!(expected.len(), 200);
        let mut seed: u64 = 0x5eed_2026;
        for _ in 0..50 {
            let mut cuts = Vec::new();
            let mut at = 0;
            loop {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                at += 1 + (seed >> 33) as usize % 17;
                if at >= bytes.len() {
                    break;
                }
                cuts.push(at);
            }
            assert_eq!(decode_in_pieces(bytes, &cuts), expected);
        }
    }

    /// Des octets réellement invalides restent remplacés, sans bloquer la suite.
    #[test]
    fn invalid_bytes_are_replaced_not_held() {
        let mut d = SseDecoder::new();
        let mut stream = b"data: a".to_vec();
        stream.push(0xFF);
        stream.extend_from_slice(b"b\n\n");
        let out = d.push(&stream);
        assert_eq!(out, vec!["a\u{FFFD}b".to_string()]);
        assert!(d.pending_bytes().is_empty());
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
    fn reasoning_details_are_merged_by_index_in_order() {
        let parts = vec![
            serde_json::json!([{"type":"reasoning.text","text":"Je ","index":0,"format":"x","signature":null}]),
            serde_json::json!([{"type":"reasoning.text","text":"réfléchis","index":0,"signature":"sig"}]),
            serde_json::json!([{"type":"reasoning.encrypted","data":"AB","index":1}]),
            serde_json::json!([{"type":"reasoning.encrypted","data":"CD","index":1}]),
        ];
        let merged = merge_reasoning_details(&parts).unwrap();
        assert_eq!(merged[0]["text"], "Je réfléchis");
        assert_eq!(merged[0]["signature"], "sig");
        assert_eq!(merged[0]["format"], "x");
        assert_eq!(merged[1]["data"], "ABCD");
        assert!(merge_reasoning_details(&[]).is_none());
    }

    #[test]
    fn openrouter_reasoning_is_read_once_not_twice() {
        let mut acc = StreamAccumulator::new();
        let chunk = serde_json::json!({
            "id":"g","model":"m",
            "choices":[{"delta":{"reasoning":"abc","reasoning_content":"abc",
                "reasoning_details":[{"type":"reasoning.text","text":"abc","index":0}]}}]
        });
        let out = acc.push_payload(&chunk.to_string());
        assert_eq!(acc.reasoning, "abc");
        assert!(
            out.iter()
                .any(|c| matches!(c, StreamChunk::ReasoningDetails(_)))
        );
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
            StreamChunk::Error {
                message, retryable, ..
            } => {
                assert_eq!(message, "surcharge");
                assert!(*retryable);
            }
            other => panic!("attendu une erreur, obtenu {other:?}"),
        }
    }

    #[test]
    fn documented_mid_stream_error_chunk_keeps_type_and_provider() {
        // Forme exacte de la documentation OpenRouter (erreurs en cours de flux).
        let mut a = StreamAccumulator::new();
        let out = a.push_payload(
            r#"{"id":"gen-abc123","object":"chat.completion.chunk","created":1234567890,
               "model":"openai/gpt-4o","provider":"OpenAI",
               "error":{"code":429,"message":"Rate limit exceeded",
                        "metadata":{"error_type":"rate_limit_exceeded"}},
               "choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#,
        );
        match &out[0] {
            StreamChunk::Error {
                message,
                retryable,
                error_type,
            } => {
                assert_eq!(message, "Rate limit exceeded (OpenAI)");
                assert!(*retryable);
                assert_eq!(error_type.as_deref(), Some("rate_limit_exceeded"));
            }
            other => panic!("attendu une erreur, obtenu {other:?}"),
        }
        let mut a = StreamAccumulator::new();
        let out = a.push_payload(
            r#"{"error":{"code":403,"message":"refused","metadata":{"error_type":"refusal"}}}"#,
        );
        assert!(matches!(
            &out[0],
            StreamChunk::Error {
                retryable: false,
                ..
            }
        ));
    }

    #[test]
    fn usage_chunk_carries_the_billed_cost() {
        // Dernier fragment documenté : `finish_reason` répété, usage complet avec `cost`.
        let mut a = StreamAccumulator::new();
        a.push_payload(r#"{"id":"gen-1","model":"z-ai/glm-5.3","provider":"Z.AI","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","native_finish_reason":"stop"}]}"#);
        let out = a.push_payload(
            r#"{"id":"gen-1","model":"z-ai/glm-5.3","provider":"Z.AI",
               "choices":[{"index":0,"delta":{"content":"","role":"assistant"},
                           "finish_reason":"stop","native_finish_reason":"stop"}],
               "usage":{"prompt_tokens":10339,"completion_tokens":60,"total_tokens":10399,
                        "prompt_tokens_details":{"cached_tokens":10318,"cache_write_tokens":0},
                        "cost":0.0012,"is_byok":false,
                        "cost_details":{"upstream_inference_cost":null,
                                        "upstream_inference_prompt_cost":0.0008,
                                        "upstream_inference_completions_cost":0.0004}}}"#,
        );
        let u = out
            .iter()
            .find_map(|c| match c {
                StreamChunk::Usage(u) => Some(*u),
                _ => None,
            })
            .unwrap();
        assert_eq!(u.cost_usd, Some(0.0012));
        assert_eq!(u.cached, 10318);
        assert_eq!(a.upstream.as_deref(), Some("Z.AI"));
        assert_eq!(a.native_finish.as_deref(), Some("stop"));
        assert_eq!(a.finish, Some(FinishReason::Stop));
        assert_eq!(a.text, "ok");
    }

    #[test]
    fn byok_cost_adds_the_upstream_inference() {
        let u = parse_usage(&serde_json::json!({
            "prompt_tokens": 10, "completion_tokens": 5, "cost": 0.0001, "is_byok": true,
            "cost_details": {"upstream_inference_cost": 0.002,
                             "upstream_inference_prompt_cost": 0.001,
                             "upstream_inference_completions_cost": 0.001},
            "prompt_tokens_details": {"cache_write_tokens": 4}
        }));
        assert!((u.cost_usd.unwrap() - 0.0021).abs() < 1e-12);
        assert_eq!(u.cache_write, 4);
        assert_eq!(
            parse_usage(&serde_json::json!({"prompt_tokens": 1})).cost_usd,
            None
        );
    }

    #[test]
    fn generated_images_are_collected() {
        let mut a = StreamAccumulator::new();
        let out = a.push_payload(
            r#"{"id":"1","model":"m","choices":[{"delta":{"content":"Voici.",
               "images":[{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}}]}}]}"#,
        );
        assert!(out.iter().any(|c| matches!(
            c,
            StreamChunk::Image { url } if url.starts_with("data:image/png;base64,")
        )));
    }

    #[test]
    fn refusals_are_not_silent() {
        let mut a = StreamAccumulator::new();
        let out = a.push_payload(
            r#"{"id":"1","model":"m","choices":[{"delta":{"refusal":"Je ne peux pas aider."},
               "finish_reason":"content_filter"}]}"#,
        );
        assert!(out.iter().any(|c| matches!(c, StreamChunk::Refusal { .. })));
        assert_eq!(a.refusal, "Je ne peux pas aider.");
        assert_eq!(a.finish, Some(FinishReason::ContentFilter));
    }

    #[test]
    fn debug_and_usage_frames_with_empty_choices_are_harmless() {
        let mut a = StreamAccumulator::new();
        let out = a.push_payload(
            r#"{"id":"gen-x","provider":"Anthropic","model":"anthropic/claude-haiku-4.5",
               "object":"chat.completion.chunk","created":1,"choices":[],
               "debug":{"echo_upstream_body":{"max_tokens":64000}}}"#,
        );
        assert!(matches!(out[0], StreamChunk::Started { .. }));
        assert!(a.text.is_empty() && a.finish.is_none());
    }
}
