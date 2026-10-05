use super::*;

fn opts() -> CodexOptions {
    CodexOptions::default()
}

fn chunks(acc: &mut ResponsesAccumulator, events: &[Value]) -> Vec<StreamChunk> {
    events
        .iter()
        .flat_map(|e| acc.push_payload(&e.to_string()))
        .collect()
}

/// Serveur simulé : répond dans l'ordre du script, garde chaque requête entière.
async fn scripted_server(script: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    tokio::spawn(async move {
        let mut queue = script.into_iter();
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut got: Vec<u8> = Vec::new();
            let mut buf = vec![0u8; 16_384];
            loop {
                let n = tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    sock.read(&mut buf),
                )
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                if let Some(head) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    let text = String::from_utf8_lossy(&got[..head]).to_lowercase();
                    let len: usize = text
                        .split("content-length:")
                        .nth(1)
                        .and_then(|r| r.split('\r').next())
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if got.len() - (head + 4) >= len {
                        break;
                    }
                }
            }
            recorder
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&got).to_string());
            let (status, body) = queue.next().unwrap_or((500, "{}".to_string()));
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), seen)
}

/// Source de jetons de test : compte les rafraîchissements.
struct FakeTokens {
    refreshes: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl TokenSource for FakeTokens {
    async fn token(&self) -> Result<CodexToken> {
        Ok(CodexToken {
            access_token: "jeton-1".into(),
            account_id: "acc_1".into(),
            plan_type: "pro".into(),
            fedramp: false,
        })
    }
    async fn refreshed(&self) -> Result<CodexToken> {
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(CodexToken {
            access_token: "jeton-2".into(),
            ..self.token().await?
        })
    }
}

fn provider(base_url: &str, refreshes: Arc<std::sync::atomic::AtomicUsize>) -> CodexProvider {
    CodexProvider::new(
        CodexOptions {
            base_url: base_url.to_string(),
            models: vec!["gpt-6-astra".into()],
            ..CodexOptions::default()
        },
        Arc::new(FakeTokens { refreshes }),
        Catalog::new(),
        "inst-1",
    )
    .unwrap()
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        session_id: Some("s-42".into()),
        messages: vec![ChatMessage::user("salut")],
        ..Default::default()
    }
}

/// #313 : le `User-Agent` reproduit `get_codex_user_agent` d'openai/codex@7f892275
/// (`codex-rs/login/src/auth/default_client.rs`), dont le test macOS attend
/// `^{originator}/\d+\.\d+\.\d+ \(Mac OS \d+\.\d+\.\d+; (x86_64|arm64)\) (\S+)$`.
#[test]
fn user_agent_matches_codex_cli() {
    let p = CodexProvider::new(
        CodexOptions {
            client_version: "0.160.0".into(),
            os: ClientOs {
                name: "Mac OS".into(),
                version: "27.0.0".into(),
                arch: "arm64".into(),
            },
            ..CodexOptions::default()
        },
        Arc::new(FakeTokens {
            refreshes: Default::default(),
        }),
        Catalog::new(),
        "inst-1",
    )
    .unwrap();
    assert_eq!(
        p.user_agent(),
        "codex_cli_rs/0.160.0 (Mac OS 27.0.0; arm64) unknown"
    );
}

/// #313 : `auto` (ou vide) prend la version du Codex CLI installé, sinon la valeur
/// intégrée ; une valeur posée gagne. Le retard se compte en versions mineures.
#[test]
fn client_version_follows_the_installed_cli() {
    assert_eq!(resolve_client_version("auto", Some("0.160.0")), "0.160.0");
    assert_eq!(resolve_client_version("", None), DEFAULT_CLIENT_VERSION);
    assert_eq!(
        resolve_client_version("0.155.0", Some("0.160.0")),
        "0.155.0"
    );
    assert_eq!(minor_versions_behind("0.104.0", "0.155.0"), Some(51));
    assert_eq!(minor_versions_behind("0.160.0", "0.155.0"), Some(0));
    assert_eq!(minor_versions_behind("0.160.0", "1.0.0"), Some(u64::MAX));
    assert_eq!(minor_versions_behind("dev", "0.155.0"), None);
}

/// #142 : la **même** identité part sur `/responses` et sur `/models` — une identité
/// incohérente vaut des heures de « servers overloaded ».
#[tokio::test]
async fn every_request_carries_the_same_identity() {
    let sse = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n\
                   data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n";
    let (url, seen) = scripted_server(vec![
        (200, sse.to_string()),
        (
            200,
            json!({"models": [{"slug": "gpt-6-astra"}]}).to_string(),
        ),
    ])
    .await;
    let p = provider(&url, Default::default());
    let _ = p
        .chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
        .await
        .expect("flux");
    p.fetch_models().await.expect("catalogue");

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    for r in &reqs {
        let lower = r.to_lowercase();
        assert!(lower.contains("originator: codex_cli_rs"), "{r}");
        assert!(lower.contains("user-agent: codex_cli_rs/"), "{r}");
        assert!(lower.contains("chatgpt-account-id: acc_1"), "{r}");
        assert!(lower.contains("x-codex-installation-id: inst-1"), "{r}");
        assert!(lower.contains("authorization: bearer jeton-1"), "{r}");
    }
    // Le catalogue est demandé pour la version de client annoncée.
    assert!(
        reqs[1].contains(&format!("client_version={DEFAULT_CLIENT_VERSION}")),
        "{}",
        reqs[1]
    );
    // La session nomme le cache de préfixe, en corps comme en en-tête.
    assert!(
        reqs[0].to_lowercase().contains("session-id: s-42"),
        "{}",
        reqs[0]
    );
}

/// #142 (décision 3) : l'abonnement ne facture pas l'appel — le coût est **connu**,
/// il vaut zéro, et n'est donc pas une estimation. Les tokens, eux, sont complets :
/// c'est d'eux que vit la compaction (#40, #136).
#[tokio::test]
async fn a_subscription_call_costs_nothing_and_says_so() {
    let sse = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n\
                   data: {\"type\":\"response.output_text.delta\",\"delta\":\"bon\"}\n\n\
                   data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\
                   \"input_tokens\":1200,\"input_tokens_details\":{\"cached_tokens\":1000},\
                   \"output_tokens\":80,\"output_tokens_details\":{\"reasoning_tokens\":60}}}}\n\n";
    let (url, _) = scripted_server(vec![(200, sse.to_string())]).await;
    let p = provider(&url, Default::default());
    let rx = p
        .chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
        .await
        .expect("flux");
    let resp = crate::provider::collect_stream_observed(
        rx,
        "codex:gpt-6-astra",
        "codex",
        &Catalog::new(),
        &|_| {},
    )
    .await
    .expect("réponse");
    assert_eq!(resp.provider, "codex");
    assert_eq!(resp.cost_usd, 0.0);
    assert!(!resp.cost_estimated, "le coût est connu : il vaut zéro");
    assert_eq!((resp.usage.prompt, resp.usage.cached), (1200, 1000));
    assert_eq!((resp.usage.completion, resp.usage.reasoning), (80, 60));
    assert_eq!(resp.message.text(), "bon");
}

/// #142 : un 401 vaut **un** rafraîchissement et **un** rejeu, pas deux.
#[tokio::test]
async fn a_401_is_refreshed_once_and_replayed_once() {
    let sse = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n\
                   data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n";
    let (url, seen) = scripted_server(vec![
        (401, json!({"error": {"message": "expiré"}}).to_string()),
        (200, sse.to_string()),
    ])
    .await;
    let refreshes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let p = provider(&url, refreshes.clone());
    p.chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
        .await
        .expect("le rejeu passe");
    assert_eq!(refreshes.load(std::sync::atomic::Ordering::SeqCst), 1);
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2, "un rejeu, pas deux");
    assert!(
        reqs[1].to_lowercase().contains("bearer jeton-2"),
        "{}",
        reqs[1]
    );

    // Un second 401 d'affilée ne se rejoue pas : c'est une erreur d'authentification.
    let (url, _) = scripted_server(vec![
        (401, "{}".to_string()),
        (401, "{}".to_string()),
        (200, sse.to_string()),
    ])
    .await;
    let p = provider(&url, Default::default());
    let e = p
        .chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
        .await
        .expect_err("401 persistant");
    assert_eq!(e.kind, LlmErrorKind::Auth);
}

/// #142 : passé le seuil, le fournisseur se met en retrait **avant** l'appel : le
/// routeur se replie au lieu d'aller chercher un 429.
#[tokio::test]
async fn a_spent_quota_steps_aside_before_calling() {
    let (url, seen) = scripted_server(vec![(200, String::new())]).await;
    let p = provider(&url, Default::default());
    p.remember(Quota {
        primary: Some(QuotaWindow {
            used_percent: 96.0,
            window_minutes: 300,
            reset_at: now_ms() / 1000 + 600,
        }),
        ..Default::default()
    });
    let e = p
        .chat_stream(request("codex:gpt-6-astra"), CancelToken::new())
        .await
        .expect_err("quota");
    assert_eq!(e.kind, LlmErrorKind::RateLimited);
    assert_eq!(e.error_type.as_deref(), Some(USAGE_LIMIT_REACHED));
    assert!(e.retry_after.unwrap_or(0) > 0);
    assert!(seen.lock().unwrap().is_empty(), "aucun appel n'est parti");
}

/// #142 : le corps est celui de l'API Responses — items typés, outils à plat, pas de
/// stockage côté serveur, cache de préfixe nommé.
#[test]
fn the_body_speaks_the_responses_dialect() {
    let req = ChatRequest {
        model: "codex:gpt-6-astra".into(),
        session_id: Some("s-42".into()),
        messages: vec![
            ChatMessage::system("Tu es Pénélope."),
            ChatMessage::user("liste les projets"),
        ],
        tools: vec![ToolDef::new(
            "fs_list",
            "liste un répertoire",
            json!({"type": "object"}),
        )],
        reasoning_effort: Some("medium".into()),
        ..Default::default()
    };
    let b = to_responses_body(&req, &opts());
    assert_eq!(
        b["model"], "gpt-6-astra",
        "le préfixe ne part pas au serveur"
    );
    assert_eq!(b["instructions"], "Tu es Pénélope.");
    assert_eq!(b["store"], false);
    assert_eq!(b["stream"], true);
    assert_eq!(b["include"][0], "reasoning.encrypted_content");
    assert_eq!(b["prompt_cache_key"], "s-42");
    assert_eq!(b["input"][0]["type"], "message");
    assert_eq!(b["input"][0]["content"][0]["type"], "input_text");
    // L'outil est à plat : pas d'objet `function` intermédiaire.
    assert_eq!(b["tools"][0]["name"], "fs_list");
    assert_eq!(b["tools"][0]["type"], "function");
    assert!(b["tools"][0].get("function").is_none());
    assert_eq!(b["tool_choice"], "auto");
    assert_eq!(b["reasoning"]["effort"], "medium");
    assert_eq!(b["reasoning"]["summary"], "auto");
    assert_eq!(b["text"]["verbosity"], "medium");
}

/// #142 : le backend est sans état — le raisonnement chiffré revient avec l'appel
/// qu'il a produit, et le résultat d'outil se rattache par `call_id`.
#[test]
fn encrypted_reasoning_and_tool_results_go_back() {
    let assistant = ChatMessage {
        tool_calls: vec![ToolCall {
            id: "call_7".into(),
            name: "fs_list".into(),
            arguments: json!({"path": "."}),
        }],
        content: vec![],
        reasoning_details: Some(json!([
            {"type": "reasoning", "id": "rs_1", "encrypted_content": "chiffré", "summary": []}
        ])),
        ..ChatMessage::assistant("")
    };
    let req = ChatRequest {
        model: "codex:gpt-6-astra".into(),
        messages: vec![
            ChatMessage::user("regarde"),
            assistant,
            ChatMessage::tool_result("call_7", "fs_list", "a.rs\nb.rs"),
            ChatMessage::user("et maintenant ?"),
        ],
        ..Default::default()
    };
    let b = to_responses_body(&req, &opts());
    let input = b["input"].as_array().expect("liste");
    assert_eq!(
        input[1]["type"], "reasoning",
        "avant l'appel qu'il a produit"
    );
    assert_eq!(input[1]["encrypted_content"], "chiffré");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_7");
    assert_eq!(input[2]["arguments"], r#"{"path":"."}"#);
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_7");
    assert_eq!(input[3]["output"], "a.rs\nb.rs");
}

/// #142 : texte, appel d'outil complet et usage, depuis les événements `response.*`.
#[test]
fn the_stream_yields_text_a_tool_call_and_usage() {
    let mut acc = ResponsesAccumulator::new("gpt-6-astra".into(), Default::default());
    let out = chunks(
        &mut acc,
        &[
            json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-6"}}),
            json!({"type": "response.output_text.delta", "delta": "bon"}),
            json!({"type": "response.reasoning_summary_text.delta", "delta": "je réfléchis"}),
            json!({"type": "response.output_item.done", "item": {
                "type": "reasoning", "id": "rs_1", "encrypted_content": "chiffré"}}),
            json!({"type": "response.output_item.done", "item": {
                "type": "function_call", "name": "fs_list",
                "arguments": "{\"path\":\".\"}", "call_id": "call_7"}}),
            json!({"type": "response.completed", "response": {"usage": {
                "input_tokens": 1200, "input_tokens_details": {"cached_tokens": 1000},
                "output_tokens": 80, "output_tokens_details": {"reasoning_tokens": 60}}}}),
        ],
    );
    assert!(matches!(
        &out[0],
        StreamChunk::Started { id, model } if id == "resp_1" && model == "gpt-6-astra"
    ));
    assert!(matches!(&out[1], StreamChunk::Delta { text } if text == "bon"));
    assert!(matches!(&out[2], StreamChunk::Reasoning { .. }));
    assert!(
        matches!(&out[3], StreamChunk::ReasoningDetails(v) if v[0]["encrypted_content"] == "chiffré")
    );
    let call = out
        .iter()
        .find_map(|c| match c {
            StreamChunk::ToolCall(t) => Some(t.clone()),
            _ => None,
        })
        .expect("appel d'outil");
    assert_eq!(call.id, "call_7", "l'identifiant vient du serveur");
    assert_eq!(call.arguments["path"], ".");
    let usage = out
        .iter()
        .find_map(|c| match c {
            StreamChunk::Usage(u) => Some(*u),
            _ => None,
        })
        .expect("usage");
    assert_eq!(
        (usage.prompt, usage.cached, usage.completion),
        (1200, 1000, 80)
    );
    assert_eq!(usage.reasoning, 60);
    assert_eq!(usage.cost_usd, Some(0.0), "l'abonnement ne facture pas");
    assert!(matches!(
        out.last(),
        Some(StreamChunk::Done {
            finish: FinishReason::ToolCalls
        })
    ));
    assert!(acc.on_eof().is_empty(), "flux clos proprement");
}

/// #142 : une fermeture sans `response.completed` est une coupure, pas une fin.
#[test]
fn a_stream_cut_before_completion_is_an_error() {
    let mut acc = ResponsesAccumulator::plain();
    let _ = chunks(
        &mut acc,
        &[json!({"type": "response.output_text.delta", "delta": "moitié"})],
    );
    assert!(matches!(
        acc.on_eof().first(),
        Some(StreamChunk::Error {
            retryable: true,
            ..
        })
    ));
}

/// #142 : `response.failed` porte la cause ; le dépassement de fenêtre et la limite de
/// sortie ne se confondent pas avec une panne.
#[test]
fn failures_carry_their_cause() {
    let mut acc = ResponsesAccumulator::plain();
    let out = chunks(
        &mut acc,
        &[json!({"type": "response.failed", "response": {"error": {
            "code": "context_length_exceeded", "message": "trop long"}}})],
    );
    assert!(matches!(
        &out[0],
        StreamChunk::Error { error_type, retryable: false, .. }
            if error_type.as_deref() == Some("context_length_exceeded")
    ));

    let mut acc = ResponsesAccumulator::plain();
    let out = chunks(
        &mut acc,
        &[json!({"type": "response.incomplete", "response": {
            "incomplete_details": {"reason": "max_output_tokens"},
            "usage": {"input_tokens": 10, "output_tokens": 5}}})],
    );
    assert!(matches!(
        out.last(),
        Some(StreamChunk::Done {
            finish: FinishReason::Length
        })
    ));
}

/// #142 : un 429 de quota n'est pas une panne — il dit quand revenir et ne se rejoue
/// pas (issue #139).
#[test]
fn a_quota_429_says_when_to_come_back() {
    let resets = now_ms() / 1000 + 3_600;
    let body = json!({"error": {"type": "usage_limit_reached", "plan_type": "plus",
                                "resets_at": resets, "message": "limite atteinte"}});
    let e = codex_error(429, &body.to_string(), &reqwest::header::HeaderMap::new());
    assert_eq!(e.kind, LlmErrorKind::RateLimited);
    assert_eq!(e.error_type.as_deref(), Some(USAGE_LIMIT_REACHED));
    assert!(
        (3_500..=3_600).contains(&e.retry_after.unwrap_or(0)),
        "{:?}",
        e.retry_after
    );
    assert!(e.message.contains("quota ChatGPT"));

    // Un 403 nomme la cause probable : l'identité empruntée n'est plus acceptée.
    let e = codex_error(403, "{}", &reqwest::header::HeaderMap::new());
    assert_eq!(e.kind, LlmErrorKind::Auth);
    assert!(e.message.contains("originator"), "{}", e.message);
}

/// #142 : les jauges du plan se lisent en en-tête comme en événement.
#[test]
fn quota_is_read_from_headers_and_events() {
    let mut h = reqwest::header::HeaderMap::new();
    for (k, v) in [
        ("x-codex-primary-used-percent", "42.5"),
        ("x-codex-primary-window-minutes", "300"),
        ("x-codex-primary-reset-at", "1790000000"),
        ("x-codex-secondary-used-percent", "12"),
        ("x-codex-active-limit", "primary"),
    ] {
        h.insert(k, v.parse().unwrap());
    }
    let q = quota_from_headers(&h, 7);
    let p = q.primary.expect("fenêtre principale");
    assert_eq!(
        (p.used_percent, p.window_minutes, p.reset_at),
        (42.5, 300, 1_790_000_000)
    );
    assert_eq!(q.active_limit, "primary");
    assert_eq!(q.read_at_ms, 7);
    assert!((q.worst_ratio() - 0.425).abs() < 1e-9);

    let e = json!({"type": "codex.rate_limits", "plan_type": "pro", "rate_limits": {
        "primary": {"used_percent": 80.0, "window_minutes": 300, "reset_at": 1790000000},
        "secondary": {"used_percent": 5.0, "window_minutes": 10080, "reset_at": 1791000000}}});
    let q = quota_from_event(&e, 9);
    assert_eq!(q.plan_type, "pro");
    assert_eq!(q.primary.unwrap().used_percent, 80.0);
    assert_eq!(q.secondary.unwrap().window_minutes, 10_080);

    // Des en-têtes muets ne remplacent pas ce qui est déjà su.
    assert!(quota_from_headers(&reqwest::header::HeaderMap::new(), 0).is_empty());
}

/// #142 : le catalogue du plan porte la vraie fenêtre, les outils, aucun prix ; sans
/// réseau, la liste de repli suffit à router.
#[test]
fn the_catalog_has_windows_tools_and_no_price() {
    let body = json!({"models": [
        {"slug": "gpt-6-astra", "context_window": 200000, "max_context_window": 272000,
         "input_modalities": ["text", "image"],
         "supported_reasoning_levels": ["low", "medium", "high"]},
        {"slug": "gpt-5.6-terra"},
    ]});
    let models = parse_codex_models(&body);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-6-astra");
    assert_eq!(models[0].context_window, 272_000);
    assert!(models[0].supports_tools());
    assert!(models[0].accepts_images());
    assert!(models[0].supports_reasoning());
    assert_eq!(models[0].price_prompt, 0.0);
    assert_eq!(models[1].context_window, DEFAULT_CODEX_WINDOW);

    let fallback = fallback_models(&["gpt-6-astra".to_string()]);
    assert_eq!(fallback.len(), 1);
    assert!(fallback[0].supports_tools());
    assert_eq!(fallback[0].provider, "codex");
}

/// #291 : au dialecte Responses, la note `developer` est le dernier item de `input`, en
/// `input_text`, après le résultat d'outil ; sans elle, rien n'est ajouté.
#[test]
fn the_developer_note_is_the_last_input_item() {
    let plain = ChatRequest {
        model: "codex:gpt-6-astra".into(),
        messages: vec![
            ChatMessage::system("Tu es Pénélope."),
            ChatMessage::user("liste les projets"),
            ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "fs_list".into(),
                arguments: json!({}),
            }]),
            ChatMessage::tool_result("c1", "fs_list", "a/ b/"),
        ],
        ..Default::default()
    };
    let hinted = ChatRequest {
        developer_note: Some("You have 500000 tokens context window.".into()),
        ..plain.clone()
    };
    let without = to_responses_body(&plain, &opts());
    let with = to_responses_body(&hinted, &opts());
    let before = without["input"].as_array().unwrap();
    let after = with["input"].as_array().unwrap();
    assert!(before.iter().all(|i| i["role"] != "developer"));
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(&after[..before.len()], &before[..]);
    let last = after.last().unwrap();
    assert_eq!(last["type"], "message");
    assert_eq!(last["role"], "developer");
    assert_eq!(last["content"][0]["type"], "input_text");
    assert_eq!(
        last["content"][0]["text"],
        "You have 500000 tokens context window."
    );
    // Le prompt système, lui, ne bouge pas : le cache de préfixe reste chaud.
    assert_eq!(with["instructions"], without["instructions"]);
}

/// #304 : l'image d'un outil, montrée par la boucle en un message utilisateur après le
/// résultat, part au backend en `input_image` après le `function_call_output`.
#[test]
fn a_tool_image_shown_after_the_result_goes_as_input_image() {
    let assistant = ChatMessage {
        tool_calls: vec![ToolCall {
            id: "call_9".into(),
            name: "mcp__tickets__attachment".into(),
            arguments: json!({"id": 1}),
        }],
        content: vec![],
        ..ChatMessage::assistant("")
    };
    let shown = ChatMessage {
        content: vec![
            Content::text("(Images rendues par les outils ci-dessus…)"),
            Content::ImageUrl {
                url: "data:image/png;base64,iVBORw0KGgo=".into(),
                detail: None,
            },
        ],
        ..ChatMessage::user("")
    };
    let req = ChatRequest {
        model: "codex:gpt-6-astra".into(),
        messages: vec![
            ChatMessage::user("regarde la capture"),
            assistant,
            ChatMessage::tool_result("call_9", "mcp__tickets__attachment", "[image …]"),
            shown,
        ],
        ..Default::default()
    };
    let b = to_responses_body(&req, &opts());
    let input = b["input"].as_array().expect("liste");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[3]["role"], "user");
    assert_eq!(input[3]["content"][0]["type"], "input_text");
    assert_eq!(input[3]["content"][1]["type"], "input_image");
    assert_eq!(
        input[3]["content"][1]["image_url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
}

/// #311 : la surcharge se reconnaît au code comme au seul message : sans code, avec un
/// code inconnu, ou par l'événement `error` nu. Toujours réessayable, jamais `Other`.
#[test]
fn an_overload_is_retryable_whatever_says_it() {
    let msg = "Our servers are currently overloaded. Please try again later.";
    let events = [
        json!({"type": "response.failed", "response": {"error": {"message": msg}}}),
        json!({"type": "response.failed", "response": {"error": {
            "code": "server_is_overloaded", "message": "indisponible"}}}),
        json!({"type": "response.failed", "response": {"error": {
            "code": "code_inconnu", "message": msg}}}),
        json!({"type": "error", "code": null, "message": msg}),
        json!({"type": "error", "error": {"type": "service_unavailable_error", "message": msg}}),
    ];
    for event in events {
        let mut acc = ResponsesAccumulator::plain();
        let out = chunks(&mut acc, std::slice::from_ref(&event));
        let Some(StreamChunk::Error {
            message,
            retryable,
            error_type,
        }) = out.last()
        else {
            panic!("{event} : {out:?}");
        };
        assert!(*retryable, "{event}");
        assert_eq!(
            error_type.as_deref(),
            Some("provider_overloaded"),
            "{event}"
        );
        let e = LlmError::mid_stream(message.clone(), *retryable, error_type.clone());
        assert_eq!(e.kind, LlmErrorKind::Transient, "{event}");
        assert!(acc.on_eof().is_empty(), "l'échec clôt le flux : {event}");
    }

    // Une erreur qui n'est pas une surcharge garde son code et ne se rejoue pas.
    let mut acc = ResponsesAccumulator::plain();
    let out = chunks(
        &mut acc,
        &[json!({"type": "response.failed", "response": {"error": {
            "code": "invalid_prompt", "message": "refusé"}}})],
    );
    assert!(matches!(
        &out[0],
        StreamChunk::Error { error_type, retryable: false, .. }
            if error_type.as_deref() == Some("invalid_prompt")
    ));
    // Sans erreur jointe : la cause par défaut, pas le nom de l'événement.
    let mut acc = ResponsesAccumulator::plain();
    let out = chunks(&mut acc, &[json!({"type": "response.failed"})]);
    assert!(matches!(
        &out[0],
        StreamChunk::Error {
            error_type: None,
            retryable: false,
            ..
        }
    ));
}
