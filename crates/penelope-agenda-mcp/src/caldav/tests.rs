use super::fake::{FakeCalDav, Mode, PASSWORD, USER};
use super::*;

const SIMPLE: &str = include_str!("../../tests/fixtures/simple.ics");
const WEEKLY: &str = include_str!("../../tests/fixtures/weekly.ics");

fn client(url: &str) -> Client {
    Client::new(url, USER, PASSWORD, Duration::from_secs(5)).unwrap()
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

#[tokio::test]
async fn discovery_goes_principal_home_then_event_calendars_only() {
    let fake = FakeCalDav::start().await;
    fake.add_calendar("perso", "Perso", &[SIMPLE]);
    fake.add_calendar("famille", "Famille & amis", &[]);
    let c = client(&fake.url);
    let calendars = c.discover().await.unwrap();
    let names: Vec<&str> = calendars.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Famille & amis", "Perso"],
        "les rappels (VTODO) et la boîte de réception sont écartés"
    );
    assert_eq!(
        calendars[1].url.as_str(),
        format!("{}/calendars/anne/perso/", fake.url)
    );
    assert_eq!(calendars[1].color.as_deref(), Some("#FF2968FF"));
    let steps: Vec<(String, String, Option<String>)> = fake
        .requests()
        .into_iter()
        .map(|r| (r.method, r.path, r.depth))
        .collect();
    assert_eq!(
        steps,
        vec![
            ("PROPFIND".into(), "/".into(), Some("0".into())),
            (
                "PROPFIND".into(),
                "/principals/anne/".into(),
                Some("0".into())
            ),
            (
                "PROPFIND".into(),
                "/calendars/anne/".into(),
                Some("1".into())
            ),
        ]
    );
}

#[tokio::test]
async fn events_are_reported_on_the_range_and_returned_whole() {
    let fake = FakeCalDav::start().await;
    fake.add_calendar("perso", "Perso", &[SIMPLE, WEEKLY]);
    let c = client(&fake.url);
    let calendar = &c.discover().await.unwrap()[0];
    let objects = c
        .events(
            &calendar.url,
            utc("2026-10-02T20:00:00Z"),
            utc("2026-10-03T20:00:00Z"),
        )
        .await
        .unwrap();
    assert_eq!(objects.len(), 2);
    assert!(objects[0].contains("UID:dentiste-2026-10-03"));
    assert!(
        objects[1].contains("RECURRENCE-ID"),
        "l'objet revient entier"
    );
    let report = fake.requests().into_iter().last().unwrap();
    assert_eq!(report.method, "REPORT");
    assert_eq!(report.path, "/calendars/anne/perso/");
    assert_eq!(report.depth.as_deref(), Some("1"));
    assert!(
        report
            .body
            .contains("<c:time-range start=\"20261002T200000Z\" end=\"20261003T200000Z\"/>"),
        "{}",
        report.body
    );
    assert!(report.body.contains("comp-filter name=\"VEVENT\""));
}

#[tokio::test]
async fn refused_credentials_are_said_without_the_password() {
    let fake = FakeCalDav::start().await;
    let c = Client::new(&fake.url, USER, "mauvais", Duration::from_secs(5)).unwrap();
    let err = c.discover().await.unwrap_err();
    assert!(matches!(err, Error::Auth(401)), "{err}");
    let text = err.to_string();
    assert!(
        text.contains("identifiants refusés") && text.contains("AGENDA_USER"),
        "{text}"
    );
    assert!(!text.contains("mauvais"));
    fake.set_mode(Mode::Unauthorized);
    let c = client(&fake.url);
    assert!(matches!(c.discover().await.unwrap_err(), Error::Auth(401)));
}

#[tokio::test]
async fn a_broken_server_names_the_step_and_the_status() {
    let fake = FakeCalDav::start().await;
    fake.set_mode(Mode::Broken);
    let err = client(&fake.url).discover().await.unwrap_err();
    match err {
        Error::Status { what, status } => {
            assert_eq!(what, "principal");
            assert_eq!(status, 500);
        }
        other => panic!("{other}"),
    }
    let c = client("http://127.0.0.1:9");
    assert!(matches!(c.discover().await.unwrap_err(), Error::Network(_)));
}

#[tokio::test]
async fn a_redirect_is_followed_with_the_same_method() {
    let fake = FakeCalDav::start().await;
    fake.set_mode(Mode::RedirectBase);
    fake.add_calendar("perso", "Perso", &[]);
    let calendars = client(&fake.url).discover().await.unwrap();
    assert_eq!(calendars.len(), 1);
    let first: Vec<(String, String)> = fake
        .requests()
        .into_iter()
        .take(2)
        .map(|r| (r.method, r.path))
        .collect();
    assert_eq!(
        first,
        vec![
            ("PROPFIND".to_string(), "/".to_string()),
            ("PROPFIND".to_string(), "/dav/".to_string())
        ]
    );
}

#[tokio::test]
async fn without_a_principal_the_base_is_listed_directly() {
    let fake = FakeCalDav::start().await;
    fake.set_mode(Mode::NoPrincipal);
    fake.add_calendar("perso", "Perso", &[]);
    let calendars = client(&fake.url).discover().await.unwrap();
    assert_eq!(calendars.len(), 1);
    let depths: Vec<Option<String>> = fake.requests().into_iter().map(|r| r.depth).collect();
    assert_eq!(
        depths,
        vec![Some("0".into()), Some("1".into())],
        "la sonde du principal, puis la liste du dossier donné"
    );
}

#[test]
fn urls_with_credentials_or_without_a_host_are_refused() {
    for bad in [
        "caldav.icloud.com",
        "https://anne:secret@caldav.icloud.com/",
        "ftp://caldav.icloud.com/",
        "https://",
    ] {
        let err = Client::new(bad, USER, PASSWORD, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(err, Error::Url(_)), "{bad} : {err}");
        assert!(!err.to_string().contains("secret"), "{err}");
    }
    assert!(
        Client::new(
            "https://caldav.icloud.com",
            USER,
            PASSWORD,
            Duration::from_secs(1)
        )
        .is_ok()
    );
}

#[test]
fn multistatus_without_an_href_or_with_failed_propstats_is_skipped() {
    let xml = "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">\
               <d:response><d:propstat><d:prop><d:displayname>x</d:displayname></d:prop>\
               <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\
               <d:response><d:href>/a/</d:href><d:propstat><d:prop><d:displayname>y</d:displayname>\
               </d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>\
               <d:response><d:href>/b/</d:href><d:propstat><d:prop><d:displayname>z</d:displayname>\
               </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\
               </d:multistatus>";
    let doc = parse_xml(xml, "test").unwrap();
    let props = ok_props(&doc);
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].0, "/b/");
    assert!(matches!(
        parse_xml("<pas fermé", "test"),
        Err(Error::Xml { .. })
    ));
}
