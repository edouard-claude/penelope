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
    self, AsMetadata, AuthRequest, ClientAuthMethod, ClientRegistration, Pkce, RedirectMode, Tokens,
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
    #[serde(default)]
    pub client_secret_ref: String,
    #[serde(default)]
    pub auth_method: ClientAuthMethod,
    pub resource: String,
    pub tokens: Tokens,
}

/// Demande en cours, retrouvée par son `state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    request: AuthRequest,
    client_id: String,
    #[serde(default)]
    client_secret_ref: String,
    #[serde(default)]
    auth_method: ClientAuthMethod,
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
    basic: Option<(&str, &str)>,
) -> Result<Value, String> {
    let mut request = client
        .post(url)
        .header("Accept", "application/json")
        .form(form);
    if let Some((client_id, secret)) = basic {
        // RFC 6749 §2.3.1 applique d'abord l'encodage formulaire aux deux identifiants.
        let encode = |value: &str| {
            url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
        };
        request = request.basic_auth(encode(client_id), Some(encode(secret)));
    }
    let resp = request
        .send()
        .await
        .map_err(|e| penelope_observe::redact(&format!("POST {url} : {e}")))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status >= 400 {
        let why = body["error_description"]
            .as_str()
            .or_else(|| body["error"].as_str())
            .unwrap_or("refus");
        return Err(penelope_observe::redact(&format!(
            "POST {url} : HTTP {status} ({why})"
        )));
    }
    Ok(body)
}

/// Résout le secret uniquement à l'appel du point d'accès des jetons.
fn token_client_auth(
    secrets: &dyn penelope_platform::SecretStore,
    method: ClientAuthMethod,
    reference: &str,
    client_id: &str,
    body: &mut BTreeMap<String, String>,
) -> Result<Option<(String, String)>, String> {
    if method == ClientAuthMethod::None {
        return Ok(None);
    }
    let name = reference
        .strip_prefix("${SECRET:")
        .and_then(|s| s.strip_suffix('}'))
        .ok_or("référence `client_secret` invalide")?;
    penelope_platform::validate_secret_name(name).map_err(|_| "nom de secret invalide")?;
    let secret = secrets
        .get(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("secret OAuth `{name}` absent"))?;
    penelope_observe::register_secret(&secret);
    match method {
        ClientAuthMethod::Post => {
            body.insert("client_secret".into(), secret);
            Ok(None)
        }
        ClientAuthMethod::Basic => {
            body.remove("client_id");
            Ok(Some((client_id.into(), secret)))
        }
        ClientAuthMethod::None => Ok(None),
    }
}

/// Prépare une autorisation : l'URL à ouvrir et la demande mémorisée.
pub async fn start(
    d: &Daemon,
    cfg: &ServerConfig,
    www_authenticate: Option<&str>,
) -> Result<AuthStart, String> {
    cfg.validate().map_err(|e| e.to_string())?;
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
    let prm = match check_endpoint(&prm_url) {
        Ok(()) => get_json(&client, &prm_url).await.ok(),
        Err(_) => None,
    };
    let from_prm = prm.as_ref().and_then(|v| {
        v["authorization_servers"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
            .map(String::from)
    });
    // Portées annoncées par la ressource : dernier recours quand ni la déclaration ni le
    // défi 401 n'en donnent (Slack n'en met pas dans le défi).
    let prm_scopes: Vec<String> = prm
        .as_ref()
        .and_then(|v| v["scopes_supported"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
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
    let auth_method = meta
        .client_auth_method(!cfg.client_secret.is_empty())
        .map_err(|e| e.to_string())?;
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
        RedirectMode::PasteBack => {
            oauth::loopback_redirect(&mcfg.callback_host, mcfg.callback_port)
        }
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
    let mut scopes = oauth::incremental_scopes(&cfg.scopes, &missing);
    // Une demande sans portée est refusée par certains serveurs d'autorisation, dont
    // Slack. `penelope mcp edit <srv> scopes '[…]'` garde la main, et une app n'accorde
    // que ce qu'elle déclare : restreindre là plutôt qu'ici.
    if scopes.is_empty() && !prm_scopes.is_empty() {
        scopes = prm_scopes;
        tracing::info!(
            serveur = %cfg.name,
            portees = %scopes.join(" "),
            "aucune portée configurée : celles annoncées par la ressource sont demandées"
        );
    }

    // 4. Client : CIMD, client configuré, ou enregistrement dynamique par issuer.
    let client_id =
        match oauth::choose_registration(&mcfg.cimd_url, Some(&cfg.client_id), &meta, &cfg.name)
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
                match s.kv_get(&key).await.map_err(|e| e.to_string())? {
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
                        s.kv_set(&key, &id).await.map_err(|e| e.to_string())?;
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
        client_secret_ref: cfg.client_secret.clone(),
        auth_method,
        token_endpoint: meta.token_endpoint.clone(),
    };
    s.kv_set(
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
    let raw = s
        .kv_get(&pending_key(&state))
        .await
        .map_err(|e| e.to_string())?
        .filter(|r| !r.is_empty())
        .ok_or("aucune autorisation en attente pour cette adresse (déjà utilisée ou expirée)")?;
    // Une adresse de retour ne sert qu'une fois, qu'elle aboutisse ou non.
    let _ = s.kv_delete(&pending_key(&state)).await;
    let pending: Pending = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let server = pending.request.server.clone();
    if s.clock.now_ms() > pending.request.expires_at_ms {
        return Err(format!("demande expirée : relancer `/mcp auth {server}`"));
    }
    let code = oauth::validate_callback(&pending.request, &cb).map_err(|e| e.to_string())?;
    check_endpoint(&pending.token_endpoint)?;
    let mut body = oauth::token_request_body(
        &code,
        &pending.request.redirect_uri,
        &pending.client_id,
        &pending.request.verifier,
        &pending.request.resource,
    );
    let basic = token_client_auth(
        s.platform.secrets.as_ref(),
        pending.auth_method,
        &pending.client_secret_ref,
        &pending.client_id,
        &mut body,
    )?;
    let v = post_form(
        &http()?,
        &pending.token_endpoint,
        &body,
        basic
            .as_ref()
            .map(|(id, secret)| (id.as_str(), secret.as_str())),
    )
    .await?;
    let tokens = Tokens::parse(&v, s.clock.now_ms()).map_err(|e| e.to_string())?;
    let grant = Grant {
        issuer: pending.request.issuer.clone(),
        token_endpoint: pending.token_endpoint.clone(),
        client_id: pending.client_id.clone(),
        client_secret_ref: pending.client_secret_ref,
        auth_method: pending.auth_method,
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
        let mut body = oauth::refresh_request_body(&refresh, &grant.client_id, &grant.resource);
        let basic = token_client_auth(
            s.platform.secrets.as_ref(),
            grant.auth_method,
            &grant.client_secret_ref,
            &grant.client_id,
            &mut body,
        )?;
        let v = post_form(
            &http()?,
            &grant.token_endpoint,
            &body,
            basic
                .as_ref()
                .map(|(id, secret)| (id.as_str(), secret.as_str())),
        )
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
            RedirectMode::PasteBack => {
                oauth::loopback_redirect(&cfg.mcp.callback_host, cfg.mcp.callback_port)
            }
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
        let origin = crate::helpers::owner_origin_of(&d.services);
        let _ = m.send_text(&origin, &text).await;
    }
}

#[cfg(test)]
mod tests;
