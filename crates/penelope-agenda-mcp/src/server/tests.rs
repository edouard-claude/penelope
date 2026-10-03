use super::*;
use crate::caldav::fake::{FakeCalDav, Mode, PASSWORD, USER};
use crate::config::Settings;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::time::Duration;

const SIMPLE: &str = include_str!("../../tests/fixtures/simple.ics");
const ALLDAY: &str = include_str!("../../tests/fixtures/allday.ics");
const WEEKLY: &str = include_str!("../../tests/fixtures/weekly.ics");
const UTC_DURATION: &str = include_str!("../../tests/fixtures/utc_duration.ics");

/// Samedi 3 octobre 2026, 06:00 à La Réunion.
const NOW: &str = "2026-10-03T02:00:00Z";

async fn server(fake: &FakeCalDav, calendars: &str) -> Server {
    let settings = Settings {
        url: fake.url.clone(),
        user: USER.into(),
        password: PASSWORD.into(),
        timezone: chrono_tz::Indian::Reunion,
        calendars: calendars
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        timeout: Duration::from_secs(5),
    };
    let now: DateTime<Utc> = DateTime::parse_from_rfc3339(NOW)
        .unwrap()
        .with_timezone(&Utc);
    let agenda = Agenda::new(settings)
        .unwrap()
        .with_clock(Arc::new(move || now));
    Server::new(agenda)
}

async fn populated() -> (FakeCalDav, Server) {
    let fake = FakeCalDav::start().await;
    fake.add_calendar("perso", "Perso", &[SIMPLE, WEEKLY, UTC_DURATION]);
    fake.add_calendar("famille", "Famille", &[ALLDAY]);
    let s = server(&fake, "").await;
    (fake, s)
}

async fn call(s: &Server, id: u64, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    s.handle(&line).await.expect("une requête a une réponse")
}

async fn tool(s: &Server, name: &str, args: Value) -> Value {
    let resp = call(s, 7, "tools/call", json!({"name": name, "arguments": args})).await;
    assert!(resp.get("error").is_none(), "{resp}");
    resp["result"].clone()
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn initialize_negotiates_down_to_the_served_version() {
    let fake = FakeCalDav::start().await;
    let s = server(&fake, "").await;
    let r = call(
        &s,
        1,
        "initialize",
        json!({"protocolVersion": "2026-07-28"}),
    )
    .await;
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "penelope-agenda-mcp");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    let r = call(
        &s,
        2,
        "initialize",
        json!({"protocolVersion": "2024-11-05"}),
    )
    .await;
    assert_eq!(
        r["result"]["protocolVersion"], "2024-11-05",
        "une version plus ancienne est servie telle quelle"
    );
    let r = call(&s, 3, "initialize", json!({})).await;
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
}

#[tokio::test]
async fn discover_is_unknown_notifications_are_silent_and_garbage_is_a_parse_error() {
    let fake = FakeCalDav::start().await;
    let s = server(&fake, "").await;
    let r = call(&s, 1, "server/discover", json!({"_meta": {}})).await;
    assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    assert!(
        s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none()
    );
    assert!(
        s.handle(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#)
            .await
            .is_none(),
        "une réponse du client n'appelle rien"
    );
    let r = s.handle("pas du json").await.unwrap();
    assert_eq!(r["error"]["code"], PARSE_ERROR);
    assert!(r["id"].is_null());
    let r = call(&s, 4, "ping", json!({})).await;
    assert_eq!(r["result"], json!({}));
}

#[tokio::test]
async fn tools_list_has_four_read_only_tools() {
    let fake = FakeCalDav::start().await;
    let s = server(&fake, "").await;
    let r = call(&s, 1, "tools/list", json!({})).await;
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "calendar_list",
            "events_today",
            "events_range",
            "event_search"
        ]
    );
    for t in tools {
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{}", t["name"]);
        assert_eq!(t["annotations"]["destructiveHint"], false);
        assert!(t["inputSchema"]["type"] == "object");
        assert!(t["outputSchema"].is_object());
        assert!(t["description"].as_str().unwrap().len() > 20);
    }
    assert_eq!(
        fake.requests().len(),
        0,
        "lister les outils ne touche pas le serveur"
    );
}

#[tokio::test]
async fn events_today_mixes_calendars_in_the_requested_timezone() {
    let (fake, s) = populated().await;
    let r = tool(&s, "events_today", json!({"timezone": "Indian/Reunion"})).await;
    let t = text(&r);
    assert!(
        t.starts_with("Aujourd'hui, samedi 3 octobre 2026 (Indian/Reunion) : 5 événement(s)"),
        "{t}"
    );
    let lines: Vec<&str> = t.lines().skip(1).collect();
    assert_eq!(
        lines,
        vec![
            "- journée Fête · Famille",
            "- 11:00–11:45 Dentiste, contrôle annuel (docteur Martin) · Perso · 12 rue des Lilas, Paris",
            "- 13:00 Relance horaire · Perso",
            "- 16:00–17:30 Visio équipe · Perso",
            "- 20:00–20:30 Appel maman · Perso",
        ]
    );
    let sc = &r["structuredContent"];
    assert_eq!(sc["date"], "2026-10-03");
    assert_eq!(sc["timezone"], "Indian/Reunion");
    assert_eq!(sc["count"], 5);
    let events = sc["events"].as_array().unwrap();
    assert_eq!(events[0]["all_day"], true);
    assert_eq!(events[0]["start"], "2026-10-03");
    assert_eq!(events[0]["end"], "2026-10-03");
    assert_eq!(events[1]["start"], "2026-10-03T11:00:00+04:00");
    assert_eq!(events[1]["end"], "2026-10-03T11:45:00+04:00");
    assert_eq!(events[1]["calendar"], "Perso");
    assert_eq!(events[1]["location"], "12 rue des Lilas, Paris");
    assert!(
        events[1].get("description").is_none(),
        "la description reste chez le serveur"
    );
    // Un REPORT par calendrier, sur la journée de La Réunion.
    let reports: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "REPORT")
        .map(|r| r.body)
        .collect();
    assert_eq!(reports.len(), 2);
    assert!(
        reports[0].contains("start=\"20261002T200000Z\" end=\"20261003T200000Z\""),
        "{}",
        reports[0]
    );

    // Le même jour vu de Paris : la Fête reste, l'appel de 20 h à La Réunion est à 18 h.
    let r = tool(&s, "events_today", json!({"timezone": "Europe/Paris"})).await;
    let t = text(&r);
    assert!(t.contains("- 09:00–09:45 Dentiste"), "{t}");
    assert!(t.contains("- 18:00–18:30 Appel maman"), "{t}");
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|r| r.method == "PROPFIND")
            .count(),
        3,
        "la liste des calendriers est en cache"
    );
}

#[tokio::test]
async fn events_range_and_search_read_their_arguments() {
    let (_fake, s) = populated().await;
    let r = tool(
        &s,
        "events_range",
        json!({"start": "2026-09-01", "end": "2026-09-30"}),
    )
    .await;
    let sc = &r["structuredContent"];
    assert_eq!(sc["count"], 6, "{}", text(&r));
    assert!(
        text(&r)
            .starts_with("6 événement(s) du 01/09/2026 00:00 au 01/10/2026 00:00 (Indian/Reunion)")
    );
    assert!(text(&r).contains("- 18:00–19:00 Piano (déplacé) · Perso"));

    let r = tool(
        &s,
        "events_range",
        json!({"start": "2026-10-05", "end": "2026-10-06"}),
    )
    .await;
    assert_eq!(
        text(&r).lines().nth(1),
        Some("- 05/10 → 07/10 Congé · Famille")
    );
    assert_eq!(r["structuredContent"]["events"][0]["end"], "2026-10-07");

    let r = tool(
        &s,
        "events_range",
        json!({"start": "2026-10-03T06:00:00+04:00", "end": "2026-10-03T14:00:00+04:00"}),
    )
    .await;
    let names: Vec<&str> = r["structuredContent"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["summary"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "Fête",
            "Dentiste, contrôle annuel (docteur Martin)",
            "Relance horaire"
        ]
    );

    let r = tool(&s, "event_search", json!({"query": "piano"})).await;
    let sc = &r["structuredContent"];
    assert_eq!(sc["total"], 6, "{}", text(&r));
    assert_eq!(sc["query"], "piano");
    assert!(
        text(&r).starts_with("6 événement(s) pour « piano » du 03/09/2026 au 03/09/2027"),
        "{}",
        text(&r)
    );

    let r = tool(
        &s,
        "event_search",
        json!({"query": "VITALE", "from": "2026-10-01", "to": "2026-10-31"}),
    )
    .await;
    assert_eq!(
        r["structuredContent"]["count"], 1,
        "la description est cherchée, sans la casse"
    );
    let r = tool(&s, "event_search", json!({"query": "introuvable"})).await;
    assert_eq!(r["structuredContent"]["count"], 0);
    assert!(text(&r).starts_with("0 événement(s)"));
}

#[tokio::test]
async fn bad_arguments_are_tool_errors_and_an_unknown_tool_a_protocol_error() {
    let (_fake, s) = populated().await;
    let cases = [
        (
            "events_today",
            json!({"timezone": "Mars/Olympus"}),
            "fuseau inconnu",
        ),
        (
            "events_range",
            json!({"start": "hier", "end": "2026-10-10"}),
            "`start` illisible",
        ),
        (
            "events_range",
            json!({"start": "2026-10-10"}),
            "`end` est obligatoire",
        ),
        (
            "events_range",
            json!({"start": "2026-10-10", "end": "2026-10-01"}),
            "la fin doit suivre",
        ),
        (
            "events_range",
            json!({"start": "2025-01-01", "end": "2026-12-31"}),
            "plage trop longue",
        ),
        ("event_search", json!({}), "`query` est obligatoire"),
    ];
    for (name, args, expected) in cases {
        let r = tool(&s, name, args).await;
        assert_eq!(r["isError"], true, "{name}");
        assert!(text(&r).contains(expected), "{name} : {}", text(&r));
    }
    let r = call(
        &s,
        1,
        "tools/call",
        json!({"name": "events_delete", "arguments": {}}),
    )
    .await;
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("events_today")
    );
    let r = call(&s, 2, "tools/call", json!({"arguments": {}})).await;
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
}

#[tokio::test]
async fn a_refused_login_is_a_readable_tool_error_without_the_password() {
    let fake = FakeCalDav::start().await;
    fake.set_mode(Mode::Unauthorized);
    let s = server(&fake, "").await;
    let r = tool(&s, "calendar_list", json!({})).await;
    assert_eq!(r["isError"], true);
    let t = text(&r);
    assert!(
        t.contains("identifiants refusés") && t.contains("HTTP 401"),
        "{t}"
    );
    assert!(!t.contains(PASSWORD));
    let r = tool(&s, "events_today", json!({})).await;
    assert_eq!(r["isError"], true);
}

#[tokio::test]
async fn calendars_can_be_restricted_by_name() {
    let fake = FakeCalDav::start().await;
    fake.add_calendar("perso", "Perso", &[SIMPLE]);
    fake.add_calendar("famille", "Famille", &[ALLDAY]);
    let s = server(&fake, "famille").await;
    let r = tool(&s, "calendar_list", json!({})).await;
    assert_eq!(text(&r), "1 calendrier(s) :\n- Famille\n");
    assert_eq!(r["structuredContent"]["calendars"][0]["name"], "Famille");
    let r = tool(&s, "events_today", json!({})).await;
    assert_eq!(r["structuredContent"]["count"], 1);
    assert_eq!(r["structuredContent"]["events"][0]["summary"], "Fête");

    let empty = FakeCalDav::start().await;
    let s = server(&empty, "").await;
    let r = tool(&s, "calendar_list", json!({})).await;
    assert_eq!(text(&r), "Aucun calendrier d'événements lisible.");
    let r = tool(&s, "events_today", json!({})).await;
    assert_eq!(
        text(&r),
        "Aucun événement aujourd'hui (samedi 3 octobre 2026, Indian/Reunion)."
    );
    assert_eq!(r["structuredContent"]["events"], json!([]));
}
