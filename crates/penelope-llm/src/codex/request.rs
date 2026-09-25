//! Requête : corps de l'API Responses.

use super::*;

/// Convertit une requête interne en corps de l'API Responses.
///
/// `store: false` et `stream: true` sont de fait obligatoires ; `include` réclame le
/// raisonnement chiffré, qui est réinjecté au tour suivant (backend sans état).
pub fn to_responses_body(req: &ChatRequest, opts: &CodexOptions) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => {
                let text = m.text();
                if !text.trim().is_empty() {
                    instructions.push(text);
                }
            }
            Role::Tool => {
                // Un résultat d'outil se rattache à son appel par `call_id`, jamais par
                // sa place dans la liste.
                let call_id = m.tool_call_id.clone().unwrap_or_default();
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": m.text(),
                }));
            }
            Role::Assistant => {
                // Le raisonnement chiffré du tour précédent précède l'appel qu'il a
                // produit : sans lui, le backend repart sans sa réflexion.
                for item in reasoning_items(m) {
                    input.push(item);
                }
                let text = m.text();
                if !text.trim().is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
                for c in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "name": c.name,
                        "arguments": c.arguments.to_string(),
                        "call_id": c.id,
                    }));
                }
            }
            Role::User => input.push(json!({
                "type": "message",
                "role": "user",
                "content": user_content(m),
            })),
        }
    }

    let mut b = json!({
        "model": strip_provider(&req.model),
        "instructions": instructions.join("\n\n"),
        "input": input,
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    let obj = b.as_object_mut().expect("objet");
    if !req.tools.is_empty() {
        obj.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    // L'API Responses met la fonction **à plat**, sans objet `function`.
                    .map(|t| {
                        json!({
                            "type": "function",
                            "name": t.name,
                            "description": t.description,
                            "strict": false,
                            "parameters": t.parameters,
                        })
                    })
                    .collect(),
            ),
        );
        obj.insert(
            "tool_choice".into(),
            json!(match req.tool_choice {
                Some(ToolChoice::None) => "none",
                Some(ToolChoice::Required) => "required",
                _ => "auto",
            }),
        );
        obj.insert("parallel_tool_calls".into(), json!(true));
    }
    let mut reasoning = serde_json::Map::new();
    if let Some(effort) = &req.reasoning_effort {
        reasoning.insert("effort".into(), json!(effort));
    }
    if !opts.reasoning_summary.is_empty() {
        reasoning.insert("summary".into(), json!(opts.reasoning_summary));
    }
    if !reasoning.is_empty() {
        obj.insert("reasoning".into(), Value::Object(reasoning));
    }
    let mut text = serde_json::Map::new();
    if !opts.verbosity.is_empty() {
        text.insert("verbosity".into(), json!(opts.verbosity));
    }
    if let Some(f) = &req.response_format {
        text.insert("format".into(), f.clone());
    }
    if !text.is_empty() {
        obj.insert("text".into(), Value::Object(text));
    }
    if let Some(m) = req.max_tokens {
        obj.insert("max_output_tokens".into(), json!(m));
    }
    if let Some(sid) = req.session_id.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("prompt_cache_key".into(), json!(sid));
    }
    b
}

/// Items `reasoning` à réinjecter pour un message d'assistant : ceux que le backend a
/// rendus, avec leur contenu chiffré. Tout autre bloc (OpenRouter) est ignoré.
fn reasoning_items(m: &ChatMessage) -> Vec<Value> {
    let Some(details) = &m.reasoning_details else {
        return Vec::new();
    };
    let items: Vec<&Value> = match details {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter(|item| {
            item.get("type").and_then(|t| t.as_str()) == Some("reasoning")
                && item.get("encrypted_content").is_some()
        })
        .cloned()
        .collect()
}

/// Contenu d'un message utilisateur : texte et images, au dialecte Responses.
fn user_content(m: &ChatMessage) -> Value {
    if m.content.is_empty() {
        return json!([{"type": "input_text", "text": ""}]);
    }
    Value::Array(
        m.content
            .iter()
            .map(|c| match c {
                Content::Text { text } => json!({"type": "input_text", "text": text}),
                Content::ImageUrl { url, .. } => json!({"type": "input_image", "image_url": url}),
                // Le backend Codex ne prend pas l'audio : le texte porte la mention.
                Content::InputAudio { format, .. } => {
                    json!({"type": "input_text", "text": format!("[audio {format} non transmis]")})
                }
            })
            .collect(),
    )
}
