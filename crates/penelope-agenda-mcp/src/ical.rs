//! iCalendar (RFC 5545), la part qu'un agenda lu demande : dépliage des lignes,
//! propriétés et paramètres, `VEVENT` avec `DTSTART`/`DTEND` (date, heure flottante, UTC,
//! `TZID`), `DURATION`, `RRULE` (voir `rrule`), `EXDATE`, `RECURRENCE-ID` et `STATUS`.
//! Écrit à la main (#295) : aucune crate raisonnable ne couvrait les récurrences sans
//! tirer son propre modèle de dates.

mod rrule;
#[cfg(test)]
mod tests;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
pub use rrule::{Freq, RRule};
use std::collections::{BTreeMap, BTreeSet};

// ------------------------------------------------------------------ syntaxe

/// Une propriété : `NAME;PARAM=valeur:valeur`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Property {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Un composant (`VCALENDAR`, `VEVENT`, `VTIMEZONE`…) et ses enfants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub props: Vec<Property>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.props.iter().find(|p| p.name == name)
    }

    pub fn props<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> + 'a {
        self.props.iter().filter(move |p| p.name == name)
    }

    /// Tous les `VEVENT`, à toute profondeur.
    pub fn events(&self) -> Vec<&Component> {
        let mut out = Vec::new();
        if self.name == "VEVENT" {
            out.push(self);
        }
        for c in &self.children {
            out.extend(c.events());
        }
        out
    }
}

/// Déplie les lignes : une ligne qui commence par une espace ou une tabulation continue
/// la précédente (§3.1). Les fins de ligne CRLF et LF sont acceptées.
pub fn unfold(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t'))
            && let Some(last) = out.last_mut()
        {
            last.push_str(rest);
        } else if !line.is_empty() {
            out.push(line.to_string());
        }
    }
    out
}

/// Coupe sur `sep` hors des valeurs entre guillemets (`CN="Durand, Anne"`).
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c == sep && !in_quotes => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Lit une ligne dépliée. Les noms de propriété et de paramètre sont rendus en majuscules,
/// les valeurs de paramètre sans leurs guillemets.
pub fn parse_line(line: &str) -> Option<Property> {
    let mut in_quotes = false;
    let colon = line.char_indices().find_map(|(i, c)| match c {
        '"' => {
            in_quotes = !in_quotes;
            None
        }
        ':' if !in_quotes => Some(i),
        _ => None,
    })?;
    let (head, value) = line.split_at(colon);
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_string(),
            ))
        })
        .collect();
    Some(Property {
        name,
        params,
        value: value[1..].to_string(),
    })
}

/// Les composants de premier niveau d'un flux iCalendar. Un composant resté ouvert
/// (fichier tronqué) est gardé tel quel.
pub fn parse(raw: &str) -> Vec<Component> {
    fn close(stack: &mut Vec<Component>, out: &mut Vec<Component>) {
        if let Some(c) = stack.pop() {
            match stack.last_mut() {
                Some(parent) => parent.children.push(c),
                None => out.push(c),
            }
        }
    }
    let mut stack: Vec<Component> = Vec::new();
    let mut out = Vec::new();
    for line in unfold(raw) {
        let Some(p) = parse_line(&line) else { continue };
        match p.name.as_str() {
            "BEGIN" => stack.push(Component {
                name: p.value.trim().to_ascii_uppercase(),
                ..Default::default()
            }),
            "END" => close(&mut stack, &mut out),
            _ => {
                if let Some(cur) = stack.last_mut() {
                    cur.props.push(p);
                }
            }
        }
    }
    while !stack.is_empty() {
        close(&mut stack, &mut out);
    }
    out
}

/// Texte iCalendar : `\n` et `\N` donnent un saut de ligne, `\,` `\;` `\\` le caractère.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// `PT1H30M`, `P1D`, `-P1W`, `P1DT12H` : une durée (§3.3.6).
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (negative, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut digits = String::new();
    let mut in_time = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        if c == 'T' {
            in_time = true;
            continue;
        }
        let n: i64 = digits.parse().ok()?;
        digits.clear();
        total += match c {
            'W' => Duration::weeks(n),
            'D' => Duration::days(n),
            'H' if in_time => Duration::hours(n),
            'M' if in_time => Duration::minutes(n),
            'S' if in_time => Duration::seconds(n),
            _ => return None,
        };
    }
    if !digits.is_empty() {
        return None;
    }
    Some(if negative { -total } else { total })
}

// ------------------------------------------------------------------ dates

/// Une valeur `DATE` ou `DATE-TIME`, telle qu'écrite : la nature compte pour les
/// récurrences (on avance en heure locale, pas en instants) et pour le rendu (une journée
/// entière n'a pas d'heure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum When {
    Date(NaiveDate),
    /// Heure sans fuseau : lue dans le fuseau par défaut.
    Floating(NaiveDateTime),
    Utc(DateTime<Utc>),
    Zoned {
        local: NaiveDateTime,
        tzid: String,
    },
}

fn parse_naive(v: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M"))
        .ok()
}

/// Le fuseau d'un `TZID` : un nom Olson, parfois précédé d'un chemin
/// (`/freeassociation.sourceforge.net/Europe/Paris`). `None` pour un nom inconnu (Windows,
/// « Romance Standard Time »).
pub fn tz_named(tzid: &str) -> Option<Tz> {
    let t = tzid.trim().trim_start_matches('/');
    if let Ok(tz) = t.parse::<Tz>() {
        return Some(tz);
    }
    let parts: Vec<&str> = t.split('/').collect();
    for n in (1..=2).rev() {
        if parts.len() > n
            && let Ok(tz) = parts[parts.len() - n..].join("/").parse::<Tz>()
        {
            return Some(tz);
        }
    }
    None
}

/// Heure locale vers instant. Une heure ambiguë (retour à l'heure d'hiver) prend la
/// première ; une heure inexistante (passage à l'heure d'été) avance d'une heure.
fn local_to_utc(tz: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&local) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
            t.with_timezone(&Utc)
        }
        chrono::LocalResult::None => tz
            .from_local_datetime(&(local + Duration::hours(1)))
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|| Utc.from_utc_datetime(&local)),
    }
}

impl When {
    /// Lit une valeur avec ses paramètres `TZID` et `VALUE`.
    pub fn parse(value: &str, tzid: Option<&str>, value_type: Option<&str>) -> Option<When> {
        let v = value.trim();
        if value_type == Some("DATE") || (v.len() == 8 && !v.contains('T')) {
            return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(When::Date);
        }
        if let Some(utc) = v.strip_suffix('Z') {
            return Some(When::Utc(Utc.from_utc_datetime(&parse_naive(utc)?)));
        }
        let local = parse_naive(v)?;
        Some(match tzid {
            Some(t) => When::Zoned {
                local,
                tzid: t.to_string(),
            },
            None => When::Floating(local),
        })
    }

    pub fn is_date(&self) -> bool {
        matches!(self, When::Date(_))
    }

    /// Le fuseau dans lequel la valeur est écrite : son `TZID` s'il est connu, UTC pour
    /// un instant, `default` sinon.
    pub fn tz(&self, default: Tz) -> Tz {
        match self {
            When::Utc(_) => Tz::UTC,
            When::Zoned { tzid, .. } => tz_named(tzid).unwrap_or(default),
            When::Date(_) | When::Floating(_) => default,
        }
    }

    /// L'instant. Une date vaut minuit dans `default` ; une heure flottante y est lue ;
    /// un `TZID` inconnu y est rabattu.
    pub fn instant(&self, default: Tz) -> DateTime<Utc> {
        match self {
            When::Utc(t) => *t,
            When::Date(d) => local_to_utc(default, d.and_time(NaiveTime::MIN)),
            When::Floating(n) => local_to_utc(default, *n),
            When::Zoned { local, tzid } => local_to_utc(tz_named(tzid).unwrap_or(default), *local),
        }
    }

    /// L'heure locale dans `tz` : la clé qui compare un `EXDATE` ou un `RECURRENCE-ID` à
    /// une occurrence, et le point de départ d'une récurrence.
    pub fn local_in(&self, tz: Tz, default: Tz) -> NaiveDateTime {
        match self {
            When::Date(d) => d.and_time(NaiveTime::MIN),
            When::Floating(n) => *n,
            When::Utc(_) | When::Zoned { .. } => {
                self.instant(default).with_timezone(&tz).naive_local()
            }
        }
    }

    /// La même nature de valeur, à une autre heure locale.
    pub fn with_local(&self, local: NaiveDateTime) -> When {
        match self {
            When::Date(_) => When::Date(local.date()),
            When::Floating(_) => When::Floating(local),
            When::Utc(_) => When::Utc(Utc.from_utc_datetime(&local)),
            When::Zoned { tzid, .. } => When::Zoned {
                local,
                tzid: tzid.clone(),
            },
        }
    }

    /// La valeur décalée de `d`, de même nature (une date avance par jours entiers).
    pub fn shifted(&self, d: Duration) -> When {
        match self {
            When::Date(date) => When::Date(*date + Duration::days(d.num_days())),
            When::Floating(n) => When::Floating(*n + d),
            When::Utc(t) => When::Utc(*t + d),
            When::Zoned { local, tzid } => When::Zoned {
                local: *local + d,
                tzid: tzid.clone(),
            },
        }
    }
}

// ------------------------------------------------------------------ événements

/// Un `VEVENT` lu : maître d'une série (`rrule`), instance surchargée (`recurrence_id`)
/// ou événement simple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: When,
    pub end: When,
    pub rrule: Option<RRule>,
    pub exdates: Vec<When>,
    pub recurrence_id: Option<When>,
    pub cancelled: bool,
}

impl Event {
    /// Lit un `VEVENT` ; `None` sans `DTSTART` lisible, ou pour un autre composant.
    pub fn from_component(c: &Component) -> Option<Event> {
        if c.name != "VEVENT" {
            return None;
        }
        let when = |p: &Property| When::parse(&p.value, p.param("TZID"), p.param("VALUE"));
        let start = when(c.prop("DTSTART")?)?;
        let end = match c.prop("DTEND").and_then(when) {
            Some(e) => e,
            None => match c.prop("DURATION").and_then(|p| parse_duration(&p.value)) {
                Some(d) => start.shifted(d),
                // Sans fin : une date vaut la journée, une heure un instant (§3.6.1).
                None if start.is_date() => start.shifted(Duration::days(1)),
                None => start.clone(),
            },
        };
        let text = |name: &str| {
            c.prop(name)
                .map(|p| unescape(&p.value).trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let exdates = c
            .props("EXDATE")
            .flat_map(|p| {
                p.value
                    .split(',')
                    .filter_map(|v| When::parse(v, p.param("TZID"), p.param("VALUE")))
                    .collect::<Vec<_>>()
            })
            .collect();
        Some(Event {
            uid: c
                .prop("UID")
                .map(|p| p.value.trim().to_string())
                .unwrap_or_default(),
            summary: text("SUMMARY").unwrap_or_else(|| "(sans titre)".into()),
            location: text("LOCATION"),
            description: text("DESCRIPTION"),
            start,
            end,
            rrule: c.prop("RRULE").and_then(|p| RRule::parse(&p.value)),
            exdates,
            recurrence_id: c.prop("RECURRENCE-ID").and_then(when),
            cancelled: c
                .prop("STATUS")
                .is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED")),
        })
    }

    pub fn all_day(&self) -> bool {
        self.start.is_date()
    }
}

/// Tous les événements d'un flux iCalendar.
pub fn events(raw: &str) -> Vec<Event> {
    parse(raw)
        .iter()
        .flat_map(|c| c.events())
        .filter_map(Event::from_component)
        .collect()
}

// ------------------------------------------------------------------ occurrences

/// Une occurrence : un événement simple, ou une instance d'une série.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: When,
    pub end: When,
    pub start_utc: DateTime<Utc>,
    pub end_utc: DateTime<Utc>,
}

impl Occurrence {
    pub fn all_day(&self) -> bool {
        self.start.is_date()
    }
}

/// Une plage d'instants, fin exclue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    /// Vrai si `[s, e)` touche la fenêtre ; un événement sans durée compte s'il commence
    /// dedans.
    pub fn overlaps(&self, s: DateTime<Utc>, e: DateTime<Utc>) -> bool {
        s < self.end && (e > self.start || s >= self.start)
    }
}

/// Les occurrences de `events` dans la fenêtre : séries développées, `EXDATE` retirées,
/// instances surchargées (`RECURRENCE-ID`) remplacées, annulées écartées ; triées par
/// début. `default` : le fuseau des dates, des heures flottantes et des `TZID` inconnus.
pub fn occurrences(events: &[Event], window: Window, default: Tz) -> Vec<Occurrence> {
    let mut overrides: BTreeMap<&str, Vec<&Event>> = BTreeMap::new();
    for e in events.iter().filter(|e| e.recurrence_id.is_some()) {
        overrides.entry(e.uid.as_str()).or_default().push(e);
    }
    let mut out = Vec::new();
    for e in events
        .iter()
        .filter(|e| e.recurrence_id.is_none() && !e.cancelled)
    {
        let tz = e.start.tz(default);
        // Les instances retirées : EXDATE, et celles qu'une surcharge remplace.
        let skip: BTreeSet<NaiveDateTime> = e
            .exdates
            .iter()
            .chain(
                overrides
                    .get(e.uid.as_str())
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o.recurrence_id.as_ref()),
            )
            .map(|w| w.local_in(tz, default))
            .collect();
        for (start, end) in instances(e, &window, tz, default) {
            if skip.contains(&start.local_in(tz, default)) {
                continue;
            }
            push_if_overlapping(&mut out, e, start, end, &window, default);
        }
    }
    for o in overrides.values().flatten().filter(|o| !o.cancelled) {
        push_if_overlapping(
            &mut out,
            o,
            o.start.clone(),
            o.end.clone(),
            &window,
            default,
        );
    }
    out.sort_by(|a, b| {
        a.start_utc
            .cmp(&b.start_utc)
            .then_with(|| a.summary.cmp(&b.summary))
    });
    out
}

/// Les débuts et fins d'un événement : lui seul, ou sa série jusqu'à la fin de la fenêtre.
fn instances(e: &Event, window: &Window, tz: Tz, default: Tz) -> Vec<(When, When)> {
    let Some(rule) = &e.rrule else {
        return vec![(e.start.clone(), e.end.clone())];
    };
    let first = e.start.local_in(tz, default);
    // Durée à l'heure murale : un cours d'une heure dure une heure le jour du changement
    // d'heure aussi.
    let length = e.end.local_in(tz, default) - first;
    // Au-delà de la fin de la fenêtre plus un jour de marge, rien ne peut la recouvrir.
    let limit = window.end.with_timezone(&tz).naive_local() + Duration::days(1);
    let until = rule.until.as_ref().map(|u| u.local_in(tz, default));
    rule.starts(first, limit, until)
        .into_iter()
        .map(|s| {
            let start = e.start.with_local(s);
            let end = start.shifted(length);
            (start, end)
        })
        .collect()
}

fn push_if_overlapping(
    out: &mut Vec<Occurrence>,
    e: &Event,
    start: When,
    end: When,
    window: &Window,
    default: Tz,
) {
    let (s, en) = (start.instant(default), end.instant(default));
    if !window.overlaps(s, en) {
        return;
    }
    out.push(Occurrence {
        uid: e.uid.clone(),
        summary: e.summary.clone(),
        location: e.location.clone(),
        description: e.description.clone(),
        start,
        end,
        start_utc: s,
        end_utc: en,
    });
}
