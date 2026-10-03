//! Le strict nécessaire de HTTP/1.1 pour recevoir un webhook (#294) : une requête lue en
//! entier dans des bornes, une réponse écrite, la connexion fermée. Le workspace a un
//! client HTTP (`reqwest`) et aucun serveur ; un POST signé avec un corps borné ne
//! justifie pas d'en ajouter un. L'en-tête, lui, est lu par `httparse` (celui de `hyper`
//! et de `tokio-tungstenite`, déjà dans le graphe) : jetons, noms d'en-tête et lignes
//! repliées sont son affaire. Ici s'ajoute ce qu'il laisse passer et qu'un intermédiaire
//! pourrait lire autrement que nous (*request smuggling*) : CR ou LF nus, `Content-Length`
//! répété ou qui n'est pas un nombre décimal, `Transfer-Encoding` avec `Content-Length`.
//! Tout cas ambigu est refusé en 400 ; la connexion est fermée après chaque réponse, donc
//! rien de ce qui suit le corps déclaré n'est jamais lu comme une requête.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Au-delà, la ligne de requête et les en-têtes sont refusés.
const MAX_HEAD: usize = 8 * 1024;
/// Au-delà, les en-têtes sont refusés (431).
const MAX_HEADERS: usize = 64;
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
    let (head_len, request) = loop {
        if let Some(parsed) = parse_head(&buf)? {
            break parsed;
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
    let declared = body_length(&request.headers)?;
    if declared > max_body {
        return Err(HttpError::BodyTooLarge {
            declared,
            max: max_body,
        });
    }
    let mut body = buf[head_len..].to_vec();
    if body.len() < declared
        && request
            .header("expect")
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
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
    Ok(Request { body, ..request })
}

/// Analyse l'en-tête reçu jusqu'ici : `None` s'il est incomplet, sinon sa longueur (ligne
/// vide comprise) et la requête sans corps. Les fins de ligne sont vérifiées avant
/// `httparse`, qui accepte un LF seul là où un intermédiaire peut ne pas le voir.
pub(super) fn parse_head(buf: &[u8]) -> Result<Option<(usize, Request)>, HttpError> {
    let head = &buf[..buf.len().min(MAX_HEAD)];
    let scanned = match head.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => &head[..i + 4],
        // Un CR en dernier octet attend peut-être son LF.
        None => head.strip_suffix(b"\r").unwrap_or(head),
    };
    bare_line_ends(scanned)?;
    let mut slots = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut parsed = httparse::Request::new(&mut slots);
    let head_len = match parsed.parse(head) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return Ok(None),
        Err(httparse::Error::TooManyHeaders) => return Err(HttpError::HeadTooLarge),
        Err(e) => return Err(HttpError::Malformed(malformed(e))),
    };
    let headers = parsed
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_ascii_lowercase(),
                String::from_utf8_lossy(h.value).trim().to_string(),
            )
        })
        .collect();
    Ok(Some((
        head_len,
        Request {
            method: parsed.method.unwrap_or_default().to_string(),
            target: parsed.path.unwrap_or_default().to_string(),
            headers,
            body: Vec::new(),
        },
    )))
}

/// Un CR qui n'est pas suivi d'un LF, ou un LF qui n'est pas précédé d'un CR.
fn bare_line_ends(head: &[u8]) -> Result<(), HttpError> {
    for (i, b) in head.iter().enumerate() {
        let bare = match b {
            b'\r' => head.get(i + 1) != Some(&b'\n'),
            b'\n' => i == 0 || head[i - 1] != b'\r',
            _ => false,
        };
        if bare {
            return Err(HttpError::Malformed("CR ou LF nu dans l'en-tête"));
        }
    }
    Ok(())
}

fn malformed(e: httparse::Error) -> &'static str {
    match e {
        httparse::Error::HeaderName => "nom d'en-tête invalide (ligne repliée ?)",
        httparse::Error::HeaderValue => "valeur d'en-tête invalide",
        httparse::Error::NewLine => "fin de ligne invalide",
        httparse::Error::Status | httparse::Error::Token => "ligne de requête invalide",
        httparse::Error::Version => "version HTTP/1.x attendue",
        httparse::Error::TooManyHeaders => "trop d'en-têtes",
    }
}

/// La longueur du corps, sans ambiguïté : `Content-Length` une seule fois, en chiffres
/// décimaux seulement (ni signe, ni liste) ; `Transfer-Encoding`, quel qu'il soit, n'est
/// pas servi (411), et refusé en 400 s'il accompagne `Content-Length` : l'un ou l'autre
/// fait foi selon l'intermédiaire.
pub(super) fn body_length(headers: &[(String, String)]) -> Result<usize, HttpError> {
    let lengths: Vec<&str> = headers
        .iter()
        .filter(|(k, _)| k == "content-length")
        .map(|(_, v)| v.as_str())
        .collect();
    let chunked = headers.iter().any(|(k, _)| k == "transfer-encoding");
    match (lengths.as_slice(), chunked) {
        ([], true) => Err(HttpError::LengthRequired),
        (_, true) => Err(HttpError::Malformed(
            "`Transfer-Encoding` et `Content-Length` ensemble",
        )),
        ([], false) => Ok(0),
        ([v], false) => {
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(HttpError::Malformed("`Content-Length` illisible"));
            }
            v.parse::<usize>()
                .map_err(|_| HttpError::Malformed("`Content-Length` illisible"))
        }
        (_, false) => Err(HttpError::Malformed("`Content-Length` répété")),
    }
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
        409 => "Conflict",
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
