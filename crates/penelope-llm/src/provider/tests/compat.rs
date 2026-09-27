//! Corps de requête, embeddings, catalogues et flux des fournisseurs OpenAI-compatibles,
//! contre un faux serveur qui répond selon le chemin demandé.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Faux serveur à plusieurs connexions : chaque requête reçoit la réponse de la première
/// route dont le chemin est préfixe ; les requêtes reçues sont rendues, dans l'ordre.
async fn routed_server(
    routes: Vec<(&'static str, String)>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut got = Vec::new();
            let mut buf = vec![0u8; 65536];
            // Lit les en-têtes, puis le corps annoncé par Content-Length.
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&got).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
            log.lock().unwrap().push(req);
            let resp = routes
                .iter()
                .find(|(p, _)| path.contains(p))
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                        .to_string()
                });
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}/v1"), seen)
}

fn json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sse_response(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}")
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![ChatMessage::user("x")],
        ..Default::default()
    }
}

/// Outils, choix d'outil, température, plafond, format et modalités passent tous dans le
/// corps, sous leurs noms OpenAI.
#[test]
fn the_openai_body_carries_every_optional_field() {
    let req = ChatRequest {
        model: "openai_compat:qwen".into(),
        messages: vec![ChatMessage::user("x")],
        tools: vec![ToolDef::new(
            "fs_read",
            "Lit un fichier",
            json!({"type": "object"}),
        )],
        tool_choice: Some(ToolChoice::Required),
        temperature: Some(0.5),
        max_tokens: Some(256),
        response_format: Some(json!({"type": "json_object"})),
        modalities: vec!["image".into(), "text".into()],
        ..Default::default()
    };
    let b = to_openai_body(&req);
    assert_eq!(b["model"], "qwen");
    assert_eq!(b["tools"][0]["type"], "function");
    assert_eq!(b["tools"][0]["function"]["name"], "fs_read");
    assert_eq!(
        b["tools"][0]["function"]["parameters"],
        json!({"type": "object"})
    );
    assert_eq!(b["tool_choice"], "required");
    assert_eq!(b["temperature"], 0.5);
    assert_eq!(b["max_tokens"], 256);
    assert_eq!(b["response_format"], json!({"type": "json_object"}));
    assert_eq!(b["modalities"], json!(["image", "text"]));
    assert!(b.get("reasoning").is_none());

    for (choice, name) in [(ToolChoice::Auto, "auto"), (ToolChoice::None, "none")] {
        let r = ChatRequest {
            tool_choice: Some(choice),
            ..req.clone()
        };
        assert_eq!(to_openai_body(&r)["tool_choice"], name);
    }
    // Sans outils, pas de choix d'outil : le champ seul serait refusé.
    let bare = ChatRequest {
        tools: Vec::new(),
        ..req
    };
    assert!(to_openai_body(&bare).get("tool_choice").is_none());
}

/// #152 : `none` éteint le raisonnement, un budget gagne sur l'effort.
#[test]
fn reasoning_is_off_budgeted_or_an_effort() {
    let with = |effort: Option<&str>, budget: Option<u32>| {
        to_openai_body(&ChatRequest {
            reasoning_effort: effort.map(String::from),
            reasoning_max_tokens: budget,
            ..request("m")
        })["reasoning"]
            .clone()
    };
    assert_eq!(
        with(Some("none"), Some(500)),
        json!({"enabled": false, "exclude": true})
    );
    assert_eq!(with(Some("high"), Some(500)), json!({"max_tokens": 500}));
    assert_eq!(with(None, Some(500)), json!({"max_tokens": 500}));
    assert_eq!(with(Some("low"), None), json!({"effort": "low"}));
}

/// Audio et image gardent la forme « parties » ; le nom d'outil ne part qu'avec un
/// message `tool`.
#[test]
fn audio_parts_and_tool_names_are_serialised() {
    let mut m = ChatMessage::user("écoute");
    m.content.push(Content::InputAudio {
        data: "AAAA".into(),
        format: "ogg".into(),
    });
    m.content.push(Content::ImageUrl {
        url: "https://x/y.png".into(),
        detail: Some("high".into()),
    });
    m.name = Some("ignoré".into());
    let v = message_to_json(&m);
    assert_eq!(v["content"][1]["type"], "input_audio");
    assert_eq!(
        v["content"][1]["input_audio"],
        json!({"data": "AAAA", "format": "ogg"})
    );
    assert_eq!(v["content"][2]["image_url"]["detail"], "high");
    assert!(
        v.get("name").is_none(),
        "un nom hors message tool n'est pas envoyé"
    );

    let t = message_to_json(&ChatMessage::tool_result("c1", "fs_read", "contenu"));
    assert_eq!(t["name"], "fs_read");
    assert_eq!(t["tool_call_id"], "c1");
}

/// Les vecteurs reviennent dans l'ordre de `index`, pas dans celui de la réponse ; la
/// clé part en `Authorization`, le modèle sans son préfixe de fournisseur.
#[tokio::test]
async fn compat_embeddings_are_reordered_by_index() {
    let body = r#"{"data":[{"index":1,"embedding":[0.5,0.25]},{"index":0,"embedding":[1.0,0.0]}]}"#;
    let (url, seen) = routed_server(vec![("/embeddings", json_response("200 OK", body))]).await;
    let p = OpenAiCompatProvider::new(format!("{url}/"), "sk-local", Catalog::new()).unwrap();
    assert_eq!(p.base_url(), url, "la barre finale est retirée");
    let v = p
        .embed("openai_compat:nomic", &["a".into(), "b".into()])
        .await
        .unwrap();
    assert_eq!(v, vec![vec![1.0, 0.0], vec![0.5, 0.25]]);
    let raw = seen.lock().unwrap()[0].to_lowercase();
    assert!(raw.contains("authorization: bearer sk-local"), "{raw}");
    assert!(raw.contains("\"model\":\"nomic\""), "{raw}");
}

/// Un nombre de vecteurs qui ne correspond pas aux textes, une erreur HTTP ou un corps
/// sans `data` sont des erreurs, jamais des vecteurs décalés.
#[tokio::test]
async fn compat_embeddings_refuse_a_wrong_answer() {
    let one = r#"{"data":[{"embedding":[1.0]}]}"#;
    let (url, _) = routed_server(vec![("/embeddings", json_response("200 OK", one))]).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let e = p.embed("m", &["a".into(), "b".into()]).await.unwrap_err();
    assert!(
        e.message.contains("1 embeddings pour 2 textes"),
        "{}",
        e.message
    );

    let err = r#"{"error":{"message":"modèle inconnu"}}"#;
    let (url, _) = routed_server(vec![("/embeddings", json_response("404 Not Found", err))]).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let e = p.embed("m", &["a".into()]).await.unwrap_err();
    assert_eq!(e.status, Some(404));

    // Une erreur dans un 200 reste une erreur.
    let (url, _) = routed_server(vec![("/embeddings", json_response("200 OK", err))]).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    assert!(p.embed("m", &["a".into()]).await.is_err());

    assert!(parse_embeddings(&json!({"objet": []})).is_err());
}

/// #53 : `GET /models` d'un serveur local alimente le catalogue ; la fenêtre vient de
/// l'endpoint quand il la donne, de la configuration sinon. Une entrée sans `id` est
/// ignorée.
#[tokio::test]
async fn compat_models_feed_the_catalog_with_their_window() {
    let body =
        r#"{"data":[{"id":"qwen","max_model_len":131072},{"id":"mistral"},{"object":"model"}]}"#;
    let (url, seen) = routed_server(vec![("/models", json_response("200 OK", body))]).await;
    let catalog = Catalog::new();
    let p = OpenAiCompatProvider::new(url, "sk-l", catalog.clone())
        .unwrap()
        .with_window(8_192)
        .with_window(0);
    let models = p.fetch_models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(catalog.get("qwen").unwrap().context_window, 131_072);
    assert_eq!(
        catalog.get("openai_compat:mistral").unwrap().context_window,
        8_192
    );
    assert!(
        seen.lock().unwrap()[0]
            .to_lowercase()
            .contains("authorization: bearer sk-l")
    );
}

/// Un serveur local sans clé ne reçoit pas d'en-tête `Authorization`, ni pour le chat,
/// ni pour le catalogue.
#[tokio::test]
async fn a_keyless_compat_server_gets_no_authorization() {
    let (url, seen) = routed_server(vec![
        ("/models", json_response("200 OK", r#"{"data":[]}"#)),
        (
            "/chat/completions",
            sse_response(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
            ),
        ),
    ])
    .await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    assert!(p.fetch_models().await.unwrap().is_empty());
    let rx = p
        .chat_stream(request("m"), CancelToken::new())
        .await
        .unwrap();
    let r = collect_stream(rx, "m", "openai_compat", &Catalog::new())
        .await
        .unwrap();
    assert_eq!(r.message.text(), "ok");
    for raw in seen.lock().unwrap().iter() {
        assert!(!raw.to_lowercase().contains("authorization"), "{raw}");
    }
}

/// §4.3 : un 5xx après envoi complet peut avoir été facturé ; un 4xx non.
#[tokio::test]
async fn a_server_error_may_have_been_billed() {
    let (url, _) = routed_server(vec![(
        "/chat/completions",
        json_response("502 Bad Gateway", r#"{"error":{"message":"amont tombé"}}"#),
    )])
    .await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let e = p
        .chat_stream(request("m"), CancelToken::new())
        .await
        .unwrap_err();
    assert_eq!(e.status, Some(502));
    assert!(e.maybe_billed);

    let (url, _) = routed_server(vec![(
        "/chat/completions",
        json_response("400 Bad Request", r#"{"error":{"message":"non"}}"#),
    )])
    .await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let e = p
        .chat_stream(request("m"), CancelToken::new())
        .await
        .unwrap_err();
    assert!(!e.maybe_billed);
}

/// Un serveur injoignable est une panne transitoire : la relance a un sens.
#[tokio::test]
async fn an_unreachable_server_is_a_transient_error() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let p = OpenAiCompatProvider::new(format!("http://127.0.0.1:{port}/v1"), "", Catalog::new())
        .unwrap();
    let e = p
        .chat_stream(request("m"), CancelToken::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, LlmErrorKind::Transient);
    let e = p.fetch_models().await.unwrap_err();
    assert_eq!(e.kind, LlmErrorKind::Transient);
}

/// #17 : le fournisseur amont précédent est épinglé par son slug, lu une fois sur
/// `/models/{id}/endpoints` puis gardé en cache ; les en-têtes d'identité partent avec.
#[tokio::test]
async fn openrouter_pins_the_previous_upstream_by_its_slug() {
    let endpoints = r#"{"data":{"endpoints":[{"provider_name":"Z.AI","tag":"z-ai/fp8"},{"provider_name":"Autre","tag":""}]}}"#;
    let chat =
        sse_response("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n");
    let (url, seen) = routed_server(vec![
        ("/endpoints", json_response("200 OK", endpoints)),
        ("/chat/completions", chat),
    ])
    .await;
    let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new())
        .unwrap()
        .with_identity("https://penelope.local", "Pénélope")
        .with_categories("personal-agent");
    for _ in 0..2 {
        let req = ChatRequest {
            pinned_upstream: Some("Z.AI".into()),
            ..request("openrouter:z-ai/glm-5.3")
        };
        let rx = p.chat_stream(req, CancelToken::new()).await.unwrap();
        collect_stream(rx, "z-ai/glm-5.3", "openrouter", &Catalog::new())
            .await
            .unwrap();
    }
    let seen = seen.lock().unwrap();
    let endpoints_calls = seen.iter().filter(|r| r.contains("/endpoints")).count();
    assert_eq!(endpoints_calls, 1, "le slug est gardé en cache");
    let chat = seen
        .iter()
        .rfind(|r| r.contains("/chat/completions"))
        .unwrap();
    assert!(chat.contains("\"order\":[\"z-ai\"]"), "{chat}");
    let lower = chat.to_lowercase();
    assert!(
        lower.contains("http-referer: https://penelope.local"),
        "{chat}"
    );
    assert!(
        lower.contains("x-openrouter-categories: personal-agent"),
        "{chat}"
    );
}

/// Un échec de lecture des endpoints n'empêche pas l'appel : il part sans épinglage.
#[tokio::test]
async fn openrouter_without_endpoints_does_not_pin() {
    let chat =
        sse_response("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n");
    let (url, seen) = routed_server(vec![
        (
            "/endpoints",
            json_response("500 Internal Server Error", "{}"),
        ),
        ("/chat/completions", chat),
    ])
    .await;
    let p = OpenRouterProvider::new(url, "k", Catalog::new()).unwrap();
    let req = ChatRequest {
        pinned_upstream: Some("Z.AI".into()),
        ..request("openrouter:z-ai/glm-5.3")
    };
    let rx = p.chat_stream(req, CancelToken::new()).await.unwrap();
    collect_stream(rx, "z-ai/glm-5.3", "openrouter", &Catalog::new())
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    let chat = seen
        .iter()
        .rfind(|r| r.contains("/chat/completions"))
        .unwrap();
    assert!(!chat.contains("\"order\""), "{chat}");
}

/// Les embeddings OpenRouter passent par le même format, avec la clé et l'identité.
#[tokio::test]
async fn openrouter_embeddings_carry_key_and_identity() {
    let body = r#"{"data":[{"embedding":[0.1,0.2]}]}"#;
    let (url, seen) = routed_server(vec![("/embeddings", json_response("200 OK", body))]).await;
    let p = OpenRouterProvider::new(url, "sk-or", Catalog::new())
        .unwrap()
        .with_identity("https://r", "T");
    let v = p
        .embed("openrouter:openai/text-embedding-3-small", &["a".into()])
        .await
        .unwrap();
    assert_eq!(v.len(), 1);
    let raw = seen.lock().unwrap()[0].to_lowercase();
    assert!(raw.contains("authorization: bearer sk-or"), "{raw}");
    assert!(raw.contains("x-openrouter-title: t"), "{raw}");
    assert!(
        raw.contains("\"model\":\"openai/text-embedding-3-small\""),
        "{raw}"
    );
}

/// Le catalogue OpenRouter refusé (clé invalide) est une erreur, et le catalogue en
/// place n'est pas remplacé par une liste vide.
#[tokio::test]
async fn a_refused_openrouter_catalog_keeps_the_previous_one() {
    let (url, _) = routed_server(vec![(
        "/models",
        json_response(
            "401 Unauthorized",
            r#"{"error":{"message":"clé invalide"}}"#,
        ),
    )])
    .await;
    let catalog = Catalog::new();
    catalog.upsert(vec![ModelInfo::minimal("a/b", "openrouter", 1000)]);
    let p = OpenRouterProvider::new(url, "k", catalog.clone()).unwrap();
    let e = p.fetch_models().await.unwrap_err();
    assert_eq!(e.status, Some(401));
    assert_eq!(catalog.len(), 1);
}

/// Un fournisseur qui ne sait que discuter.
struct ChatOnly;

#[async_trait::async_trait]
impl Provider for ChatOnly {
    fn name(&self) -> &str {
        "bavard"
    }
    async fn chat_stream(&self, _: ChatRequest, _: CancelToken) -> Result<ChunkStream> {
        Err(LlmError::new(LlmErrorKind::Other, "pas ici"))
    }
    async fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }
}

/// Embeddings, transcription et synthèse absents : une erreur de requête qui nomme le
/// fournisseur, pas un appel réseau.
#[tokio::test]
async fn a_provider_without_audio_or_vectors_says_so() {
    let p = ChatOnly;
    let e = p.embed("m", &["a".into()]).await.unwrap_err();
    assert_eq!(e.kind, LlmErrorKind::BadRequest);
    assert!(
        e.message.contains("`bavard` ne calcule pas d'embeddings"),
        "{}",
        e.message
    );
    let e = p.transcribe("m", vec![1], "a.ogg", None).await.unwrap_err();
    assert!(
        e.message.contains("ne sait pas transcrire"),
        "{}",
        e.message
    );
    let e = p.speak("m", "bonjour", "v", "wav").await.unwrap_err();
    assert!(
        e.message.contains("ne sait pas synthétiser"),
        "{}",
        e.message
    );
}

/// #41 : un texte trop long est refusé avant l'envoi ; une réponse JSON, une erreur ou
/// un audio vide ne passent jamais pour de l'audio.
#[tokio::test]
async fn speech_refuses_what_is_not_audio() {
    let p = OpenAiCompatProvider::new("http://127.0.0.1:9/v1", "", Catalog::new()).unwrap();
    let long = "a".repeat(SPEECH_MAX_CHARS + 1);
    let e = p.speak("m", &long, "v", "wav").await.unwrap_err();
    assert_eq!(e.kind, LlmErrorKind::BadRequest);
    assert!(e.message.contains("trop long"), "{}", e.message);

    let json_ok = json_response("200 OK", r#"{"error":"voix inconnue"}"#);
    let empty = "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
    let refused = "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: text/plain\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnon".to_string();
    for (resp, what) in [(json_ok, "json"), (refused, "422")] {
        let (url, _) = routed_server(vec![("/audio/speech", resp)]).await;
        let p = OpenAiCompatProvider::new(url, "k", Catalog::new()).unwrap();
        assert!(p.speak("m", "bonjour", "v", "wav").await.is_err(), "{what}");
    }
    let (url, _) = routed_server(vec![("/audio/speech", empty)]).await;
    let p = OpenAiCompatProvider::new(url, "k", Catalog::new()).unwrap();
    let e = p.speak("m", "bonjour", "v", "wav").await.unwrap_err();
    assert_eq!(e.message, "synthèse vide");
}

/// Une transcription trop grosse est refusée avant l'envoi ; un refus, un corps
/// illisible ou une erreur dans un 200 sont des erreurs ; la durée se lit aussi dans
/// `duration`.
#[tokio::test]
async fn transcription_errors_are_never_text() {
    let p = OpenAiCompatProvider::new("http://127.0.0.1:9/v1", "", Catalog::new()).unwrap();
    let e = p
        .transcribe("m", vec![0; 26 * 1024 * 1024], "a.ogg", None)
        .await
        .unwrap_err();
    assert!(e.message.contains("audio trop gros"), "{}", e.message);

    let cases = [
        json_response("401 Unauthorized", r#"{"error":"clé"}"#),
        json_response("200 OK", "<html>proxy</html>"),
        json_response("200 OK", r#"{"error":{"message":"format"}}"#),
    ];
    for resp in cases {
        let (url, _) = routed_server(vec![("/audio/transcriptions", resp)]).await;
        let p = OpenAiCompatProvider::new(url, "k", Catalog::new()).unwrap();
        assert!(p.transcribe("m", vec![1], "a.ogg", Some("")).await.is_err());
    }
    let (url, _) = routed_server(vec![(
        "/audio/transcriptions",
        json_response("200 OK", r#"{"text":"salut","duration":3.5}"#),
    )])
    .await;
    let p = OpenAiCompatProvider::new(url, "k", Catalog::new()).unwrap();
    let t = p.transcribe("m", vec![1], "a.ogg", None).await.unwrap();
    assert_eq!(t.seconds, Some(3.5));
    assert_eq!(t.cost_usd, None);
}
