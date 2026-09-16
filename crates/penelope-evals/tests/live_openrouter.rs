//! Suite `live-openrouter` (§20.1, réseau) : streaming, outils, raisonnement, image, usage,
//! contre OpenRouter pour de vrai.
//!
//! ```bash
//! OPENROUTER_API_KEY=… penelope eval live-openrouter
//! ```
//!
//! Modèles : `PENELOPE_LIVE_MODEL` (texte et outils), `PENELOPE_LIVE_REASONING_MODEL`,
//! `PENELOPE_LIVE_IMAGE_MODEL`. Chaque appel reste court : quelques centimes au total.

use penelope_evals::live;
use penelope_kernel::clock::{SharedClock, SystemClock};
use penelope_llm::provider::{CancelToken, collect_stream};
use penelope_llm::types::{ChatMessage, ChatRequest, Content, ToolChoice, ToolDef};
use serde_json::json;
use std::sync::Arc;

async fn ask(
    model: &str,
    request: ChatRequest,
) -> (penelope_llm::types::ChatResponse, std::time::Duration) {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(SystemClock);
    let d = live::daemon(dir.path(), clock).await;
    let provider = d.provider_for(model).await.expect("provider");
    let started = std::time::Instant::now();
    let rx = provider
        .chat_stream(request, CancelToken::new())
        .await
        .expect("flux ouvert");
    let response = collect_stream(rx, model, provider.name(), &d.services.catalog)
        .await
        .expect("flux complet");
    (response, started.elapsed())
}

#[tokio::test]
#[ignore = "réseau : OPENROUTER_API_KEY"]
async fn streaming_answers_with_usage_and_cost() {
    let model = live::model(
        "PENELOPE_LIVE_MODEL",
        "openrouter:deepseek/deepseek-v4-flash",
    );
    let (r, elapsed) = ask(
        &model,
        ChatRequest {
            model: model.clone(),
            messages: vec![ChatMessage::user(
                "Réponds uniquement par le mot pong, en minuscules.",
            )],
            stream: true,
            max_tokens: Some(400),
            ..Default::default()
        },
    )
    .await;
    assert!(
        r.message.text().to_lowercase().contains("pong"),
        "{:?}",
        r.message
    );
    assert!(
        r.usage.prompt > 0 && r.usage.completion > 0,
        "{:?}",
        r.usage
    );
    assert!(r.cost_usd >= 0.0);
    assert!(elapsed.as_secs() < 120);
    eprintln!(
        "streaming : {} ms, {} + {} tokens, {:.6} $ ({})",
        elapsed.as_millis(),
        r.usage.prompt,
        r.usage.completion,
        r.cost_usd,
        if r.cost_estimated {
            "estimé"
        } else {
            "facturé"
        }
    );
}

#[tokio::test]
#[ignore = "réseau : OPENROUTER_API_KEY"]
async fn a_tool_is_called_with_structured_arguments() {
    let model = live::model(
        "PENELOPE_LIVE_MODEL",
        "openrouter:deepseek/deepseek-v4-flash",
    );
    let (r, _) = ask(
        &model,
        ChatRequest {
            model: model.clone(),
            messages: vec![ChatMessage::user(
                "Utilise l'outil addition pour calculer 17 + 25.",
            )],
            tools: vec![ToolDef::new(
                "addition",
                "Additionne deux entiers.",
                json!({
                    "type": "object",
                    "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
                    "required": ["a", "b"]
                }),
            )],
            tool_choice: Some(ToolChoice::Auto),
            stream: true,
            max_tokens: Some(800),
            ..Default::default()
        },
    )
    .await;
    let call = r
        .message
        .tool_calls
        .first()
        .unwrap_or_else(|| panic!("aucun appel d'outil : {:?}", r.message));
    assert_eq!(call.name, "addition");
    let (a, b) = (call.arguments["a"].as_i64(), call.arguments["b"].as_i64());
    assert_eq!(
        [a, b].iter().flatten().sum::<i64>(),
        42,
        "{}",
        call.arguments
    );
}

#[tokio::test]
#[ignore = "réseau : OPENROUTER_API_KEY"]
async fn reasoning_is_exposed_when_requested() {
    let model = live::model(
        "PENELOPE_LIVE_REASONING_MODEL",
        "openrouter:deepseek/deepseek-v4-pro",
    );
    let (r, _) = ask(
        &model,
        ChatRequest {
            model: model.clone(),
            messages: vec![ChatMessage::user(
                "Combien de lundis y a-t-il en février 2027 ? Réponds par le nombre.",
            )],
            reasoning_effort: Some("low".into()),
            stream: true,
            max_tokens: Some(4_000),
            ..Default::default()
        },
    )
    .await;
    assert!(r.message.text().contains('4'), "{:?}", r.message);
    assert!(
        !r.reasoning.is_empty() || r.usage.reasoning > 0,
        "raisonnement ni exposé ni compté : {:?}",
        r.usage
    );
}

#[tokio::test]
#[ignore = "réseau : OPENROUTER_API_KEY"]
async fn an_image_is_generated() {
    let model = live::model(
        "PENELOPE_LIVE_IMAGE_MODEL",
        "openrouter:google/gemini-3.1-flash-image",
    );
    let (r, _) = ask(
        &model,
        ChatRequest {
            model: model.clone(),
            messages: vec![ChatMessage::user(
                "Dessine un carré rouge sur fond blanc, rien d'autre.",
            )],
            modalities: vec!["image".into(), "text".into()],
            stream: true,
            ..Default::default()
        },
    )
    .await;
    let images = r
        .message
        .content
        .iter()
        .filter(|c| matches!(c, Content::ImageUrl { url, .. } if url.starts_with("data:image/")))
        .count();
    assert!(images >= 1, "aucune image : {:?}", r.message.text());
}

#[tokio::test]
#[ignore = "réseau : OPENROUTER_API_KEY"]
async fn the_catalog_lists_the_configured_models() {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(SystemClock);
    let d = live::daemon(dir.path(), clock).await;
    let model = live::model(
        "PENELOPE_LIVE_MODEL",
        "openrouter:deepseek/deepseek-v4-flash",
    );
    let models = d
        .provider_for(&model)
        .await
        .expect("provider")
        .fetch_models()
        .await
        .expect("catalogue");
    assert!(models.len() > 50, "{} modèles", models.len());
    let wanted = penelope_llm::catalog::strip_provider(&model);
    assert!(
        models.iter().any(|m| m.id == wanted),
        "`{wanted}` absent du catalogue"
    );
}
