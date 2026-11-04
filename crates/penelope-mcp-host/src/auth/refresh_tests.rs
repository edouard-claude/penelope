//! #431 : le jeton se renouvelle quelques minutes avant son expiration, une seule fois
//! même sous des appels concurrents (rotation à usage unique) ; un refus du serveur
//! d'autorisation devient « autorisation requise » ; un jeton refusé par la ressource est
//! renouvelé à la demande.

use super::*;
use penelope_kernel::clock::{Clock as _, TestClock};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Un serveur de jetons à rotation : `rt-N` donne `at-N+1` et `rt-N+1`, et ne sert
/// qu'une fois ; tout autre jeton de rafraîchissement est refusé (`invalid_grant`).
async fn rotating_token_server() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let posts = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(std::sync::Mutex::new(1usize));
    let p = posts.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let (p, current) = (p.clone(), current.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut n = 0;
                loop {
                    let k = stream.read(&mut buf[n..]).await.unwrap_or(0);
                    n += k;
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    if k == 0 || text.contains("refresh_token=") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                p.fetch_add(1, Ordering::SeqCst);
                // Un renouvellement lent : deux appels concurrents se chevauchent.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let given: usize = text
                    .split("refresh_token=rt-")
                    .nth(1)
                    .and_then(|r| r.split('&').next())
                    .and_then(|r| r.trim().parse().ok())
                    .unwrap_or(0);
                let (status, body) = {
                    let mut c = current.lock().unwrap();
                    if given == *c {
                        *c += 1;
                        (
                            "200 OK",
                            json!({"access_token": format!("at-{}", *c),
                                   "refresh_token": format!("rt-{}", *c),
                                   "expires_in": 3600, "token_type": "Bearer"})
                            .to_string(),
                        )
                    } else {
                        (
                            "400 Bad Request",
                            json!({"error": "invalid_grant"}).to_string(),
                        )
                    }
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    (base, posts)
}

async fn services(clock: &TestClock) -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    (dir, s)
}

/// Une autorisation de `slack` dont le jeton `at-1` expire dans `in_ms`.
fn seed(s: &Services, base: &str, refresh: &str, now: i64, in_ms: i64) {
    let grant = Grant {
        issuer: base.into(),
        token_endpoint: format!("{base}/token"),
        client_id: "client".into(),
        client_secret_ref: String::new(),
        auth_method: Default::default(),
        resource: format!("{base}/mcp"),
        tokens: Tokens {
            access_token: "at-1".into(),
            refresh_token: Some(refresh.into()),
            token_type: "Bearer".into(),
            expires_in: Some(3600),
            scope: None,
            expires_at_ms: now + in_ms,
        },
    };
    s.platform
        .secrets
        .set(
            &secret_name("slack"),
            &serde_json::to_string(&grant).unwrap(),
        )
        .unwrap();
}

fn stored_refresh(s: &Services) -> Option<String> {
    let raw = s
        .platform
        .secrets
        .get(&secret_name("slack"))
        .unwrap()
        .unwrap();
    serde_json::from_str::<Grant>(&raw)
        .unwrap()
        .tokens
        .refresh_token
}

#[tokio::test]
async fn the_token_is_renewed_minutes_before_expiry_once_under_concurrency() {
    let clock = TestClock::new(1_789_516_800_000);
    let (_dir, s) = services(&clock).await;
    let (base, posts) = rotating_token_server().await;
    let url = format!("{base}/mcp");
    let now = clock.now_ms();

    // Dix minutes de marge : rien à faire.
    seed(&s, &base, "rt-1", now, 10 * 60_000);
    assert_eq!(
        authorization_header(&s, "slack", &url)
            .await
            .unwrap()
            .as_deref(),
        Some("Bearer at-1")
    );
    assert_eq!(posts.load(Ordering::SeqCst), 0);

    // Quatre minutes : renouvelé, et une seule fois pour cinq appels simultanés.
    seed(&s, &base, "rt-1", now, 4 * 60_000);
    let calls = (0..5).map(|_| authorization_header(&s, "slack", &url));
    let headers = futures::future::join_all(calls).await;
    for h in headers {
        assert_eq!(h.unwrap().as_deref(), Some("Bearer at-2"));
    }
    assert_eq!(
        posts.load(Ordering::SeqCst),
        1,
        "un seul usage du jeton tournant"
    );
    assert_eq!(
        stored_refresh(&s).as_deref(),
        Some("rt-2"),
        "le jeton tourné est gardé"
    );
}

#[tokio::test]
async fn a_refused_renewal_asks_for_authorization_again() {
    let clock = TestClock::new(1_789_516_800_000);
    let (_dir, s) = services(&clock).await;
    let (base, _) = rotating_token_server().await;
    seed(&s, &base, "rt-ancien", clock.now_ms(), 0);
    let e = authorization_header(&s, "slack", &format!("{base}/mcp"))
        .await
        .unwrap_err();
    assert!(e.starts_with(AUTH_REQUIRED), "{e}");
    assert!(e.contains("penelope mcp auth slack"), "{e}");
    assert!(!e.contains("rt-ancien"), "jamais le jeton dans le message");
}

#[tokio::test]
async fn a_token_refused_by_the_resource_is_renewed_on_demand() {
    let clock = TestClock::new(1_789_516_800_000);
    let (_dir, s) = services(&clock).await;
    let (base, posts) = rotating_token_server().await;
    let url = format!("{base}/mcp");
    seed(&s, &base, "rt-1", clock.now_ms(), 60 * 60_000);
    assert!(invalidate(&s, "slack").await);
    assert_eq!(
        authorization_header(&s, "slack", &url)
            .await
            .unwrap()
            .as_deref(),
        Some("Bearer at-2")
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    // Sans jeton de rafraîchissement, rien à renouveler : l'appel garde son erreur.
    forget(&s, "slack").unwrap();
    assert!(!invalidate(&s, "slack").await);
}
