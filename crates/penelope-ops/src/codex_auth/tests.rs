use super::*;

use std::sync::Mutex as StdMutex;

/// Serveur d'autorisation simulé : répond dans l'ordre du script, garde chaque
/// requête entière (en-têtes et corps) pour les assertions.
async fn scripted_server(script: Vec<(u16, String)>) -> (String, Arc<StdMutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let recorder = seen.clone();
    tokio::spawn(async move {
        let mut queue = script.into_iter();
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut got: Vec<u8> = Vec::new();
            let mut buf = vec![0u8; 16_384];
            loop {
                let n = tokio::time::timeout(Duration::from_millis(200), sock.read(&mut buf))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                // Corps complet : les en-têtes, puis `Content-Length` octets.
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

fn b64(v: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
}

/// Jeton d'identité d'un compte connecté.
fn id_token() -> String {
    format!(
        "x.{}.y",
        b64(&json!({
            "email": "moi@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc_1", "chatgpt_plan_type": "pro"
            }
        }))
    )
}

/// Services de test dont le serveur d'autorisation est `issuer`.
async fn with_issuer(issuer: &str) -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let issuer = issuer.to_string();
    s.publish_config("test", move |c| {
        c.providers.codex.issuer = issuer.clone();
        c.providers.codex.enabled = true;
        Ok(vec!["providers.codex.issuer".into()])
    })
    .unwrap();
    (dir, s)
}

/// #142 : le code d'appareil — `interval` en chaîne, 403 puis 404 en attente, échange
/// en formulaire avec le vérifieur **du serveur**, connexion rangée au magasin.
#[tokio::test]
async fn the_device_code_flow_stores_a_grant() {
    let access = format!("x.{}.y", b64(&json!({"exp": 1_900_000_000i64})));
    let (url, seen) = scripted_server(vec![
        (
            200,
            json!({"device_auth_id": "dev_1", "user_code": "ABCD-EF", "interval": "1"}).to_string(),
        ),
        (403, "{}".to_string()),
        (404, "{}".to_string()),
        (
            200,
            json!({"authorization_code": "code_1", "code_challenge": "ch",
                   "code_verifier": "ver_1"})
            .to_string(),
        ),
        (
            200,
            json!({"access_token": access, "refresh_token": "r1", "id_token": id_token()})
                .to_string(),
        ),
    ])
    .await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;

    let login = start(s).await.expect("code d'appareil");
    assert_eq!(login.user_code, "ABCD-EF");
    assert_eq!(
        login.interval_s, 1,
        "`interval` est une chaîne côté serveur"
    );
    assert!(login.verification_url.ends_with("/codex/device"));

    let grant = wait_for(s, &login).await.expect("connexion");
    assert_eq!(grant.plan_type, "pro");
    assert_eq!(grant.email, "moi@example.test");
    assert_eq!(grant.refresh_token, "r1");
    assert!(load(s).unwrap().is_some(), "la connexion est rangée");

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 5, "un code, trois sondages, un échange");
    let exchange = reqs.last().unwrap();
    assert!(
        exchange.contains("application/x-www-form-urlencoded"),
        "l'échange du code est en formulaire : {exchange}"
    );
    assert!(exchange.contains("code_verifier=ver_1"), "{exchange}");
    assert!(
        exchange.contains("grant_type=authorization_code"),
        "{exchange}"
    );
}

/// #142 : un compte sans code d'appareil activé le dit, au lieu d'un statut brut.
#[tokio::test]
async fn a_missing_device_code_says_what_to_do() {
    let (url, _) = scripted_server(vec![(404, "{}".to_string())]).await;
    let (_dir, s) = with_issuer(&url).await;
    let e = start(&s).await.expect_err("404");
    assert!(e.contains("code d'appareil n'est pas activé"), "{e}");
}

/// #142 : le rafraîchissement est en JSON, la rotation est écrite avant tout usage, et
/// deux appels concurrents ne font qu'une requête — un `refresh_token` réutilisé
/// déconnecte le compte pour de bon.
#[tokio::test]
async fn refresh_is_json_rotates_once_and_a_reuse_disconnects() {
    let (url, seen) = scripted_server(vec![
        (
            200,
            json!({"access_token": "a2", "refresh_token": "r2", "expires_in": 3600}).to_string(),
        ),
        (200, json!({"access_token": "jamais"}).to_string()),
    ])
    .await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;
    let now = s.clock.now_ms();
    store(
        s,
        &Grant {
            access_token: "a1".into(),
            refresh_token: "r1".into(),
            id_token: id_token(),
            account_id: "acc_1".into(),
            plan_type: "pro".into(),
            expires_at: now - 1,
            last_refresh: now - 1,
            ..Default::default()
        },
    )
    .unwrap();

    // Deux rafraîchissements concurrents : un seul appel réseau, sinon le second
    // rejouerait `r1` et vaudrait `refresh_token_reused`.
    let (a, b) = tokio::join!(refresh(s), refresh(s));
    assert_eq!(a.expect("jeton").access_token, "a2");
    assert_eq!(b.expect("jeton").access_token, "a2");
    assert_eq!(seen.lock().unwrap().len(), 1, "un seul rafraîchissement");
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.contains("application/json"), "en JSON : {req}");
    assert!(req.contains("\"grant_type\":\"refresh_token\""), "{req}");
    let rotated = load(s).unwrap().expect("connexion");
    assert_eq!(rotated.refresh_token, "r2", "la rotation est persistée");
    assert_eq!(rotated.account_id, "acc_1");
}

/// #142 : un `refresh_token` réutilisé est définitif — plus rien ne part, et l'état le
/// dit pour que la carte de reconnexion parte.
#[tokio::test]
async fn a_reused_refresh_token_disconnects_for_good() {
    let (url, _) = scripted_server(vec![(
        400,
        json!({"error": "refresh_token_reused"}).to_string(),
    )])
    .await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;
    let now = s.clock.now_ms();
    store(
        s,
        &Grant {
            access_token: "a1".into(),
            refresh_token: "r1".into(),
            expires_at: now - 1,
            last_refresh: now - 1,
            ..Default::default()
        },
    )
    .unwrap();
    let e = refresh(s).await.expect_err("réutilisation");
    assert!(e.contains("déconnecté"), "{e}");
    let dead = load(s).unwrap().expect("connexion");
    assert_eq!(dead.disconnected.as_deref(), Some("refresh_token_reused"));
    assert!(valid_token(s).await.is_err(), "plus rien ne part");
    let st = status(s).unwrap().expect("état");
    assert!(!st.connected);
    // L'événement permet à la carte de reconnexion de partir.
    let events = s.events.range(0, 50).await.unwrap();
    assert!(
        events.iter().any(|e| e.kind == "llm.provider_disconnected"),
        "{events:?}"
    );
}

/// #142 : les claims du jeton d'identité donnent compte, plan et FedRAMP ; le jeton
/// d'accès donne son expiration.
#[test]
fn tokens_carry_the_account_the_plan_and_their_expiry() {
    let b64 = |v: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    let id_token = format!(
        "x.{}.y",
        b64(&json!({
            "email": "moi@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc_1",
                "chatgpt_plan_type": "pro",
                "chatgpt_account_is_fedramp": true
            }
        }))
    );
    let access = format!("x.{}.y", b64(&json!({"exp": 1_800_000_000i64})));
    let g = grant_from_tokens(
        &json!({"access_token": access, "refresh_token": "r1", "id_token": id_token}),
        None,
        1_000,
    )
    .expect("connexion");
    assert_eq!(g.account_id, "acc_1");
    assert_eq!(g.plan_type, "pro");
    assert_eq!(g.email, "moi@example.test");
    assert!(g.fedramp);
    assert_eq!(g.expires_at, 1_800_000_000_000);
    assert_eq!(g.last_refresh, 1_000);
    assert!(g.disconnected.is_none());
}

/// #142 : une réponse de rafraîchissement sans `refresh_token` garde l'ancien ; avec
/// un neuf, il remplace l'ancien (rotation).
#[test]
fn refresh_keeps_or_rotates_the_refresh_token() {
    let before = Grant {
        refresh_token: "r1".into(),
        id_token: "i1".into(),
        account_id: "acc_1".into(),
        plan_type: "plus".into(),
        ..Default::default()
    };
    let kept = grant_from_tokens(
        &json!({"access_token": "a2", "expires_in": 3600}),
        Some(&before),
        0,
    )
    .unwrap();
    assert_eq!(kept.refresh_token, "r1");
    assert_eq!(
        kept.account_id, "acc_1",
        "le compte survit au rafraîchissement"
    );
    assert_eq!(kept.plan_type, "plus");
    assert_eq!(kept.expires_at, 3_600_000);

    let rotated = grant_from_tokens(
        &json!({"access_token": "a3", "refresh_token": "r2"}),
        Some(&before),
        0,
    )
    .unwrap();
    assert_eq!(rotated.refresh_token, "r2");
}

/// #142 : un jeton proche de l'expiration, ou dormant depuis plus de huit jours, est
/// rafraîchi ; un jeton frais ne l'est pas.
#[test]
fn a_token_is_refreshed_before_it_dies_or_after_eight_days() {
    let now = 1_000_000_000i64;
    let fresh = Grant {
        expires_at: now + 3_600_000,
        last_refresh: now - 60_000,
        ..Default::default()
    };
    assert!(!fresh.needs_refresh(now));
    assert!(
        Grant {
            expires_at: now + 60_000,
            ..fresh.clone()
        }
        .needs_refresh(now),
        "moins de cinq minutes"
    );
    assert!(
        Grant {
            last_refresh: now - 9 * 24 * 3_600_000,
            ..fresh.clone()
        }
        .needs_refresh(now),
        "neuf jours"
    );
}
