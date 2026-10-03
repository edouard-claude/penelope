use super::*;
use chrono::Datelike;
use chrono_tz::Indian::Reunion;

const SIMPLE: &str = include_str!("../../tests/fixtures/simple.ics");
const ALLDAY: &str = include_str!("../../tests/fixtures/allday.ics");
const WEEKLY: &str = include_str!("../../tests/fixtures/weekly.ics");
const DAILY_UNTIL: &str = include_str!("../../tests/fixtures/daily_until.ics");
const YEARLY: &str = include_str!("../../tests/fixtures/yearly.ics");
const MONTHLY: &str = include_str!("../../tests/fixtures/monthly.ics");
const UTC_DURATION: &str = include_str!("../../tests/fixtures/utc_duration.ics");

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// Une fenêtre de jours entiers à La Réunion, fin exclue.
fn days(from: &str, to: &str) -> Window {
    let local = |d: &str| {
        Reunion
            .from_local_datetime(
                &NaiveDate::parse_from_str(d, "%Y-%m-%d")
                    .unwrap()
                    .and_time(NaiveTime::MIN),
            )
            .unwrap()
            .with_timezone(&Utc)
    };
    Window {
        start: local(from),
        end: local(to),
    }
}

fn starts_local(occ: &[Occurrence]) -> Vec<String> {
    occ.iter()
        .map(|o| {
            o.start_utc
                .with_timezone(&Reunion)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .collect()
}

#[test]
fn lines_are_unfolded_and_properties_split() {
    let lines = unfold("A:1\r\nB;X=\"a:b\":deux\r\n  suite\r\n\tencore\r\nC:3\n");
    assert_eq!(lines, vec!["A:1", "B;X=\"a:b\":deux suiteencore", "C:3"]);
    let p = parse_line("ATTENDEE;CN=\"Durand, Anne\";ROLE=REQ-PARTICIPANT:mailto:a@b.fr").unwrap();
    assert_eq!(p.name, "ATTENDEE");
    assert_eq!(p.param("CN"), Some("Durand, Anne"));
    assert_eq!(p.param("ROLE"), Some("REQ-PARTICIPANT"));
    assert_eq!(p.value, "mailto:a@b.fr");
    let p = parse_line("dtstart;tzid=Europe/Paris:20261003T090000").unwrap();
    assert_eq!(p.name, "DTSTART");
    assert_eq!(p.param("TZID"), Some("Europe/Paris"));
    assert!(parse_line("pas de deux-points").is_none());
    assert_eq!(unescape("a\\, b\\; c\\nd\\Ne\\\\"), "a, b; c\nd\ne\\");
}

#[test]
fn components_nest_and_events_are_found_at_any_depth() {
    let cals = parse(SIMPLE);
    assert_eq!(cals.len(), 1);
    assert_eq!(cals[0].name, "VCALENDAR");
    assert_eq!(cals[0].children.len(), 2, "VTIMEZONE et VEVENT");
    let events = cals[0].events();
    assert_eq!(events.len(), 1);
    // Le RRULE du VTIMEZONE ne contamine pas l'événement.
    assert!(events[0].prop("RRULE").is_none());
    // Un fichier tronqué garde ce qu'il a ouvert.
    let cut = parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nDTSTART:20261003T090000Z\n");
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].events().len(), 1);
}

#[test]
fn a_timed_event_with_tzid_is_an_instant() {
    let ev = events(SIMPLE);
    assert_eq!(ev.len(), 1);
    let e = &ev[0];
    assert_eq!(e.uid, "dentiste-2026-10-03");
    assert_eq!(e.summary, "Dentiste, contrôle annuel (docteur Martin)");
    assert_eq!(e.location.as_deref(), Some("12 rue des Lilas, Paris"));
    assert_eq!(
        e.description.as_deref(),
        Some("Apporter la carte vitale.\nPrendre le dossier.")
    );
    assert!(!e.all_day());
    // 09:00 à Paris (UTC+2 en octobre) : 07:00Z, soit 11:00 à La Réunion.
    assert_eq!(e.start.instant(Reunion), utc("2026-10-03T07:00:00Z"));
    assert_eq!(e.end.instant(Reunion), utc("2026-10-03T07:45:00Z"));
    let occ = occurrences(&ev, days("2026-10-03", "2026-10-04"), Reunion);
    assert_eq!(starts_local(&occ), vec!["2026-10-03 11:00"]);
    assert!(occurrences(&ev, days("2026-10-04", "2026-10-05"), Reunion).is_empty());
}

#[test]
fn all_day_events_span_their_dates() {
    let ev = events(ALLDAY);
    assert_eq!(ev.len(), 2);
    let fete = ev.iter().find(|e| e.uid == "fete-3-octobre").unwrap();
    assert!(fete.all_day());
    assert_eq!(
        fete.end,
        When::Date(NaiveDate::from_ymd_opt(2026, 10, 4).unwrap()),
        "sans DTEND, une date vaut la journée"
    );
    let today = occurrences(&ev, days("2026-10-03", "2026-10-04"), Reunion);
    assert_eq!(today.len(), 1);
    assert_eq!(today[0].summary, "Fête");
    assert!(today[0].all_day());
    // Le congé du 5 au 7 (DTEND exclu) couvre le 6, pas le 8.
    let sixth = occurrences(&ev, days("2026-10-06", "2026-10-07"), Reunion);
    assert_eq!(sixth.len(), 1);
    assert_eq!(sixth[0].summary, "Congé");
    assert!(occurrences(&ev, days("2026-10-08", "2026-10-09"), Reunion).is_empty());
}

#[test]
fn weekly_rule_expands_with_byday_exdate_count_and_overrides() {
    let ev = events(WEEKLY);
    assert_eq!(ev.len(), 3, "le maître et deux surcharges");
    let occ = occurrences(&ev, days("2026-09-01", "2026-10-01"), Reunion);
    assert_eq!(
        starts_local(&occ),
        vec![
            "2026-09-07 17:00",
            "2026-09-09 17:00",
            "2026-09-14 17:00",
            // le 16 : EXDATE ; le 21 : déplacé au 22 à 18 h ; le 23 : annulé
            "2026-09-22 18:00",
            "2026-09-28 17:00",
            "2026-09-30 17:00",
        ]
    );
    let moved = occ.iter().find(|o| o.summary == "Piano (déplacé)").unwrap();
    assert_eq!(moved.end_utc - moved.start_utc, Duration::hours(1));
    // COUNT=8 compte les instances générées, EXDATE comprise : rien en octobre.
    assert!(occurrences(&ev, days("2026-10-01", "2026-11-01"), Reunion).is_empty());
}

#[test]
fn daily_until_stops_at_the_bound_given_in_utc() {
    let ev = events(DAILY_UNTIL);
    let occ = occurrences(&ev, days("2026-09-25", "2026-10-20"), Reunion);
    let paris: Vec<String> = occ
        .iter()
        .map(|o| {
            o.start_utc
                .with_timezone(&chrono_tz::Europe::Paris)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .collect();
    assert_eq!(
        paris,
        vec![
            "10-01 08:30",
            "10-03 08:30",
            "10-05 08:30",
            "10-07 08:30",
            "10-09 08:30"
        ],
        "UNTIL 06:30Z vaut 08:30 à Paris : le 9 est compris, le 11 non"
    );
}

#[test]
fn a_yearly_all_day_birthday_falls_every_year() {
    let ev = events(YEARLY);
    let occ = occurrences(&ev, days("2026-04-01", "2026-05-01"), Reunion);
    assert_eq!(occ.len(), 1);
    assert_eq!(occ[0].summary, "Anniversaire de Paul");
    assert_eq!(
        occ[0].start,
        When::Date(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap())
    );
    assert!(occ[0].all_day());
    assert!(occurrences(&ev, days("2026-05-01", "2026-06-01"), Reunion).is_empty());
}

#[test]
fn monthly_by_month_day_and_last_friday() {
    let ev = events(MONTHLY);
    let occ = occurrences(&ev, days("2026-10-01", "2026-11-01"), Reunion);
    let names: Vec<(&str, String)> = occ
        .iter()
        .map(|o| {
            (
                o.summary.as_str(),
                o.start_utc
                    .with_timezone(&Reunion)
                    .format("%Y-%m-%d")
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("Loyer", "2026-10-15".to_string()),
            ("Apéro d'équipe", "2026-10-30".to_string())
        ]
    );
    // Février 2026 : le 15 existe, le dernier vendredi est le 27.
    let feb = occurrences(&ev, days("2026-02-01", "2026-03-01"), Reunion);
    assert_eq!(
        starts_local(&feb),
        vec!["2026-02-15 00:00", "2026-02-27 18:30"]
    );
}

#[test]
fn utc_duration_floating_and_unknown_frequency() {
    let ev = events(UTC_DURATION);
    assert_eq!(ev.len(), 3);
    let occ = occurrences(&ev, days("2026-10-03", "2026-10-04"), Reunion);
    let found: Vec<(String, String)> = occ
        .iter()
        .map(|o| {
            (
                o.summary.clone(),
                format!(
                    "{}–{}",
                    o.start_utc.with_timezone(&Reunion).format("%H:%M"),
                    o.end_utc.with_timezone(&Reunion).format("%H:%M")
                ),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            // 09:00Z, fréquence horaire inconnue : une seule occurrence, sans durée
            ("Relance horaire".into(), "13:00–13:00".into()),
            // 12:00Z + PT1H30M
            ("Visio équipe".into(), "16:00–17:30".into()),
            // heure flottante, lue à La Réunion
            ("Appel maman".into(), "20:00–20:30".into()),
        ]
    );
    assert!(
        ev.iter()
            .find(|e| e.uid == "horaire")
            .unwrap()
            .rrule
            .is_none()
    );
}

#[test]
fn an_unknown_tzid_falls_back_to_the_default_timezone() {
    let raw = "BEGIN:VEVENT\nUID:w\nDTSTART;TZID=Romance Standard Time:20261003T090000\n\
               DTEND;TZID=Romance Standard Time:20261003T100000\nSUMMARY:Réunion\nEND:VEVENT\n";
    let ev = events(raw);
    assert_eq!(ev[0].start.tz(Reunion), Reunion);
    assert_eq!(ev[0].start.instant(Reunion), utc("2026-10-03T05:00:00Z"));
    assert_eq!(
        tz_named("/freeassociation.sourceforge.net/Europe/Paris"),
        Some(chrono_tz::Europe::Paris)
    );
    assert_eq!(tz_named("Indian/Reunion"), Some(Reunion));
    assert_eq!(tz_named("Mars/Olympus"), None);
}

#[test]
fn a_window_keeps_what_overlaps_it() {
    let raw = "BEGIN:VEVENT\nUID:nuit\nDTSTART;TZID=Indian/Reunion:20261003T230000\n\
               DTEND;TZID=Indian/Reunion:20261004T010000\nSUMMARY:Garde de nuit\nEND:VEVENT\n";
    let ev = events(raw);
    assert_eq!(
        occurrences(&ev, days("2026-10-03", "2026-10-04"), Reunion).len(),
        1
    );
    assert_eq!(
        occurrences(&ev, days("2026-10-04", "2026-10-05"), Reunion).len(),
        1
    );
    assert!(occurrences(&ev, days("2026-10-05", "2026-10-06"), Reunion).is_empty());
    let w = days("2026-10-03", "2026-10-04");
    // Sans durée, un événement compte s'il commence dans la fenêtre.
    assert!(w.overlaps(w.start, w.start));
    assert!(!w.overlaps(w.end, w.end));
}

#[test]
fn durations_are_read() {
    assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
    assert_eq!(parse_duration("P1D"), Some(Duration::days(1)));
    assert_eq!(parse_duration("-P1W"), Some(Duration::weeks(-1)));
    assert_eq!(parse_duration("P1DT12H"), Some(Duration::hours(36)));
    assert_eq!(parse_duration("PT15S"), Some(Duration::seconds(15)));
    assert_eq!(
        parse_duration("P1M"),
        None,
        "un mois hors du temps : refusé"
    );
    assert_eq!(parse_duration("1H"), None);
    assert_eq!(parse_duration("PT1"), None);
}

#[test]
fn rrule_parsing_covers_the_served_parts() {
    let r = RRule::parse("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,-1SU,2TU;WKST=SU;COUNT=5").unwrap();
    assert_eq!(r.freq, Freq::Weekly);
    assert_eq!(r.interval, 2);
    assert_eq!(r.count, Some(5));
    assert_eq!(r.wkst, chrono::Weekday::Sun);
    assert_eq!(
        r.by_day,
        vec![
            (None, chrono::Weekday::Mon),
            (Some(-1), chrono::Weekday::Sun),
            (Some(2), chrono::Weekday::Tue)
        ]
    );
    let r = RRule::parse("FREQ=YEARLY;BYMONTH=3,13;BYMONTHDAY=-1").unwrap();
    assert_eq!(r.by_month, vec![3], "un mois 13 est ignoré");
    assert_eq!(r.by_month_day, vec![-1]);
    assert!(RRule::parse("FREQ=MINUTELY").is_none());
    assert!(RRule::parse("INTERVAL=2").is_none(), "sans FREQ");
    assert!(RRule::parse("FREQ=DAILY;INTERVAL=0").is_none());
}

#[test]
fn rule_bounds_hold_without_until_or_count() {
    // Une série infinie est coupée à la fenêtre, pas à un garde-fou.
    let r = RRule::parse("FREQ=DAILY").unwrap();
    let first = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(8, 0, 0)
        .unwrap();
    let limit = NaiveDate::from_ymd_opt(2026, 1, 11)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    assert_eq!(r.starts(first, limit, None).len(), 10);
    // Tous les jours de semaine : BYDAY filtre une règle quotidienne.
    let r = RRule::parse("FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR").unwrap();
    let starts = r.starts(first, limit, None);
    // Du jeudi 1er au samedi 10 janvier 2026 : 1, 2, 5, 6, 7, 8, 9.
    assert_eq!(starts.len(), 7);
    assert!(
        starts
            .iter()
            .all(|s| s.weekday().num_days_from_monday() < 5)
    );
    // Le 31 de chaque mois saute les mois courts.
    let r = RRule::parse("FREQ=MONTHLY").unwrap();
    let first = NaiveDate::from_ymd_opt(2026, 1, 31)
        .unwrap()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let limit = NaiveDate::from_ymd_opt(2026, 6, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let months: Vec<u32> = r
        .starts(first, limit, None)
        .iter()
        .map(|s| s.month())
        .collect();
    assert_eq!(months, vec![1, 3, 5]);
}

#[test]
fn until_before_the_window_yields_nothing() {
    let raw = "BEGIN:VEVENT\nUID:fini\nDTSTART;VALUE=DATE:20250101\n\
               RRULE:FREQ=DAILY;UNTIL=20250110\nSUMMARY:Fini\nEND:VEVENT\n";
    let ev = events(raw);
    assert!(occurrences(&ev, days("2026-10-03", "2026-10-04"), Reunion).is_empty());
}
