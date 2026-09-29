//! Corps réels de `mlx_lm.server` (0.31.3, MLX 0.32.3, M2), capturés le 29/09/2026 pour
//! l'issue #259 et rejoués par le faux serveur : texte avec raisonnement séparé, appels
//! d'outils natifs (Qwen3), appels rendus en texte (Llama 3.2), erreurs et catalogue.
//!
//! Les fichiers de `fixtures/mlx_lm/` sont les octets reçus, commentaires `: keepalive`
//! compris ; seuls les en-têtes HTTP ont été retirés des erreurs.

use super::compat::{json_response, routed_server, sse_response};
use super::*;

const QWEN3_TEXT: &str = include_str!("fixtures/mlx_lm/qwen3-text.sse");
const QWEN3_TOOL: &str = include_str!("fixtures/mlx_lm/qwen3-tool.sse");
const QWEN3_TWO_TOOLS: &str = include_str!("fixtures/mlx_lm/qwen3-two-tools.sse");
const LLAMA_TOOL: &str = include_str!("fixtures/mlx_lm/llama-tool.sse");
const LLAMA_TEXT: &str = include_str!("fixtures/mlx_lm/llama-text.sse");
const ERROR_UNKNOWN_MODEL: &str = include_str!("fixtures/mlx_lm/error-unknown-model.json");
const ERROR_BAD_JSON: &str = include_str!("fixtures/mlx_lm/error-bad-json.json");
const MODELS: &str = include_str!("fixtures/mlx_lm/models.json");

fn addition() -> ToolDef {
    ToolDef::new(
        "addition",
        "Additionne deux entiers.",
        json!({
            "type": "object",
            "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
            "required": ["a", "b"]
        }),
    )
}

/// Rejoue un flux capturé à travers le fournisseur local et rend la réponse complète,
/// avec les fragments de texte tels que l'affichage les aurait reçus.
async fn replay(sse: &str, tools: Vec<ToolDef>) -> (ChatResponse, Vec<String>) {
    let (url, _) = routed_server(vec![("/chat/completions", sse_response(sse))]).await;
    let catalog = Catalog::new();
    let p = OpenAiCompatProvider::new(url, "", catalog.clone()).unwrap();
    let model = "local:mlx-community/Qwen3-1.7B-4bit";
    let rx = p
        .chat_stream(
            ChatRequest {
                model: model.into(),
                messages: vec![ChatMessage::user("x")],
                tools,
                ..Default::default()
            },
            CancelToken::new(),
        )
        .await
        .unwrap();
    let deltas = std::sync::Mutex::new(Vec::new());
    let r = collect_stream_observed(rx, model, p.name(), &catalog, &|c| {
        if let StreamChunk::Delta { text } = c {
            deltas.lock().unwrap().push(text.clone());
        }
    })
    .await
    .unwrap();
    (r, deltas.into_inner().unwrap())
}

/// Qwen3 : raisonnement en `delta.reasoning`, réponse en `content`, `usage` en dernier
/// fragment (`choices: []`) avec `prompt_tokens_details.cached_tokens` ; coût nul,
/// puisque le modèle local n'a pas de prix au catalogue.
#[tokio::test]
async fn a_qwen3_text_stream_keeps_reasoning_usage_and_costs_nothing() {
    let (r, _) = replay(QWEN3_TEXT, vec![]).await;
    assert_eq!(r.message.text().trim(), "pong");
    assert!(r.reasoning.starts_with("\nOkay"), "{:?}", r.reasoning);
    assert_eq!(r.finish, FinishReason::Stop);
    assert_eq!(r.model, "mlx-community/Qwen3-1.7B-4bit");
    assert_eq!((r.usage.prompt, r.usage.completion), (18, 89));
    assert_eq!(r.usage.cached, 0);
    assert_eq!(r.usage.cost_usd, None, "mlx_lm ne facture rien");
    assert_eq!(r.cost_usd, 0.0);
    assert!(r.message.tool_calls.is_empty());
}

/// Qwen3 : l'appel arrive d'un bloc (nom, arguments complets, identifiant UUID) et le
/// flux finit en `tool_calls`. Le texte blanc qui le précède ne gêne pas.
#[tokio::test]
async fn qwen3_native_tool_calls_are_read_as_is() {
    let (r, _) = replay(QWEN3_TOOL, vec![addition()]).await;
    assert_eq!(r.finish, FinishReason::ToolCalls);
    assert_eq!(r.message.tool_calls.len(), 1);
    let c = &r.message.tool_calls[0];
    assert_eq!(c.name, "addition");
    assert_eq!(c.id, "e4c4b2ce-9ebd-4ca6-9e34-99607789d53d");
    assert_eq!(c.arguments, json!({"a": 17, "b": 25}));
    assert_eq!(r.usage.cached, 1);

    let (two, _) = replay(QWEN3_TWO_TOOLS, vec![addition()]).await;
    let args: Vec<_> = two
        .message
        .tool_calls
        .iter()
        .map(|c| c.arguments.clone())
        .collect();
    assert_eq!(args, vec![json!({"a": 1, "b": 2}), json!({"a": 3, "b": 4})]);
}

/// #259 : Llama 3.2 sous mlx_lm.server rend son appel en texte (le serveur journalise
/// « model does not support tool calling »), `<|python_tag|>` en tête, `parameters` au
/// lieu d'`arguments`, et finit en `stop`. Sans normalisation, la boucle affichait
/// l'appel au propriétaire comme une réponse. Il devient un vrai appel, et rien n'en
/// part à l'affichage.
#[tokio::test]
async fn a_llama_call_rendered_as_text_becomes_a_tool_call() {
    let (r, deltas) = replay(LLAMA_TOOL, vec![addition()]).await;
    assert_eq!(r.finish, FinishReason::ToolCalls);
    assert_eq!(
        r.message.text(),
        "",
        "le texte de l'appel n'est pas une réponse"
    );
    assert!(deltas.is_empty(), "rien d'affiché : {deltas:?}");
    assert_eq!(r.message.tool_calls.len(), 1);
    let c = &r.message.tool_calls[0];
    assert_eq!(c.name, "addition");
    // Les entiers arrivent en chaînes : c'est au contrôle des arguments de le dire au
    // modèle, pas au décodage de les deviner.
    assert_eq!(c.arguments, json!({"a": "17", "b": "25"}));
    assert!(!c.id.is_empty());

    // La même forme sans `<|python_tag|>` : l'objet nu, rendu alors qu'on demandait
    // « sans outil ».
    let (bare, _) = replay(LLAMA_TEXT, vec![addition()]).await;
    assert_eq!(bare.finish, FinishReason::ToolCalls);
    assert_eq!(
        bare.message.tool_calls[0].arguments,
        json!({"a": "5", "b": "3"})
    );
}

/// Sans outil déclaré, ou pour un outil inconnu de la requête, le même texte reste du
/// texte : la normalisation ne fabrique pas d'appel que personne n'a proposé.
#[tokio::test]
async fn a_text_call_is_kept_as_text_without_a_matching_tool() {
    let (r, deltas) = replay(LLAMA_TOOL, vec![]).await;
    assert!(r.message.tool_calls.is_empty());
    assert!(r.message.text().starts_with("<|python_tag|>{\"name\""));
    assert!(!deltas.is_empty());
    assert_eq!(r.finish, FinishReason::Stop);

    let other = ToolDef::new("soustraction", "…", json!({"type": "object"}));
    let (r, deltas) = replay(LLAMA_TEXT, vec![other]).await;
    assert!(r.message.tool_calls.is_empty());
    assert_eq!(
        deltas.concat(),
        r#"{"name": "addition", "parameters": {"a": "5", "b": "3"}}"#,
        "le texte retenu repart en entier"
    );
}

/// Une réponse ordinaire, outils déclarés, part en direct : seul un début qui peut être
/// un appel (`{`, `<|python_tag|>`) est retenu.
#[tokio::test]
async fn ordinary_text_still_streams_when_tools_are_declared() {
    let (r, deltas) = replay(QWEN3_TEXT, vec![addition()]).await;
    assert_eq!(r.message.text().trim(), "pong");
    // Le blanc de tête attend le premier mot, puis tout part en direct.
    assert_eq!(deltas, ["\n\npong"]);
    assert_eq!(r.finish, FinishReason::Stop);
}

/// #259 : mlx_lm.server rend ses erreurs en `{"error": "…"}`, une chaîne et non l'objet
/// d'OpenRouter. Le message était perdu (« erreur du provider ») : il est gardé, et le
/// 404 d'un modèle absent du cache reste un modèle inconnu.
#[test]
fn mlx_string_errors_keep_their_message() {
    let e = LlmError::from_status(404, ERROR_UNKNOWN_MODEL);
    assert_eq!(e.kind, LlmErrorKind::UnknownModel);
    assert!(
        e.message
            .starts_with("Cannot find an appropriate cached snapshot"),
        "{}",
        e.message
    );
    let e = LlmError::from_status(400, ERROR_BAD_JSON);
    assert_eq!(e.kind, LlmErrorKind::BadRequest);
    assert!(
        e.message.starts_with("Invalid JSON in request body"),
        "{}",
        e.message
    );
}

/// La même forme au milieu d'un flux garde aussi son message.
#[test]
fn a_mid_stream_string_error_keeps_its_message() {
    let mut acc = crate::sse::StreamAccumulator::new();
    let out = acc.push_payload(r#"{"error": "Metal out of memory"}"#);
    match &out[..] {
        [StreamChunk::Error { message, .. }] => assert_eq!(message, "Metal out of memory"),
        other => panic!("{other:?}"),
    }
}

/// Le catalogue de mlx_lm.server liste le cache Hugging Face, sans fenêtre : c'est la
/// fenêtre configurée qui vaut.
#[tokio::test]
async fn the_mlx_catalog_lists_the_cache_with_the_configured_window() {
    let (url, _) = routed_server(vec![("/models", json_response("200 OK", MODELS))]).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new())
        .unwrap()
        .with_window(40_960);
    let models = p.fetch_models().await.unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "mlx-community/Qwen3-1.7B-4bit",
            "mlx-community/Llama-3.2-1B-Instruct-4bit"
        ]
    );
    assert!(models.iter().all(|m| m.context_window == 40_960));
}

/// `doctor` lit ce que le serveur sert sans toucher au catalogue ; une fenêtre annoncée
/// (llama.cpp, vLLM) est rendue, celle de mlx_lm.server manque. Serveur arrêté : erreur
/// transitoire, comme au tour.
#[tokio::test]
async fn served_models_are_read_without_the_catalog() {
    let (url, _) = routed_server(vec![("/models", json_response("200 OK", MODELS))]).await;
    let catalog = Catalog::new();
    let p = OpenAiCompatProvider::new(url, "", catalog.clone()).unwrap();
    let served = p
        .served_models(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(served.len(), 2);
    assert_eq!(served[0].id, "mlx-community/Qwen3-1.7B-4bit");
    assert_eq!(served[0].window, None);
    assert!(catalog.is_empty());

    let llama_cpp = r#"{"data":[{"id":"qwen3","meta":{"n_ctx":40960}}]}"#;
    let (url, _) = routed_server(vec![("/models", json_response("200 OK", llama_cpp))]).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let served = p
        .served_models(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(served[0].window, Some(40_960));

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = closed.local_addr().unwrap();
    drop(closed);
    let p = OpenAiCompatProvider::new(format!("http://{addr}/v1"), "", Catalog::new()).unwrap();
    let e = p
        .served_models(std::time::Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(e.kind, LlmErrorKind::Transient);
}
