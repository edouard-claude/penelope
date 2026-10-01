//! Faux serveur S3 (tests, #289) : un bucket en mémoire derrière HTTP/1.1, qui vérifie la
//! signature SigV4 de chaque requête comme MinIO le ferait, puis joue PUT (simple et en
//! parties), HEAD, GET, LIST paginé et DELETE. Un mode d'erreur fait tout tomber en 403 ou
//! en 404 NoSuchBucket ; une partie désignée peut échouer pour éprouver l'abandon.

use super::s3::S3Client;
use super::sigv4::{self, Credentials};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Forbidden,
    NoBucket,
}

#[derive(Default)]
pub struct State {
    pub objects: BTreeMap<String, Vec<u8>>,
    pub uploads: BTreeMap<String, BTreeMap<u32, Vec<u8>>>,
    /// `MÉTHODE /chemin?requête`, dans l'ordre reçu.
    pub requests: Vec<String>,
    pub mode: Mode,
    /// Objets par page de LIST (0 : tout d'un coup).
    pub page_size: usize,
    /// Numéro de partie dont l'envoi échoue en 500.
    pub fail_part: Option<u32>,
    next_upload: u32,
}

pub struct FakeS3 {
    pub url: String,
    pub bucket: String,
    pub creds: Credentials,
    pub state: Arc<Mutex<State>>,
}

impl FakeS3 {
    pub async fn start(bucket: &str) -> FakeS3 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let creds = Credentials {
            access_key: "AKIAFAKE0000000EXAMPLE".into(),
            secret_key: "secret-de-test-qui-ne-sort-jamais".into(),
        };
        let state = Arc::new(Mutex::new(State::default()));
        let (st, b, c) = (state.clone(), bucket.to_string(), creds.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let (st, b, c) = (st.clone(), b.clone(), c.clone());
                tokio::spawn(handle(sock, st, b, c));
            }
        });
        FakeS3 {
            url,
            bucket: bucket.to_string(),
            creds,
            state,
        }
    }

    /// Un client sur ce serveur, en adressage par chemin.
    pub fn client(&self) -> S3Client {
        S3Client::new(
            &self.url,
            &self.bucket,
            "us-east-1",
            true,
            self.creds.clone(),
        )
        .unwrap()
    }

    pub fn set_mode(&self, mode: Mode) {
        self.state.lock().unwrap().mode = mode;
    }

    pub fn put(&self, key: &str, bytes: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(key.to_string(), bytes.to_vec());
    }

    pub fn keys(&self) -> Vec<String> {
        self.state.lock().unwrap().objects.keys().cloned().collect()
    }

    pub fn object(&self, key: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().objects.get(key).cloned()
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Lit une requête HTTP/1.1 : ligne, en-têtes, corps de `Content-Length` octets.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    let header_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < length {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((&target, ""));
    let query = raw_query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    Some(Request {
        method,
        path: percent_decode(raw_path),
        query,
        headers,
        body,
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Vérifie l'autorisation comme le serveur réel : la clé d'accès, le hachage du corps, et
/// la signature recalculée depuis la requête telle qu'elle est arrivée.
fn authorize(req: &Request, creds: &Credentials) -> Result<(), &'static str> {
    let auth = header(req, "authorization").ok_or("MissingAuthorization")?;
    let field = |name: &str| -> Option<String> {
        auth.split([' ', ','])
            .find_map(|p| p.strip_prefix(&format!("{name}=")))
            .map(String::from)
    };
    let credential = field("Credential").ok_or("MalformedAuthorization")?;
    let mut scope = credential.split('/');
    let access_key = scope.next().unwrap_or_default();
    let _date = scope.next();
    let region = scope.next().unwrap_or_default().to_string();
    let service = scope.next().unwrap_or_default().to_string();
    if access_key != creds.access_key {
        return Err("InvalidAccessKeyId");
    }
    let signed_headers = field("SignedHeaders").ok_or("MalformedAuthorization")?;
    let signature = field("Signature").ok_or("MalformedAuthorization")?;
    let payload_hash = header(req, "x-amz-content-sha256").ok_or("MissingContentSha256")?;
    if payload_hash != sigv4::payload_hash(&req.body) {
        return Err("XAmzContentSHA256Mismatch");
    }
    let amz_date = header(req, "x-amz-date").ok_or("MissingDate")?;
    let now = chrono::NaiveDateTime::parse_from_str(amz_date, "%Y%m%dT%H%M%SZ")
        .map_err(|_| "MalformedDate")?
        .and_utc();
    let headers: Vec<(String, String)> = signed_headers
        .split(';')
        .filter(|h| *h != "x-amz-date")
        .filter_map(|h| header(req, h).map(|v| (h.to_string(), v.to_string())))
        .collect();
    let expected = sigv4::sign(
        &sigv4::Request {
            method: &req.method,
            path: &req.path,
            query: &req.query,
            headers: &headers,
            payload_hash,
            now,
            region: &region,
            service: &service,
        },
        creds,
    );
    let ok = expected
        .iter()
        .any(|(k, v)| k == "authorization" && v.ends_with(&format!("Signature={signature}")));
    if ok {
        Ok(())
    } else {
        Err("SignatureDoesNotMatch")
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    head_only: bool,
}

fn error(status: u16, code: &str, message: &str) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: format!("<Error><Code>{code}</Code><Message>{message}</Message></Error>")
            .into_bytes(),
        head_only: false,
    }
}

fn ok(body: Vec<u8>, headers: Vec<(String, String)>) -> Reply {
    Reply {
        status: 200,
        headers,
        body,
        head_only: false,
    }
}

fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", &sigv4::payload_hash(bytes)[..32])
}

fn respond(req: &Request, state: &Mutex<State>, bucket: &str, creds: &Credentials) -> Reply {
    let mut st = state.lock().unwrap();
    let q: String = req
        .query
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                k.clone()
            } else {
                format!("{k}={v}")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    st.requests.push(format!(
        "{} {}{}{}",
        req.method,
        req.path,
        if q.is_empty() { "" } else { "?" },
        q
    ));
    if let Err(code) = authorize(req, creds) {
        return error(403, code, "signature refusée par le faux serveur");
    }
    match st.mode {
        Mode::Forbidden => return error(403, "AccessDenied", "Access Denied."),
        Mode::NoBucket => {
            return error(404, "NoSuchBucket", "The specified bucket does not exist");
        }
        Mode::Normal => {}
    }
    let Some(rest) = req.path.strip_prefix(&format!("/{bucket}")) else {
        return error(404, "NoSuchBucket", "The specified bucket does not exist");
    };
    let key = rest.trim_start_matches('/').to_string();
    let param = |name: &str| -> Option<String> {
        req.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let head = req.method == "HEAD";
    match (req.method.as_str(), key.is_empty()) {
        ("HEAD", true) => ok(Vec::new(), Vec::new()),
        ("GET", true) => {
            let prefix = param("prefix").unwrap_or_default();
            let after = param("continuation-token");
            let all: Vec<(&String, &Vec<u8>)> = st
                .objects
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .filter(|(k, _)| after.as_ref().is_none_or(|a| *k > a))
                .collect();
            let page = if st.page_size == 0 {
                all.len()
            } else {
                st.page_size.min(all.len())
            };
            let truncated = page < all.len();
            let mut xml = String::from("<ListBucketResult>");
            xml.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
            if truncated {
                xml.push_str(&format!(
                    "<NextContinuationToken>{}</NextContinuationToken>",
                    all[page - 1].0
                ));
            }
            for (k, v) in &all[..page] {
                xml.push_str(&format!(
                    "<Contents><Key>{k}</Key><LastModified>2026-10-01T04:00:00.000Z\
                     </LastModified><ETag>{}</ETag><Size>{}</Size></Contents>",
                    etag(v).replace('"', "&quot;"),
                    v.len()
                ));
            }
            xml.push_str("</ListBucketResult>");
            ok(xml.into_bytes(), Vec::new())
        }
        ("POST", false) if param("uploads").is_some() => {
            st.next_upload += 1;
            let id = format!("upload-{}", st.next_upload);
            st.uploads.insert(id.clone(), BTreeMap::new());
            ok(
                format!("<InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>")
                    .into_bytes(),
                Vec::new(),
            )
        }
        ("PUT", false) if param("uploadId").is_some() => {
            let id = param("uploadId").unwrap_or_default();
            let n: u32 = param("partNumber")
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
            if st.fail_part == Some(n) {
                return error(500, "InternalError", "partie refusée pour le test");
            }
            let Some(parts) = st.uploads.get_mut(&id) else {
                return error(404, "NoSuchUpload", "The specified upload does not exist");
            };
            let tag = etag(&req.body);
            parts.insert(n, req.body.clone());
            ok(Vec::new(), vec![("ETag".into(), tag)])
        }
        ("POST", false) if param("uploadId").is_some() => {
            let id = param("uploadId").unwrap_or_default();
            let Some(parts) = st.uploads.remove(&id) else {
                return error(404, "NoSuchUpload", "The specified upload does not exist");
            };
            let body: Vec<u8> = parts.into_values().flatten().collect();
            let tag = etag(&body);
            st.objects.insert(key, body);
            ok(
                format!("<CompleteMultipartUploadResult><ETag>{}</ETag></CompleteMultipartUploadResult>", tag.replace('"', "&quot;"))
                    .into_bytes(),
                Vec::new(),
            )
        }
        ("DELETE", false) if param("uploadId").is_some() => {
            st.uploads.remove(&param("uploadId").unwrap_or_default());
            Reply {
                status: 204,
                headers: Vec::new(),
                body: Vec::new(),
                head_only: false,
            }
        }
        ("PUT", false) => {
            let tag = etag(&req.body);
            st.objects.insert(key, req.body.clone());
            ok(Vec::new(), vec![("ETag".into(), tag)])
        }
        ("HEAD", false) | ("GET", false) => match st.objects.get(&key) {
            Some(v) => Reply {
                status: 200,
                headers: vec![("ETag".into(), etag(v))],
                body: v.clone(),
                head_only: head,
            },
            None if head => Reply {
                status: 404,
                headers: Vec::new(),
                body: Vec::new(),
                head_only: true,
            },
            None => error(404, "NoSuchKey", "The specified key does not exist."),
        },
        ("DELETE", false) => {
            st.objects.remove(&key);
            Reply {
                status: 204,
                headers: Vec::new(),
                body: Vec::new(),
                head_only: false,
            }
        }
        _ => error(405, "MethodNotAllowed", "méthode inconnue du faux serveur"),
    }
}

async fn handle(
    mut sock: tokio::net::TcpStream,
    state: Arc<Mutex<State>>,
    bucket: String,
    creds: Credentials,
) {
    let Some(req) = read_request(&mut sock).await else {
        return;
    };
    let reply = respond(&req, &state, &bucket, &creds);
    let reason = match reply.status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let mut out = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    if !reply.head_only {
        bytes.extend_from_slice(&reply.body);
    }
    let _ = sock.write_all(&bytes).await;
    let _ = sock.flush().await;
    let _ = sock.shutdown().await;
}
