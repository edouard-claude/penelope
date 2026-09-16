//! Autorisation OAuth des serveurs MCP HTTP (§8.5), côté daemon.
//!
//! Découverte (ressource protégée puis serveur d'autorisation), enregistrement du client
//! (CIMD, client configuré, ou enregistrement dynamique indexé par issuer), PKCE S256,
//! `resource` systématique. La machine n'a pas de navigateur : l'URL part sur Telegram,
//! et l'adresse de retour est soit reçue par le serveur local `127.0.0.1`, soit **collée**
//! dans la conversation. Les jetons vivent dans le SecretStore et sont rafraîchis avant
//! chaque connexion.

use crate::runtime::{Daemon, Services};
use penelope_kernel::event::EventDraft;
use penelope_mcp::ServerConfig;
use penelope_mcp::oauth::{
    self, AsMetadata, AuthRequest, ClientRegistration, Pkce, RedirectMode, Tokens,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// Durée de validité d'une demande d'autorisation (§8.5 : 10 min).
pub const REQUEST_TTL_MS: i64 = 10 * 60_000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Autorisation obtenue pour un serveur, rangée dans le SecretStore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub issuer: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub resource: String,
    pub tokens: Tokens,
}

/// Demande en cours, retrouvée par son `state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    request: AuthRequest,
    client_id: String,
    token_endpoint: String,
}

/// Demande d'autorisation prête à être envoyée au propriétaire.
#[derive(Debug, Clone, Serialize)]
pub struct AuthStart {
    pub server: String,
    pub url: String,
    pub mode: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub expires_at_ms: i64,
}

fn secret_name(server: &str) -> String {
    format!("mcp.{server}.oauth")
}

fn pending_key(state: &str) -> String {
    format!("mcp.oauth.pending.{state}")
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("penelope/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

/// Un code, un vérificateur PKCE ou un jeton ne voyagent que chiffrés : HTTPS, ou HTTP
/// vers la boucle locale **exacte** (`127.0.0.0/8`, `[::1]`, `localhost`).
///
/// L'hôte est lu par un analyseur d'URL, pas par préfixe : `http://localhost.exemple.org`,
/// `http://127.0.0.1.exemple.org` ou `http://localhost@exemple.org` ne passent pas pour la
/// boucle locale. Une URL portant des identifiants est toujours refusée.
pub fn check_endpoint(raw: &str) -> Result<(), String> {
    let refuse = || {
        Err(format!(
            "point d'accès OAuth refusé (HTTPS obligatoire hors boucle locale) : {raw}"
        ))
    };
    let Ok(url) = url::Url::parse(raw) else {
        return refuse();
    };
    if !url.username().is_empty() || url.password().is_some() {
        return refuse();
    }
    match (url.scheme(), url.host()) {
        ("https", Some(_)) => Ok(()),
        ("http", Some(url::Host::Ipv4(ip))) if ip.is_loopback() => Ok(()),
        ("http", Some(url::Host::Ipv6(ip))) if ip.is_loopback() => Ok(()),
        ("http", Some(url::Host::Domain(host))) if host.eq_ignore_ascii_case("localhost") => Ok(()),
        _ => refuse(),
    }
}

/// `schéma://hôte[:port]` d'une URL, lu par l'analyseur ; `None` si l'URL porte des
/// identifiants ou n'a pas d'hôte.
fn origin(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let resp = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("GET {url} : {e}"))?;
    let status = resp.status().as_u16();
    if status >= 400 {
        return Err(format!("GET {url} : HTTP {status}"));
    }
    resp.json().await.map_err(|e| format!("GET {url} : {e}"))
}

async fn post_form(
    client: &reqwest::Client,
    url: &str,
    form: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let resp = client
        .post(url)
        .header("Accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| format!("POST {url} : {e}"))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status >= 400 {
        let why = body["error_description"]
            .as_str()
            .or_else(|| body["error"].as_str())
            .unwrap_or("refus");
        return Err(format!("POST {url} : HTTP {status} ({why})"));
    }
    Ok(body)
}

/// Prépare une autorisation : l'URL à ouvrir et la demande mémorisée.
pub async fn start(
    d: &Daemon,
    cfg: &ServerConfig,
    www_authenticate: Option<&str>,
) -> Result<AuthStart, String> {
    let s = &d.services;
    if !matches!(cfg.effective_transport(), "http" | "sse") {
        return Err(format!(
            "`{}` n'est pas un serveur HTTP : l'autorisation OAuth ne s'applique pas",
            cfg.name
        ));
    }
    // Le jeton obtenu partira vers ce serveur : jamais en clair.
    check_endpoint(&cfg.url).map_err(|_| {
        format!(
            "`{}` n'est pas servi en HTTPS : un jeton OAuth ne lui sera pas confié",
            cfg.name
        )
    })?;
    let client = http()?;
    let www = www_authenticate.map(oauth::parse_www_authenticate);

    // 1. Ressource protégée (RFC 9728), sinon l'origine du serveur MCP. Des métadonnées
    //    lues en clair pourraient détourner l'autorisation : HTTPS là aussi.
    let prm_url = www
        .as_ref()
        .and_then(|w| w.resource_metadata.clone())
        .filter(|u| check_endpoint(u).is_ok())
        .unwrap_or_else(|| oauth::protected_resource_url(&cfg.url));
    let from_prm = match check_endpoint(&prm_url) {
        Ok(()) => get_json(&client, &prm_url).await.ok().and_then(|v| {
            v["authorization_servers"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .map(String::from)
        }),
        Err(_) => None,
    };
    let issuer = from_prm
        .or_else(|| origin(&cfg.url))
        .ok_or_else(|| format!("URL du serveur `{}` illisible", cfg.name))?;
    check_endpoint(&issuer)?;

    // 2. Serveur d'autorisation (RFC 8414, puis OpenID Connect Discovery).
    let mut meta: Option<AsMetadata> = None;
    let mut last_error = String::new();
    for url in oauth::discovery_urls(&issuer) {
        match get_json(&client, &url).await {
            Ok(v) => match AsMetadata::parse(&v) {
                Ok(m) => {
                    meta = Some(m);
                    break;
                }
                Err(e) => last_error = e.to_string(),
            },
            Err(e) => last_error = e,
        }
    }
    let meta = meta.ok_or_else(|| {
        format!("métadonnées du serveur d'autorisation introuvables pour {issuer} ({last_error})")
    })?;
    if !meta.supports_s256() {
        return Err("le serveur d'autorisation ne propose pas PKCE S256".into());
    }
    check_endpoint(&meta.authorization_endpoint)?;
    check_endpoint(&meta.token_endpoint)?;
    let issuer = if meta.issuer.is_empty() {
        issuer
    } else {
        meta.issuer.clone()
    };

    // 3. Retour et scopes (consentement incrémental sur 401/403).
    let mcfg = s.config.config().mcp.clone();
    let mode = RedirectMode::parse(&mcfg.oauth_redirect_mode);
    let redirect_uri = match mode {
        RedirectMode::PublicCallback => mcfg.public_callback_url.clone(),
        RedirectMode::PasteBack => oauth::loopback_redirect(mcfg.callback_port),
    };
    check_endpoint(&redirect_uri)?;
    if !mcfg.cimd_url.is_empty() && !mcfg.cimd_url.starts_with("https://") {
        return Err(format!(
            "mcp.cimd_url doit être une URL HTTPS : {}",
            mcfg.cimd_url
        ));
    }
    let missing: Vec<String> = www
        .as_ref()
        .and_then(|w| w.scope.clone())
        .map(|sc| sc.split_whitespace().map(String::from).collect())
        .unwrap_or_default();
    let scopes = oauth::incremental_scopes(&cfg.scopes, &missing);

    // 4. Client : CIMD, client configuré, ou enregistrement dynamique par issuer.
    let client_id = match oauth::choose_registration(&mcfg.cimd_url, Some(&cfg.client_id), &meta)
        .map_err(|e| e.to_string())?
    {
        ClientRegistration::Cimd { url } => url,
        ClientRegistration::PreRegistered { client_id } => client_id,
        ClientRegistration::Dynamic { endpoint } => {
            let key = format!(
                "mcp.oauth.client.{}",
                &penelope_kernel::canonical::sha256_hex(
                    format!("{issuer}|{redirect_uri}").as_bytes()
                )[..24]
            );
            match d.kv_get(&key).await.map_err(|e| e.to_string())? {
                Some(id) if !id.is_empty() => id,
                _ => {
                    check_endpoint(&endpoint)?;
                    let resp = client
                        .post(&endpoint)
                        .json(&oauth::dcr_body(&redirect_uri, &scopes))
                        .send()
                        .await
                        .map_err(|e| format!("enregistrement du client : {e}"))?;
                    let v: Value = resp.json().await.unwrap_or(Value::Null);
                    let id = v["client_id"]
                        .as_str()
                        .ok_or("enregistrement du client refusé (pas de `client_id`)")?
                        .to_string();
                    d.kv_set(&key, &id).await.map_err(|e| e.to_string())?;
                    id
                }
            }
        }
    };

    // 5. PKCE, `state`, demande mémorisée.
    let pkce = Pkce::generate();
    let state = oauth::random_state();
    let url = oauth::authorize_url(
        &meta,
        &client_id,
        &redirect_uri,
        &pkce,
        &state,
        &cfg.url,
        &scopes,
    );
    let expires_at_ms = s.clock.now_ms() + REQUEST_TTL_MS;
    let pending = Pending {
        request: AuthRequest {
            server: cfg.name.clone(),
            issuer,
            state: state.clone(),
            verifier: pkce.verifier,
            redirect_uri: redirect_uri.clone(),
            resource: cfg.url.clone(),
            scopes: scopes.clone(),
            authorize_url: url.clone(),
            expires_at_ms,
        },
        client_id,
        token_endpoint: meta.token_endpoint.clone(),
    };
    d.kv_set(
        &pending_key(&state),
        &serde_json::to_string(&pending).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = s
        .events
        .append(EventDraft::new(
            "mcp.auth_requested",
            json!({"server": cfg.name, "mode": mcfg.oauth_redirect_mode, "scopes": scopes}),
        ))
        .await;
    Ok(AuthStart {
        server: cfg.name.clone(),
        url,
        mode: mcfg.oauth_redirect_mode,
        redirect_uri,
        scopes,
        expires_at_ms,
    })
}

/// Termine une autorisation à partir de l'URL de retour (collée ou reçue). Renvoie le
/// nom du serveur autorisé.
pub async fn complete(d: &Daemon, callback: &str) -> Result<String, String> {
    let s = &d.services;
    let cb = oauth::parse_callback(callback);
    let state = cb.state.clone().ok_or("adresse de retour sans `state`")?;
    let raw = d
        .kv_get(&pending_key(&state))
        .await
        .map_err(|e| e.to_string())?
        .filter(|r| !r.is_empty())
        .ok_or("aucune autorisation en attente pour cette adresse (déjà utilisée ou expirée)")?;
    // Une adresse de retour ne sert qu'une fois, qu'elle aboutisse ou non.
    let _ = d.kv_delete(&pending_key(&state)).await;
    let pending: Pending = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let server = pending.request.server.clone();
    if s.clock.now_ms() > pending.request.expires_at_ms {
        return Err(format!("demande expirée : relancer `/mcp auth {server}`"));
    }
    let code = oauth::validate_callback(&pending.request, &cb).map_err(|e| e.to_string())?;
    check_endpoint(&pending.token_endpoint)?;
    let body = oauth::token_request_body(
        &code,
        &pending.request.redirect_uri,
        &pending.client_id,
        &pending.request.verifier,
        &pending.request.resource,
    );
    let v = post_form(&http()?, &pending.token_endpoint, &body).await?;
    let tokens = Tokens::parse(&v, s.clock.now_ms()).map_err(|e| e.to_string())?;
    let grant = Grant {
        issuer: pending.request.issuer.clone(),
        token_endpoint: pending.token_endpoint.clone(),
        client_id: pending.client_id.clone(),
        resource: pending.request.resource.clone(),
        tokens,
    };
    s.platform
        .secrets
        .set(
            &secret_name(&server),
            &serde_json::to_string(&grant).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let _ = s
        .events
        .append(EventDraft::new(
            "mcp.authorized",
            json!({"server": server, "issuer": grant.issuer, "scope": grant.tokens.scope}),
        ))
        .await;
    Ok(server)
}

/// En-tête `Authorization` d'un serveur autorisé, jeton rafraîchi au besoin. `None` : pas
/// d'autorisation, ou autorisation expirée sans jeton de rafraîchissement. Un serveur qui
/// n'est pas servi en HTTPS (ni en boucle locale) ne reçoit jamais le jeton.
pub async fn authorization_header(
    s: &Services,
    server: &str,
    server_url: &str,
) -> Result<Option<String>, String> {
    let Some(raw) = s
        .platform
        .secrets
        .get(&secret_name(server))
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    check_endpoint(server_url).map_err(|_| {
        format!("jeton OAuth de `{server}` refusé : {server_url} n'est pas servi en HTTPS")
    })?;
    let mut grant: Grant = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let now = s.clock.now_ms();
    if grant.tokens.needs_refresh(now) {
        check_endpoint(&grant.token_endpoint)?;
        let Some(refresh) = grant.tokens.refresh_token.clone() else {
            return Ok(None);
        };
        let body = oauth::refresh_request_body(&refresh, &grant.client_id, &grant.resource);
        let v = post_form(&http()?, &grant.token_endpoint, &body)
            .await
            .map_err(|e| format!("rafraîchissement du jeton de `{server}` : {e}"))?;
        let mut tokens = Tokens::parse(&v, now).map_err(|e| e.to_string())?;
        // Rotation : un nouveau jeton de rafraîchissement remplace l'ancien, sinon on le garde.
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = Some(refresh);
        }
        grant.tokens = tokens;
        s.platform
            .secrets
            .set(
                &secret_name(server),
                &serde_json::to_string(&grant).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(Some(grant.tokens.header_value()))
}

/// Oublie l'autorisation d'un serveur.
pub fn forget(s: &Services, server: &str) -> Result<(), String> {
    s.platform
        .secrets
        .delete(&secret_name(server))
        .map_err(|e| e.to_string())
}

/// Message d'autorisation pour un canal sans boutons.
pub fn prompt_text(a: &AuthStart) -> String {
    let scopes = if a.scopes.is_empty() {
        "par défaut".to_string()
    } else {
        a.scopes.join(" ")
    };
    let mut t = format!(
        "🔐 **Autorisation requise** pour `{}` (scopes : {scopes})\n\n[Autoriser]({})",
        a.server, a.url
    );
    if a.mode == "paste_back" {
        t.push_str(
            "\n\nSi la page finit sur une erreur `127.0.0.1`, copie l'adresse de la barre \
             du navigateur et colle-la ici. Valable 10 minutes.",
        );
    }
    t
}

// ------------------------------------------------------------------ callback local

/// Serveur de retour local (`127.0.0.1:<callback_port>`) : reçoit le retour OAuth quand le
/// navigateur y a accès (tunnel SSH, `public_callback`), et sert le document CIMD.
pub async fn callback_server(d: Arc<Daemon>) {
    let port = d.services.config.config().mcp.callback_port;
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(port, error = %e, "callback OAuth local indisponible : le collage dans Telegram reste possible");
            return;
        }
    };
    while !d.handle.is_shutting_down() {
        let accepted = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
        let Ok(Ok((stream, _))) = accepted else {
            continue;
        };
        let d2 = d.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_callback(&d2, stream).await {
                tracing::debug!(error = %e, "requête du callback OAuth");
            }
        });
    }
}

async fn handle_callback(
    d: &Arc<Daemon>,
    mut stream: tokio::net::TcpStream,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 8192];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .map_err(|_| std::io::Error::other("délai"))??;
    let head = String::from_utf8_lossy(&buf[..n]);
    let target = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let cfg = d.services.config.config();
    let cimd_path = cfg
        .mcp
        .cimd_url
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_string()));
    let (status, ctype, body) = if target.starts_with("/oauth/callback") {
        match complete(d, &target).await {
            Ok(server) => {
                reconnect_and_tell(d, &server).await;
                (
                    "200 OK",
                    "text/html; charset=utf-8",
                    format!(
                        "<p>Autorisation de <b>{server}</b> reçue. Tu peux fermer cette page.</p>"
                    ),
                )
            }
            Err(e) => (
                "400 Bad Request",
                "text/html; charset=utf-8",
                format!("<p>Autorisation impossible : {}</p>", html_escape(&e)),
            ),
        }
    } else if cimd_path.as_deref() == Some(target.as_str()) {
        let redirect = match RedirectMode::parse(&cfg.mcp.oauth_redirect_mode) {
            RedirectMode::PublicCallback => cfg.mcp.public_callback_url.clone(),
            RedirectMode::PasteBack => oauth::loopback_redirect(cfg.mcp.callback_port),
        };
        (
            "200 OK",
            "application/json",
            oauth::cimd_document(&cfg.mcp.cimd_url, &redirect).to_string(),
        )
    } else {
        ("404 Not Found", "text/plain", "introuvable".to_string())
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.shutdown().await
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Après une autorisation : reconnexion du serveur et message au propriétaire.
pub async fn reconnect_and_tell(d: &Daemon, server: &str) {
    let text = match d.hooks.mcp_supervisor() {
        Some(sup) => match sup.restart(server).await {
            Ok(st) => format!(
                "🔐 `{server}` autorisé : {} outil(s) disponible(s).",
                st.tool_count
            ),
            Err(e) => format!("🔐 `{server}` autorisé, mais la reconnexion échoue : {e}"),
        },
        None => format!("🔐 `{server}` autorisé."),
    };
    if let Some(m) = d.hooks.messenger() {
        let origin = crate::scheduler::owner_origin(d);
        let _ = m.send_text(&origin, &text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

    /// Faux serveur d'autorisation : métadonnées, enregistrement, jetons.
    async fn fake_authorization_server() -> (String, Seen) {
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
                    log.lock()
                        .unwrap()
                        .push((method.clone(), path.clone(), body.clone()));
                    let json = match (method.as_str(), path.as_str()) {
                        ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                            json!({"resource": format!("{b}/mcp"), "authorization_servers": [b]})
                        }
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
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), shared)
                .await
                .unwrap(),
        );
        let d = Daemon::from_services(s.clone());
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
            serde_json::from_str(&s.platform.secrets.get("mcp.suivi.oauth").unwrap().unwrap())
                .unwrap();
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
}
