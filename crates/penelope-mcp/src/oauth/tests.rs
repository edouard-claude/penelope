use super::*;
use serde_json::json;

fn meta() -> AsMetadata {
    AsMetadata {
        issuer: "https://auth.example.com".into(),
        authorization_endpoint: "https://auth.example.com/authorize".into(),
        token_endpoint: "https://auth.example.com/token".into(),
        registration_endpoint: Some("https://auth.example.com/register".into()),
        scopes_supported: vec!["repo:read".into(), "repo:write".into()],
        code_challenge_methods_supported: vec!["S256".into()],
        grant_types_supported: vec!["authorization_code".into(), "refresh_token".into()],
        token_endpoint_auth_methods_supported: vec![],
    }
}

#[test]
fn pkce_is_s256_and_url_safe() {
    let p = Pkce::from_verifier("verifieur-de-test-suffisamment-long-123456");
    assert_ne!(p.challenge, p.verifier);
    assert!(!p.challenge.contains('+') && !p.challenge.contains('/') && !p.challenge.contains('='));
    // Déterminisme : le même verifier donne le même challenge.
    assert_eq!(
        Pkce::from_verifier("verifieur-de-test-suffisamment-long-123456").challenge,
        p.challenge
    );
    assert!(
        Pkce::generate().verifier.len() >= 43,
        "longueur minimale RFC 7636"
    );
}

#[test]
fn www_authenticate_parsing() {
    let w = parse_www_authenticate(
        r#"Bearer realm="mcp", error="insufficient_scope", scope="repo:write issues:read", resource_metadata="https://api.example.com/.well-known/oauth-protected-resource""#,
    );
    assert_eq!(w.scheme, "Bearer");
    assert_eq!(w.scope.as_deref(), Some("repo:write issues:read"));
    assert_eq!(w.error.as_deref(), Some("insufficient_scope"));
    assert!(
        w.resource_metadata
            .unwrap()
            .contains("oauth-protected-resource")
    );
}

#[test]
fn protected_resource_url_follows_rfc9728() {
    assert_eq!(
        protected_resource_url("https://api.example.com/mcp"),
        "https://api.example.com/.well-known/oauth-protected-resource/mcp"
    );
    assert_eq!(
        protected_resource_url("https://api.example.com/"),
        "https://api.example.com/.well-known/oauth-protected-resource"
    );
}

#[test]
fn discovery_tries_both_documents() {
    let u = discovery_urls("https://auth.example.com/");
    assert_eq!(
        u[0],
        "https://auth.example.com/.well-known/oauth-authorization-server"
    );
    assert_eq!(
        u[1],
        "https://auth.example.com/.well-known/openid-configuration"
    );
}

#[test]
fn authorize_url_carries_pkce_and_resource() {
    let p = Pkce::from_verifier("v-123456789012345678901234567890123456789012");
    let u = authorize_url(
        &meta(),
        "client-abc",
        "http://127.0.0.1:7777/oauth/callback",
        &p,
        "etat-123",
        "https://api.example.com/mcp",
        &["repo:read".to_string()],
    );
    assert!(u.starts_with("https://auth.example.com/authorize?"));
    assert!(u.contains("code_challenge_method=S256"));
    assert!(u.contains(&format!("code_challenge={}", urlencode(&p.challenge))));
    assert!(u.contains("resource=https%3A%2F%2Fapi.example.com%2Fmcp"));
    assert!(u.contains("state=etat-123"));
    assert!(u.contains("scope=repo%3Aread"));
}

#[test]
fn callback_parsing_accepts_full_url_or_query() {
    let full = "http://127.0.0.1:7777/oauth/callback?code=abc&state=xyz&iss=https%3A%2F%2Fauth.example.com";
    let c = parse_callback(full);
    assert_eq!(c.code.as_deref(), Some("abc"));
    assert_eq!(c.state.as_deref(), Some("xyz"));
    assert_eq!(c.iss.as_deref(), Some("https://auth.example.com"));
    let q = parse_callback("code=abc&state=xyz");
    assert_eq!(q.code.as_deref(), Some("abc"));
}

fn request() -> AuthRequest {
    AuthRequest {
        server: "forge".into(),
        issuer: "https://auth.example.com".into(),
        state: "etat-123".into(),
        verifier: "v".into(),
        redirect_uri: loopback_redirect("127.0.0.1", 7777),
        resource: "https://api.example.com/mcp".into(),
        scopes: vec![],
        authorize_url: String::new(),
        expires_at_ms: 0,
    }
}

#[test]
fn callback_validation_accepts_matching_state_and_issuer() {
    let code = validate_callback(
        &request(),
        &parse_callback("code=abc&state=etat-123&iss=https://auth.example.com/"),
    )
    .unwrap();
    assert_eq!(code, "abc");
}

#[test]
fn invalid_issuer_is_refused() {
    let e = validate_callback(
        &request(),
        &parse_callback("code=abc&state=etat-123&iss=https://attaquant.example"),
    )
    .unwrap_err();
    assert!(e.to_string().contains("iss"), "{e}");
}

#[test]
fn invalid_state_is_refused() {
    let e = validate_callback(&request(), &parse_callback("code=abc&state=autre")).unwrap_err();
    assert!(e.to_string().contains("state"), "{e}");
}

#[test]
fn provider_error_is_surfaced() {
    let e = validate_callback(
        &request(),
        &parse_callback("error=access_denied&error_description=refus%20utilisateur&state=etat-123"),
    )
    .unwrap_err();
    assert!(e.to_string().contains("access_denied"));
    assert!(e.to_string().contains("refus utilisateur"));
}

#[test]
fn registration_preference_order() {
    let m = meta();
    assert_eq!(
        choose_registration("https://penelope.example/cimd.json", Some("id"), &m, "srv").unwrap(),
        ClientRegistration::Cimd {
            url: "https://penelope.example/cimd.json".into()
        }
    );
    assert_eq!(
        choose_registration("", Some("id-preenregistre"), &m, "srv").unwrap(),
        ClientRegistration::PreRegistered {
            client_id: "id-preenregistre".into()
        }
    );
    assert_eq!(
        choose_registration("", None, &m, "srv").unwrap(),
        ClientRegistration::Dynamic {
            endpoint: "https://auth.example.com/register".into()
        }
    );
    let mut sans = m.clone();
    sans.registration_endpoint = None;
    assert!(choose_registration("", None, &sans, "srv").is_err());
}

#[test]
fn dcr_body_picks_application_type() {
    let native = dcr_body("http://127.0.0.1:7777/oauth/callback", &[]);
    assert_eq!(native["application_type"], "native");
    let web = dcr_body("https://penelope.example/oauth/callback", &[]);
    assert_eq!(web["application_type"], "web");
}

#[test]
fn token_bodies_include_resource_indicator() {
    let b = token_request_body(
        "code",
        "redir",
        "cid",
        "verif",
        "https://api.example.com/mcp",
    );
    assert_eq!(b["resource"], "https://api.example.com/mcp");
    assert_eq!(b["code_verifier"], "verif");
    let r = refresh_request_body("rt", "cid", "https://api.example.com/mcp");
    assert_eq!(r["grant_type"], "refresh_token");
    assert_eq!(r["resource"], "https://api.example.com/mcp");
}

#[test]
fn confidential_client_method_follows_server_metadata() {
    let mut m = meta();
    assert_eq!(m.client_auth_method(false).unwrap(), ClientAuthMethod::None);
    m.token_endpoint_auth_methods_supported = vec!["client_secret_post".into()];
    assert_eq!(m.client_auth_method(true).unwrap(), ClientAuthMethod::Post);
    m.token_endpoint_auth_methods_supported = vec!["client_secret_basic".into()];
    assert_eq!(m.client_auth_method(true).unwrap(), ClientAuthMethod::Basic);
    m.token_endpoint_auth_methods_supported = vec!["private_key_jwt".into()];
    assert!(m.client_auth_method(true).is_err());
}

#[test]
fn tokens_expiry_and_refresh_margin() {
    let t = Tokens::parse(
        &json!({"access_token":"at","refresh_token":"rt","expires_in":3600}),
        1_000_000,
    )
    .unwrap();
    assert_eq!(t.expires_at_ms, 1_000_000 + 3_600_000);
    assert!(!t.needs_refresh(1_000_000));
    assert!(t.needs_refresh(1_000_000 + 3_600_000 - 30_000));
    assert_eq!(t.header_value(), "Bearer at");

    let sans = Tokens::parse(&json!({"access_token":"at"}), 0).unwrap();
    assert!(
        !sans.needs_refresh(i64::MAX / 2),
        "sans expiration, pas de refresh"
    );
}

#[test]
fn token_response_without_access_token_is_an_error() {
    assert!(Tokens::parse(&json!({"error":"invalid_grant"}), 0).is_err());
}

#[test]
fn incremental_scopes_merge_without_duplicates() {
    let v = incremental_scopes(
        &["repo:read".into(), "issues:read".into()],
        &["repo:write".into(), "repo:read".into()],
    );
    assert_eq!(v, vec!["issues:read", "repo:read", "repo:write"]);
}

#[test]
fn as_metadata_requires_endpoints() {
    assert!(AsMetadata::parse(&json!({"issuer":"x"})).is_err());
    let m = AsMetadata::parse(&json!({
        "issuer":"https://a",
        "authorization_endpoint":"https://a/auth",
        "token_endpoint":"https://a/token"
    }))
    .unwrap();
    assert!(
        m.supports_s256(),
        "absence de liste = S256 supposé supporté"
    );
}

#[test]
fn cimd_document_is_self_describing() {
    let d = cimd_document(
        "https://penelope.example/cimd.json",
        "http://127.0.0.1:7777/oauth/callback",
    );
    assert_eq!(d["client_id"], "https://penelope.example/cimd.json");
    assert_eq!(d["token_endpoint_auth_method"], "none");
}
