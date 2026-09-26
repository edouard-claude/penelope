use super::*;
use penelope_kernel::clock::TestClock;
use penelope_platform::SecretStore;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

/// Faux serveur d'autorisation : métadonnées, enregistrement, jetons.
async fn fake_authorization_server() -> (String, Seen) {
    fake_authorization_server_opts(false).await
}

/// `prereg` : à la manière de Slack, la ressource annonce ses portées, le défi 401
/// n'en porte aucune et le serveur d'autorisation n'offre pas d'enregistrement
/// dynamique. Seul un client pré-enregistré passe.
async fn fake_authorization_server_opts(prereg: bool) -> (String, Seen) {
    fake_authorization_server_with_auth(prereg, None).await
}

async fn fake_authorization_server_with_auth(
    prereg: bool,
    auth_method: Option<&'static str>,
) -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let (b, log) = (base.clone(), seen.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let (b, log) = (b.clone(), log.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_string();
                        break (head, buf[i + 4..].to_vec());
                    }
                };
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while body.len() < length {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..n]);
                }
                let mut first = head.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("").to_string();
                let path = first.next().unwrap_or("").to_string();
                let body = String::from_utf8_lossy(&body).to_string();
                let recorded = format!("{head}\n\n{body}");
                log.lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), recorded));
                let json = match (method.as_str(), path.as_str()) {
                    ("GET", "/.well-known/oauth-protected-resource/mcp") if prereg => {
                        json!({
                            "resource": format!("{b}/mcp"),
                            "authorization_servers": [b],
                            "scopes_supported": ["channels:history", "chat:write"],
                        })
                    }
                    ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                        json!({"resource": format!("{b}/mcp"), "authorization_servers": [b]})
                    }
                    ("GET", "/.well-known/oauth-authorization-server") if prereg => json!({
                        "issuer": b,
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                        "code_challenge_methods_supported": ["S256"],
                        "token_endpoint_auth_methods_supported": auth_method.into_iter().collect::<Vec<_>>(),
                    }),
                    ("GET", "/.well-known/oauth-authorization-server") => json!({
                        "issuer": b,
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                        "registration_endpoint": format!("{b}/register"),
                        "code_challenge_methods_supported": ["S256"],
                    }),
                    ("POST", "/register") => json!({"client_id": "client-dyn"}),
                    ("POST", "/token") if body.contains("grant_type=authorization_code") => {
                        json!({
                            "access_token": "at-1", "refresh_token": "rt-1",
                            "expires_in": 3600, "token_type": "Bearer", "scope": "read"
                        })
                    }
                    ("POST", "/token") => json!({
                        "access_token": "at-2", "expires_in": 3600, "token_type": "Bearer"
                    }),
                    _ => Value::Null,
                };
                let (status, text) = if json.is_null() {
                    ("404 Not Found", String::from("{}"))
                } else {
                    ("200 OK", json.to_string())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    (base, seen)
}

fn param(url: &str, name: &str) -> String {
    url.split(['?', '&'])
        .find_map(|p| p.strip_prefix(&format!("{name}=")))
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn authorization_code_flow_with_paste_back_and_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    let d = s.clone();
    let (base, seen) = fake_authorization_server().await;
    let cfg = ServerConfig {
        name: "suivi".into(),
        transport: "http".into(),
        url: format!("{base}/mcp"),
        scopes: vec!["read".into()],
        ..Default::default()
    };

    let start = start(&d, &cfg, Some(r#"Bearer realm="suivi", scope="write""#))
        .await
        .unwrap();
    assert!(
        start.url.starts_with(&format!("{base}/authorize?")),
        "{}",
        start.url
    );
    assert_eq!(param(&start.url, "client_id"), "client-dyn");
    assert_eq!(param(&start.url, "code_challenge_method"), "S256");
    assert!(!param(&start.url, "code_challenge").is_empty());
    assert!(param(&start.url, "resource").contains("mcp"), "RFC 8707");
    assert_eq!(
        start.scopes,
        vec!["read", "write"],
        "consentement incrémental"
    );
    assert!(start.redirect_uri.starts_with("http://127.0.0.1:"));
    assert!(prompt_text(&start).contains("colle-la ici"));

    let state = param(&start.url, "state");
    let callback = format!("{}?code=abc&state={state}&iss={base}", start.redirect_uri);
    assert_eq!(complete(&d, &callback).await.unwrap(), "suivi");
    let token_call = seen
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p, b)| p == "/token" && b.contains("authorization_code"))
        .cloned()
        .expect("échange du code");
    assert!(token_call.2.contains("code_verifier="));
    assert!(token_call.2.contains("resource="));
    assert!(
        complete(&d, &callback).await.is_err(),
        "une adresse ne sert qu'une fois"
    );

    assert_eq!(
        authorization_header(&s, "suivi", &cfg.url)
            .await
            .unwrap()
            .as_deref(),
        Some("Bearer at-1")
    );
    assert!(
        authorization_header(&s, "suivi", "http://mcp.exemple.org/mcp")
            .await
            .is_err(),
        "jamais de jeton vers un serveur en clair"
    );
    clock.advance_hours(2);
    assert_eq!(
        authorization_header(&s, "suivi", &cfg.url)
            .await
            .unwrap()
            .as_deref(),
        Some("Bearer at-2"),
        "jeton rafraîchi"
    );
    let grant: Grant =
        serde_json::from_str(&s.platform.secrets.get("mcp.suivi.oauth").unwrap().unwrap()).unwrap();
    assert_eq!(
        grant.tokens.refresh_token.as_deref(),
        Some("rt-1"),
        "gardé sans rotation"
    );

    // Deuxième autorisation : le client enregistré est réutilisé pour cet issuer.
    let again = super::start(&d, &cfg, None).await.unwrap();
    let registrations = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, p, _)| p == "/register")
        .count();
    assert_eq!(registrations, 1);

    // Un `iss` différent de l'issuer enregistré est refusé (RFC 9207).
    let state = param(&again.url, "state");
    let forged = format!(
        "{}?code=abc&state={state}&iss=https://ailleurs.example",
        again.redirect_uri
    );
    assert!(complete(&d, &forged).await.unwrap_err().contains("iss"));

    // Une demande expire au bout de 10 minutes.
    let late = super::start(&d, &cfg, None).await.unwrap();
    clock.advance_ms(REQUEST_TTL_MS + 1);
    let state = param(&late.url, "state");
    let err = complete(&d, &format!("?code=x&state={state}"))
        .await
        .unwrap_err();
    assert!(err.contains("expirée"), "{err}");

    forget(&s, "suivi").unwrap();
    assert!(
        authorization_header(&s, "suivi", &cfg.url)
            .await
            .unwrap()
            .is_none()
    );

    // Un serveur MCP en clair ne démarre même pas d'autorisation.
    let clear = ServerConfig {
        name: "clair".into(),
        transport: "http".into(),
        url: "http://mcp.exemple.org/mcp".into(),
        ..Default::default()
    };
    assert!(
        super::start(&d, &clear, None)
            .await
            .unwrap_err()
            .contains("HTTPS")
    );
}

async fn confidential_flow(method: &'static str) {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    s.platform
        .secrets
        .set("mcp_client_secret", "secret-test")
        .unwrap();
    let d = s.clone();
    let (base, seen) = fake_authorization_server_with_auth(true, Some(method)).await;
    let cfg = ServerConfig {
        name: format!("confidentiel-{method}"),
        transport: "http".into(),
        url: format!("{base}/mcp"),
        client_id: "client-test".into(),
        client_secret: "${SECRET:mcp_client_secret}".into(),
        ..Default::default()
    };
    let started = start(&d, &cfg, None).await.unwrap();
    let state = param(&started.url, "state");
    complete(&d, &format!("?code=abc&state={state}"))
        .await
        .unwrap();
    clock.advance_hours(2);
    assert_eq!(
        authorization_header(&s, &cfg.name, &cfg.url)
            .await
            .unwrap()
            .as_deref(),
        Some("Bearer at-2")
    );
    let calls: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, path, _)| path == "/token")
        .map(|(_, _, request)| request.clone())
        .collect();
    assert_eq!(calls.len(), 2, "échange et rafraîchissement");
    for request in calls {
        let logged = penelope_observe::redact(&json!({"request_debug": &request}).to_string());
        assert!(!logged.contains("secret-test"), "{logged}");
        match method {
            "client_secret_post" => {
                assert!(request.contains("client_secret=secret-test"), "{request}")
            }
            "client_secret_basic" => {
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: basic"),
                    "{request}"
                );
                assert!(!request.contains("client_secret="), "{request}");
                let encoded = request
                    .lines()
                    .find(|line| {
                        line.to_ascii_lowercase()
                            .starts_with("authorization: basic ")
                    })
                    .and_then(|line| line.split_whitespace().last())
                    .expect("header Basic du serveur simulé");
                assert!(!logged.contains(encoded), "{logged}");
            }
            _ => unreachable!(),
        }
    }
    let grant = s
        .platform
        .secrets
        .get(&secret_name(&cfg.name))
        .unwrap()
        .unwrap();
    assert!(
        !grant.contains("secret-test"),
        "la valeur ne doit pas être persistée"
    );
    assert!(grant.contains("${SECRET:mcp_client_secret}"));
}

#[tokio::test]
async fn confidential_client_uses_secret_post_for_exchange_and_refresh() {
    confidential_flow("client_secret_post").await;
}

#[tokio::test]
async fn confidential_client_uses_secret_basic_for_exchange_and_refresh() {
    confidential_flow("client_secret_basic").await;
}

#[test]
fn confidential_client_secret_is_registered_for_redaction() {
    let secrets = penelope_platform::MemorySecretStore::new();
    secrets.set("oauth_test", "secret-redaction-test").unwrap();
    let mut body = BTreeMap::new();
    token_client_auth(
        &secrets,
        ClientAuthMethod::Post,
        "${SECRET:oauth_test}",
        "client-test",
        &mut body,
    )
    .unwrap();
    let rendered =
        penelope_observe::redact(&format!("erreur distante : {}", body["client_secret"]));
    assert!(!rendered.contains("secret-redaction-test"), "{rendered}");
}

#[test]
fn basic_authorization_value_is_redacted_even_in_a_generic_structured_field() {
    use base64::Engine as _;
    let secrets = penelope_platform::MemorySecretStore::new();
    secrets.set("oauth_basic_test", "s3cr3t").unwrap();
    let mut body = BTreeMap::from([("client_id".into(), "c".into())]);
    token_client_auth(
        &secrets,
        ClientAuthMethod::Basic,
        "${SECRET:oauth_basic_test}",
        "c",
        &mut body,
    )
    .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode("c:s3cr3t");
    let event = json!({
        "request_debug": format!("headers = {{ Authorization: Basic {encoded} }}"),
        "error": "s3cr3t",
    });
    let logged = penelope_observe::redact(&event.to_string());
    assert!(!logged.contains("s3cr3t"), "{logged}");
    assert!(!logged.contains(&encoded), "{logged}");
}

#[test]
fn endpoints_must_be_https_unless_local() {
    for ok in [
        "https://auth.example/token",
        "HTTPS://AUTH.EXAMPLE/token",
        "http://127.0.0.1:9000/token",
        "http://127.0.0.2/token",
        "http://[::1]:9000/token",
        "http://localhost:8765/oauth/callback",
    ] {
        assert!(check_endpoint(ok).is_ok(), "{ok}");
    }
    // Contournements par préfixe, identifiants dans l'URL, autres schémas.
    for refused in [
        "http://auth.example/token",
        "http://localhost.evil.example/token",
        "http://127.0.0.1.evil.example/token",
        "http://localhost@evil.example/token",
        "http://127.0.0.1:8080@evil.example/token",
        "http://[::1]@evil.example/token",
        "https://user:secret@auth.example/token",
        "http://0.0.0.0/token",
        "ftp://auth.example/token",
        "pas une url",
    ] {
        assert!(check_endpoint(refused).is_err(), "{refused}");
    }
    assert_eq!(
        origin("https://mcp.example:8443/v1?x=1").as_deref(),
        Some("https://mcp.example:8443")
    );
    assert_eq!(origin("http://localhost@evil.example/x"), None);
}

/// #159 : Slack ne met pas de `scope` dans son défi 401 et n'en a pas dans sa
/// déclaration ; sans repli, l'autorisation partirait sans portée et serait refusée.
#[tokio::test]
async fn scopes_fall_back_to_those_advertised_by_the_resource() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    let d = s.clone();
    let (base, _) = fake_authorization_server_opts(true).await;
    let cfg = ServerConfig {
        name: "slack".into(),
        transport: "http".into(),
        url: format!("{base}/mcp"),
        client_id: "client-preenregistre".into(),
        scopes: vec![],
        ..Default::default()
    };

    let start = start(&d, &cfg, Some(r#"Bearer realm="slack""#))
        .await
        .unwrap();
    let scope = param(&start.url, "scope");
    assert_eq!(scope, "channels%3Ahistory%20chat%3Awrite", "{}", start.url);
    assert_eq!(param(&start.url, "client_id"), "client-preenregistre");
}

/// Sans `client_id`, le message doit dire quoi faire plutôt que constater l'impasse.
#[tokio::test]
async fn a_server_without_registration_names_the_command_to_run() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    let d = s.clone();
    let (base, _) = fake_authorization_server_opts(true).await;
    let cfg = ServerConfig {
        name: "slack".into(),
        transport: "http".into(),
        url: format!("{base}/mcp"),
        ..Default::default()
    };

    let e = start(&d, &cfg, None).await.unwrap_err();
    assert!(e.contains("penelope mcp edit slack client_id"), "{e}");
    assert!(e.contains("docs/mcp.md"), "{e}");
}

/// #159 : Slack n'enregistre que `localhost` ; l'URL envoyée doit être exactement
/// celle enregistrée, à l'autorisation comme à l'échange.
#[tokio::test]
async fn the_callback_host_is_configurable() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    s.config
        .mutate("test", |c| {
            c.mcp.callback_host = "localhost".into();
            Ok(vec!["mcp.callback_host".into()])
        })
        .unwrap();
    let d = s.clone();
    let (base, seen) = fake_authorization_server_opts(true).await;
    let cfg = ServerConfig {
        name: "slack".into(),
        transport: "http".into(),
        url: format!("{base}/mcp"),
        client_id: "client-preenregistre".into(),
        ..Default::default()
    };

    let start = start(&d, &cfg, None).await.unwrap();
    assert_eq!(
        param(&start.url, "redirect_uri"),
        "http%3A%2F%2Flocalhost%3A7777%2Foauth%2Fcallback",
        "{}",
        start.url
    );

    let code = "code-1";
    let url = format!(
        "http://localhost:7777/oauth/callback?code={code}&state={}",
        param(&start.url, "state")
    );
    complete(&d, &url).await.unwrap();
    let exchange = seen
        .lock()
        .unwrap()
        .iter()
        .find(|(m, p, _)| m == "POST" && p == "/token")
        .cloned()
        .expect("échange de jeton");
    assert!(
        exchange
            .2
            .contains("redirect_uri=http%3A%2F%2Flocalhost%3A7777"),
        "{}",
        exchange.2
    );
}

/// Requête HTTP brute vers le serveur de retour local ; rend la réponse entière.
async fn get(port: u16, target: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

/// Le serveur de retour local mène l'autorisation à terme et prévient le propriétaire,
/// explique un retour refusé sans injecter de HTML, sert le document CIMD et s'arrête
/// avec le daemon.
#[tokio::test]
async fn the_local_callback_server_completes_an_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared.clone())
            .await
            .unwrap(),
    );
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    s.config
        .mutate("test", |c| {
            c.mcp.callback_port = port;
            c.mcp.cimd_url = "https://penelope.exemple.org/oauth/client.json".into();
            Ok(vec!["mcp.callback_port".into(), "mcp.cimd_url".into()])
        })
        .unwrap();
    let rec = penelope_app::testing::RecordingMessenger::new();
    let ctx = AuthContext {
        services: s.clone(),
        messenger: Slot::default(),
        mcp_admin: Slot::default(),
        supervision: Supervision {
            tasks: Arc::default(),
            handle: penelope_app::ports::Handle::new(0),
            clock: shared,
            events: s.events.clone(),
        },
    };
    ctx.messenger.set(Some(rec.clone()));
    let handle = ctx.supervision.handle.clone();
    let server = tokio::spawn(callback_server(ctx));
    // Le serveur écoute dès que la connexion aboutit.
    let mut ready = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready, "serveur de retour à l'écoute");

    let (base, _seen) = fake_authorization_server().await;
    let cfg = ServerConfig {
        name: "suivi".into(),
        transport: "http".into(),
        url: format!("{base}/mcp"),
        ..Default::default()
    };
    let started = start(&s, &cfg, None).await.unwrap();
    let ok = get(
        port,
        &format!(
            "/oauth/callback?code=abc&state={}",
            param(&started.url, "state")
        ),
    )
    .await;
    assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
    assert!(ok.contains("Autorisation de <b>suivi</b> reçue"), "{ok}");
    assert_eq!(rec.texts(), vec!["🔐 `suivi` autorisé."]);

    let refused = get(port, "/oauth/callback?code=<b>&state=<script>").await;
    assert!(refused.starts_with("HTTP/1.1 400"), "{refused}");
    assert!(!refused.contains("<script>"), "{refused}");

    let cimd = get(port, "/oauth/client.json").await;
    assert!(cimd.contains("application/json"), "{cimd}");
    let body: Value = serde_json::from_str(cimd.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body["client_id"],
        "https://penelope.exemple.org/oauth/client.json"
    );
    assert!(
        body.to_string().contains(&format!("127.0.0.1:{port}")),
        "{body}"
    );

    assert!(get(port, "/ailleurs").await.starts_with("HTTP/1.1 404"));

    handle.shutdown();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("arrêt avec le daemon")
        .unwrap();
}

/// Port déjà pris : le serveur de retour renonce, le collage reste possible.
#[tokio::test]
async fn a_busy_callback_port_is_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let shared: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared.clone())
            .await
            .unwrap(),
    );
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    s.config
        .mutate("test", |c| {
            c.mcp.callback_port = port;
            Ok(vec!["mcp.callback_port".into()])
        })
        .unwrap();
    let ctx = AuthContext {
        services: s.clone(),
        messenger: Slot::default(),
        mcp_admin: Slot::default(),
        supervision: Supervision {
            tasks: Arc::default(),
            handle: penelope_app::ports::Handle::new(0),
            clock: shared,
            events: s.events.clone(),
        },
    };
    tokio::time::timeout(Duration::from_secs(5), callback_server(ctx))
        .await
        .expect("rend la main sans écouter");
}

/// Après l'autorisation, la reconnexion qui échoue est dite au propriétaire.
#[tokio::test]
async fn a_failed_reconnection_after_authorization_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let shared: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    let sup = crate::testing::supervisor(
        s.clone(),
        Arc::new(crate::testing::FakeConnector::default()),
    );
    let rec = penelope_app::testing::RecordingMessenger::new();
    reconnect_and_tell(&s, Some(sup), Some(rec.clone()), "fantome").await;
    let texts = rec.texts();
    assert_eq!(texts.len(), 1);
    assert!(
        texts[0].starts_with("🔐 `fantome` autorisé, mais la reconnexion échoue"),
        "{texts:?}"
    );
}
