//! Tests des fournisseurs contre un faux serveur HTTP local.

use super::*;

/// Faux serveur HTTP : répond une fois avec les octets fournis, puis garde la
/// connexion ouverte le temps indiqué.
async fn one_shot_server(response: String, hold: std::time::Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 65536];
        let _ = sock.read(&mut buf).await;
        sock.write_all(response.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();
        tokio::time::sleep(hold).await;
    });
    format!("http://{addr}/api/v1")
}

#[tokio::test]
async fn rate_limits_keep_the_retry_after_hint() {
    let body = r#"{"error":{"code":429,"message":"Rate limit exceeded","metadata":{"error_type":"rate_limit_exceeded"}}}"#;
    let resp = format!(
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let url = one_shot_server(resp, std::time::Duration::from_millis(10)).await;
    let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
    let err = p
        .chat_stream(
            ChatRequest {
                model: "a/b".into(),
                messages: vec![ChatMessage::user("x")],
                ..Default::default()
            },
            CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, LlmErrorKind::RateLimited);
    assert_eq!(err.retry_after, Some(7));
}

#[tokio::test]
async fn a_documented_stream_is_decoded_end_to_end() {
    let events = [
        ": OPENROUTER PROCESSING",
        r#"data: {"id":"gen-1","object":"chat.completion.chunk","created":1,"model":"z-ai/glm-5.3","provider":"Z.AI","choices":[{"index":0,"delta":{"role":"assistant","content":"Bon"},"finish_reason":null}]}"#,
        r#"data: {"id":"gen-1","object":"chat.completion.chunk","created":1,"model":"z-ai/glm-5.3","provider":"Z.AI","choices":[{"index":0,"delta":{"content":"jour"},"finish_reason":"stop","native_finish_reason":"stop"}]}"#,
        r#"data: {"id":"gen-1","object":"chat.completion.chunk","created":1,"model":"z-ai/glm-5.3","provider":"Z.AI","choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop","native_finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15,"cost":0.00042}}"#,
        "data: [DONE]",
    ];
    let sse: String = events.iter().map(|e| format!("{e}\n\n")).collect();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{sse}"
    );
    let url = one_shot_server(resp, std::time::Duration::from_millis(10)).await;
    let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
    let rx = p
        .chat_stream(
            ChatRequest {
                model: "openrouter:z-ai/glm-5.3".into(),
                messages: vec![ChatMessage::user("x")],
                ..Default::default()
            },
            CancelToken::new(),
        )
        .await
        .unwrap();
    let r = collect_stream(rx, "z-ai/glm-5.3", "openrouter", &Catalog::new())
        .await
        .unwrap();
    assert_eq!(r.message.text(), "Bonjour");
    assert_eq!(r.id, "gen-1");
    assert_eq!(r.cost_usd, 0.00042);
    assert!(!r.cost_estimated);
    assert_eq!(r.upstream.as_deref(), Some("Z.AI"));
}

/// Faux serveur qui capture la requête reçue avant de répondre.
async fn capturing_server(response: String) -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut got = Vec::new();
        let mut buf = vec![0u8; 65536];
        // Lit jusqu'à la fin du corps multipart (délimiteur final `--\r\n`).
        loop {
            let n =
                tokio::time::timeout(std::time::Duration::from_millis(500), sock.read(&mut buf))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or(0);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
            if got.ends_with(b"--\r\n") {
                break;
            }
        }
        sock.write_all(response.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();
        let _ = tx.send(got);
    });
    (format!("http://{addr}/v1"), rx)
}

#[tokio::test]
async fn local_whisper_transcription_uses_the_openai_multipart_form() {
    let body = r#"{"text":" Bonjour Pénélope, rappelle-moi demain. "}"#;
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, seen) = capturing_server(resp).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let t = p
        .transcribe(
            "openai_compat:whisper",
            b"OggS-fake-audio".to_vec(),
            "voice.ogg",
            Some("fr"),
        )
        .await
        .unwrap();
    assert_eq!(t.text, "Bonjour Pénélope, rappelle-moi demain.");
    assert_eq!(t.cost_usd, None);

    let raw = String::from_utf8_lossy(&seen.await.unwrap()).to_string();
    assert!(raw.starts_with("POST /v1/audio/transcriptions"), "{raw}");
    assert!(raw.contains("multipart/form-data"), "{raw}");
    assert!(
        raw.contains("name=\"file\"; filename=\"voice.ogg\""),
        "{raw}"
    );
    assert!(raw.contains("name=\"language\"\r\n\r\nfr"), "{raw}");
    assert!(raw.contains("name=\"model\"\r\n\r\nwhisper"), "{raw}");
    assert!(
        !raw.to_lowercase().contains("authorization"),
        "pas de clé : pas d'en-tête"
    );
}

/// Issue #41 : la synthèse locale envoie modèle, texte, voix et format, et rend l'audio.
#[tokio::test]
async fn local_speech_posts_json_and_returns_audio_bytes() {
    let wav = b"RIFF\x24\x00\x00\x00WAVEfmt ";
    let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        wav.len()
    )
    .into_bytes();
    resp.extend_from_slice(wav);
    let (url, seen) = capturing_server(String::from_utf8_lossy(&resp).to_string()).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let audio = p
        .speak(
            "openai_compat:mlx-community/Voxtral-4B-TTS-2603-mlx-4bit",
            "Bonjour Edouard.",
            "fr_female",
            "wav",
        )
        .await
        .unwrap();
    assert!(audio.starts_with(b"RIFF"));
    let raw = String::from_utf8_lossy(&seen.await.unwrap()).to_string();
    assert!(raw.starts_with("POST /v1/audio/speech"), "{raw}");
    assert!(raw.contains("\"voice\":\"fr_female\""), "{raw}");
    assert!(
        raw.contains("\"model\":\"mlx-community/Voxtral-4B-TTS-2603-mlx-4bit\""),
        "{raw}"
    );
    assert!(raw.contains("\"response_format\":\"wav\""), "{raw}");
    let empty = p.speak("m", "  ", "fr_female", "wav").await.unwrap_err();
    assert_eq!(empty.kind, LlmErrorKind::BadRequest);
}

#[tokio::test]
async fn openrouter_transcription_reports_its_cost() {
    let body = r#"{"text":"Salut","usage":{"seconds":9.2,"total_tokens":113,"cost":0.000508}}"#;
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, seen) = capturing_server(resp).await;
    let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
    let t = p
        .transcribe(
            "openrouter:openai/whisper-large-v3",
            b"ID3".to_vec(),
            "a.mp3",
            None,
        )
        .await
        .unwrap();
    assert_eq!(t.text, "Salut");
    assert_eq!(t.cost_usd, Some(0.000508));
    assert_eq!(t.seconds, Some(9.2));
    let raw = String::from_utf8_lossy(&seen.await.unwrap()).to_lowercase();
    assert!(raw.contains("authorization: bearer sk-or-v1-test"), "{raw}");
    assert!(raw.contains("x-openrouter-title"), "{raw}");
    assert!(raw.contains("openai/whisper-large-v3"), "{raw}");

    let empty = p
        .transcribe("m", Vec::new(), "a.ogg", None)
        .await
        .unwrap_err();
    assert_eq!(empty.kind, LlmErrorKind::BadRequest);
}

#[tokio::test]
async fn cancelling_a_silent_stream_does_not_wait_for_the_next_byte() {
    // Le serveur n'envoie que les en-têtes puis se tait (modèle qui réfléchit).
    let resp =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n: OPENROUTER PROCESSING\n\n"
            .to_string();
    let url = one_shot_server(resp, std::time::Duration::from_secs(30)).await;
    let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new()).unwrap();
    let cancel = CancelToken::new();
    let mut rx = p
        .chat_stream(
            ChatRequest {
                model: "a/b".into(),
                messages: vec![ChatMessage::user("x")],
                ..Default::default()
            },
            cancel.clone(),
        )
        .await
        .unwrap();
    let started = std::time::Instant::now();
    cancel.cancel();
    let last = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("l'annulation doit clore le flux sans attendre le serveur");
    assert!(matches!(
        last,
        Some(StreamChunk::Done {
            finish: FinishReason::Cancelled
        })
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

/// #51 : en-têtes puis silence. Le flux est coupé sur le délai d'inactivité, avec une
/// erreur réessayable, au lieu d'attendre le délai global.
#[tokio::test]
async fn a_mute_stream_is_cut_on_the_idle_timeout() {
    let resp =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n: OPENROUTER PROCESSING\n\n"
            .to_string();
    let url = one_shot_server(resp, std::time::Duration::from_secs(30)).await;
    let p = OpenRouterProvider::new(url, "sk-or-v1-test", Catalog::new())
        .unwrap()
        .with_stream_idle(std::time::Duration::from_millis(400));
    let mut rx = p
        .chat_stream(
            ChatRequest {
                model: "a/b".into(),
                messages: vec![ChatMessage::user("x")],
                ..Default::default()
            },
            CancelToken::new(),
        )
        .await
        .unwrap();
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("le flux muet doit être coupé");
    match chunk {
        Some(StreamChunk::Error {
            message, retryable, ..
        }) => {
            assert!(retryable, "l'erreur doit être réessayable");
            assert!(message.contains("aucune donnée depuis"), "{message}");
        }
        other => panic!("erreur d'inactivité attendue : {other:?}"),
    }
}

/// #51 : un flux qui parle régulièrement n'est jamais coupé, même longtemps.
#[tokio::test]
async fn a_slow_but_talking_stream_is_never_cut() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 65536];
        let _ = sock.read(&mut buf).await;
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
            .await
            .unwrap();
        // Des commentaires SSE pendant plus de trois fois le délai d'inactivité.
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let _ = sock.write_all(b": OPENROUTER PROCESSING\n\n").await;
            let _ = sock.flush().await;
        }
        let _ = sock
            .write_all(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"fini\"}}]}\n\ndata: [DONE]\n\n",
            )
            .await;
        let _ = sock.flush().await;
    });
    let p = OpenRouterProvider::new(
        format!("http://{addr}/api/v1"),
        "sk-or-v1-test",
        Catalog::new(),
    )
    .unwrap()
    .with_stream_idle(std::time::Duration::from_millis(300));
    let rx = p
        .chat_stream(
            ChatRequest {
                model: "a/b".into(),
                messages: vec![ChatMessage::user("x")],
                ..Default::default()
            },
            CancelToken::new(),
        )
        .await
        .unwrap();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        collect_stream(rx, "a/b", "openrouter", &Catalog::new()),
    )
    .await
    .expect("le flux ne doit pas être coupé")
    .expect("réponse complète");
    assert_eq!(r.message.text(), "fini");
}

/// #53 : un flux local qui finit par un fragment `usage` alimente le comptage.
#[tokio::test]
async fn a_local_stream_ending_with_usage_feeds_the_token_count() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"salut\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1234,\"completion_tokens\":7}}\n\n",
        "data: [DONE]\n\n"
    );
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
    );
    let url = one_shot_server(resp, std::time::Duration::from_millis(50)).await;
    let p = OpenAiCompatProvider::new(url, "", Catalog::new()).unwrap();
    let rx = p
        .chat_stream(
            ChatRequest {
                model: "openai_compat:qwen".into(),
                messages: vec![ChatMessage::user("x")],
                ..Default::default()
            },
            CancelToken::new(),
        )
        .await
        .unwrap();
    let r = collect_stream(rx, "openai_compat:qwen", "openai_compat", &Catalog::new())
        .await
        .unwrap();
    assert_eq!(r.message.text(), "salut");
    assert_eq!(r.usage.prompt, 1234);
    assert_eq!(r.usage.completion, 7);
}
