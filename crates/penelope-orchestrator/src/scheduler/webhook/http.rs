//! Le strict nécessaire de HTTP/1.1 pour recevoir un webhook (#294) : une requête lue en
//! entier dans des bornes, une réponse écrite, la connexion fermée. Le workspace a un
//! client HTTP (`reqwest`) et aucun serveur ; un POST signé avec un corps borné ne
//! justifie pas d'en ajouter un.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Au-delà, la ligne de requête et les en-têtes sont refusés.
const MAX_HEAD: usize = 8 * 1024;
/// Attente maximale pour lire la requête entière.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Une requête reçue ; les noms d'en-tête sont en minuscules.
pub(super) struct Request {
    pub method: String,
    /// Cible telle que reçue, requête (`?…`) comprise.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// Valeur d'un en-tête (nom en minuscules), le premier s'il est répété.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Le chemin, sans la requête.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or(&self.target)
    }
}

/// Ce qui empêche de lire une requête ; chacun a son statut.
#[derive(Debug)]
pub(super) enum HttpError {
    Malformed(&'static str),
    HeadTooLarge,
    BodyTooLarge { declared: usize, max: usize },
    LengthRequired,
    Timeout,
    Io(std::io::Error),
}

impl HttpError {
    /// Statut et motif, pour la réponse et le journal.
    pub fn status(&self) -> (u16, String) {
        match self {
            HttpError::Malformed(why) => (400, format!("requête illisible : {why}")),
            HttpError::HeadTooLarge => (431, "en-têtes trop longs".into()),
            HttpError::BodyTooLarge { declared, max } => (
                413,
                format!("corps de {declared} octets, au-delà du plafond de {max}"),
            ),
            HttpError::LengthRequired => (
                411,
                "`Content-Length` obligatoire (pas de transfert par morceaux)".into(),
            ),
            HttpError::Timeout => (408, "requête incomplète dans le délai".into()),
            HttpError::Io(e) => (400, format!("lecture interrompue : {e}")),
        }
    }
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::Io(e)
    }
}

/// Lit une requête entière : en-têtes bornés, corps déclaré par `Content-Length` et
/// borné par `max_body` **avant** d'être lu. Un `Expect: 100-continue` reçoit son feu
/// vert une fois la taille acceptée (curl l'envoie pour les corps longs).
pub(super) async fn read_request(
    stream: &mut TcpStream,
    max_body: usize,
) -> Result<Request, HttpError> {
    tokio::time::timeout(READ_TIMEOUT, read_request_inner(stream, max_body))
        .await
        .map_err(|_| HttpError::Timeout)?
}

async fn read_request_inner(stream: &mut TcpStream, max_body: usize) -> Result<Request, HttpError> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(i) = find_head_end(&buf) {
            break i;
        }
        if buf.len() >= MAX_HEAD {
            return Err(HttpError::HeadTooLarge);
        }
        let mut chunk = [0u8; 1024];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(HttpError::Malformed(
                "connexion fermée avant la fin des en-têtes",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(HttpError::Malformed("ligne de requête absente"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or(HttpError::Malformed("méthode absente"))?
        .to_string();
    let target = parts
        .next()
        .ok_or(HttpError::Malformed("cible absente"))?
        .to_string();
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/1.")) {
        return Err(HttpError::Malformed("version HTTP/1.x attendue"));
    }
    let mut headers = Vec::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (k, v) = line
            .split_once(':')
            .ok_or(HttpError::Malformed("en-tête sans `:`"))?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if header("transfer-encoding").is_some_and(|v| !v.eq_ignore_ascii_case("identity")) {
        return Err(HttpError::LengthRequired);
    }
    let declared = match header("content-length") {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| HttpError::Malformed("`Content-Length` illisible"))?,
        None => 0,
    };
    if declared > max_body {
        return Err(HttpError::BodyTooLarge {
            declared,
            max: max_body,
        });
    }
    let mut body = buf[head_end + 4..].to_vec();
    if body.len() < declared
        && header("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    while body.len() < declared {
        let mut chunk = vec![0u8; (declared - body.len()).min(16 * 1024)];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(HttpError::Malformed(
                "connexion fermée avant la fin du corps",
            ));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(declared);
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Une réponse : statut, en-têtes propres, corps JSON.
pub(super) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn json(status: u16, body: &serde_json::Value) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    pub fn with_header(mut self, name: &str, value: impl ToString) -> Response {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// Écrit la réponse, ferme le côté émission, puis avale ce que le client envoyait encore
/// (un corps refusé avant d'être lu, des en-têtes trop longs) jusqu'à sa fermeture, dans
/// une borne : fermer avec des octets non lus enverrait un RST et le client perdrait la
/// réponse qui lui dit pourquoi.
pub(super) async fn write_response(
    stream: &mut TcpStream,
    response: &Response,
) -> std::io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    for (k, v) in &response.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&response.body);
    stream.write_all(out.as_bytes()).await?;
    stream.shutdown().await?;
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, drain(stream)).await;
    Ok(())
}

/// Attente maximale de la fermeture par le client, une fois la réponse partie.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Au-delà, le client n'est plus attendu.
const DRAIN_MAX: usize = 256 * 1024;

async fn drain(stream: &mut TcpStream) {
    let mut sink = [0u8; 4096];
    let mut total = 0;
    while total < DRAIN_MAX {
        match stream.read(&mut sink).await {
            Ok(0) | Err(_) => return,
            Ok(n) => total += n,
        }
    }
}
