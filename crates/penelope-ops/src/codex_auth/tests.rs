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

fn expired_grant(s: &Services, refresh_token: &str) -> Grant {
    let now = s.clock.now_ms();
    Grant {
        access_token: "a1".into(),
        refresh_token: refresh_token.into(),
        account_id: "acc_1".into(),
        expires_at: now - 1,
        last_refresh: now - 1,
        ..Default::default()
    }
}

/// #148 : la révocation envoie chaque jeton non vide et dit si l'un est passé.
#[tokio::test]
async fn revocation_sends_both_tokens_and_says_if_one_passed() {
    let (url, seen) = scripted_server(vec![(500, "{}".into()), (200, "{}".into())]).await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;
    assert!(revoke(s, &expired_grant(s, "r1")).await);
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].contains("/oauth/revoke") && reqs[0].contains("token=r1"));
    assert!(reqs[1].contains("token=a1"));
    let events = s.events.range(0, 50).await.unwrap();
    let ev = events
        .iter()
        .find(|e| e.kind == "llm.provider_tokens_revoked")
        .unwrap();
    assert_eq!(ev.payload["ok"], true);

    let (url, _) = scripted_server(vec![(500, "{}".into())]).await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;
    assert!(
        !revoke(s, &expired_grant(s, "")).await,
        "seul le jeton d'accès part, et il est refusé"
    );
}

#[tokio::test]
async fn the_installation_id_is_created_once() {
    let (_dir, services) = with_issuer("http://127.0.0.1:9").await;
    let s = &*services;
    let first = installation_id(s).await;
    assert_eq!(first.len(), 26);
    assert_eq!(first, first.to_lowercase());
    assert_eq!(installation_id(s).await, first);
}

/// Un refus du serveur, à la demande du code ou au sondage, est dit avec son statut ;
/// la demande en attente est oubliée même en échec.
#[tokio::test]
async fn device_code_refusals_are_reported_and_the_pending_code_forgotten() {
    let (url, _) = scripted_server(vec![(500, json!({"error": "panne"}).to_string())]).await;
    let (_dir, services) = with_issuer(&url).await;
    let err = start(&services).await.unwrap_err();
    assert!(
        err.contains("refusée (500)") && err.contains("panne"),
        "{err}"
    );

    let (url, _) = scripted_server(vec![
        (
            200,
            json!({"device_auth_id": "dev_1", "usercode": "WXYZ", "interval": 1}).to_string(),
        ),
        (429, json!({"error": "trop"}).to_string()),
    ])
    .await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;
    let err = wait_pending(s).await.unwrap_err();
    assert!(err.contains("aucune connexion Codex en attente"), "{err}");
    let login = start_pending(s).await.unwrap();
    assert_eq!(login.user_code, "WXYZ", "ancien nom du champ accepté");
    let err = wait_pending(s).await.unwrap_err();
    assert!(err.contains("code d'appareil refusé (429)"), "{err}");
    assert_eq!(
        s.kv_get("codex.oauth.pending").await.unwrap().as_deref(),
        Some(""),
        "un code mort n'est pas resservi"
    );
}

/// Un refus passager du rafraîchissement laisse la connexion ; un 401 la coupe, et le
/// jeton n'est plus servi, raison comprise.
#[tokio::test]
async fn refresh_failures_are_transient_or_final() {
    let (url, _) = scripted_server(vec![
        (503, json!({"error": {"type": "surcharge"}}).to_string()),
        (401, json!({"error": {"code": "token_expired"}}).to_string()),
    ])
    .await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;
    let err = refresh(s).await.unwrap_err();
    assert_eq!(err, NOT_CONNECTED, "sans connexion");

    store(s, &expired_grant(s, "")).unwrap();
    let err = refresh(s).await.unwrap_err();
    assert!(err.contains("pas de jeton de rafraîchissement"), "{err}");

    store(s, &expired_grant(s, "r1")).unwrap();
    let err = refresh(s).await.unwrap_err();
    assert!(err.contains("rafraîchissement refusé (503)"), "{err}");
    assert!(load(s).unwrap().unwrap().disconnected.is_none());

    let tokens = DaemonTokens::new(services.clone());
    let err = penelope_llm::TokenSource::refreshed(&tokens)
        .await
        .unwrap_err();
    assert_eq!(err.kind, LlmErrorKind::Auth);
    assert!(err.message.contains("token_expired"), "{}", err.message);
    let err = penelope_llm::TokenSource::token(&tokens).await.unwrap_err();
    assert!(err.message.contains("(token_expired)"), "{}", err.message);
    let err = refresh(s).await.unwrap_err();
    assert!(err.contains("(token_expired)"), "{err}");
}

/// Un jeton encore frais est servi sans appel réseau ; l'état montre le compte à
/// défaut d'adresse.
#[tokio::test]
async fn a_fresh_token_is_served_as_is() {
    let (_dir, services) = with_issuer("http://127.0.0.1:9").await;
    let s = &*services;
    let now = s.clock.now_ms();
    store(
        s,
        &Grant {
            access_token: "frais".into(),
            refresh_token: "r1".into(),
            account_id: "acc_9".into(),
            expires_at: now + 3_600_000,
            last_refresh: now,
            ..Default::default()
        },
    )
    .unwrap();
    let tokens = DaemonTokens::new(services.clone());
    let t = penelope_llm::TokenSource::token(&tokens).await.unwrap();
    assert_eq!(t.access_token, "frais");
    assert_eq!(refresh(s).await.unwrap().access_token, "frais");
    let st = status(s).unwrap().unwrap();
    assert!(st.connected);
    assert_eq!(st.account, "acc_9");
}

/// La déconnexion révoque le jeton de rafraîchissement puis oublie la connexion.
#[tokio::test]
async fn logout_revokes_then_forgets() {
    let (url, seen) = scripted_server(vec![(200, "{}".into())]).await;
    let (_dir, services) = with_issuer(&url).await;
    let s = &*services;
    store(s, &expired_grant(s, "r1")).unwrap();
    logout(s).await.unwrap();
    assert!(load(s).unwrap().is_none());
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].contains("token=r1"), "{}", reqs[0]);
    let events = s.events.range(0, 50).await.unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "llm.provider_disconnected" && e.payload["reason"] == "logout")
    );
}

/// Une déconnexion n'est annoncée qu'une fois par raison.
#[tokio::test]
async fn a_disconnection_is_announced_once() {
    let (_dir, services) = with_issuer("http://127.0.0.1:9").await;
    let s = &*services;
    let rec = penelope_app::testing::RecordingMessenger::new();
    let dead = Grant {
        disconnected: Some("refresh_token_reused".into()),
        ..Default::default()
    };
    notify_disconnected(s, Some(rec.clone()), &dead).await;
    notify_disconnected(s, Some(rec.clone()), &dead).await;
    let texts = rec.texts();
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("Compte ChatGPT déconnecté** (refresh_token_reused)"));
    let other = Grant {
        disconnected: Some("invalid_grant".into()),
        ..Default::default()
    };
    notify_disconnected(s, None, &other).await;
    notify_disconnected(s, Some(rec.clone()), &other).await;
    assert_eq!(rec.texts().len(), 1, "déjà notée, même sans canal");
}

/// #142 : la boucle hors tour lit la jauge rangée par le fournisseur et alerte, puis
/// annonce la déconnexion ; `doctor` dit les deux.
#[tokio::test]
async fn the_refresh_loop_alerts_on_quota_and_announces_a_disconnection() {
    let (_dir, services) = with_issuer("http://127.0.0.1:9").await;
    let s = &*services;
    let reset = s.clock.now_ms() / 1000 + 3600;
    let quota = penelope_llm::Quota {
        primary: Some(penelope_llm::codex::QuotaWindow {
            used_percent: 99.0,
            window_minutes: 300,
            reset_at: reset,
        }),
        ..Default::default()
    };
    penelope_llm::QuotaSink::record(
        &crate::codex_quota::QuotaWriter::new(services.clone()),
        quota,
    );
    for _ in 0..100 {
        if crate::codex_quota::snapshot(s).await.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        crate::codex_quota::snapshot(s).await.is_some(),
        "jauge rangée"
    );
    store(
        s,
        &Grant {
            disconnected: Some("refresh_token_reused".into()),
            ..expired_grant(s, "r1")
        },
    )
    .unwrap();

    let rec = penelope_app::testing::RecordingMessenger::new();
    let slot: Slot<dyn Messenger> = Slot::default();
    slot.set(Some(rec.clone()));
    let task = tokio::spawn(refresh_loop(services.clone(), slot));
    for _ in 0..200 {
        if rec.texts().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();
    let texts = rec.texts();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts[1].contains("Compte ChatGPT déconnecté"), "{texts:?}");

    let checks = crate::doctor::codex_checks(s).await;
    let provider = checks.iter().find(|c| c.id == "provider.codex").unwrap();
    assert!(!provider.ok);
    assert!(
        provider
            .detail
            .starts_with("compte déconnecté (refresh_token_reused)"),
        "{}",
        provider.detail
    );
    let gauge = checks
        .iter()
        .find(|c| c.id == "provider.codex.quota")
        .unwrap();
    assert!(
        !gauge.ok && gauge.detail.contains("en retrait"),
        "{gauge:?}"
    );
}

/// Magasin qui refuse d'écrire en recopiant la valeur, comme le Trousseau de #148.
struct RefusingStore;
impl penelope_platform::secrets::SecretStore for RefusingStore {
    fn backend(&self) -> String {
        "refus".into()
    }
    fn get(&self, _: &str) -> penelope_platform::Result<Option<String>> {
        Ok(None)
    }
    fn set(&self, _: &str, value: &str) -> penelope_platform::Result<()> {
        Err(penelope_platform::PlatformError::Secret(format!(
            "security: valeur refusée : {value}"
        )))
    }
    fn delete(&self, _: &str) -> penelope_platform::Result<()> {
        Ok(())
    }
    fn list(&self) -> penelope_platform::Result<Vec<String>> {
        Ok(vec![])
    }
}

/// #148 : connexion réussie mais rangement impossible : les jetons sont révoqués et
/// l'erreur ne les recopie pas.
#[tokio::test]
async fn an_unstorable_grant_is_revoked_and_never_echoed() {
    let access = format!("x.{}.y", b64(&json!({"exp": 1_900_000_000i64})));
    let (url, seen) = scripted_server(vec![
        (
            200,
            json!({"device_auth_id": "dev_1", "user_code": "ABCD", "interval": "1"}).to_string(),
        ),
        (
            200,
            json!({"authorization_code": "code_1", "code_verifier": "ver_1"}).to_string(),
        ),
        (
            200,
            json!({"access_token": access, "refresh_token": "rt-secret-148", "id_token": id_token()})
                .to_string(),
        ),
        (200, "{}".to_string()),
        (200, "{}".to_string()),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let mut s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    let Some(platform) = Arc::get_mut(&mut s.platform) else {
        panic!("la plateforme des services de test est partagée");
    };
    platform.secrets = Box::new(RefusingStore);
    let issuer = url.clone();
    s.publish_config("test", move |c| {
        c.providers.codex.issuer = issuer.clone();
        c.providers.codex.enabled = true;
        Ok(vec!["providers.codex.issuer".into()])
    })
    .unwrap();

    let login = start(&s).await.unwrap();
    let err = wait_for(&s, &login).await.unwrap_err();
    assert!(err.starts_with("connexion annulée"), "{err}");
    assert!(err.contains("ils ont été révoqués"), "{err}");
    assert!(!err.contains("rt-secret-148"), "{err}");
    let reqs = seen.lock().unwrap().clone();
    let revoked: Vec<&String> = reqs
        .iter()
        .filter(|r| r.contains("/oauth/revoke"))
        .collect();
    assert_eq!(
        revoked.len(),
        2,
        "jeton de rafraîchissement et jeton d'accès"
    );
    assert!(revoked[0].contains("token=rt-secret-148"));
}
