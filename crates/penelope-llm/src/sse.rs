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

/// Accumulateur d'un flux SSE : reçoit les charges utiles `data:` dans l'ordre et rend
/// les fragments à publier. Un dialecte par implémentation (`chat/completions` ici,
/// l'API Responses du backend Codex dans `crate::codex`), un seul transport.
pub trait EventAccumulator: Send {
    /// Traite une charge utile `data:`.
    fn push_payload(&mut self, data: &str) -> Vec<StreamChunk>;
    /// Le serveur a fermé le flux sans fin explicite : à chacun de dire si c'est une
    /// clôture propre ou une erreur.
    fn on_eof(&mut self) -> Vec<StreamChunk>;
}

impl EventAccumulator for StreamAccumulator {
    fn push_payload(&mut self, data: &str) -> Vec<StreamChunk> {
        StreamAccumulator::push_payload(self, data)
    }
    /// `chat/completions` : on clôt proprement avec ce qui a été reçu.
    fn on_eof(&mut self) -> Vec<StreamChunk> {
        StreamAccumulator::push_payload(self, "[DONE]")
    }
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
mod tests;
