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
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if is_blocked_ip(&ip) {
            return Err(ToolError::Denied(format!(
                "adresse privée ou réservée refusée : {ip}"
            )));
        }
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

/// Vérifie les adresses résolues : une URL publique peut pointer vers une IP privée.
pub async fn check_resolved_addresses(host: &str, port: u16) -> ToolResult<()> {
    let target = format!("{host}:{port}");
    let addrs = tokio::net::lookup_host(target)
        .await
        .map_err(|e| ToolError::Network(format!("résolution de `{host}` : {e}")))?;
    let mut any = false;
    for a in addrs {
        any = true;
        if is_blocked_ip(&a.ip()) {
            return Err(ToolError::Denied(format!(
                "`{host}` résout vers une adresse privée ({}) : accès refusé",
                a.ip()
            )));
        }
    }
    if !any {
        return Err(ToolError::Network(format!(
            "`{host}` ne résout vers aucune adresse"
        )));
    }
    Ok(())
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
    let mut url = check_url(raw_url, allowlist)?;
    if block_private {
        let port = url.port_or_known_default().unwrap_or(443);
        if let Some(h) = url.host_str() {
            check_resolved_addresses(h, port).await?;
        }
    }

    let m = match method.to_uppercase().as_str() {
        "POST" => reqwest::Method::POST,
        "HEAD" => reqwest::Method::HEAD,
        _ => reqwest::Method::GET,
    };

    for hop in 0..5 {
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
            // Chaque saut est revérifié : une redirection vers 169.254.169.254 est le
            // scénario SSRF classique.
            url = check_url(next.as_str(), allowlist)?;
            if block_private {
                let port = url.port_or_known_default().unwrap_or(443);
                if let Some(h) = url.host_str() {
                    check_resolved_addresses(h, port).await?;
                }
            }
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
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::Network(e.to_string()))?;
        let truncated = bytes.len() > max_bytes;
        let slice = &bytes[..bytes.len().min(max_bytes)];
        let text = String::from_utf8_lossy(slice).to_string();

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
