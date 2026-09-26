//! Faux serveur HTTP local des scénarios (`[[http]]`) : ce qu'une méthode va chercher sur
//! le réseau (métadonnées OAuth, releases) lui est servi depuis `127.0.0.1`, sans une
//! requête vers l'extérieur.
//!
//! ```text
//!  scenario.toml [[http]] ─► Server::start (127.0.0.1:0) ─► base `http://127.0.0.1:<port>`
//!        │                                                        │
//!  `{{http}}` remplacé par la base dans la configuration,   requêtes reçues relevées
//!  les paramètres RPC, les corps servis                      (`http_request`) dans le monde
//! ```
//!
//! Chaque route rend un contenu fixe ; `{{http}}` y est la base et `{{q:nom}}` un
//! paramètre de la requête reçue (un serveur d'autorisation renvoie le `state` qu'on lui
//! donne). Le port change à chaque rejeu : la normalisation rend la base en `{{http}}`.
//! Les valeurs tirées au sort par le client (`state`, `code_verifier`,
//! `code_challenge`) sont relevées masquées.

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// Paramètres dont la valeur est tirée au sort à chaque rejeu.
const RANDOM_PARAMS: &[&str] = &["state", "code_verifier", "code_challenge"];
/// Au-delà, une requête reçue est refusée : le faux serveur ne sert que de petits JSON.
const MAX_REQUEST: usize = 1024 * 1024;

/// Une route du faux serveur.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Chemin exact, sans la requête (`/releases`, `/.well-known/…`).
    pub path: String,
    /// `GET`, `POST`… ; toute méthode sans lui.
    #[serde(default)]
    pub method: Option<String>,
    /// Statut rendu ; 302 par défaut avec `location`, 200 sinon.
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub body: String,
    #[serde(default = "json_type")]
    pub content_type: String,
    /// En-tête `Location` d'une redirection (retour d'un serveur d'autorisation).
    #[serde(default)]
    pub location: Option<String>,
}

fn json_type() -> String {
    "application/json".into()
}

/// Le serveur d'un rejeu : arrêté quand il est lâché.
pub struct Server {
    base: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Server {
    pub async fn start(routes: Vec<Route>) -> anyhow::Result<Server> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("faux serveur HTTP")?;
        let base = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (routes, seen, b) = (Arc::new(routes), requests.clone(), base.clone());
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (routes, seen, b) = (routes.clone(), seen.clone(), b.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, &routes, &seen, &b).await;
                });
            }
        });
        Ok(Server {
            base,
            requests,
            task,
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// `{{http}}` remplacé par la base, partout dans une valeur.
    pub fn fill(&self, v: Value) -> Value {
        match v {
            Value::String(s) => Value::String(s.replace("{{http}}", &self.base)),
            Value::Array(a) => Value::Array(a.into_iter().map(|x| self.fill(x)).collect()),
            Value::Object(o) => {
                Value::Object(o.into_iter().map(|(k, x)| (k, self.fill(x))).collect())
            }
            other => other,
        }
    }

    /// Les requêtes reçues, dans l'ordre : une ligne `http_request` chacune.
    pub fn requests(&self) -> Vec<Value> {
        lock(&self.requests).clone()
    }

    /// La base telle qu'elle apparaît encodée dans une URL (`resource=http%3A%2F%2F…`).
    pub fn encoded_base(&self) -> String {
        self.base.replace(':', "%3A").replace('/', "%2F")
    }
}

/// Une requête reçue, lue jusqu'au bout de son corps.
struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    content_type: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "requête coupée");
        buf.extend_from_slice(&chunk[..n]);
        anyhow::ensure!(buf.len() <= MAX_REQUEST, "requête trop grosse");
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_string();
    let target = first.next().unwrap_or_default().to_string();
    let header = |name: &str| {
        head.lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    let length: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    anyhow::ensure!(length <= MAX_REQUEST, "corps trop gros");
    let mut body = buf[head_end..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "corps coupé");
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Ok(Request {
        method,
        path: path.to_string(),
        query: parse_form(query),
        content_type: header("content-type").unwrap_or_default(),
        body,
    })
}

async fn serve(
    mut stream: TcpStream,
    routes: &[Route],
    seen: &Mutex<Vec<Value>>,
    base: &str,
) -> anyhow::Result<()> {
    let req = read_request(&mut stream).await?;
    lock(seen).push(record(&req));
    let route = routes.iter().find(|r| {
        r.path == req.path
            && r.method
                .as_deref()
                .is_none_or(|m| m.eq_ignore_ascii_case(&req.method))
    });
    let fill = |s: &str| {
        let mut out = s.replace("{{http}}", base);
        for (k, v) in &req.query {
            out = out.replace(&format!("{{{{q:{k}}}}}"), v);
        }
        out
    };
    let (status, body, content_type, location) = match route {
        Some(r) => (
            r.status
                .unwrap_or(if r.location.is_some() { 302 } else { 200 }),
            fill(&r.body),
            r.content_type.clone(),
            r.location.as_deref().map(fill),
        ),
        None => (404, "{}".to_string(), json_type(), None),
    };
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reason(status),
        body.len()
    );
    if let Some(l) = location {
        head.push_str(&format!("Location: {l}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Status",
    }
}

/// La ligne relevée d'une requête : méthode, chemin, requête et corps lus (JSON ou
/// formulaire), les valeurs tirées au sort masquées.
fn record(req: &Request) -> Value {
    let masked = |mut m: BTreeMap<String, String>| {
        for (k, v) in m.iter_mut() {
            if RANDOM_PARAMS.contains(&k.as_str()) {
                *v = "{{masked}}".into();
            }
        }
        json!(m)
    };
    let text = String::from_utf8_lossy(&req.body).to_string();
    let body = if req.body.is_empty() {
        Value::Null
    } else if req.content_type.contains("json") {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    } else if req.content_type.contains("x-www-form-urlencoded") {
        masked(parse_form(&text))
    } else {
        Value::String(text)
    };
    let mut line = json!({"type": "http_request", "method": req.method, "path": req.path});
    if !req.query.is_empty() {
        line["query"] = masked(req.query.clone());
    }
    if !body.is_null() {
        line["body"] = body;
    }
    line
}

/// `a=1&b=x%20y` en paires décodées (`+` est une espace).
fn parse_form(raw: &str) -> BTreeMap<String, String> {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => match s.get(i + 1..i + 3).map(|h| u8::from_str_radix(h, 16)) {
                Some(Ok(b)) => {
                    out.push(b);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Le navigateur du propriétaire, réduit à une requête : `GET` sans suivre de
/// redirection. Seul le faux serveur est joignable : toute autre adresse est refusée.
pub async fn open(server: &Server, url: &str) -> anyhow::Result<Value> {
    let rest = url
        .strip_prefix(server.base())
        .with_context(|| format!("`{url}` : seul le faux serveur ({{{{http}}}}) est joignable"))?;
    let target = if rest.starts_with('/') {
        rest.to_string()
    } else {
        format!("/{rest}")
    };
    let host = server.base().trim_start_matches("http://");
    let mut stream = TcpStream::connect(host).await?;
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .context("réponse HTTP illisible")?;
    let location = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.trim().to_string());
    let mut out = json!({"status": status});
    if let Some(l) = location {
        out["location"] = json!(l);
    }
    if !body.is_empty() {
        out["body"] = serde_json::from_str(body).unwrap_or(json!(body));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(path: &str, body: &str) -> Route {
        Route {
            path: path.into(),
            method: None,
            status: None,
            body: body.into(),
            content_type: json_type(),
            location: None,
        }
    }

    #[test]
    fn a_form_is_decoded() {
        let f = parse_form("a=1&b=x%20y+z&redirect_uri=http%3A%2F%2F127.0.0.1%3A8765%2Fcb&c");
        assert_eq!(f["b"], "x y z");
        assert_eq!(f["redirect_uri"], "http://127.0.0.1:8765/cb");
        assert_eq!(f["c"], "");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }

    /// Une route sert son contenu, `{{http}}` et `{{q:…}}` remplis ; la requête est
    /// relevée, ses valeurs aléatoires masquées ; une adresse hors du faux serveur est
    /// refusée.
    #[tokio::test]
    async fn the_fake_server_serves_redirects_and_records() {
        let mut authorize = route("/authorize", "");
        authorize.location = Some("{{q:redirect_uri}}?code=abc&state={{q:state}}".into());
        let server = Server::start(vec![route("/meta", r#"{"issuer": "{{http}}"}"#), authorize])
            .await
            .unwrap();
        let base = server.base().to_string();

        let meta = open(&server, &format!("{base}/meta")).await.unwrap();
        assert_eq!(meta, json!({"status": 200, "body": {"issuer": base}}));

        let back = open(
            &server,
            &format!("{base}/authorize?state=s3cr3t&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcb"),
        )
        .await
        .unwrap();
        assert_eq!(back["status"], 302);
        assert_eq!(
            back["location"],
            "http://127.0.0.1:1/cb?code=abc&state=s3cr3t"
        );

        let missing = open(&server, &format!("{base}/absent")).await.unwrap();
        assert_eq!(missing["status"], 404);
        assert!(open(&server, "https://example.org/").await.is_err());

        let seen = server.requests();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1]["query"]["state"], "{{masked}}");
        assert_eq!(seen[1]["query"]["redirect_uri"], "http://127.0.0.1:1/cb");
        assert_eq!(
            server.fill(json!({"u": "{{http}}/x"})),
            json!({"u": format!("{base}/x")})
        );
    }
}
