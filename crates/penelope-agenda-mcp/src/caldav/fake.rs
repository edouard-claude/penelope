//! Faux serveur CalDAV des tests : un principal, un dossier avec des calendriers
//! d'événements, un calendrier de rappels (`VTODO` seul) et une boîte de réception, et un
//! `REPORT` qui rend les objets déposés. Il vérifie l'authentification Basic, journalise
//! chaque requête (méthode, chemin, `Depth`, corps) et sait tomber en panne.

use base64::Engine;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    /// Tout est refusé en 401.
    Unauthorized,
    /// Tout tombe en 500.
    Broken,
    /// `/` redirige vers `/dav/`, qui sert le principal (Nextcloud, `.well-known`).
    RedirectBase,
    /// Aucun `current-user-principal` : l'adresse donnée est déjà le dossier.
    NoPrincipal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub depth: Option<String>,
    pub body: String,
}

#[derive(Default)]
pub struct State {
    pub requests: Vec<Recorded>,
    pub mode: Mode,
    /// `slug -> (nom affiché, objets iCalendar)`.
    pub calendars: BTreeMap<String, (String, Vec<String>)>,
}

pub struct FakeCalDav {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

pub const USER: &str = "anne@exemple.fr";
pub const PASSWORD: &str = "abcd-efgh-ijkl-mnop";

impl FakeCalDav {
    pub async fn start() -> FakeCalDav {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(handle(sock, st.clone()));
            }
        });
        FakeCalDav { url, state }
    }

    pub fn add_calendar(&self, slug: &str, name: &str, objects: &[&str]) {
        self.state.lock().unwrap().calendars.insert(
            slug.to_string(),
            (
                name.to_string(),
                objects.iter().map(|s| s.to_string()).collect(),
            ),
        );
    }

    pub fn set_mode(&self, mode: Mode) {
        self.state.lock().unwrap().mode = mode;
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().requests.clone()
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
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
    let mut parts = lines.next()?.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
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
    Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn authorised(req: &Request) -> bool {
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{PASSWORD}"))
    );
    header(req, "authorization") == Some(expected.as_str())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn multistatus(responses: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <d:multistatus xmlns:d=\"DAV:\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\" \
         xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:ical=\"http://apple.com/ns/ical/\">\
         {responses}</d:multistatus>"
    )
}

fn response(href: &str, props: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop>\
         <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
    )
}

/// Le dossier des calendriers : le dossier lui-même, les calendriers déposés, un
/// calendrier de rappels, une boîte de réception, et un `displayname` refusé en 404 pour
/// le dossier (comme iCloud le fait).
fn listing(state: &State) -> String {
    let mut r = String::new();
    r.push_str(
        "<d:response><d:href>/calendars/anne/</d:href>\
         <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>\
         <d:status>HTTP/1.1 200 OK</d:status></d:propstat>\
         <d:propstat><d:prop><d:displayname/></d:prop>\
         <d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>",
    );
    for (slug, (name, _)) in &state.calendars {
        r.push_str(&response(
            &format!("/calendars/anne/{slug}/"),
            &format!(
                "<d:displayname>{}</d:displayname>\
                 <d:resourcetype><d:collection/><cal:calendar/></d:resourcetype>\
                 <cal:supported-calendar-component-set><cal:comp name=\"VEVENT\"/></cal:supported-calendar-component-set>\
                 <ical:calendar-color>#FF2968FF</ical:calendar-color>",
                xml_escape(name)
            ),
        ));
    }
    r.push_str(&response(
        "/calendars/anne/rappels/",
        "<d:displayname>Rappels</d:displayname>\
         <d:resourcetype><d:collection/><cal:calendar/></d:resourcetype>\
         <cal:supported-calendar-component-set><cal:comp name=\"VTODO\"/></cal:supported-calendar-component-set>",
    ));
    r.push_str(&response(
        "/calendars/anne/inbox/",
        "<d:displayname>Inbox</d:displayname>\
         <d:resourcetype><d:collection/><cal:schedule-inbox/></d:resourcetype>",
    ));
    multistatus(&r)
}

fn report(state: &State, slug: &str) -> Option<String> {
    let (_, objects) = state.calendars.get(slug)?;
    let r: String = objects
        .iter()
        .enumerate()
        .map(|(i, ics)| {
            response(
                &format!("/calendars/anne/{slug}/{i}.ics"),
                &format!(
                    "<d:getetag>\"{i}\"</d:getetag><cal:calendar-data><![CDATA[{ics}]]></cal:calendar-data>"
                ),
            )
        })
        .collect();
    Some(multistatus(&r))
}

/// `(statut, en-têtes, corps)` d'une requête.
fn answer(state: &mut State, req: &Request) -> (u16, Vec<(String, String)>, String) {
    let xml = |body: String| {
        (
            207,
            vec![(
                "Content-Type".to_string(),
                "application/xml; charset=utf-8".to_string(),
            )],
            body,
        )
    };
    let mode = state.mode;
    if mode == Mode::Broken {
        return (500, vec![], "panne".into());
    }
    if mode == Mode::Unauthorized || !authorised(req) {
        return (
            401,
            vec![(
                "WWW-Authenticate".to_string(),
                "Basic realm=\"caldav\"".to_string(),
            )],
            String::new(),
        );
    }
    let principal = xml(multistatus(&response(
        "/",
        "<d:current-user-principal><d:href>/principals/anne/</d:href></d:current-user-principal>",
    )));
    match (req.method.as_str(), req.path.as_str()) {
        ("PROPFIND", "/") if mode == Mode::RedirectBase => (
            301,
            vec![("Location".to_string(), "/dav/".to_string())],
            String::new(),
        ),
        ("PROPFIND", "/") if mode == Mode::NoPrincipal => xml(listing(state)),
        ("PROPFIND", "/") | ("PROPFIND", "/dav/") => principal,
        ("PROPFIND", "/principals/anne/") => xml(multistatus(&response(
            "/principals/anne/",
            "<cal:calendar-home-set><d:href>/calendars/anne/</d:href></cal:calendar-home-set>",
        ))),
        ("PROPFIND", "/calendars/anne/") => xml(listing(state)),
        ("REPORT", path) => {
            let slug = path
                .trim_start_matches("/calendars/anne/")
                .trim_end_matches('/');
            match report(state, slug) {
                Some(body) => xml(body),
                None => (404, vec![], String::new()),
            }
        }
        _ => (404, vec![], String::new()),
    }
}

async fn handle(mut sock: tokio::net::TcpStream, state: Arc<Mutex<State>>) {
    let Some(req) = read_request(&mut sock).await else {
        return;
    };
    let (status, headers, body) = {
        let mut st = state.lock().unwrap();
        st.requests.push(Recorded {
            method: req.method.clone(),
            path: req.path.clone(),
            depth: header(&req, "depth").map(str::to_string),
            body: req.body.clone(),
        });
        answer(&mut st, &req)
    };
    let reason = match status {
        207 => "Multi-Status",
        301 => "Moved Permanently",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let _ = sock.write_all(out.as_bytes()).await;
    let _ = sock.write_all(body.as_bytes()).await;
    let _ = sock.shutdown().await;
}
