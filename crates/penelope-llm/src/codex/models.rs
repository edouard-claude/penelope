//! Catalogue des modèles du backend Codex.

use super::*;

/// Fenêtre annoncée par le plan quand `/models` ne dit rien.
pub const DEFAULT_CODEX_WINDOW: u64 = 272_000;

/// Analyse `GET /models` du backend Codex. Aucun prix : l'abonnement ne facture pas.
pub fn parse_codex_models(body: &Value) -> Vec<ModelInfo> {
    body.get("models")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(parse_codex_model).collect())
        .unwrap_or_default()
}

fn parse_codex_model(m: &Value) -> Option<ModelInfo> {
    let slug = m.get("slug")?.as_str()?.to_string();
    let window = m
        .get("max_context_window")
        .or_else(|| m.get("context_window"))
        .and_then(|w| w.as_u64())
        .filter(|w| *w > 0)
        .unwrap_or(DEFAULT_CODEX_WINDOW);
    let list = |k: &str| -> Vec<String> {
        m.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut input = list("input_modalities");
    if input.is_empty() {
        input.push("text".into());
    }
    let efforts = m
        .get("supported_reasoning_levels")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        });
    let mut supported = vec!["tools".to_string(), "tool_choice".to_string()];
    if efforts.is_some() {
        supported.push("reasoning".into());
    }
    Some(ModelInfo {
        id: slug.clone(),
        name: slug,
        provider: "codex".into(),
        context_window: window,
        max_output: None,
        price_prompt: 0.0,
        price_completion: 0.0,
        price_cached_read: 0.0,
        price_image: 0.0,
        input_modalities: input,
        output_modalities: vec!["text".into()],
        supported_parameters: supported,
        price_cache_write: 0.0,
        reasoning_efforts: efforts,
        reasoning_mandatory: false,
    })
}

/// Catalogue de repli : ce que la configuration déclare, sans appel réseau.
pub fn fallback_models(models: &[String]) -> Vec<ModelInfo> {
    models
        .iter()
        .map(|slug| ModelInfo {
            provider: "codex".into(),
            input_modalities: vec!["text".into(), "image".into()],
            reasoning_efforts: Some(vec![
                "minimal".into(),
                "low".into(),
                "medium".into(),
                "high".into(),
            ]),
            ..ModelInfo::minimal(slug, "codex", DEFAULT_CODEX_WINDOW)
        })
        .collect()
}
