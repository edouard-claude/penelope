//! Suite `mcp-conformance` (§8, CA 8).
//!
//! Chaque ligne du tableau §8.4 a au moins un test vert par version concernée, contre des
//! serveurs simulés, sans réseau.

use penelope_evals::mcp_servers::{MockAuthServer, conformance_matrix, server};
use penelope_mcp::client::McpClient;
use penelope_mcp::error::McpError;
use penelope_mcp::oauth::*;
use penelope_mcp::protocol::{ClientFeatures, ContentBlock, ProtocolVersion};
use serde_json::json;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

async fn connect(v: ProtocolVersion, transport: &'static str) -> McpClient {
    McpClient::connect(
        "tracker",
        server(v, transport),
        ProtocolVersion::V20260728,
        TIMEOUT,
        4,
        ClientFeatures::default(),
    )
    .await
    .unwrap_or_else(|e| panic!("connexion {v} / {transport} : {e}"))
}

/// La matrice complète : chaque combinaison négocie et liste ses outils.
#[tokio::test]
async fn ca_8_1_every_version_and_transport_negotiates() {
    let matrix = conformance_matrix();
    assert!(matrix.len() >= 13, "matrice trop courte : {}", matrix.len());

    for (v, t) in matrix {
        let c = connect(v, t).await;
        assert_eq!(c.version(), v, "version négociée incorrecte pour {v} / {t}");
        assert_eq!(
            c.negotiated().stateless,
            v.is_stateless_core(),
            "mode sans état incorrect pour {v}"
        );
        let tools = c.list_tools().await.unwrap();
        assert_eq!(tools.len(), 4, "{v} / {t} : pagination non suivie");
        assert!(tools.iter().any(|x| x.name == "search_issues"));
    }
}

/// CA 8 : un serveur 2026-07-28 et un serveur 2025-06-18 derrière la même URL sont
/// correctement détectés.
#[tokio::test]
async fn ca_8_2_mixed_versions_behind_the_same_url() {
    let modern = connect(ProtocolVersion::V20260728, "http").await;
    assert!(modern.negotiated().stateless);
    assert_eq!(modern.version(), ProtocolVersion::V20260728);

    let legacy = connect(ProtocolVersion::V20250618, "http").await;
    assert!(!legacy.negotiated().stateless);
    assert_eq!(legacy.version(), ProtocolVersion::V20250618);
}

#[tokio::test]
async fn tool_annotations_drive_risk_classification() {
    let c = connect(ProtocolVersion::V20260728, "http").await;
    let tools = c.list_tools().await.unwrap();
    let by = |n: &str| tools.iter().find(|t| t.name == n).unwrap().clone();

    use penelope_kernel::risk::{RiskClass, classify_annotations};
    assert_eq!(
        classify_annotations(&by("get_issue").annotations),
        RiskClass::Read
    );
    assert_eq!(
        classify_annotations(&by("create_issue").annotations),
        RiskClass::External
    );
    assert_eq!(
        classify_annotations(&by("delete_project").annotations),
        RiskClass::Destructive
    );
}

#[tokio::test]
async fn structured_output_is_validated_when_the_version_supports_it() {
    for v in [
        ProtocolVersion::V20250618,
        ProtocolVersion::V20251125,
        ProtocolVersion::V20260728,
    ] {
        let c = connect(v, "http").await;
        let tools = c.list_tools().await.unwrap();
        let schema = tools
            .iter()
            .find(|t| t.name == "get_issue")
            .and_then(|t| t.output_schema.clone())
            .unwrap_or_else(|| panic!("{v} : outputSchema attendu"));

        let r = c
            .call_tool("get_issue", json!({"id": 4312}), Some(&schema), None)
            .await
            .unwrap();
        assert!(!r.is_error, "{v} : sortie conforme refusée");
        assert_eq!(r.structured.unwrap()["subject"], "TVA incorrecte");
    }
}

#[tokio::test]
async fn execution_errors_come_back_to_the_model() {
    let c = connect(ProtocolVersion::V20260728, "http").await;
    let r = c.call_tool("failing", json!({}), None, None).await.unwrap();
    assert!(r.is_error, "isError doit être préservé");
    assert!(r.render_text().contains("n'existe pas"));
}

#[tokio::test]
async fn cacheable_results_are_read_on_2026() {
    let c = connect(ProtocolVersion::V20260728, "http").await;
    let r = c
        .call_tool("get_issue", json!({"id": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(r.cache_ttl_ms, Some(60_000));
    assert_eq!(r.cache_scope.as_deref(), Some("session"));
    assert!(r.is_complete());

    // Avant 2026-07-28, pas de `cacheable` : le TTL par défaut s'applique côté client.
    let old = connect(ProtocolVersion::V20250618, "stdio").await;
    let r = old
        .call_tool("get_issue", json!({"id": 1}), None, None)
        .await
        .unwrap();
    assert!(r.cache_ttl_ms.is_none());
}

#[tokio::test]
async fn resources_and_templates_are_reachable() {
    for v in [ProtocolVersion::V20241105, ProtocolVersion::V20260728] {
        let c = connect(v, "stdio").await;
        let resources = c.list_resources().await.unwrap();
        assert_eq!(resources.len(), 1, "{v}");
        let templates = c.list_resource_templates().await.unwrap();
        assert_eq!(templates.len(), 1, "{v}");

        let contents = c.read_resource("tracker://projets").await.unwrap();
        match &contents[0] {
            ContentBlock::Resource { text, .. } => {
                assert!(text.as_ref().unwrap().contains("Pénélope"))
            }
            other => panic!("{other:?}"),
        }
        c.subscribe_resource("tracker://projets").await.unwrap();
    }
}

#[tokio::test]
async fn prompts_and_completions_follow_the_version() {
    let modern = connect(ProtocolVersion::V20260728, "http").await;
    let prompts = modern.list_prompts().await.unwrap();
    assert_eq!(prompts[0]["name"], "resume_ticket");
    let got = modern
        .get_prompt("resume_ticket", json!({"id": 1}))
        .await
        .unwrap();
    assert!(got["messages"].is_array());

    let values = modern
        .complete(
            json!({"type":"ref/prompt","name":"resume_ticket"}),
            json!({"name":"id","value":"43"}),
        )
        .await
        .unwrap();
    assert_eq!(values, vec!["4312", "4313"]);

    // 2024-11-05 ne connaît pas les completions : l'appel échoue proprement.
    let old = connect(ProtocolVersion::V20241105, "stdio").await;
    assert!(
        old.complete(json!({}), json!({})).await.is_err(),
        "les completions n'existent pas avant 2025-03-26"
    );
}

#[tokio::test]
async fn change_subscriptions_follow_the_version() {
    let modern = connect(ProtocolVersion::V20260728, "http").await;
    assert!(modern.listen_changes().await.unwrap());

    let legacy = connect(ProtocolVersion::V20250618, "stdio").await;
    assert!(
        legacy.listen_changes().await.unwrap(),
        "avant 2026-07-28, les notifications arrivent d'elles-mêmes"
    );
}

#[tokio::test]
async fn logging_uses_meta_or_set_level() {
    let mut modern = connect(ProtocolVersion::V20260728, "http").await;
    modern.apply_log_level("debug").await.unwrap();

    let mut legacy = connect(ProtocolVersion::V20250618, "stdio").await;
    legacy.apply_log_level("debug").await.unwrap();
}

#[tokio::test]
async fn health_probe_works_on_every_version() {
    for (v, t) in conformance_matrix() {
        connect(v, t).await.health().await.unwrap_or_else(|e| {
            panic!("sonde de santé {v} / {t} : {e}");
        });
    }
}

#[tokio::test]
async fn tasks_are_only_available_from_2025_11_25() {
    let modern = connect(ProtocolVersion::V20260728, "http").await;
    let r = modern
        .call_tool("long_task", json!({}), None, None)
        .await
        .unwrap();
    assert!(!r.is_complete());
    assert_eq!(r.task_ref.as_deref(), Some("task-1"));
    let status = modern.task_get("task-1").await.unwrap();
    assert_eq!(status["status"], "completed");

    let old = connect(ProtocolVersion::V20250618, "stdio").await;
    assert!(old.task_get("task-1").await.is_err());
}

#[tokio::test]
async fn mrtr_input_required_is_surfaced() {
    let c = connect(ProtocolVersion::V20260728, "http").await;
    let r = c
        .call_tool("needs_input", json!({}), None, None)
        .await
        .unwrap();
    assert!(r.needs_input());
    let requests = r.input_requests.as_ref().unwrap();
    assert_eq!(requests["i1"]["method"], "elicitation/create");
    assert_eq!(r.request_state.as_deref(), Some("etat-1"));
}

#[tokio::test]
async fn capabilities_and_extensions_are_stored() {
    let c = connect(ProtocolVersion::V20260728, "http").await;
    let caps = c.capabilities();
    assert!(caps.tools && caps.resources && caps.prompts && caps.logging);
    assert!(caps.tasks, "l'extension Tasks doit être vue");
    assert_eq!(c.server_info()["name"], "tracker-mock");
    assert!(c.instructions().unwrap().contains("get_issue"));
}

#[tokio::test]
async fn unknown_tools_produce_a_clean_error() {
    let c = connect(ProtocolVersion::V20260728, "http").await;
    match c.call_tool("inexistant", json!({}), None, None).await {
        Err(McpError::Rpc { code, .. }) => assert_eq!(code, -32602),
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------------ OAuth

/// CA 8 : suite OAuth contre un serveur d'autorisation mock.
#[test]
fn ca_8_3_oauth_discovery_and_pkce() {
    let auth = MockAuthServer::new();

    // 1. Découverte via `WWW-Authenticate`.
    let w = parse_www_authenticate(&auth.www_authenticate(Some("tickets:write")));
    assert_eq!(w.scope.as_deref(), Some("tickets:write"));
    assert!(w.resource_metadata.is_some());

    // 2. Métadonnées de ressource protégée puis du serveur d'autorisation.
    let prm = auth.protected_resource_metadata();
    assert_eq!(prm["authorization_servers"][0], auth.issuer);
    let meta = AsMetadata::parse(&auth.as_metadata()).unwrap();
    assert!(meta.supports_s256());

    // 3. PKCE S256 et URL d'autorisation.
    let pkce = Pkce::generate();
    let state = random_state();
    let redirect = loopback_redirect(7777);
    let url = authorize_url(
        &meta,
        "client-1",
        &redirect,
        &pkce,
        &state,
        &auth.resource,
        &["tickets:read".to_string()],
    );
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("resource=https%3A%2F%2Fapi.example.com%2Fmcp"));

    // 4. Callback et validation.
    let req = AuthRequest {
        server: "tracker".into(),
        issuer: auth.issuer.clone(),
        state: state.clone(),
        verifier: pkce.verifier.clone(),
        redirect_uri: redirect.clone(),
        resource: auth.resource.clone(),
        scopes: vec!["tickets:read".into()],
        authorize_url: url,
        expires_at_ms: 0,
    };
    let cb = parse_callback(&format!(
        "{redirect}?code=abc&state={state}&iss={}",
        auth.issuer
    ));
    let code = validate_callback(&req, &cb).unwrap();
    assert_eq!(code, "abc");

    // 5. Échange de code avec vérification PKCE côté serveur.
    let tokens = Tokens::parse(&auth.token(&pkce.verifier, &pkce.challenge).unwrap(), 0).unwrap();
    assert_eq!(tokens.access_token, "at-123");
    assert!(tokens.refresh_token.is_some());
    assert_eq!(tokens.header_value(), "Bearer at-123");

    // Un mauvais verifier est refusé.
    assert!(auth.token("mauvais-verifier", &pkce.challenge).is_err());
}

#[test]
fn ca_8_3_invalid_issuer_is_rejected() {
    let auth = MockAuthServer::new();
    let state = random_state();
    let req = AuthRequest {
        server: "tracker".into(),
        issuer: auth.issuer.clone(),
        state: state.clone(),
        verifier: "v".into(),
        redirect_uri: loopback_redirect(7777),
        resource: auth.resource.clone(),
        scopes: vec![],
        authorize_url: String::new(),
        expires_at_ms: 0,
    };
    let cb = parse_callback(&format!(
        "http://127.0.0.1:7777/oauth/callback?code=abc&state={state}&iss=https://attaquant.example"
    ));
    assert!(validate_callback(&req, &cb).is_err());
}

#[test]
fn ca_8_3_incremental_consent_and_registration() {
    let auth = MockAuthServer::new();
    let meta = AsMetadata::parse(&auth.as_metadata()).unwrap();

    // Scope manquant détecté sur 403.
    let e = McpError::Unauthorized {
        status: 403,
        www_authenticate: auth.www_authenticate(Some("tickets:write")),
    };
    assert_eq!(e.missing_scopes(), vec!["tickets:write"]);
    let merged = incremental_scopes(&["tickets:read".into()], &e.missing_scopes());
    assert_eq!(merged, vec!["tickets:read", "tickets:write"]);

    // Ordre d'enregistrement : CIMD, puis pré-enregistré, puis DCR.
    assert!(matches!(
        choose_registration("https://penelope.example/cimd.json", None, &meta).unwrap(),
        ClientRegistration::Cimd { .. }
    ));
    assert!(matches!(
        choose_registration("", Some("client-1"), &meta).unwrap(),
        ClientRegistration::PreRegistered { .. }
    ));
    let dcr = choose_registration("", None, &meta).unwrap();
    assert!(matches!(dcr, ClientRegistration::Dynamic { .. }));

    let registered = auth.register(&dcr_body(&loopback_redirect(7777), &merged));
    assert_eq!(registered["client_id"], "client-dyn-1");
}

/// CA 8 : le flux `paste_back` fonctionne de bout en bout via le mock Telegram.
#[tokio::test]
async fn ca_8_3_paste_back_flow_over_telegram() {
    use penelope_telegram::{Incoming, classify};

    let auth = MockAuthServer::new();
    let state = random_state();
    let pasted = format!(
        "http://127.0.0.1:7777/oauth/callback?code=abc123&state={state}&iss={}",
        auth.issuer
    );

    // L'utilisateur colle l'URL dans Telegram.
    let update = penelope_telegram::mock::updates::text_message(1, 42, 42, &pasted);
    let url = match classify(&update, &penelope_telegram::Access::owner_only(42)) {
        Incoming::OAuthCallback { url, .. } => url,
        other => panic!("l'URL collée doit être reconnue : {other:?}"),
    };

    let req = AuthRequest {
        server: "tracker".into(),
        issuer: auth.issuer.clone(),
        state,
        verifier: "verifier-de-test-1234567890123456789012".into(),
        redirect_uri: loopback_redirect(7777),
        resource: auth.resource.clone(),
        scopes: vec![],
        authorize_url: String::new(),
        expires_at_ms: 0,
    };
    let code = validate_callback(&req, &parse_callback(&url)).unwrap();
    assert_eq!(code, "abc123");
}
