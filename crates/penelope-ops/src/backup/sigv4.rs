//! Signature AWS SigV4 (#289), écrite ici : trois verbes sur un bucket ne justifient pas
//! un SDK. Le service est `s3` ; les vecteurs de test officiels d'AWS la relisent.
//!
//! ```text
//!  requête ─► requête canonique ─► chaîne à signer ─► HMAC en chaîne (date, région,
//!  service, aws4_request) ─► en-tête Authorization
//! ```

use chrono::{DateTime, Utc};
use penelope_kernel::canonical::hex;
use sha2::{Digest, Sha256};

/// Identifiants d'accès.
#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Credentials({}, ****)", self.access_key)
    }
}

/// Ce que la signature regarde d'une requête.
pub struct Request<'a> {
    pub method: &'a str,
    /// Chemin brut (`/bucket/clé`), encodé ici segment par segment.
    pub path: &'a str,
    /// Paramètres de requête, bruts ; triés et encodés ici.
    pub query: &'a [(String, String)],
    /// En-têtes à signer, `host` compris ; `x-amz-date` est ajouté.
    pub headers: &'a [(String, String)],
    /// SHA-256 hexadécimal du corps (`sha256("")` pour un corps vide).
    pub payload_hash: &'a str,
    pub now: DateTime<Utc>,
    pub region: &'a str,
    pub service: &'a str,
}

/// En-têtes à poser : `x-amz-date` et `authorization`.
pub fn sign(req: &Request<'_>, creds: &Credentials) -> Vec<(String, String)> {
    let amz_date = req.now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = req.now.format("%Y%m%d").to_string();

    let mut headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), collapse(v)))
        .collect();
    headers.push(("x-amz-date".into(), amz_date.clone()));
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        canonical_uri(req.path),
        canonical_query(req.query),
        canonical_headers,
        signed_headers,
        req.payload_hash
    );
    let scope = format!("{date}/{}/{}/aws4_request", req.region, req.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );
    let k_date = hmac(
        format!("AWS4{}", creds.secret_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, req.region.as_bytes());
    let k_service = hmac(&k_region, req.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    vec![
        ("x-amz-date".into(), amz_date),
        (
            "authorization".into(),
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, \
                 Signature={signature}",
                creds.access_key
            ),
        ),
    ]
}

/// HMAC-SHA256 (RFC 2104) : blocs de 64 octets, clé longue hachée d'abord.
fn hmac(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let mut h = Sha256::new();
    h.update(&inner);
    h.update(message);
    let inner_hash = h.finalize();
    let mut h = Sha256::new();
    h.update(&outer);
    h.update(inner_hash);
    h.finalize().into()
}

/// Espaces en tête, en queue et en série réduits, comme la norme le demande.
fn collapse(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Encodage RFC 3986 strict : seuls `A-Z a-z 0-9 - _ . ~` passent tels quels.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// S3 : le chemin est encodé une fois, les `/` gardés ; vide devient `/`.
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    uri_encode(path, true)
}

/// Paramètres triés par nom puis valeur, encodés, `nom=` pour une valeur vide.
fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// SHA-256 hexadécimal d'un corps, tel que `x-amz-content-sha256` l'attend.
pub fn payload_hash(body: &[u8]) -> String {
    hex(&Sha256::digest(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn signature_of(headers: &[(String, String)]) -> String {
        headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .and_then(|(_, v)| v.rsplit("Signature=").next().map(String::from))
            .unwrap()
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Les identifiants de la suite de tests officielle (`aws-sig-v4-test-suite`).
    fn suite_creds() -> Credentials {
        Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        }
    }

    /// Les identifiants des exemples S3 de la documentation d'AWS (bucket `examplebucket`).
    fn s3_doc_creds() -> Credentials {
        Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        }
    }

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    /// Vecteur `get-vanilla` de la suite officielle.
    #[test]
    fn aws_test_suite_get_vanilla() {
        let headers = h(&[("Host", "example.amazonaws.com")]);
        let out = sign(
            &Request {
                method: "GET",
                path: "/",
                query: &[],
                headers: &headers,
                payload_hash: EMPTY,
                now: Utc.with_ymd_and_hms(2015, 8, 30, 12, 36, 0).unwrap(),
                region: "us-east-1",
                service: "service",
            },
            &suite_creds(),
        );
        assert_eq!(
            signature_of(&out),
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert_eq!(
            out[0],
            ("x-amz-date".to_string(), "20150830T123600Z".to_string())
        );
        assert!(
            out[1].1.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature="
            ),
            "{}",
            out[1].1
        );
    }

    /// Vecteur `post-x-www-form-urlencoded` de la suite officielle : un corps signé.
    #[test]
    fn aws_test_suite_post_with_a_signed_body() {
        let body = b"Param1=value1";
        let hash = payload_hash(body);
        assert_eq!(
            hash,
            "9095672bbd1f56dfc5b65f3e153adc8731a4a654192329106275f4c7b24d0b6e"
        );
        let headers = h(&[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Host", "example.amazonaws.com"),
            ("Content-Length", "13"),
            ("x-amz-content-sha256", &hash),
        ]);
        let out = sign(
            &Request {
                method: "POST",
                path: "/",
                query: &[],
                headers: &headers,
                payload_hash: &hash,
                now: Utc.with_ymd_and_hms(2015, 8, 30, 12, 36, 0).unwrap(),
                region: "us-east-1",
                service: "service",
            },
            &suite_creds(),
        );
        assert_eq!(
            signature_of(&out),
            "d3875051da38690788ef43de4db0d8f280229d82040bfac253562e56c3f20e0b"
        );
    }

    /// Exemple « GET Object » de la documentation S3 (en-tête `Range`).
    #[test]
    fn s3_documentation_get_object() {
        let headers = h(&[
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("Range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY),
        ]);
        let out = sign(
            &Request {
                method: "GET",
                path: "/test.txt",
                query: &[],
                headers: &headers,
                payload_hash: EMPTY,
                now: Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
                region: "us-east-1",
                service: "s3",
            },
            &s3_doc_creds(),
        );
        assert_eq!(
            signature_of(&out),
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    /// Exemple « PUT Object » de la documentation S3 : un `$` dans la clé, un corps.
    #[test]
    fn s3_documentation_put_object() {
        let body = b"Welcome to Amazon S3.";
        let hash = payload_hash(body);
        assert_eq!(
            hash,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let headers = h(&[
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ("x-amz-content-sha256", &hash),
        ]);
        let out = sign(
            &Request {
                method: "PUT",
                path: "/test$file.text",
                query: &[],
                headers: &headers,
                payload_hash: &hash,
                now: Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
                region: "us-east-1",
                service: "s3",
            },
            &s3_doc_creds(),
        );
        assert_eq!(
            signature_of(&out),
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    /// Exemples « GET Bucket » de la documentation S3 : paramètres de requête, avec et
    /// sans valeur.
    #[test]
    fn s3_documentation_list_objects_and_lifecycle() {
        let headers = h(&[
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-content-sha256", EMPTY),
        ]);
        let now = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let listing = [
            ("max-keys".to_string(), "2".to_string()),
            ("prefix".to_string(), "J".to_string()),
        ];
        let out = sign(
            &Request {
                method: "GET",
                path: "/",
                query: &listing,
                headers: &headers,
                payload_hash: EMPTY,
                now,
                region: "us-east-1",
                service: "s3",
            },
            &s3_doc_creds(),
        );
        assert_eq!(
            signature_of(&out),
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
        let lifecycle = [("lifecycle".to_string(), String::new())];
        let out = sign(
            &Request {
                method: "GET",
                path: "/",
                query: &lifecycle,
                headers: &headers,
                payload_hash: EMPTY,
                now,
                region: "us-east-1",
                service: "s3",
            },
            &s3_doc_creds(),
        );
        assert_eq!(
            signature_of(&out),
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
    }

    #[test]
    fn uri_encoding_is_strict_and_keeps_slashes_only_in_paths() {
        assert_eq!(uri_encode("a b/é$~", true), "a%20b/%C3%A9%24~");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(canonical_uri(""), "/");
    }
}
