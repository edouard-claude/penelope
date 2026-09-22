//! Autorisation OAuth 2.1 + PKCE pour les serveurs MCP HTTP (§8.5).
//!
//! Couvre : découverte PRM (RFC 9728) et AS metadata (RFC 8414 / OIDC), enregistrement du
//! client par CIMD puis client pré-enregistré puis DCR (RFC 7591), Resource Indicators
//! (RFC 8707), validation de `iss` (RFC 9207), consentement incrémental, rafraîchissement
//! des jetons, et le flux **headless** `paste_back`.

use crate::error::{McpError, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// ------------------------------------------------------------------ PKCE

/// Paire PKCE (S256 obligatoire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Pkce {
        let mut raw = [0u8; 32];
        let _ = getrandom::getrandom(&mut raw);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        Pkce::from_verifier(&verifier)
    }

    pub fn from_verifier(verifier: &str) -> Pkce {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h.finalize());
        Pkce {
            verifier: verifier.to_string(),
            challenge,
        }
    }
}

pub fn random_state() -> String {
    let mut raw = [0u8; 24];
    let _ = getrandom::getrandom(&mut raw);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

// ------------------------------------------------------------------ découverte

/// Contenu utile d'un en-tête `WWW-Authenticate`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WwwAuthenticate {
    pub scheme: String,
    pub realm: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
    /// RFC 9728 : URL des métadonnées de la ressource protégée.
    pub resource_metadata: Option<String>,
}

/// Analyse `Bearer realm="x", scope="a b", resource_metadata="https://…"`.
pub fn parse_www_authenticate(h: &str) -> WwwAuthenticate {
    let h = h.trim();
    let (scheme, rest) = match h.split_once(' ') {
        Some((s, r)) => (s.to_string(), r),
        None => (h.to_string(), ""),
    };
    let mut out = WwwAuthenticate {
        scheme,
        ..Default::default()
    };
    for part in split_params(rest) {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_matches('"').to_string();
        match k.as_str() {
            "realm" => out.realm = Some(v),
            "scope" => out.scope = Some(v),
            "error" => out.error = Some(v),
            "resource_metadata" | "resource_metadata_uri" => out.resource_metadata = Some(v),
            _ => {}
        }
    }
    out
}

/// Découpe une liste de paramètres en respectant les guillemets.
fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            ',' if !in_q => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Métadonnées du serveur d'autorisation (RFC 8414 / OIDC Discovery).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AsMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    pub grant_types_supported: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,
}

/// Mode d'authentification au point d'accès des jetons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAuthMethod {
    #[default]
    None,
    Post,
    Basic,
}

impl AsMetadata {
    pub fn parse(v: &Value) -> Result<AsMetadata> {
        let s = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let arr = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let m = AsMetadata {
            issuer: s("issuer"),
            authorization_endpoint: s("authorization_endpoint"),
            token_endpoint: s("token_endpoint"),
            registration_endpoint: v
                .get("registration_endpoint")
                .and_then(|x| x.as_str())
                .map(String::from),
            scopes_supported: arr("scopes_supported"),
            code_challenge_methods_supported: arr("code_challenge_methods_supported"),
            grant_types_supported: arr("grant_types_supported"),
            token_endpoint_auth_methods_supported: arr("token_endpoint_auth_methods_supported"),
        };
        if m.authorization_endpoint.is_empty() || m.token_endpoint.is_empty() {
            return Err(McpError::OAuth(
                "métadonnées du serveur d'autorisation incomplètes".into(),
            ));
        }
        Ok(m)
    }

    /// OAuth 2.1 impose PKCE ; S256 est le seul mode accepté par Pénélope.
    pub fn supports_s256(&self) -> bool {
        self.code_challenge_methods_supported.is_empty()
            || self
                .code_challenge_methods_supported
                .iter()
                .any(|m| m == "S256")
    }

    pub fn client_auth_method(&self, has_secret: bool) -> Result<ClientAuthMethod> {
        if !has_secret {
            return Ok(ClientAuthMethod::None);
        }
        let methods = &self.token_endpoint_auth_methods_supported;
        // RFC 8414 : en l'absence de cette propriété, le défaut est client_secret_basic.
        if methods.is_empty() || methods.iter().any(|m| m == "client_secret_basic") {
            return Ok(ClientAuthMethod::Basic);
        }
        if methods.iter().any(|m| m == "client_secret_post") {
            return Ok(ClientAuthMethod::Post);
        }
        Err(McpError::OAuth(
            "aucune méthode d'authentification compatible avec `client_secret`".into(),
        ))
    }
}

/// URLs de découverte à essayer, dans l'ordre, pour un issuer donné.
pub fn discovery_urls(issuer: &str) -> Vec<String> {
    let base = issuer.trim_end_matches('/');
    vec![
        format!("{base}/.well-known/oauth-authorization-server"),
        format!("{base}/.well-known/openid-configuration"),
    ]
}

/// URL des métadonnées de ressource protégée (RFC 9728).
pub fn protected_resource_url(resource: &str) -> String {
    match url::Url::parse(resource) {
        Ok(u) => {
            let origin = format!(
                "{}://{}{}",
                u.scheme(),
                u.host_str().unwrap_or_default(),
                u.port().map(|p| format!(":{p}")).unwrap_or_default()
            );
            let path = u.path().trim_end_matches('/');
            if path.is_empty() {
                format!("{origin}/.well-known/oauth-protected-resource")
            } else {
                format!("{origin}/.well-known/oauth-protected-resource{path}")
            }
        }
        Err(_) => format!(
            "{}/.well-known/oauth-protected-resource",
            resource.trim_end_matches('/')
        ),
    }
}

// ------------------------------------------------------------------ flux

/// Mode de redirection (§8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectMode {
    /// URL HTTPS publique vers le serveur local : callback traité automatiquement.
    PublicCallback,
    /// `http://127.0.0.1:<port>/oauth/callback`, avec collage manuel en secours.
    PasteBack,
}

impl RedirectMode {
    pub fn parse(s: &str) -> RedirectMode {
        match s {
            "public_callback" => RedirectMode::PublicCallback,
            _ => RedirectMode::PasteBack,
        }
    }
}

/// Demande d'autorisation en cours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthRequest {
    pub server: String,
    pub issuer: String,
    pub state: String,
    pub verifier: String,
    pub redirect_uri: String,
    pub resource: String,
    pub scopes: Vec<String>,
    pub authorize_url: String,
    pub expires_at_ms: i64,
}

/// Construit l'URL d'autorisation.
pub fn authorize_url(
    meta: &AsMetadata,
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
    resource: &str,
    scopes: &[String],
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("response_type", "code".into()),
        ("client_id", client_id.into()),
        ("redirect_uri", redirect_uri.into()),
        ("code_challenge", pkce.challenge.clone()),
        ("code_challenge_method", "S256".into()),
        ("state", state.into()),
        // RFC 8707 : indicateur de ressource systématique.
        ("resource", resource.into()),
    ];
    if !scopes.is_empty() {
        params.push(("scope", scopes.join(" ")));
    }
    let query: String = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let sep = if meta.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{}{sep}{query}", meta.authorization_endpoint)
}

/// Résultat du callback (ou d'une URL collée dans Telegram).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub iss: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Extrait `code`, `state` et `iss` d'une URL collée (§8.5, flux `paste_back`).
///
/// Accepte une URL complète, un fragment `?code=…`, ou la seule chaîne de requête.
pub fn parse_callback(input: &str) -> CallbackParams {
    let query = input
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or(input)
        .split('#')
        .next()
        .unwrap_or("");
    let mut out = CallbackParams::default();
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let v = urldecode(v);
        match k {
            "code" => out.code = Some(v),
            "state" => out.state = Some(v),
            "iss" => out.iss = Some(v),
            "error" => out.error = Some(v),
            "error_description" => out.error_description = Some(v),
            _ => {}
        }
    }
    out
}

/// Vérifie le retour d'autorisation.
///
/// - `state` DOIT correspondre ;
/// - si `iss` est présent, il DOIT correspondre à l'issuer enregistré (RFC 9207), sinon
///   **échec** : c'est une défense contre la substitution de serveur d'autorisation.
pub fn validate_callback(req: &AuthRequest, cb: &CallbackParams) -> Result<String> {
    if let Some(e) = &cb.error {
        return Err(McpError::OAuth(format!(
            "autorisation refusée : {e}{}",
            cb.error_description
                .as_ref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        )));
    }
    let Some(state) = &cb.state else {
        return Err(McpError::OAuth("réponse sans `state`".into()));
    };
    if state != &req.state {
        return Err(McpError::OAuth(
            "`state` invalide : la demande ne correspond pas".into(),
        ));
    }
    if let Some(iss) = &cb.iss
        && normalise_issuer(iss) != normalise_issuer(&req.issuer)
    {
        return Err(McpError::OAuth(format!(
            "`iss` inattendu : {iss} au lieu de {}",
            req.issuer
        )));
    }
    cb.code
        .clone()
        .ok_or_else(|| McpError::OAuth("réponse sans `code`".into()))
}

fn normalise_issuer(s: &str) -> String {
    s.trim_end_matches('/').to_lowercase()
}

/// Corps de l'échange code contre jeton.
pub fn token_request_body(
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    verifier: &str,
    resource: &str,
) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("grant_type".into(), "authorization_code".into());
    m.insert("code".into(), code.into());
    m.insert("redirect_uri".into(), redirect_uri.into());
    m.insert("client_id".into(), client_id.into());
    m.insert("code_verifier".into(), verifier.into());
    m.insert("resource".into(), resource.into());
    m
}

/// Corps du rafraîchissement.
pub fn refresh_request_body(
    refresh_token: &str,
    client_id: &str,
    resource: &str,
) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("grant_type".into(), "refresh_token".into());
    m.insert("refresh_token".into(), refresh_token.into());
    m.insert("client_id".into(), client_id.into());
    m.insert("resource".into(), resource.into());
    m
}

/// Jetons reçus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    /// Instant d'expiration absolu, calculé à la réception.
    #[serde(default)]
    pub expires_at_ms: i64,
}

impl Tokens {
    pub fn parse(v: &Value, now_ms: i64) -> Result<Tokens> {
        let access_token = v
            .get("access_token")
            .and_then(|x| x.as_str())
            .ok_or_else(|| McpError::OAuth("réponse de jeton sans `access_token`".into()))?
            .to_string();
        let expires_in = v.get("expires_in").and_then(|x| x.as_u64());
        Ok(Tokens {
            access_token,
            refresh_token: v
                .get("refresh_token")
                .and_then(|x| x.as_str())
                .map(String::from),
            token_type: v
                .get("token_type")
                .and_then(|x| x.as_str())
                .unwrap_or("Bearer")
                .to_string(),
            expires_at_ms: expires_in
                .map(|s| now_ms + (s as i64) * 1000)
                .unwrap_or(i64::MAX),
            expires_in,
            scope: v.get("scope").and_then(|x| x.as_str()).map(String::from),
        })
    }

    pub fn header_value(&self) -> String {
        let t = if self.token_type.is_empty() {
            "Bearer"
        } else {
            &self.token_type
        };
        format!("{t} {}", self.access_token)
    }

    /// Vrai s'il faut rafraîchir (marge de 60 s).
    pub fn needs_refresh(&self, now_ms: i64) -> bool {
        self.expires_at_ms != i64::MAX && now_ms + 60_000 >= self.expires_at_ms
    }
}

/// Stratégie d'enregistrement du client, par ordre de préférence (§8.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRegistration {
    /// Client ID Metadata Document, servi par Pénélope à une URL HTTPS.
    Cimd { url: String },
    /// Client pré-enregistré dans la configuration.
    PreRegistered { client_id: String },
    /// Dynamic Client Registration (RFC 7591, déprécié mais supporté).
    Dynamic { endpoint: String },
}

/// Choisit la stratégie d'enregistrement.
pub fn choose_registration(
    cimd_url: &str,
    configured_client_id: Option<&str>,
    meta: &AsMetadata,
    server: &str,
) -> Result<ClientRegistration> {
    if !cimd_url.is_empty() {
        return Ok(ClientRegistration::Cimd {
            url: cimd_url.to_string(),
        });
    }
    if let Some(id) = configured_client_id.filter(|s| !s.is_empty()) {
        return Ok(ClientRegistration::PreRegistered {
            client_id: id.to_string(),
        });
    }
    if let Some(ep) = &meta.registration_endpoint {
        return Ok(ClientRegistration::Dynamic {
            endpoint: ep.clone(),
        });
    }
    Err(McpError::OAuth(format!(
        "ce serveur exige un client pré-enregistré : ni CIMD, ni enregistrement dynamique. \
         Créer l'app chez le fournisseur, puis `penelope mcp edit {server} client_id <id>` ; \
         voir docs/mcp.md § Client pré-enregistré"
    )))
}

/// Corps d'enregistrement dynamique (RFC 7591).
pub fn dcr_body(redirect_uri: &str, scopes: &[String]) -> Value {
    // `application_type` : `native` pour une redirection loopback, `web` sinon.
    let native = redirect_uri.starts_with("http://127.0.0.1")
        || redirect_uri.starts_with("http://localhost");
    serde_json::json!({
        "client_name": "Penelope",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": if native { "native" } else { "web" },
        "scope": scopes.join(" "),
    })
}

/// Document CIMD servi par Pénélope.
pub fn cimd_document(cimd_url: &str, redirect_uri: &str) -> Value {
    serde_json::json!({
        "client_id": cimd_url,
        "client_name": "Penelope",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    })
}

/// Fusionne les scopes déjà accordés et ceux qui manquent (consentement incrémental).
pub fn incremental_scopes(granted: &[String], missing: &[String]) -> Vec<String> {
    let mut v: Vec<String> = granted.to_vec();
    for m in missing {
        if !v.iter().any(|g| g == m) {
            v.push(m.clone());
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Adresse de redirection locale. `host` vaut `127.0.0.1` ou `localhost` ; la
/// configuration le valide, et l'URL doit correspondre exactement à celle enregistrée
/// chez le fournisseur.
pub fn loopback_redirect(host: &str, port: u16) -> String {
    format!("http://{host}:{port}/oauth/callback")
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
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
        assert!(
            !p.challenge.contains('+') && !p.challenge.contains('/') && !p.challenge.contains('=')
        );
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
            &parse_callback(
                "error=access_denied&error_description=refus%20utilisateur&state=etat-123",
            ),
        )
        .unwrap_err();
        assert!(e.to_string().contains("access_denied"));
        assert!(e.to_string().contains("refus utilisateur"));
    }

    #[test]
    fn registration_preference_order() {
        let m = meta();
        assert_eq!(
            choose_registration("https://penelope.example/cimd.json", Some("id"), &m, "srv")
                .unwrap(),
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
}
