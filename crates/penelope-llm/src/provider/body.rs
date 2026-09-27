//! Corps de requête « chat completions ».

use super::*;

/// Convertit une requête interne en corps « chat completions ».
pub fn to_openai_body(req: &ChatRequest) -> Value {
    // Le raisonnement accompagne les messages qui appellent un outil, et eux seuls : c'est
    // là qu'il sert à enchaîner l'appel et sa suite. La règle ne dépend pas du tour en
    // cours, sinon le tour suivant réécrirait ces messages et casserait le cache de
    // préfixe (issue #17) ; une réponse finale ne le renvoie jamais.
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let mut v = message_to_json(m);
            if m.role == Role::Assistant
                && !m.tool_calls.is_empty()
                && let Some(o) = v.as_object_mut()
            {
                if let Some(d) = &m.reasoning_details {
                    o.insert("reasoning_details".into(), d.clone());
                } else if let Some(r) = &m.reasoning {
                    o.insert("reasoning".into(), json!(r));
                }
            }
            v
        })
        .collect();
    let mut b = json!({
        "model": strip_provider(&req.model),
        "messages": messages,
        "stream": true,
        // Sans cela, un serveur local (vLLM, llama.cpp, LM Studio, mlx_lm) ne renvoie
        // jamais `usage` en streaming : plus de comptage de tokens, ni de compaction sur
        // la taille réelle (issue #53).
        "stream_options": {"include_usage": true},
    });
    let obj = b.as_object_mut().expect("objet");
    if !req.tools.is_empty() {
        obj.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            ),
        );
        if let Some(tc) = req.tool_choice {
            obj.insert(
                "tool_choice".into(),
                Value::String(
                    match tc {
                        ToolChoice::Auto => "auto",
                        ToolChoice::None => "none",
                        ToolChoice::Required => "required",
                    }
                    .into(),
                ),
            );
        }
    }
    if let Some(t) = req.temperature {
        obj.insert("temperature".into(), json!(t));
    }
    if let Some(m) = req.max_tokens {
        obj.insert("max_tokens".into(), json!(m));
    }
    // Raisonnement : soit éteint, soit budgété (issue #152). `max_tokens` borne la
    // sortie raisonnement compris chez OpenRouter ; un budget de raisonnement à part est
    // ce qui empêche la réflexion de manger la réponse.
    match (&req.reasoning_effort, req.reasoning_max_tokens) {
        // `none` n'est pas un niveau d'effort : c'est l'extinction. OpenRouter l'entend
        // par `enabled: false` ; envoyé comme `effort: "none"`, plusieurs modèles
        // l'ignorent et réfléchissent quand même, jusqu'à dépenser tout `max_tokens`
        // avant d'écrire la moindre réponse.
        (Some(r), _) if r == "none" => {
            obj.insert(
                "reasoning".into(),
                json!({"enabled": false, "exclude": true}),
            );
        }
        // `max_tokens` et `effort` s'excluent dans le paramètre unifié : le budget, plus
        // précis, gagne.
        (_, Some(budget)) => {
            obj.insert("reasoning".into(), json!({"max_tokens": budget}));
        }
        (Some(r), None) => {
            obj.insert("reasoning".into(), json!({ "effort": r }));
        }
        (None, None) => {}
    }
    if let Some(f) = &req.response_format {
        obj.insert("response_format".into(), f.clone());
    }
    if !req.modalities.is_empty() {
        obj.insert("modalities".into(), json!(req.modalities));
    }
    b
}

pub(super) fn message_to_json(m: &ChatMessage) -> Value {
    let content: Value = if m.content.is_empty() {
        // `content: []` est refusé ou mal lu par plusieurs providers : un message
        // d'appel d'outil sans texte porte `null`, les autres une chaîne vide.
        if m.tool_calls.is_empty() {
            Value::String(String::new())
        } else {
            Value::Null
        }
    } else if m.content.len() == 1 && matches!(m.content[0], Content::Text { .. }) {
        let text = m.content[0].as_text().unwrap_or("").to_string();
        if m.cache_marker {
            // Forme « parties » avec `cache_control`, comprise par Anthropic via
            // OpenRouter ; les autres providers ignorent le champ supplémentaire.
            json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"}
            }])
        } else {
            Value::String(text)
        }
    } else {
        Value::Array(
            m.content
                .iter()
                .map(|c| match c {
                    Content::Text { text } => json!({"type":"text","text":text}),
                    Content::ImageUrl { url, detail } => json!({
                        "type":"image_url",
                        "image_url": {"url": url, "detail": detail.clone().unwrap_or_else(|| "auto".into())}
                    }),
                    Content::InputAudio { data, format } => json!({
                        "type":"input_audio",
                        "input_audio": {"data": data, "format": format}
                    }),
                })
                .collect(),
        )
    };

    let mut o = json!({"role": m.role.as_str(), "content": content});
    let obj = o.as_object_mut().expect("objet");
    if !m.tool_calls.is_empty() {
        obj.insert(
            "tool_calls".into(),
            Value::Array(
                m.tool_calls
                    .iter()
                    .map(|t| {
                        json!({
                            "id": t.id,
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "arguments": t.arguments.to_string(),
                            }
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(id) = &m.tool_call_id {
        obj.insert("tool_call_id".into(), json!(id));
    }
    if let Some(n) = &m.name
        && m.role == Role::Tool
    {
        obj.insert("name".into(), json!(n));
    }
    o
}
