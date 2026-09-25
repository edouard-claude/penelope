use super::*;

mod server;

/// #142 : un modèle `codex:` n'est servi que par le fournisseur Codex. Sans compte
/// connecté, aucun provider : le routeur se replie sur l'alias suivant plutôt que
/// d'envoyer `codex:gpt-6-astra` à OpenRouter comme nom de modèle.
#[test]
fn a_codex_model_never_falls_back_to_another_provider() {
    let catalog = Catalog::new();
    let set = ProviderSet {
        openrouter: Some(Arc::new(
            OpenRouterProvider::new("https://x", "k", catalog.clone()).unwrap(),
        )),
        compat: Some(Arc::new(
            OpenAiCompatProvider::new("http://127.0.0.1:1", "", catalog.clone()).unwrap(),
        )),
        codex: None,
        catalog,
    };
    assert!(set.get("codex:gpt-6-astra").is_none());
    assert_eq!(
        set.get("openrouter:a/b").map(|p| p.name().to_string()),
        Some("openrouter".into())
    );
    assert_eq!(
        set.get("openai_compat:x").map(|p| p.name().to_string()),
        Some("openai_compat".into())
    );
}

#[test]
fn reasoning_goes_back_only_with_tool_calls() {
    let with_reasoning = |text: &str| ChatMessage {
        reasoning: Some(format!("pensée {text}")),
        reasoning_details: Some(json!([{"type":"reasoning.text","text":text,"index":0}])),
        ..ChatMessage::assistant(text)
    };
    let req = ChatRequest {
        model: "openrouter:a/b".into(),
        messages: vec![
            ChatMessage::user("ancien"),
            ChatMessage {
                tool_calls: vec![ToolCall {
                    id: "c0".into(),
                    name: "fs_list".into(),
                    arguments: json!({}),
                }],
                content: vec![],
                ..with_reasoning("appel ancien")
            },
            ChatMessage::tool_result("c0", "fs_list", "liste"),
            with_reasoning("ancienne réponse"),
            ChatMessage::user("nouveau"),
            ChatMessage {
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    arguments: json!({}),
                }],
                content: vec![],
                ..with_reasoning("appel")
            },
            ChatMessage::tool_result("c1", "fs_read", "contenu"),
        ],
        ..Default::default()
    };
    let b = to_openai_body(&req);
    assert_eq!(
        b["messages"][1]["reasoning_details"][0]["text"], "appel ancien",
        "un appel d'outil d'un tour précédent garde le sien : le préfixe ne bouge pas"
    );
    assert!(
        b["messages"][3].get("reasoning_details").is_none(),
        "réponse finale : rien"
    );
    assert_eq!(b["messages"][5]["reasoning_details"][0]["text"], "appel");
    assert!(
        b["messages"][5].get("reasoning").is_none(),
        "les blocs priment sur le texte"
    );
}

#[test]
fn empty_content_is_never_an_empty_array() {
    let call = ChatMessage {
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "fs_read".into(),
            arguments: json!({}),
        }],
        content: vec![],
        ..ChatMessage::assistant("")
    };
    let req = ChatRequest {
        model: "openrouter:a/b".into(),
        messages: vec![
            call,
            ChatMessage {
                content: vec![],
                ..ChatMessage::user("")
            },
        ],
        ..Default::default()
    };
    let b = to_openai_body(&req);
    assert!(b["messages"][0]["content"].is_null(), "{b}");
    assert_eq!(b["messages"][1]["content"], "");
}

#[test]
fn body_uses_string_content_for_plain_text() {
    let req = ChatRequest {
        model: "openrouter:a/b".into(),
        messages: vec![ChatMessage::user("salut")],
        ..Default::default()
    };
    let b = to_openai_body(&req);
    assert_eq!(b["model"], "a/b", "le préfixe de provider est retiré");
    assert_eq!(b["messages"][0]["content"], "salut");
    assert_eq!(b["stream"], true);
}

#[test]
fn cache_marker_emits_cache_control() {
    let req = ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::system("préfixe stable").cached()],
        ..Default::default()
    };
    let b = to_openai_body(&req);
    assert_eq!(
        b["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn tool_calls_are_serialised_with_string_arguments() {
    let m = ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
        id: "c1".into(),
        name: "fs_read".into(),
        arguments: json!({"path":"a.rs"}),
    }]);
    let v = message_to_json(&m);
    assert_eq!(v["tool_calls"][0]["function"]["name"], "fs_read");
    assert_eq!(
        v["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
        "{\"path\":\"a.rs\"}"
    );
}

#[test]
fn tool_result_message_carries_id_and_name() {
    let v = message_to_json(&ChatMessage::tool_result("c1", "fs_read", "contenu"));
    assert_eq!(v["role"], "tool");
    assert_eq!(v["tool_call_id"], "c1");
    assert_eq!(v["name"], "fs_read");
}

#[test]
fn images_use_the_parts_form() {
    let m = ChatMessage {
        role: Role::User,
        content: vec![
            Content::text("que vois-tu ?"),
            Content::ImageUrl {
                url: "data:image/png;base64,AA".into(),
                detail: None,
            },
        ],
        tool_calls: vec![],
        tool_call_id: None,
        name: None,
        cache_marker: false,
        reasoning: None,
        reasoning_details: None,
    };
    let v = message_to_json(&m);
    assert_eq!(v["content"][0]["type"], "text");
    assert_eq!(v["content"][1]["type"], "image_url");
    assert_eq!(v["content"][1]["image_url"]["detail"], "auto");
}

#[test]
fn reasoning_effort_is_forwarded() {
    let req = ChatRequest {
        model: "m".into(),
        messages: vec![],
        reasoning_effort: Some("high".into()),
        ..Default::default()
    };
    assert_eq!(to_openai_body(&req)["reasoning"]["effort"], "high");

    // #152 : `none` éteint le raisonnement, il ne le règle pas au plus bas. Envoyé
    // comme un effort, `deepseek-v4-flash` continuait de réfléchir jusqu'à épuiser
    // `max_tokens` sans rien écrire.
    let off = ChatRequest {
        model: "deepseek/deepseek-v4-flash".into(),
        messages: vec![],
        reasoning_effort: Some("none".into()),
        ..Default::default()
    };
    let body = to_openai_body(&off);
    assert_eq!(body["reasoning"]["enabled"], false);
    assert_eq!(body["reasoning"]["exclude"], true);
    assert!(
        body["reasoning"]["effort"].is_null(),
        "pas d'effort quand le raisonnement est coupé : {}",
        body["reasoning"]
    );

    // #152 : gardé, le raisonnement reçoit son propre budget, et `max_tokens` porte
    // la somme — sinon la réflexion mange la place de la réponse.
    let budgeted = ChatRequest {
        model: "deepseek/deepseek-v4-flash".into(),
        messages: vec![],
        reasoning_max_tokens: Some(8_000),
        max_tokens: Some(10_600),
        ..Default::default()
    };
    let body = to_openai_body(&budgeted);
    assert_eq!(body["reasoning"]["max_tokens"], 8_000);
    assert_eq!(body["max_tokens"], 10_600);
    assert!(
        body["reasoning"]["effort"].is_null() && body["reasoning"]["enabled"].is_null(),
        "un budget se suffit : {}",
        body["reasoning"]
    );

    // L'extinction gagne sur le budget : `off` ne doit pas réserver des jetons de
    // réflexion.
    let both = ChatRequest {
        model: "m".into(),
        messages: vec![],
        reasoning_effort: Some("none".into()),
        reasoning_max_tokens: Some(8_000),
        ..Default::default()
    };
    assert_eq!(to_openai_body(&both)["reasoning"]["enabled"], false);
}

#[test]
fn cancel_token_propagates() {
    let t = CancelToken::new();
    let c = t.child();
    assert!(!c.is_cancelled());
    t.cancel();
    assert!(c.is_cancelled());
}

#[tokio::test]
async fn collect_stream_builds_a_response() {
    let (tx, rx) = mpsc::channel(8);
    let catalog = Catalog::new();
    catalog.upsert(vec![{
        let mut m = ModelInfo::minimal("a/b", "openrouter", 128_000);
        m.price_prompt = 1e-6;
        m.price_completion = 2e-6;
        m
    }]);
    tokio::spawn(async move {
        tx.send(StreamChunk::Started {
            id: "id1".into(),
            model: "a/b".into(),
        })
        .await
        .unwrap();
        tx.send(StreamChunk::Delta {
            text: "bonjour".into(),
        })
        .await
        .unwrap();
        tx.send(StreamChunk::Usage(Usage {
            prompt: 1000,
            completion: 10,
            ..Default::default()
        }))
        .await
        .unwrap();
        tx.send(StreamChunk::Done {
            finish: FinishReason::Stop,
        })
        .await
        .unwrap();
    });
    let r = collect_stream(rx, "a/b", "openrouter", &catalog)
        .await
        .unwrap();
    assert_eq!(r.message.text(), "bonjour");
    assert_eq!(r.usage.prompt, 1000);
    assert!((r.cost_usd - (1000.0 * 1e-6 + 10.0 * 2e-6)).abs() < 1e-12);
    assert!(
        r.cost_estimated,
        "sans `usage.cost`, le coût vient du catalogue"
    );
}

#[tokio::test]
async fn billed_cost_wins_over_the_catalog() {
    let (tx, rx) = mpsc::channel(8);
    let catalog = Catalog::new();
    catalog.upsert(vec![{
        let mut m = ModelInfo::minimal("a/b", "openrouter", 128_000);
        m.price_prompt = 1e-6;
        m
    }]);
    tokio::spawn(async move {
        for c in [
            StreamChunk::Started {
                id: "gen-1".into(),
                model: "a/b".into(),
            },
            StreamChunk::Meta {
                upstream: Some("Z.AI".into()),
                native_finish: None,
            },
            StreamChunk::Refusal { text: "non".into() },
            StreamChunk::Meta {
                upstream: None,
                native_finish: Some("refusal".into()),
            },
            StreamChunk::Usage(Usage {
                prompt: 1000,
                completion: 10,
                cost_usd: Some(0.5),
                ..Default::default()
            }),
            StreamChunk::Done {
                finish: FinishReason::ContentFilter,
            },
        ] {
            tx.send(c).await.unwrap();
        }
    });
    let r = collect_stream(rx, "a/b", "openrouter", &catalog)
        .await
        .unwrap();
    assert_eq!(r.cost_usd, 0.5);
    assert!(!r.cost_estimated);
    assert_eq!(r.upstream.as_deref(), Some("Z.AI"));
    assert_eq!(r.native_finish.as_deref(), Some("refusal"));
    assert_eq!(r.refusal.as_deref(), Some("non"));
}

fn openrouter() -> OpenRouterProvider {
    OpenRouterProvider::new("http://127.0.0.1:9/api/v1", "sk-or-v1-test", Catalog::new()).unwrap()
}

#[test]
fn openrouter_body_carries_session_and_fallbacks() {
    let req = ChatRequest {
        model: "openrouter:z-ai/glm-5.3".into(),
        messages: vec![ChatMessage::user("salut")],
        session_id: Some("01J9SESSION".into()),
        fallback_models: vec![
            "openrouter:z-ai/glm-5.3".into(),
            "openrouter:deepseek/deepseek-v4-flash".into(),
            "openai_compat:local".into(),
            "openrouter:deepseek/deepseek-v4-flash".into(),
        ],
        ..Default::default()
    };
    let b = openrouter().body(&req, None);
    assert_eq!(b["model"], "z-ai/glm-5.3");
    assert_eq!(b["session_id"], "01J9SESSION");
    assert_eq!(b["models"], json!(["deepseek/deepseek-v4-flash"]));
    assert!(
        b.get("usage").is_none(),
        "l'usage arrive toujours, rien à demander"
    );
    assert!(b.get("provider").is_none(), "pas de préférences par défaut");

    let long = ChatRequest {
        model: "a/b".into(),
        session_id: Some("x".repeat(400)),
        ..Default::default()
    };
    let b = openrouter().body(&long, None);
    assert_eq!(b["session_id"].as_str().unwrap().len(), 256);
    assert!(b.get("models").is_none());
}

/// Issue #17 : le fournisseur amont de l'appel précédent passe en tête de
/// `provider.order`, sauf ordre imposé par la configuration.
#[test]
fn the_previous_upstream_is_pinned_unless_routing_is_configured() {
    let slugs = endpoint_slugs(&json!({"data": {"endpoints": [
        {"provider_name": "Z.AI", "tag": "z-ai/fp8"},
        {"provider_name": "Sail Research", "tag": "sail-research"},
        {"provider_name": "sans tag"}
    ]}}));
    assert_eq!(slugs.get("z.ai").map(String::as_str), Some("z-ai"));
    assert_eq!(
        slugs.get("sail research").map(String::as_str),
        Some("sail-research")
    );
    assert_eq!(slugs.len(), 2);

    let req = ChatRequest {
        model: "openrouter:z-ai/glm-5.3".into(),
        messages: vec![ChatMessage::user("salut")],
        ..Default::default()
    };
    let b = openrouter().body(&req, Some("z-ai"));
    assert_eq!(b["provider"], json!({"order": ["z-ai"]}));
    let configured = openrouter().with_routing(json!({"order": ["together"], "sort": "price"}));
    assert_eq!(
        configured.body(&req, Some("z-ai"))["provider"]["order"],
        json!(["together"])
    );
    let sorted = openrouter().with_routing(json!({"sort": "price"}));
    assert_eq!(
        sorted.body(&req, Some("z-ai"))["provider"],
        json!({"sort": "price", "order": ["z-ai"]})
    );
}

/// #56, #57 : un jeton enfant suit son parent sans l'entraîner.
#[test]
fn a_child_token_follows_its_parent_but_not_the_other_way() {
    let run = CancelToken::new();
    let step = run.child();
    let sibling = run.child();
    step.cancel();
    assert!(step.is_cancelled(), "l'étape est arrêtée");
    assert!(!run.is_cancelled(), "le run continue");
    assert!(!sibling.is_cancelled(), "la sœur continue");

    let grandchild = sibling.child();
    run.cancel();
    assert!(sibling.is_cancelled(), "le run arrête ses enfants");
    assert!(grandchild.is_cancelled(), "et leurs enfants");
}

/// #53 : sans `stream_options.include_usage`, un serveur local ne renvoie jamais
/// `usage` en streaming : plus de comptage ni de compaction sur la taille réelle.
#[test]
fn the_openai_body_asks_for_usage_in_the_stream() {
    let body = to_openai_body(&ChatRequest {
        model: "openai_compat:qwen".into(),
        messages: vec![ChatMessage::user("salut")],
        ..Default::default()
    });
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

/// #53 : la fenêtre d'un modèle local vient de l'endpoint quand il la donne, de la
/// configuration sinon.
#[test]
fn a_local_model_window_comes_from_the_endpoint_or_the_configuration() {
    assert_eq!(
        local_window(&json!({"id": "qwen", "context_length": 131072}), 32_768),
        131_072
    );
    assert_eq!(
        local_window(&json!({"id": "q", "meta": {"n_ctx": 8192}}), 32_768),
        8_192
    );
    assert_eq!(local_window(&json!({"id": "q"}), 120_000), 120_000);
    assert_eq!(
        local_window(&json!({"id": "q", "max_model_len": 0}), 32_768),
        32_768
    );
}

#[tokio::test]
async fn collect_stream_propagates_errors() {
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        tx.send(StreamChunk::Error {
            message: "surcharge".into(),
            retryable: true,
            error_type: None,
        })
        .await
        .unwrap();
    });
    let e = collect_stream(rx, "m", "p", &Catalog::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, LlmErrorKind::Transient);
}
