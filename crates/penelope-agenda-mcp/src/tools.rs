//! Les quatre outils, tous en lecture, et leur exécution : fenêtre dans le fuseau
//! demandé, `REPORT` par calendrier, occurrences développées, rendu texte et structuré.
//!
//! Un échec de l'outil (identifiants refusés, date illisible) revient en `isError` : c'est
//! une réponse que le modèle, ou le digest, peut lire et corriger ; seul un outil inconnu
//! est une erreur de protocole.

use crate::caldav::{Calendar, Client, Error};
use crate::config::Settings;
use crate::ical::{self, Occurrence, When, Window};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use penelope_mcp::protocol::{INVALID_PARAMS, RpcError};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

pub const CALENDAR_LIST: &str = "calendar_list";
pub const EVENTS_TODAY: &str = "events_today";
pub const EVENTS_RANGE: &str = "events_range";
pub const EVENT_SEARCH: &str = "event_search";

/// Durée de vie de la liste des calendriers : la découverte coûte trois requêtes.
const CALENDARS_TTL: std::time::Duration = std::time::Duration::from_secs(600);
/// Plage maximale d'`events_range` et d'`event_search`, en jours.
pub const MAX_RANGE_DAYS: i64 = 366;
/// Résultats d'une recherche au plus.
const SEARCH_LIMIT: usize = 50;
/// Recherche sans bornes : d'un mois en arrière à onze mois en avant, dans la plage
/// maximale.
const SEARCH_BACK_DAYS: i64 = 30;
const SEARCH_FORWARD_DAYS: i64 = MAX_RANGE_DAYS - SEARCH_BACK_DAYS - 1;

const DAYS: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];
const MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

/// L'horloge, remplaçable dans les tests.
pub type Now = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct Agenda {
    settings: Settings,
    client: Client,
    calendars: Mutex<Option<(Instant, Vec<Calendar>)>>,
    now: Now,
}

/// Une occurrence et le calendrier d'où elle vient.
struct Item {
    calendar: String,
    occurrence: Occurrence,
}

impl Agenda {
    pub fn new(settings: Settings) -> Result<Agenda, Error> {
        let client = Client::new(
            &settings.url,
            &settings.user,
            &settings.password,
            settings.timeout,
        )?;
        Ok(Agenda {
            settings,
            client,
            calendars: Mutex::new(None),
            now: Arc::new(Utc::now),
        })
    }

    pub fn with_clock(mut self, now: Now) -> Agenda {
        self.now = now;
        self
    }

    /// Les descripteurs de `tools/list`.
    pub fn descriptors() -> Vec<Value> {
        let read = json!({
            "readOnlyHint": true, "destructiveHint": false,
            "idempotentHint": true, "openWorldHint": true
        });
        let tz = json!({
            "type": "string",
            "description": "Fuseau IANA des heures rendues (Indian/Reunion, Europe/Paris) ; \
                            défaut : celui du serveur."
        });
        let events_out = json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}, "events": {"type": "array"}},
            "required": ["events"]
        });
        let date = |role: &str| {
            json!({"type": "string", "description": format!(
                "{role}, `AAAA-MM-JJ` (jour entier, heure locale) ou RFC 3339.")})
        };
        vec![
            json!({
                "name": CALENDAR_LIST, "title": "Calendriers",
                "description": "Liste les calendriers lisibles de l'agenda (nom, adresse, couleur).",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
                "outputSchema": {"type": "object", "properties": {"calendars": {"type": "array"}},
                                 "required": ["calendars"]},
                "annotations": read
            }),
            json!({
                "name": EVENTS_TODAY, "title": "Agenda du jour",
                "description": "Les événements d'aujourd'hui dans le fuseau demandé, journée \
                                entière comprise, triés par heure.",
                "inputSchema": {"type": "object", "properties": {"timezone": tz},
                                "additionalProperties": false},
                "outputSchema": events_out,
                "annotations": read
            }),
            json!({
                "name": EVENTS_RANGE, "title": "Agenda d'une période",
                "description": "Les événements entre deux dates (366 jours au plus), dans le \
                                fuseau demandé, triés par début.",
                "inputSchema": {"type": "object",
                                "properties": {"start": date("Début"), "end": date("Fin, incluse"),
                                               "timezone": tz},
                                "required": ["start", "end"], "additionalProperties": false},
                "outputSchema": events_out,
                "annotations": read
            }),
            json!({
                "name": EVENT_SEARCH, "title": "Chercher un événement",
                "description": "Cherche des mots dans le titre, le lieu et la description des \
                                événements (sans la casse), d'un mois en arrière à onze mois en \
                                avant par défaut ; 50 résultats au plus.",
                "inputSchema": {"type": "object",
                                "properties": {"query": {"type": "string",
                                                         "description": "Mots cherchés."},
                                               "from": date("Début"), "to": date("Fin, incluse"),
                                               "timezone": tz},
                                "required": ["query"], "additionalProperties": false},
                "outputSchema": events_out,
                "annotations": read
            }),
        ]
    }

    /// Le résultat d'un `tools/call` : `Ok` même quand l'outil échoue (`isError`), `Err`
    /// pour un outil inconnu.
    pub async fn call(&self, name: &str, args: &Value) -> Result<Value, RpcError> {
        let outcome = match name {
            CALENDAR_LIST => self.calendar_list().await,
            EVENTS_TODAY => self.events_today(args).await,
            EVENTS_RANGE => self.events_range(args).await,
            EVENT_SEARCH => self.event_search(args).await,
            other => {
                return Err(RpcError {
                    code: INVALID_PARAMS,
                    message: format!(
                        "outil inconnu : {other} (outils : {CALENDAR_LIST}, {EVENTS_TODAY}, \
                         {EVENTS_RANGE}, {EVENT_SEARCH})"
                    ),
                    data: None,
                });
            }
        };
        Ok(outcome.unwrap_or_else(failure))
    }

    // ------------------------------------------------------------ outils

    async fn calendar_list(&self) -> Result<Value, String> {
        let calendars = self.calendars().await.map_err(|e| e.to_string())?;
        let text = if calendars.is_empty() {
            "Aucun calendrier d'événements lisible.".to_string()
        } else {
            let mut t = format!("{} calendrier(s) :\n", calendars.len());
            for c in &calendars {
                t.push_str(&format!("- {}\n", c.name));
            }
            t
        };
        let structured: Vec<Value> = calendars
            .iter()
            .map(|c| json!({"name": c.name, "url": c.url.as_str(), "color": c.color}))
            .collect();
        Ok(success(text, json!({"calendars": structured})))
    }

    async fn events_today(&self, args: &Value) -> Result<Value, String> {
        let tz = self.timezone(args)?;
        let today = (self.now)().with_timezone(&tz).date_naive();
        let window = day_window(today, today, tz);
        let items = self.collect(window).await?;
        let title = if items.is_empty() {
            format!(
                "Aucun événement aujourd'hui ({}, {}).",
                french_date(today),
                tz.name()
            )
        } else {
            format!(
                "Aujourd'hui, {} ({}) : {} événement(s)",
                french_date(today),
                tz.name(),
                items.len()
            )
        };
        let mut structured = structured(&items, tz, window);
        structured["date"] = json!(today.format("%Y-%m-%d").to_string());
        Ok(success(render(&title, &items, tz), structured))
    }

    async fn events_range(&self, args: &Value) -> Result<Value, String> {
        let tz = self.timezone(args)?;
        let start = bound(args, "start", tz, false)?.ok_or("`start` est obligatoire")?;
        let end = bound(args, "end", tz, true)?.ok_or("`end` est obligatoire")?;
        let window = checked_window(start, end)?;
        let items = self.collect(window).await?;
        let title = format!(
            "{} événement(s) du {} au {} ({})",
            items.len(),
            window.start.with_timezone(&tz).format("%d/%m/%Y %H:%M"),
            window.end.with_timezone(&tz).format("%d/%m/%Y %H:%M"),
            tz.name()
        );
        Ok(success(
            render(&title, &items, tz),
            structured(&items, tz, window),
        ))
    }

    async fn event_search(&self, args: &Value) -> Result<Value, String> {
        let tz = self.timezone(args)?;
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or("`query` est obligatoire : les mots cherchés")?
            .to_lowercase();
        let today = (self.now)().with_timezone(&tz).date_naive();
        let start = match bound(args, "from", tz, false)? {
            Some(s) => s,
            None => day_window(today - Duration::days(SEARCH_BACK_DAYS), today, tz).start,
        };
        let end = match bound(args, "to", tz, true)? {
            Some(e) => e,
            None => day_window(today, today + Duration::days(SEARCH_FORWARD_DAYS), tz).end,
        };
        let window = checked_window(start, end)?;
        let mut items: Vec<Item> = self
            .collect(window)
            .await?
            .into_iter()
            .filter(|i| {
                let o = &i.occurrence;
                [
                    Some(&o.summary),
                    o.location.as_ref(),
                    o.description.as_ref(),
                ]
                .into_iter()
                .flatten()
                .any(|s| s.to_lowercase().contains(&query))
            })
            .collect();
        let total = items.len();
        items.truncate(SEARCH_LIMIT);
        let title = format!(
            "{total} événement(s) pour « {query} » du {} au {} ({}){}",
            window.start.with_timezone(&tz).format("%d/%m/%Y"),
            (window.end - Duration::seconds(1))
                .with_timezone(&tz)
                .format("%d/%m/%Y"),
            tz.name(),
            if total > SEARCH_LIMIT {
                format!(", les {SEARCH_LIMIT} premiers")
            } else {
                String::new()
            }
        );
        let mut structured = structured(&items, tz, window);
        structured["total"] = json!(total);
        structured["query"] = json!(query);
        Ok(success(render(&title, &items, tz), structured))
    }

    // ------------------------------------------------------------ lecture

    fn timezone(&self, args: &Value) -> Result<Tz, String> {
        match args.get("timezone").and_then(Value::as_str).map(str::trim) {
            None | Some("") => Ok(self.settings.timezone),
            Some(name) => name.parse::<Tz>().map_err(|_| {
                format!("fuseau inconnu : « {name} » (attendu un nom IANA, Europe/Paris)")
            }),
        }
    }

    /// Les calendriers servis, en cache dix minutes, filtrés par `AGENDA_CALENDARS`.
    async fn calendars(&self) -> Result<Vec<Calendar>, Error> {
        let mut cache = self.calendars.lock().await;
        if let Some((at, list)) = cache.as_ref()
            && at.elapsed() < CALENDARS_TTL
        {
            return Ok(list.clone());
        }
        let mut list = self.client.discover().await?;
        if !self.settings.calendars.is_empty() {
            list.retain(|c| {
                self.settings
                    .calendars
                    .iter()
                    .any(|w| w.eq_ignore_ascii_case(&c.name))
            });
        }
        *cache = Some((Instant::now(), list.clone()));
        Ok(list)
    }

    /// Les occurrences de tous les calendriers dans la fenêtre, triées par début. Les
    /// heures flottantes, les dates et les `TZID` inconnus sont lus dans le fuseau du
    /// serveur (`AGENDA_TIMEZONE`), pas dans celui de l'affichage : « 20 h » dans l'agenda
    /// du propriétaire reste 20 h chez lui, quel que soit le fuseau demandé.
    async fn collect(&self, window: Window) -> Result<Vec<Item>, String> {
        let calendars = self.calendars().await.map_err(|e| e.to_string())?;
        let mut items = Vec::new();
        for c in &calendars {
            let objects = self
                .client
                .events(&c.url, window.start, window.end)
                .await
                .map_err(|e| format!("calendrier « {} » : {e}", c.name))?;
            let events: Vec<ical::Event> = objects.iter().flat_map(|o| ical::events(o)).collect();
            for occurrence in ical::occurrences(&events, window, self.settings.timezone) {
                items.push(Item {
                    calendar: c.name.clone(),
                    occurrence,
                });
            }
        }
        items.sort_by(|a, b| {
            // Les journées entières d'abord, puis l'heure, puis le titre.
            b.occurrence
                .all_day()
                .cmp(&a.occurrence.all_day())
                .then(a.occurrence.start_utc.cmp(&b.occurrence.start_utc))
                .then_with(|| a.occurrence.summary.cmp(&b.occurrence.summary))
        });
        Ok(items)
    }
}

// ------------------------------------------------------------------ fenêtres

/// La fenêtre `[from 00:00, to 24:00)` en heure locale.
fn day_window(from: NaiveDate, to: NaiveDate, tz: Tz) -> Window {
    let at = |d: NaiveDate| {
        tz.from_local_datetime(&d.and_time(NaiveTime::MIN))
            .earliest()
            .or_else(|| {
                tz.from_local_datetime(
                    &d.and_time(NaiveTime::from_hms_opt(1, 0, 0).unwrap_or(NaiveTime::MIN)),
                )
                .earliest()
            })
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|| Utc.from_utc_datetime(&d.and_time(NaiveTime::MIN)))
    };
    Window {
        start: at(from),
        end: at(to + Duration::days(1)),
    }
}

/// Une borne lue dans `args[key]` : `AAAA-MM-JJ` (minuit local ; le jour entier pour une
/// fin) ou RFC 3339. `Ok(None)` si absente.
fn bound(args: &Value, key: &str, tz: Tz, end: bool) -> Result<Option<DateTime<Utc>>, String> {
    let Some(raw) = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let w = day_window(d, d, tz);
        return Ok(Some(if end { w.end } else { w.start }));
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|t| Some(t.with_timezone(&Utc)))
        .map_err(|_| format!("`{key}` illisible : « {raw} » (attendu AAAA-MM-JJ ou RFC 3339)"))
}

fn checked_window(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Window, String> {
    if end <= start {
        return Err("la fin doit suivre le début".into());
    }
    if end - start > Duration::days(MAX_RANGE_DAYS) {
        return Err(format!(
            "plage trop longue : {MAX_RANGE_DAYS} jours au plus"
        ));
    }
    Ok(Window { start, end })
}

// ------------------------------------------------------------------ rendu

fn french_date(d: NaiveDate) -> String {
    format!(
        "{} {} {} {}",
        DAYS[d.weekday().num_days_from_monday() as usize],
        d.day(),
        MONTHS[d.month0() as usize],
        d.year()
    )
}

/// Dernier jour (inclus) d'une occurrence sur la journée : `DTEND` est exclu.
fn last_day(o: &Occurrence) -> Option<NaiveDate> {
    match (&o.start, &o.end) {
        (When::Date(s), When::Date(e)) => Some((*e - Duration::days(1)).max(*s)),
        (When::Date(s), _) => Some(*s),
        _ => None,
    }
}

/// Une ligne lisible : `- 09:00–09:45 Dentiste · Perso · 12 rue des Lilas`.
fn line(item: &Item, tz: Tz) -> String {
    let o = &item.occurrence;
    let when = match (&o.start, last_day(o)) {
        (When::Date(s), Some(e)) if e > *s => {
            format!("{} → {}", s.format("%d/%m"), e.format("%d/%m"))
        }
        (When::Date(_), _) => "journée".to_string(),
        _ => {
            let s = o.start_utc.with_timezone(&tz);
            let e = o.end_utc.with_timezone(&tz);
            if e == s {
                s.format("%H:%M").to_string()
            } else if s.date_naive() == e.date_naive() {
                format!("{}–{}", s.format("%H:%M"), e.format("%H:%M"))
            } else {
                format!("{} → {}", s.format("%d/%m %H:%M"), e.format("%d/%m %H:%M"))
            }
        }
    };
    let mut l = format!("- {when} {} · {}", o.summary, item.calendar);
    if let Some(loc) = &o.location {
        l.push_str(&format!(" · {loc}"));
    }
    l
}

fn render(title: &str, items: &[Item], tz: Tz) -> String {
    let mut t = title.to_string();
    for i in items {
        t.push('\n');
        t.push_str(&line(i, tz));
    }
    t
}

/// L'événement en données : dates seules pour une journée entière, RFC 3339 dans le
/// fuseau sinon.
fn event_json(item: &Item, tz: Tz) -> Value {
    let o = &item.occurrence;
    let (start, end) = match (&o.start, last_day(o)) {
        (When::Date(s), Some(e)) => (
            s.format("%Y-%m-%d").to_string(),
            e.format("%Y-%m-%d").to_string(),
        ),
        _ => (
            o.start_utc.with_timezone(&tz).to_rfc3339(),
            o.end_utc.with_timezone(&tz).to_rfc3339(),
        ),
    };
    json!({
        "uid": o.uid,
        "summary": o.summary,
        "calendar": item.calendar,
        "all_day": o.all_day(),
        "start": start,
        "end": end,
        "location": o.location,
    })
}

fn structured(items: &[Item], tz: Tz, window: Window) -> Value {
    json!({
        "timezone": tz.name(),
        "start": window.start.with_timezone(&tz).to_rfc3339(),
        "end": window.end.with_timezone(&tz).to_rfc3339(),
        "count": items.len(),
        "events": items.iter().map(|i| event_json(i, tz)).collect::<Vec<_>>(),
    })
}

fn success(text: String, structured: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": structured
    })
}

fn failure(text: String) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": true
    })
}
