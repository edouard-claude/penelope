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
