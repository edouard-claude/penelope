//! Les trois outils d'écriture : créer, modifier et supprimer un événement. Chaque
//! écriture est conditionnelle (`If-None-Match` à la création, `If-Match` sur la version
//! lue sinon) : un événement changé ailleurs entre-temps n'est jamais écrasé.
//!
//! Les heures sont écrites en UTC (`DTSTART:…Z`) : valeur valable partout, sans bloc
//! `VTIMEZONE` à fabriquer ; la lecture les rend dans le fuseau demandé. Une série
//! récurrente ne se modifie pas ici (une occurrence ou la série entière demanderait des
//! `RECURRENCE-ID`) ; elle se supprime en entier, et le résultat le dit.

#[cfg(test)]
mod tests;

use super::{Agenda, success};
use crate::caldav::{Calendar, Resource};
use crate::ical::{self, When};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};
use std::hash::BuildHasher;

pub const EVENT_CREATE: &str = "event_create";
pub const EVENT_UPDATE: &str = "event_update";
pub const EVENT_DELETE: &str = "event_delete";

/// Propriétés qu'une modification réécrit, plus celles qu'elle renouvelle toujours.
const RENEWED: &[&str] = &["DTSTAMP", "LAST-MODIFIED", "SEQUENCE"];

/// Un début ou une fin donnés par le modèle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moment {
    Day(NaiveDate),
    At(DateTime<Utc>),
}

impl Moment {
    /// `AAAA-MM-JJ` (journée entière), `AAAA-MM-JJTHH:MM[:SS]` (heure locale dans `tz`) ou
    /// RFC 3339.
    fn parse(raw: &str, tz: Tz, key: &str) -> Result<Moment, String> {
        let raw = raw.trim();
        if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            return Ok(Moment::Day(d));
        }
        if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
            return Ok(Moment::At(t.with_timezone(&Utc)));
        }
        for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
            if let Ok(n) = NaiveDateTime::parse_from_str(raw, f) {
                return tz
                    .from_local_datetime(&n)
                    .earliest()
                    .map(|t| Moment::At(t.with_timezone(&Utc)))
                    .ok_or_else(|| format!("`{key}` : heure inexistante dans {}", tz.name()));
            }
        }
        Err(format!(
            "`{key}` illisible : « {raw} » (attendu AAAA-MM-JJ, AAAA-MM-JJTHH:MM ou RFC 3339)"
        ))
    }
}

/// `DTSTART` et `DTEND` d'un événement : une journée entière (fin incluse côté outil,
/// exclue en iCalendar) ou deux instants.
fn span(start: Moment, end: Option<Moment>) -> Result<(String, String), String> {
    match (start, end) {
        (Moment::Day(s), None) => Ok((
            date_prop("DTSTART", s),
            date_prop("DTEND", s + Duration::days(1)),
        )),
        (Moment::Day(s), Some(Moment::Day(e))) if e >= s => Ok((
            date_prop("DTSTART", s),
            date_prop("DTEND", e + Duration::days(1)),
        )),
        (Moment::At(s), None) => Ok((
            utc_prop("DTSTART", s),
            utc_prop("DTEND", s + Duration::hours(1)),
        )),
        (Moment::At(s), Some(Moment::At(e))) if e > s => {
            Ok((utc_prop("DTSTART", s), utc_prop("DTEND", e)))
        }
        (Moment::Day(_), Some(Moment::At(_))) | (Moment::At(_), Some(Moment::Day(_))) => {
            Err("début et fin de même nature : deux dates (journée entière) ou deux heures".into())
        }
        _ => Err("la fin doit suivre le début".into()),
    }
}

fn date_prop(name: &str, d: NaiveDate) -> String {
    format!("{name};VALUE=DATE:{}", d.format("%Y%m%d"))
}

fn utc_prop(name: &str, t: DateTime<Utc>) -> String {
    format!("{name}:{}", t.format("%Y%m%dT%H%M%SZ"))
}

/// Un texte iCalendar (RFC 5545 §3.3.11).
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}

/// Les lignes pliées à 75 octets (§3.1), sans couper un caractère.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        if width + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out.push_str("\r\n");
    out
}

fn text_props(args: &Value) -> Vec<String> {
    [
        ("SUMMARY", "summary"),
        ("LOCATION", "location"),
        ("DESCRIPTION", "description"),
    ]
    .into_iter()
    .filter_map(|(prop, key)| {
        let v = args.get(key)?.as_str()?.trim();
        (!v.is_empty()).then(|| format!("{prop}:{}", escape(v)))
    })
    .collect()
}

/// Un `UID` neuf, sans dépendance : l'heure et un aléa du processus.
fn new_uid(now: DateTime<Utc>) -> String {
    let nanos = now.timestamp_nanos_opt().unwrap_or_default();
    let noise = std::collections::hash_map::RandomState::new().hash_one(nanos);
    format!("{nanos:x}-{noise:016x}@penelope")
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Réécrit le premier `VEVENT` : les propriétés `replaced` sont ôtées, `added` posées à
/// la fin du composant, `SEQUENCE` incrémentée.
fn patch(ics: &str, replaced: &[&str], added: &[String]) -> String {
    let lines = ical::unfold(ics);
    let name_of = |l: &str| {
        l.split([';', ':'])
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase()
    };
    let sequence = lines
        .iter()
        .find(|l| name_of(l) == "SEQUENCE")
        .and_then(|l| l.split_once(':'))
        .and_then(|(_, v)| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    // Les composants ouverts : une propriété n'est ôtée que directement sous le premier
    // `VEVENT`, jamais dans un `VALARM` qu'il contient (sa `DESCRIPTION` reste).
    let mut open: Vec<String> = Vec::new();
    let (mut out, mut done) = (String::new(), false);
    for l in &lines {
        let upper = l.trim().to_ascii_uppercase();
        let in_event = !done && open.last().is_some_and(|c| c == "VEVENT");
        if let Some(c) = upper.strip_prefix("BEGIN:") {
            open.push(c.to_string());
        } else if let Some(c) = upper.strip_prefix("END:") {
            if in_event && c == "VEVENT" {
                for a in added {
                    out.push_str(&fold(a));
                }
                out.push_str(&fold(&format!("SEQUENCE:{}", sequence + 1)));
                done = true;
            }
            open.pop();
        } else if in_event {
            let name = name_of(l);
            if replaced.contains(&name.as_str()) || RENEWED.contains(&name.as_str()) {
                continue;
            }
        }
        out.push_str(&fold(l));
    }
    out
}

impl Agenda {
    /// Le calendrier nommé, parmi ceux servis.
    async fn calendar_named(&self, name: &str) -> Result<Calendar, String> {
        let list = self.calendars().await.map_err(|e| e.to_string())?;
        list.iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| {
                let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
                format!(
                    "calendrier inconnu : « {name} » (calendriers : {})",
                    names.join(", ")
                )
            })
    }

    /// L'objet qui porte ce `UID`, et son calendrier : dans `calendar` s'il est nommé,
    /// sinon dans tous.
    async fn locate(&self, args: &Value) -> Result<(Calendar, Resource), String> {
        let uid =
            str_arg(args, "uid").ok_or("`uid` est obligatoire (champ `uid` d'une lecture)")?;
        let calendars = match str_arg(args, "calendar") {
            Some(name) => vec![self.calendar_named(name).await?],
            None => self.calendars().await.map_err(|e| e.to_string())?,
        };
        for c in calendars {
            if let Some(r) = self
                .client
                .find(&c.url, uid)
                .await
                .map_err(|e| format!("calendrier « {} » : {e}", c.name))?
            {
                return Ok((c, r));
            }
        }
        Err(format!("aucun événement d'UID « {uid} »"))
    }

    pub(super) async fn event_create(&self, args: &Value) -> Result<Value, String> {
        let tz = self.timezone(args)?;
        let calendar = self
            .calendar_named(str_arg(args, "calendar").ok_or("`calendar` est obligatoire")?)
            .await?;
        str_arg(args, "summary").ok_or("`summary` est obligatoire : le titre")?;
        let start = Moment::parse(
            str_arg(args, "start").ok_or("`start` est obligatoire")?,
            tz,
            "start",
        )?;
        let end = str_arg(args, "end")
            .map(|e| Moment::parse(e, tz, "end"))
            .transpose()?;
        let (dtstart, dtend) = span(start, end)?;
        let now = (self.now)();
        let uid = new_uid(now);
        let mut ics = String::new();
        for l in [
            "BEGIN:VCALENDAR".to_string(),
            "VERSION:2.0".into(),
            "PRODID:-//Penelope//agenda-mcp//FR".into(),
            "BEGIN:VEVENT".into(),
            format!("UID:{uid}"),
            utc_prop("DTSTAMP", now),
            dtstart,
            dtend,
        ]
        .into_iter()
        .chain(text_props(args))
        .chain(["END:VEVENT".to_string(), "END:VCALENDAR".into()])
        {
            ics.push_str(&fold(&l));
        }
        let href = calendar
            .url
            .join(&format!("{}.ics", uid.replace('@', "-")))
            .map_err(|e| e.to_string())?;
        self.client
            .put(&href, &ics, None)
            .await
            .map_err(|e| e.to_string())?;
        Ok(success(
            format!("Événement créé dans « {} » (uid {uid}).", calendar.name),
            json!({"uid": uid, "calendar": calendar.name, "href": href.as_str()}),
        ))
    }

    pub(super) async fn event_update(&self, args: &Value) -> Result<Value, String> {
        let tz = self.timezone(args)?;
        let (calendar, resource) = self.locate(args).await?;
        let events = ical::events(&resource.ics);
        let Some(current) = events.first() else {
            return Err("objet iCalendar sans événement lisible".into());
        };
        if current.rrule.is_some() || events.len() > 1 {
            return Err(
                "série récurrente : sa modification n'est pas prise en charge ; la supprimer \
                 et la recréer, ou la modifier dans l'application d'agenda"
                    .into(),
            );
        }
        let mut replaced: Vec<&str> = Vec::new();
        let mut added = text_props(args);
        for (prop, key) in [
            ("SUMMARY", "summary"),
            ("LOCATION", "location"),
            ("DESCRIPTION", "description"),
        ] {
            if args.get(key).is_some() {
                replaced.push(prop);
            }
        }
        let start = str_arg(args, "start")
            .map(|s| Moment::parse(s, tz, "start"))
            .transpose()?;
        let end = str_arg(args, "end")
            .map(|e| Moment::parse(e, tz, "end"))
            .transpose()?;
        if start.is_some() || end.is_some() {
            let (start, end) = moments_of(current, start, end, self.settings.timezone);
            let (dtstart, dtend) = span(start, Some(end))?;
            replaced.extend(["DTSTART", "DTEND", "DURATION"]);
            added.extend([dtstart, dtend]);
        }
        if replaced.is_empty() {
            return Err(
                "rien à modifier : donner summary, location, description, start ou end".into(),
            );
        }
        added.push(utc_prop("DTSTAMP", (self.now)()));
        let ics = patch(&resource.ics, &replaced, &added);
        self.client
            .put(&resource.href, &ics, resource.etag.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        Ok(success(
            format!(
                "Événement « {} » modifié dans « {} ».",
                current.summary, calendar.name
            ),
            json!({"uid": current.uid, "calendar": calendar.name}),
        ))
    }

    pub(super) async fn event_delete(&self, args: &Value) -> Result<Value, String> {
        let (calendar, resource) = self.locate(args).await?;
        let events = ical::events(&resource.ics);
        let series = events.iter().any(|e| e.rrule.is_some());
        let summary = events
            .first()
            .map(|e| e.summary.clone())
            .unwrap_or_else(|| "(sans titre)".into());
        self.client
            .delete(&resource.href, resource.etag.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        Ok(success(
            format!(
                "Événement « {summary} » supprimé de « {} »{}.",
                calendar.name,
                if series { ", série entière" } else { "" }
            ),
            json!({"calendar": calendar.name, "series": series}),
        ))
    }
}

/// Début et fin complets d'une modification : ce qui n'est pas donné garde sa valeur,
/// et un début seul garde la durée.
fn moments_of(
    current: &ical::Event,
    start: Option<Moment>,
    end: Option<Moment>,
    default: Tz,
) -> (Moment, Moment) {
    let as_moment = |w: &When| match w {
        When::Date(d) => Moment::Day(*d),
        other => Moment::At(other.instant(default)),
    };
    let (old_start, old_end) = (as_moment(&current.start), as_moment(&current.end));
    // La fin d'une journée entière est exclue en iCalendar, incluse côté outil.
    let old_end = match old_end {
        Moment::Day(d) => Moment::Day(d - Duration::days(1)),
        m => m,
    };
    match (start, end) {
        (Some(s), Some(e)) => (s, e),
        (None, Some(e)) => (old_start, e),
        (Some(s), None) => {
            let e = match (old_start, old_end, s) {
                (Moment::At(a), Moment::At(b), Moment::At(n)) => Moment::At(n + (b - a)),
                (Moment::Day(a), Moment::Day(b), Moment::Day(n)) => Moment::Day(n + (b - a)),
                (_, _, other) => other,
            };
            (s, e)
        }
        (None, None) => (old_start, old_end),
    }
}

/// Les descripteurs des outils d'écriture.
pub fn descriptors(date: impl Fn(&str) -> Value, tz: &Value) -> Vec<Value> {
    let write = json!({
        "readOnlyHint": false, "destructiveHint": false,
        "idempotentHint": false, "openWorldHint": true
    });
    let destroy = json!({
        "readOnlyHint": false, "destructiveHint": true,
        "idempotentHint": true, "openWorldHint": true
    });
    let s = |d: &str| json!({"type": "string", "description": d});
    let uid = s("UID de l'événement, tel que rendu par une lecture (champ `uid`).");
    let calendar = s("Nom du calendrier (voir calendar_list).");
    let moment = |role: &str| {
        let mut v = date(role);
        v["description"] = json!(format!(
            "{role} : `AAAA-MM-JJ` (journée entière, fin incluse), `AAAA-MM-JJTHH:MM` (heure \
             locale du fuseau) ou RFC 3339."
        ));
        v
    };
    vec![
        json!({
            "name": EVENT_CREATE, "title": "Créer un événement",
            "description": "Crée un événement dans un calendrier. Sans fin : une heure, ou la \
                            journée pour une date.",
            "inputSchema": {"type": "object", "properties": {
                "calendar": calendar, "summary": s("Titre."), "start": moment("Début"),
                "end": moment("Fin"), "location": s("Lieu."), "description": s("Description."),
                "timezone": tz},
                "required": ["calendar", "summary", "start"], "additionalProperties": false},
            "annotations": write
        }),
        json!({
            "name": EVENT_UPDATE, "title": "Modifier un événement",
            "description": "Modifie le titre, le lieu, la description ou les horaires d'un \
                            événement non récurrent ; un début seul garde la durée. Refusé si \
                            l'événement a changé ailleurs depuis sa lecture.",
            "inputSchema": {"type": "object", "properties": {
                "uid": uid, "calendar": calendar, "summary": s("Nouveau titre."),
                "start": moment("Nouveau début"), "end": moment("Nouvelle fin"),
                "location": s("Nouveau lieu."), "description": s("Nouvelle description."),
                "timezone": tz},
                "required": ["uid"], "additionalProperties": false},
            "annotations": write
        }),
        json!({
            "name": EVENT_DELETE, "title": "Supprimer un événement",
            "description": "Supprime un événement ; une série récurrente l'est en entier.",
            "inputSchema": {"type": "object", "properties": {"uid": uid, "calendar": calendar},
                "required": ["uid"], "additionalProperties": false},
            "annotations": destroy
        }),
    ]
}
