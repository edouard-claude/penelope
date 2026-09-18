//! `http_fetch` (§11, §13) : liste blanche optionnelle, **SSRF bloquée**.
//!
//! Les adresses privées, la boucle locale et les points de métadonnées cloud sont refusés,
//! y compris après redirection : c'est la porte d'entrée classique vers les identifiants
//! d'une instance.

use crate::error::{ToolError, ToolResult};
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Points de métadonnées connus des fournisseurs cloud.
pub const METADATA_HOSTS: &[&str] = &[
    "169.254.169.254",
    "metadata.google.internal",
    "metadata.goog",
    "100.100.100.200",
    "fd00:ec2::254",
];

/// Vrai si l'adresse est privée, locale ou réservée.
pub fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || is_shared_address_space(v4)
                || v4.octets()[0] == 0
                || (v4.octets()[0] & 0xf0) == 240 // 240.0.0.0/4 réservé
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || is_unique_local(v6)
                || is_link_local_v6(v6)
                || v6
                    .to_ipv4_mapped()
                    .map(|m| is_blocked_ip(&IpAddr::V4(m)))
                    .unwrap_or(false)
        }
    }
}

/// 100.64.0.0/10, l'espace partagé des opérateurs (RFC 6598).
fn is_shared_address_space(v4: &Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

fn is_unique_local(v6: &Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

fn is_link_local_v6(v6: &Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

/// Vérifie une URL avant toute requête.
pub fn check_url(raw: &str, allowlist: &[String]) -> ToolResult<url::Url> {
    let u = url::Url::parse(raw).map_err(|e| ToolError::Invalid(format!("URL invalide : {e}")))?;

    if !matches!(u.scheme(), "http" | "https") {
        return Err(ToolError::Denied(format!(
            "schéma `{}` refusé : seuls http et https sont autorisés",
            u.scheme()
        )));
    }
    let host = u
        .host_str()
        .ok_or_else(|| ToolError::Invalid("URL sans hôte".into()))?
        .to_lowercase();

    if METADATA_HOSTS.iter().any(|m| host == *m) {
        return Err(ToolError::Denied(format!(
            "`{host}` est un point de métadonnées cloud : accès refusé"
        )));
    }
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".internal") {
        return Err(ToolError::Denied(format!("hôte local refusé : {host}")));
    }
    // `host_str()` rend une IPv6 entre crochets (`[::1]`) : les retirer avant de
    // parser, sinon la boucle locale v6 passe à travers le filtre.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>()
        && is_blocked_ip(&ip)
    {
        return Err(ToolError::Denied(format!(
            "adresse privée ou réservée refusée : {ip}"
        )));
    }
    if !allowlist.is_empty() && !host_allowed(&host, allowlist) {
        return Err(ToolError::Denied(format!(
            "`{host}` n'est pas dans la liste blanche de domaines"
        )));
    }
    Ok(u)
}

/// Correspondance de liste blanche : `example.com` couvre `a.example.com`.
pub fn host_allowed(host: &str, allowlist: &[String]) -> bool {
    allowlist.iter().any(|a| {
        let a = a.trim().to_lowercase();
        host == a || host.ends_with(&format!(".{a}"))
    })
}

/// Résolution de noms et politique d'adresses du garde SSRF, injectables pour les tests
/// (issue #93).
#[async_trait::async_trait]
pub trait AddressGuard: Send + Sync {
    async fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<std::net::SocketAddr>>;
    fn blocked(&self, ip: &IpAddr) -> bool {
        is_blocked_ip(ip)
    }
}

/// Résolveur du système, adresses privées et réservées refusées.
pub struct SystemGuard;

#[async_trait::async_trait]
impl AddressGuard for SystemGuard {
    async fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<std::net::SocketAddr>> {
        Ok(tokio::net::lookup_host(format!("{host}:{port}"))
            .await?
            .collect())
    }
}

/// Résout `host` et vérifie **toutes** ses adresses : une URL publique peut pointer vers
/// une IP privée. Rend les adresses vérifiées, sur lesquelles la connexion est épinglée.
pub async fn resolve_checked(
    guard: &dyn AddressGuard,
    host: &str,
    port: u16,
) -> ToolResult<Vec<std::net::SocketAddr>> {
    let addrs = guard
        .lookup(host, port)
        .await
        .map_err(|e| ToolError::Network(format!("résolution de `{host}` : {e}")))?;
    if addrs.is_empty() {
        return Err(ToolError::Network(format!(
            "`{host}` ne résout vers aucune adresse"
        )));
    }
    if let Some(a) = addrs.iter().find(|a| guard.blocked(&a.ip())) {
        return Err(ToolError::Denied(format!(
            "`{host}` résout vers une adresse privée ({}) : accès refusé",
            a.ip()
        )));
    }
    Ok(addrs)
}

/// Vérifie les adresses résolues avec le résolveur du système.
pub async fn check_resolved_addresses(host: &str, port: u16) -> ToolResult<()> {
    resolve_checked(&SystemGuard, host, port).await.map(|_| ())
}

/// Délai d'une requête épinglée, le même que celui du client partagé du daemon.
const PINNED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Client d'un saut, dont la connexion va **sur les adresses vérifiées** : reqwest ne
/// résout plus le nom une seconde fois, un DNS à TTL nul ne peut plus basculer vers
/// 127.0.0.1 ou 169.254.169.254 entre le contrôle et la connexion (issue #93). SNI, `Host`
/// et vérification du certificat restent sur le nom.
fn pinned_client(host: &str, addrs: &[std::net::SocketAddr]) -> ToolResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(PINNED_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, addrs)
        .build()
        .map_err(|e| ToolError::Network(e.to_string()))
}

/// Taille annoncée au-delà de laquelle la réponse est refusée sans être lue : ce multiple
/// de `max_bytes` sépare « une page un peu longue, tronquée » d'« un fichier qu'on ne veut
/// pas » (issue #64).
const OVERSIZE_FACTOR: u64 = 20;

/// Lit le corps jusqu'à `max_bytes`, puis coupe la connexion : le second retour dit si
/// quelque chose a été laissé (issue #64).
async fn read_capped(resp: reqwest::Response, max_bytes: usize) -> ToolResult<(Vec<u8>, bool)> {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut out: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ToolError::Network(e.to_string()))?;
        if out.len() >= max_bytes {
            truncated = true;
            break;
        }
        let room = max_bytes - out.len();
        if chunk.len() > room {
            out.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        out.extend_from_slice(&chunk);
    }
    Ok((out, truncated))
}

/// Exécute la requête. Les redirections sont **suivies manuellement** pour re-vérifier
/// chaque saut.
// Les garde-fous (liste blanche, IP privées, taille) sont des paramètres explicites :
// aucun n'a de valeur par défaut sûre.
#[allow(clippy::too_many_arguments)]
pub async fn fetch(
    client: &reqwest::Client,
    raw_url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    allowlist: &[String],
    max_bytes: usize,
    block_private: bool,
) -> ToolResult<Value> {
    let guard: Option<&dyn AddressGuard> = block_private.then_some(&SystemGuard as _);
    fetch_guarded(
        client, raw_url, method, headers, body, allowlist, max_bytes, guard,
    )
    .await
}

/// `fetch`, garde d'adresses explicite. Avec un garde, chaque saut résout le nom une
/// seule fois, vérifie les adresses, et se connecte sur elles.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_guarded(
    client: &reqwest::Client,
    raw_url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    allowlist: &[String],
    max_bytes: usize,
    guard: Option<&dyn AddressGuard>,
) -> ToolResult<Value> {
    let mut url = check_url(raw_url, allowlist)?;

    let m = match method.to_uppercase().as_str() {
        "POST" => reqwest::Method::POST,
        "HEAD" => reqwest::Method::HEAD,
        _ => reqwest::Method::GET,
    };

    for hop in 0..5 {
        // Chaque saut est revérifié : une redirection vers 169.254.169.254 est le scénario
        // SSRF classique ; et la connexion part sur les adresses vérifiées (#93).
        let pinned = match (guard, url.host_str()) {
            (Some(g), Some(h)) => {
                let port = url.port_or_known_default().unwrap_or(443);
                let addrs = resolve_checked(g, h, port).await?;
                Some(pinned_client(h, &addrs)?)
            }
            _ => None,
        };
        let client = pinned.as_ref().unwrap_or(client);
        let mut req = client.request(m.clone(), url.clone());
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ToolError::Network(e.to_string()))?;
        let status = resp.status().as_u16();

        if (300..400).contains(&status) {
            let Some(loc) = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(String::from)
            else {
                return Err(ToolError::Network(format!(
                    "redirection {status} sans en-tête Location"
                )));
            };
            let next = url
                .join(&loc)
                .map_err(|e| ToolError::Network(format!("redirection illisible : {e}")))?;
            url = check_url(next.as_str(), allowlist)?;
            if hop == 4 {
                return Err(ToolError::Network("trop de redirections".into()));
            }
            continue;
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Taille annoncée absurde : refusée avant d'ouvrir le robinet (issue #64).
        if let Some(len) = resp.content_length()
            && len > (max_bytes as u64).saturating_mul(OVERSIZE_FACTOR)
        {
            return Err(ToolError::Denied(format!(
                "réponse de {len} octets annoncée, au-delà de {} fois la limite de \
                 {max_bytes} octets",
                OVERSIZE_FACTOR
            )));
        }
        // Lecture par morceaux : un corps de 200 Mo ne passe plus en mémoire pour être
        // coupé ensuite à `max_bytes` (issue #64).
        let (bytes, truncated) = read_capped(resp, max_bytes).await?;
        let text = String::from_utf8_lossy(&bytes).to_string();

        return Ok(json!({
            "url": url.to_string(),
            "status": status,
            "contentType": content_type,
            "bytes": bytes.len(),
            "truncated": truncated,
            "body": text,
        }));
    }
    Err(ToolError::Network("trop de redirections".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serveur minimal : rend `response` à chaque connexion. Renvoie son adresse.
    async fn one_shot(response: Vec<u8>) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = response.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(&body).await;
                    let _ = sock.flush().await;
                });
            }
        });
        addr
    }

    /// Client de test : aucune redirection suivie par lui-même (comme en production), et
    /// `essai.test` épinglé sur le serveur local, pour partir d'un nom public.
    fn client(addr: std::net::SocketAddr) -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("essai.test", addr)
            .build()
            .unwrap()
    }

    fn url_of(addr: std::net::SocketAddr, path: &str) -> String {
        format!("http://essai.test:{}{path}", addr.port())
    }

    /// Résolveur scripté : une liste de réponses par nom, la dernière se répète ; les
    /// adresses de `blocked` jouent le rôle des adresses privées.
    struct Scripted {
        answers: std::sync::Mutex<std::collections::HashMap<String, Vec<Vec<IpAddr>>>>,
        blocked: Vec<IpAddr>,
        lookups: std::sync::atomic::AtomicUsize,
    }

    impl Scripted {
        fn new(answers: &[(&str, Vec<Vec<IpAddr>>)], blocked: &[IpAddr]) -> Self {
            Scripted {
                answers: std::sync::Mutex::new(
                    answers
                        .iter()
                        .map(|(h, a)| (h.to_string(), a.clone()))
                        .collect(),
                ),
                blocked: blocked.to_vec(),
                lookups: Default::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl AddressGuard for Scripted {
        async fn lookup(
            &self,
            host: &str,
            port: u16,
        ) -> std::io::Result<Vec<std::net::SocketAddr>> {
            self.lookups
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut g = self.answers.lock().unwrap();
            let list = g.get_mut(host).expect("nom inconnu du résolveur scripté");
            let ips = if list.len() > 1 {
                list.remove(0)
            } else {
                list[0].clone()
            };
            Ok(ips
                .into_iter()
                .map(|ip| std::net::SocketAddr::new(ip, port))
                .collect())
        }
        fn blocked(&self, ip: &IpAddr) -> bool {
            self.blocked.contains(ip)
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn plain() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    /// #93 : le nom répond « public » au contrôle puis « privé » : la connexion part sur
    /// l'adresse contrôlée, le nom n'est pas résolu une seconde fois.
    #[tokio::test]
    async fn the_connection_goes_to_the_checked_address() {
        let server = one_shot(
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nPUBLIC".to_vec(),
        )
        .await;
        let guard = Scripted::new(
            &[(
                "rebind.test",
                vec![vec![ip("127.0.0.1")], vec![ip("127.0.0.2")]],
            )],
            &[ip("127.0.0.2")],
        );
        let v = fetch_guarded(
            &plain(),
            &format!("http://rebind.test:{}/", server.port()),
            "GET",
            &[],
            None,
            &[],
            4096,
            Some(&guard),
        )
        .await
        .unwrap();
        assert_eq!(v["status"], 200);
        assert!(v.to_string().contains("PUBLIC"), "{v}");
        assert_eq!(
            guard.lookups.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "une seule résolution par saut"
        );
    }

    /// #93 : une redirection vers un nom qui résout en privé est refusée au saut suivant.
    #[tokio::test]
    async fn a_redirect_to_a_rebinding_name_is_refused() {
        let redirect = one_shot(
            b"HTTP/1.1 302 Found\r\nLocation: http://second.test:9/x\r\n\
              Content-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        )
        .await;
        let guard = Scripted::new(
            &[
                ("first.test", vec![vec![ip("127.0.0.1")]]),
                ("second.test", vec![vec![ip("127.0.0.2")]]),
            ],
            &[ip("127.0.0.2")],
        );
        let e = fetch_guarded(
            &plain(),
            &format!("http://first.test:{}/", redirect.port()),
            "GET",
            &[],
            None,
            &[],
            4096,
            Some(&guard),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("adresse privée"), "{e}");
    }

    /// #93 : un nom à plusieurs adresses vérifiées reste joignable par la seconde quand la
    /// première refuse la connexion.
    #[tokio::test]
    async fn a_name_with_several_checked_addresses_falls_back() {
        let server = one_shot(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK".to_vec(),
        )
        .await;
        let guard = Scripted::new(
            &[("multi.test", vec![vec![ip("::1"), ip("127.0.0.1")]])],
            &[],
        );
        let v = fetch_guarded(
            &plain(),
            &format!("http://multi.test:{}/", server.port()),
            "GET",
            &[],
            None,
            &[],
            4096,
            Some(&guard),
        )
        .await
        .unwrap();
        assert_eq!(v["status"], 200);
    }

    /// #64 : une redirection vers la boucle locale est refusée, même quand le premier
    /// saut est autorisé : c'est le scénario SSRF que le module dit couvrir.
    #[tokio::test]
    async fn a_redirect_to_a_private_address_is_refused() {
        let secret = one_shot(
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nSECRET".to_vec(),
        )
        .await;
        let redirect = one_shot(
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/final\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n",
                secret.port()
            )
            .into_bytes(),
        )
        .await;

        let e = fetch(
            &client(redirect),
            &url_of(redirect, "/start"),
            "GET",
            &[],
            None,
            &[],
            4096,
            false,
        )
        .await
        .unwrap_err();
        let m = e.to_string();
        assert!(m.contains("privée") || m.contains("réservée"), "{m}");
    }

    /// #64 : une redirection hors liste blanche est refusée.
    #[tokio::test]
    async fn a_redirect_outside_the_allowlist_is_refused() {
        let redirect = one_shot(
            b"HTTP/1.1 302 Found\r\nLocation: https://exfiltration.example/x\r\n\
              Content-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        )
        .await;
        let e = fetch(
            &client(redirect),
            &url_of(redirect, "/start"),
            "GET",
            &[],
            None,
            &["essai.test".to_string()],
            4096,
            false,
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("liste"), "{e}");
    }

    /// #64 : un corps sans taille annoncée est coupé à `max_bytes` sans être lu en entier.
    #[tokio::test]
    async fn a_huge_body_is_read_up_to_the_cap_only() {
        let mut body =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n".to_vec();
        body.extend(std::iter::repeat_n(b'a', 4 * 1024 * 1024));
        let addr = one_shot(body).await;
        let v = fetch(
            &client(addr),
            &url_of(addr, "/gros"),
            "GET",
            &[],
            None,
            &[],
            1024,
            false,
        )
        .await
        .unwrap();
        assert_eq!(v["truncated"], true, "{v}");
        assert_eq!(v["body"].as_str().unwrap_or_default().len(), 1024);
    }

    /// #64 : une taille annoncée absurde est refusée avant lecture.
    #[tokio::test]
    async fn an_oversized_content_length_is_refused_before_reading() {
        let addr = one_shot(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100000000\r\nConnection: close\r\n\r\n".to_vec(),
        )
        .await;
        let e = fetch(
            &client(addr),
            &url_of(addr, "/enorme"),
            "GET",
            &[],
            None,
            &[],
            1024,
            false,
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("100000000"), "{e}");
    }

    #[test]
    fn private_addresses_are_blocked() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(
                is_blocked_ip(&ip.parse().unwrap()),
                "adresse acceptée à tort : {ip}"
            );
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_blocked_ip(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn metadata_endpoints_are_refused() {
        for u in [
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/computeMetadata/v1/",
        ] {
            let e = check_url(u, &[]).unwrap_err();
            assert!(e.to_string().contains("refusé"), "{u} → {e}");
        }
    }

    #[test]
    fn localhost_and_internal_are_refused() {
        assert!(check_url("http://localhost:8080/x", &[]).is_err());
        assert!(check_url("http://api.internal/x", &[]).is_err());
        assert!(check_url("http://127.0.0.1/x", &[]).is_err());
    }

    #[test]
    fn non_http_schemes_are_refused() {
        for u in ["file:///etc/passwd", "ftp://x/y", "gopher://x"] {
            assert!(check_url(u, &[]).is_err(), "{u}");
        }
    }

    #[test]
    fn allowlist_matches_subdomains() {
        let allow = vec!["example.com".to_string()];
        assert!(check_url("https://example.com/a", &allow).is_ok());
        assert!(check_url("https://api.example.com/a", &allow).is_ok());
        assert!(check_url("https://autre.org/a", &allow).is_err());
        assert!(
            check_url("https://notexample.com/a", &allow).is_err(),
            "la correspondance ne doit pas être un simple suffixe de chaîne"
        );
    }

    #[test]
    fn empty_allowlist_allows_public_hosts() {
        assert!(check_url("https://exemple.org/a", &[]).is_ok());
    }

    #[test]
    fn host_allowed_is_exact_or_subdomain() {
        let allow = vec!["exemple.fr".to_string()];
        assert!(host_allowed("exemple.fr", &allow));
        assert!(host_allowed("www.exemple.fr", &allow));
        assert!(!host_allowed("exemple.fr.attaquant.com", &allow));
    }

    #[tokio::test]
    async fn resolution_check_rejects_private_targets() {
        // `localhost` résout vers 127.0.0.1 : le contrôle post-résolution doit refuser.
        let e = check_resolved_addresses("localhost", 80).await;
        assert!(e.is_err(), "une résolution privée doit être refusée");
    }
}
