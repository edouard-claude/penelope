use super::*;
use crate::caldav::fake::{FakeCalDav, PASSWORD, USER};
use crate::config::Settings;
use crate::tools::EVENTS_RANGE;
use std::sync::Arc;

const SIMPLE: &str = include_str!("../../../tests/fixtures/simple.ics");
const WEEKLY: &str = include_str!("../../../tests/fixtures/weekly.ics");

/// Samedi 3 octobre 2026, 06:00 à La Réunion.
fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-03T02:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

async fn agenda() -> (FakeCalDav, Agenda) {
    let fake = FakeCalDav::start().await;
    fake.add_calendar("perso", "Perso", &[SIMPLE, WEEKLY]);
    let settings = Settings {
        url: fake.url.clone(),
        user: USER.into(),
        password: PASSWORD.into(),
        token: String::new(),
        timezone: chrono_tz::Indian::Reunion,
        calendars: vec![],
        timeout: std::time::Duration::from_secs(5),
    };
    let a = Agenda::new(settings).unwrap().with_clock(Arc::new(now));
    (fake, a)
}

fn uid_of(ics: &str) -> String {
    ical::events(ics)[0].uid.clone()
}

/// Création : un objet neuf, heures en UTC, textes échappés, puis relu par la lecture.
#[tokio::test]
async fn an_event_is_created_then_read_back() {
    let (fake, a) = agenda().await;
    let r = a
        .call(
            EVENT_CREATE,
            &json!({"calendar": "perso", "summary": "Dentiste, contrôle", "start": "2026-10-05T09:00",
                    "end": "2026-10-05T09:45", "location": "12 rue des Lilas"}),
        )
        .await
        .unwrap();
    assert!(r.get("isError").is_none(), "{r}");
    let uid = r["structuredContent"]["uid"].as_str().unwrap().to_string();
    let objects = fake.objects("perso");
    assert_eq!(objects.len(), 3);
    let created = objects.values().find(|ics| ics.contains(&uid)).unwrap();
    // 09:00 à La Réunion = 05:00 UTC.
    assert!(created.contains("DTSTART:20261005T050000Z"), "{created}");
    assert!(created.contains("DTEND:20261005T054500Z"), "{created}");
    assert!(
        created.contains("SUMMARY:Dentiste\\, contrôle"),
        "{created}"
    );
    assert!(created.lines().all(|l| l.len() <= 76), "{created}");

    let read = a
        .call(
            EVENTS_RANGE,
            &json!({"start": "2026-10-05", "end": "2026-10-05"}),
        )
        .await
        .unwrap();
    assert!(
        read["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("09:00–09:45 Dentiste, contrôle"),
        "{read}"
    );
    let put = fake
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert!(put.path.starts_with("/calendars/anne/perso/"));
}

/// Une journée entière : la fin donnée est incluse, exclue en iCalendar.
#[tokio::test]
async fn an_all_day_event_spans_its_last_day() {
    let (fake, a) = agenda().await;
    let r = a
        .call(
            EVENT_CREATE,
            &json!({"calendar": "Perso", "summary": "Vacances", "start": "2026-10-19", "end": "2026-10-23"}),
        )
        .await
        .unwrap();
    let uid = r["structuredContent"]["uid"].as_str().unwrap().to_string();
    let ics = fake
        .objects("perso")
        .into_values()
        .find(|i| i.contains(&uid))
        .unwrap();
    assert!(ics.contains("DTSTART;VALUE=DATE:20261019"), "{ics}");
    assert!(ics.contains("DTEND;VALUE=DATE:20261024"), "{ics}");
}

/// Modification : un début seul garde la durée, la séquence avance, le reste demeure.
#[tokio::test]
async fn an_update_keeps_the_duration_and_bumps_the_sequence() {
    let (fake, a) = agenda().await;
    let uid = uid_of(SIMPLE);
    let before = ical::events(SIMPLE)[0].clone();
    let r = a
        .call(
            EVENT_UPDATE,
            &json!({"uid": uid, "start": "2026-10-10T14:00", "summary": "Nouveau titre"}),
        )
        .await
        .unwrap();
    assert!(r.get("isError").is_none(), "{r}");
    let ics = fake.objects("perso")["0.ics"].clone();
    let after = ical::events(&ics)[0].clone();
    assert_eq!(after.summary, "Nouveau titre");
    assert_eq!(after.location, before.location);
    let tz = chrono_tz::Indian::Reunion;
    assert_eq!(
        after.end.instant(tz) - after.start.instant(tz),
        before.end.instant(tz) - before.start.instant(tz)
    );
    assert!(ics.contains("SEQUENCE:1"), "{ics}");
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 1);
}

/// Un événement changé ailleurs entre la lecture et l'écriture n'est pas écrasé.
#[tokio::test]
async fn a_concurrent_change_refuses_the_write() {
    let (fake, a) = agenda().await;
    let uid = uid_of(SIMPLE);
    let (_, resource) = a.locate(&json!({"uid": uid})).await.unwrap();
    fake.touch("perso", "0.ics");
    let err = a
        .client
        .put(&resource.href, &resource.ics, resource.etag.as_deref())
        .await
        .unwrap_err();
    assert!(matches!(err, crate::caldav::Error::Conflict), "{err}");
    // La création, elle, ne remplace jamais un objet existant.
    let err = a.client.put(&resource.href, "x", None).await.unwrap_err();
    assert!(matches!(err, crate::caldav::Error::Conflict), "{err}");
}

/// Une série : pas de modification ; la suppression l'emporte entière, et le dit.
#[tokio::test]
async fn a_series_is_deleted_whole_but_never_updated() {
    let (fake, a) = agenda().await;
    let uid = uid_of(WEEKLY);
    let r = a
        .call(EVENT_UPDATE, &json!({"uid": uid, "summary": "x"}))
        .await
        .unwrap();
    assert_eq!(r["isError"], true);
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("série récurrente")
    );

    let r = a.call(EVENT_DELETE, &json!({"uid": uid})).await.unwrap();
    assert!(r.get("isError").is_none(), "{r}");
    assert_eq!(r["structuredContent"]["series"], true);
    assert_eq!(fake.objects("perso").len(), 1);
    let del = fake
        .requests()
        .into_iter()
        .find(|r| r.method == "DELETE")
        .unwrap();
    assert_eq!(del.path, "/calendars/anne/perso/1.ics");
}

/// Les erreurs se lisent : calendrier ou UID inconnus, dates de natures mêlées.
#[tokio::test]
async fn mistakes_are_tool_errors() {
    let (_fake, a) = agenda().await;
    for (tool, args, says) in [
        (
            EVENT_CREATE,
            json!({"calendar": "Travail", "summary": "x", "start": "2026-10-05"}),
            "calendrier inconnu",
        ),
        (
            EVENT_CREATE,
            json!({"calendar": "Perso", "summary": "x", "start": "2026-10-05", "end": "2026-10-05T10:00"}),
            "même nature",
        ),
        (
            EVENT_CREATE,
            json!({"calendar": "Perso", "summary": "x", "start": "2026-10-05T10:00", "end": "2026-10-05T09:00"}),
            "suivre le début",
        ),
        (EVENT_DELETE, json!({"uid": "absent@x"}), "aucun événement"),
        (
            EVENT_UPDATE,
            json!({"uid": uid_of(SIMPLE)}),
            "rien à modifier",
        ),
    ] {
        let r = a.call(tool, &args).await.unwrap();
        assert_eq!(r["isError"], true, "{tool} {args}");
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(says), "{tool} : {text}");
    }
}

/// Une alarme dans l'événement garde sa propre description quand celle de l'événement
/// change.
#[test]
fn a_patch_leaves_nested_alarms_alone() {
    let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDESCRIPTION:avant\r\nSEQUENCE:4\r\n\
               BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:rappel\r\nEND:VALARM\r\n\
               END:VEVENT\r\nEND:VCALENDAR\r\n";
    let out = patch(ics, &["DESCRIPTION"], &["DESCRIPTION:après".into()]);
    assert!(out.contains("DESCRIPTION:rappel"), "{out}");
    assert!(!out.contains("DESCRIPTION:avant"), "{out}");
    assert!(
        out.contains("DESCRIPTION:après\r\nSEQUENCE:5\r\nEND:VEVENT"),
        "{out}"
    );
}

/// Un titre ne peut pas injecter de propriété : retour chariot seul et caractères de
/// contrôle sont neutralisés.
#[test]
fn text_values_cannot_inject_properties() {
    let e = escape("Rdv\rATTENDEE:mailto:x@y\u{0}\u{b}fin");
    assert!(
        !e.contains('\r') && !e.chars().any(|c| c.is_control()),
        "{e:?}"
    );
    assert_eq!(e, "Rdv\\nATTENDEE:mailto:x@yfin");
}

/// L'UID cherché doit être exact : un UID qui en contient un autre ne le désigne pas.
#[tokio::test]
async fn a_uid_matches_exactly_never_by_substring() {
    let (fake, a) = agenda().await;
    let uid = uid_of(SIMPLE);
    let prefix: String = uid.chars().take(uid.len() - 2).collect();
    let r = a.call(EVENT_DELETE, &json!({"uid": prefix})).await.unwrap();
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(fake.objects("perso").len(), 2, "rien de supprimé");
}
