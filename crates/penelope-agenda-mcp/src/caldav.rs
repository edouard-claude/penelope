//! Client CalDAV (RFC 4791) : découverte du principal et du dossier des calendriers par
//! `PROPFIND` (RFC 6764), liste des calendriers, `REPORT calendar-query` sur une plage ou
//! sur un `UID`, et écriture d'un objet par `PUT` et `DELETE` conditionnels (`If-Match`,
//! `If-None-Match`) : une modification faite ailleurs entre la lecture et l'écriture
//! refuse l'écriture au lieu d'être écrasée. Authentification Basic avec un mot de passe
//! d'application ; le mot de passe ne figure dans aucune erreur ni aucun journal.

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};
use reqwest::Method;
use std::time::Duration;
use url::Url;

const NS_DAV: &str = "DAV:";
const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const NS_APPLE: &str = "http://apple.com/ns/ical/";

/// Redirections suivies au plus (`/.well-known/caldav`, Nextcloud).
const MAX_REDIRECTS: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("adresse CalDAV invalide « {0} » : attendu https://hôte/chemin, sans identifiants")]
    Url(String),
    #[error(
        "identifiants refusés par le serveur CalDAV (HTTP {0}) : vérifier AGENDA_USER et le \
         mot de passe d'application, ou l'autorisation OAuth (`penelope mcp auth`)"
    )]
    Auth(u16),
    #[error("serveur CalDAV injoignable : {0}")]
    Network(String),
    #[error("{what} : HTTP {status} inattendu")]
    Status { what: String, status: u16 },
    #[error("réponse CalDAV illisible ({what}) : {reason}")]
    Xml { what: String, reason: String },
    #[error("découverte CalDAV : {0}")]
    Discovery(String),
    #[error(
        "l'événement a changé sur le serveur depuis sa lecture (HTTP 412) : relire puis \
         recommencer"
    )]
    Conflict,
}

/// Un objet iCalendar stocké : son adresse, sa version (`ETag`) et son texte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub href: Url,
    pub etag: Option<String>,
    pub ics: String,
}

/// Un calendrier d'événements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub name: String,
    pub url: Url,
    pub color: Option<String>,
}

pub struct Client {
    http: reqwest::Client,
    base: Url,
    user: String,
    password: String,
    /// Jeton OAuth : présent, il remplace l'authentification Basic.
    token: Option<String>,
}

/// Sans le mot de passe : un `{:?}` dans un journal ne doit rien révéler.
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(url: &str, user: &str, password: &str, timeout: Duration) -> Result<Client, Error> {
        let base = Url::parse(url.trim()).map_err(|_| Error::Url(url.trim().to_string()))?;
        if !base.username().is_empty() || base.password().is_some() {
            // L'erreur ne répète pas des identifiants écrits au mauvais endroit.
            let mut shown = base.clone();
            let _ = shown.set_username("");
            let _ = shown.set_password(None);
            return Err(Error::Url(shown.to_string()));
        }
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(Error::Url(url.trim().to_string()));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            // Suivies à la main, avec la même méthode : reqwest les changerait en GET.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("penelope-agenda-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;
        Ok(Client {
            http,
            base,
            user: user.to_string(),
            password: password.to_string(),
            token: None,
        })
    }

    /// Le même client, authentifié par un jeton OAuth (`Bearer`) au lieu du mot de passe.
    pub fn with_token(mut self, token: &str) -> Client {
        self.token = (!token.is_empty()).then(|| token.to_string());
        self
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req.basic_auth(&self.user, Some(&self.password)),
        }
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Une requête WebDAV ; rend le corps d'une réponse 200 ou 207.
    async fn dav(
        &self,
        method: &str,
        url: &Url,
        depth: &str,
        body: &str,
        what: &str,
    ) -> Result<String, Error> {
        let mut url = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            let m =
                Method::from_bytes(method.as_bytes()).map_err(|e| Error::Network(e.to_string()))?;
            let resp = self
                .authed(self.http.request(m, url.clone()))
                .header("Depth", depth)
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(body.to_string())
                .send()
                .await
                .map_err(|e| Error::Network(e.to_string()))?;
            let status = resp.status();
            if status.is_redirection() {
                let location = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(Error::Status {
                        what: what.to_string(),
                        status: status.as_u16(),
                    })?;
                url = url
                    .join(location)
                    .map_err(|_| Error::Url(location.to_string()))?;
                continue;
            }
            return match status.as_u16() {
                401 | 403 => Err(Error::Auth(status.as_u16())),
                200 | 207 => {
                    let body = resp
                        .text()
                        .await
                        .map_err(|e| Error::Network(e.to_string()))?;
                    // `AGENDA_TRACE=<fichier>` : chaque réponse WebDAV y est ajoutée, pour
                    // comprendre un serveur qui ne rend aucun calendrier. Jamais les
                    // en-têtes : ni identifiants ni jeton.
                    if let Ok(path) = std::env::var("AGENDA_TRACE")
                        && let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                    {
                        use std::io::Write;
                        let shown: String = body.chars().take(4000).collect();
                        let _ = writeln!(f, "{method} {url} → {status} :\n{shown}\n");
                    }
                    Ok(body)
                }
                s => Err(Error::Status {
                    what: what.to_string(),
                    status: s,
                }),
            };
        }
        Err(Error::Discovery(format!("{what} : trop de redirections")))
    }

    /// Le principal du compte (`current-user-principal`), depuis l'adresse de base.
    pub async fn principal(&self) -> Result<Url, Error> {
        let what = "principal";
        let xml = self
            .dav(
                "PROPFIND",
                &self.base,
                "0",
                &propfind("<d:current-user-principal/>"),
                what,
            )
            .await?;
        let doc = parse_xml(&xml, what)?;
        first_href(&doc, NS_DAV, "current-user-principal")
            .and_then(|h| self.base.join(&h).ok())
            .ok_or_else(|| {
                Error::Discovery(
                    "le serveur ne nomme pas le principal (current-user-principal absent)".into(),
                )
            })
    }

    /// Le dossier des calendriers du principal (`calendar-home-set`).
    pub async fn calendar_home(&self, principal: &Url) -> Result<Url, Error> {
        let what = "dossier des calendriers";
        let xml = self
            .dav(
                "PROPFIND",
                principal,
                "0",
                &propfind("<c:calendar-home-set/>"),
                what,
            )
            .await?;
        let doc = parse_xml(&xml, what)?;
        first_href(&doc, NS_CALDAV, "calendar-home-set")
            .and_then(|h| principal.join(&h).ok())
            .ok_or_else(|| Error::Discovery("calendar-home-set absent du principal".into()))
    }

    /// Les calendriers d'événements d'un dossier : les collections `calendar` dont les
    /// composants acceptés comprennent `VEVENT` (les rappels, `VTODO` seul, et les boîtes
    /// de réception sont écartés).
    pub async fn calendars(&self, home: &Url) -> Result<Vec<Calendar>, Error> {
        let what = "liste des calendriers";
        let props = "<d:displayname/><d:resourcetype/><c:supported-calendar-component-set/>\
                     <ic:calendar-color/>";
        let xml = self
            .dav("PROPFIND", home, "1", &propfind(props), what)
            .await?;
        let doc = parse_xml(&xml, what)?;
        let mut out = Vec::new();
        for (href, prop) in ok_props(&doc) {
            let Some(url) = home.join(&href).ok() else {
                continue;
            };
            let is_calendar = descendant(prop, NS_DAV, "resourcetype")
                .is_some_and(|rt| rt.children().any(|c| is(c, NS_CALDAV, "calendar")));
            if !is_calendar {
                continue;
            }
            let components: Vec<&str> =
                descendant(prop, NS_CALDAV, "supported-calendar-component-set")
                    .map(|s| {
                        s.children()
                            .filter(|c| is(*c, NS_CALDAV, "comp"))
                            .filter_map(|c| c.attribute("name"))
                            .collect()
                    })
                    .unwrap_or_default();
            if !components.is_empty() && !components.contains(&"VEVENT") {
                continue;
            }
            let name = descendant(prop, NS_DAV, "displayname")
                .map(text)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| last_segment(&url));
            let color = descendant(prop, NS_APPLE, "calendar-color")
                .map(text)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            out.push(Calendar { name, url, color });
        }
        Ok(out)
    }

    /// Principal, dossier, calendriers. Un serveur qui ne nomme pas de principal est
    /// interrogé directement sur son dossier des calendriers : Google sert son principal
    /// (`…/caldav/v2/<compte>/user`) sans `current-user-principal`, mais avec
    /// `calendar-home-set` (#409). Sans l'un ni l'autre, l'adresse est prise pour le dossier
    /// lui-même, et à défaut pour un calendrier.
    pub async fn discover(&self) -> Result<Vec<Calendar>, Error> {
        let home = match self.principal().await {
            Ok(principal) => self.calendar_home(&principal).await?,
            Err(Error::Discovery(_)) => match self.calendar_home(&self.base).await {
                Ok(home) => home,
                Err(Error::Discovery(_)) => self.base.clone(),
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        };
        self.calendars(&home).await
    }

    /// Les objets iCalendar d'un calendrier dont un `VEVENT` touche `[start, end)` : le
    /// serveur filtre, séries comprises ; les objets reviennent entiers (maître et
    /// surcharges), à développer côté client.
    pub async fn events(
        &self,
        calendar: &Url,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<String>, Error> {
        let what = "événements";
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\
             <d:prop><d:getetag/><c:calendar-data/></d:prop>\
             <c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\">\
             <c:time-range start=\"{}\" end=\"{}\"/>\
             </c:comp-filter></c:comp-filter></c:filter></c:calendar-query>",
            start.format("%Y%m%dT%H%M%SZ"),
            end.format("%Y%m%dT%H%M%SZ")
        );
        let xml = self.dav("REPORT", calendar, "1", &body, what).await?;
        let doc = parse_xml(&xml, what)?;
        Ok(ok_props(&doc)
            .into_iter()
            .filter_map(|(_, prop)| descendant(prop, NS_CALDAV, "calendar-data").map(text))
            .filter(|s| !s.trim().is_empty())
            .collect())
    }
}

impl Client {
    /// L'objet d'un calendrier qui porte ce `UID`, s'il existe.
    pub async fn find(&self, calendar: &Url, uid: &str) -> Result<Option<Resource>, Error> {
        let what = "recherche d'un événement";
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\
             <d:prop><d:getetag/><c:calendar-data/></d:prop>\
             <c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\">\
             <c:prop-filter name=\"UID\"><c:text-match collation=\"i;octet\">{}</c:text-match>\
             </c:prop-filter></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>",
            xml_escape(uid)
        );
        let xml = self.dav("REPORT", calendar, "1", &body, what).await?;
        let doc = parse_xml(&xml, what)?;
        // `text-match` cherche une sous-chaîne : seul un objet dont l'`UID` est exactement
        // celui demandé compte, et deux objets pour un même `UID` refusent l'écriture
        // plutôt que d'en toucher un au hasard.
        let mut found: Vec<Resource> = ok_props(&doc)
            .into_iter()
            .filter_map(|(href, prop)| {
                let ics = descendant(prop, NS_CALDAV, "calendar-data").map(text)?;
                crate::ical::events(&ics)
                    .iter()
                    .any(|e| e.uid == uid)
                    .then_some(())?;
                Some(Resource {
                    href: calendar.join(&href).ok()?,
                    etag: descendant(prop, NS_DAV, "getetag").map(|e| text(e).trim().to_string()),
                    ics,
                })
            })
            .collect();
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            n => Err(Error::Discovery(format!(
                "{n} objets portent l'UID « {uid} »"
            ))),
        }
    }

    /// Écrit un objet : création (`etag` absent, `If-None-Match: *`) ou remplacement de la
    /// version lue (`If-Match`). Rend le nouvel `ETag` s'il est donné.
    pub async fn put(
        &self,
        url: &Url,
        ics: &str,
        etag: Option<&str>,
    ) -> Result<Option<String>, Error> {
        let req = self
            .http
            .put(url.clone())
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(ics.to_string());
        let req = match etag {
            Some(e) => req.header("If-Match", e),
            None => req.header("If-None-Match", "*"),
        };
        self.write(req, "écriture d'un événement").await
    }

    /// Supprime un objet, à la version lue.
    pub async fn delete(&self, url: &Url, etag: Option<&str>) -> Result<(), Error> {
        let mut req = self.http.delete(url.clone());
        if let Some(e) = etag {
            req = req.header("If-Match", e);
        }
        self.write(req, "suppression d'un événement")
            .await
            .map(|_| ())
    }

    async fn write(
        &self,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<Option<String>, Error> {
        let resp = self
            .authed(req)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        match resp.status().as_u16() {
            200 | 201 | 204 => Ok(resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)),
            401 | 403 => Err(Error::Auth(resp.status().as_u16())),
            412 => Err(Error::Conflict),
            s => Err(Error::Status {
                what: what.to_string(),
                status: s,
            }),
        }
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Un corps `PROPFIND` demandant `props`.
fn propfind(props: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" \
         xmlns:ic=\"http://apple.com/ns/ical/\"><d:prop>{props}</d:prop></d:propfind>"
    )
}

// ------------------------------------------------------------------ XML

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn parse_xml<'a>(xml: &'a str, what: &str) -> Result<roxmltree::Document<'a>, Error> {
    roxmltree::Document::parse(xml).map_err(|e| Error::Xml {
        what: what.to_string(),
        reason: e.to_string(),
    })
}

fn is(node: Node<'_, '_>, ns: &str, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == Some(ns)
}

fn descendant<'a, 'i>(node: Node<'a, 'i>, ns: &str, name: &str) -> Option<Node<'a, 'i>> {
    node.descendants().find(|n| is(*n, ns, name))
}

/// Le texte d'un élément, nœuds de texte et CDATA réunis.
fn text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

fn first_href(doc: &roxmltree::Document<'_>, ns: &str, name: &str) -> Option<String> {
    descendant(doc.root(), ns, name)
        .and_then(|n| descendant(n, NS_DAV, "href"))
        .map(|h| text(h).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Les `(href, prop)` des `propstat` en 200 d'un `multistatus`.
fn ok_props<'a, 'i>(doc: &'i roxmltree::Document<'a>) -> Vec<(String, Node<'a, 'i>)> {
    let mut out = Vec::new();
    for resp in doc
        .root()
        .descendants()
        .filter(|n| is(*n, NS_DAV, "response"))
    {
        let Some(href) = resp
            .children()
            .find(|n| is(*n, NS_DAV, "href"))
            .map(|h| text(h).trim().to_string())
        else {
            continue;
        };
        for ps in resp.children().filter(|n| is(*n, NS_DAV, "propstat")) {
            let ok = ps
                .children()
                .find(|n| is(*n, NS_DAV, "status"))
                .map(|s| text(s).contains(" 200"))
                .unwrap_or(true);
            if !ok {
                continue;
            }
            if let Some(prop) = ps.children().find(|n| is(*n, NS_DAV, "prop")) {
                out.push((href.clone(), prop));
            }
        }
    }
    out
}

/// Le dernier segment d'une adresse : nom de repli d'un calendrier sans `displayname`.
fn last_segment(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut s| s.rfind(|p| !p.is_empty()).map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "calendrier".into())
}
