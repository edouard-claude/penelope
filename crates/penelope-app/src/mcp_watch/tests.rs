use super::*;
use crate::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

/// L'erreur que range l'hôte MCP quand le serveur refuse le jeton : elle porte déjà un
/// remède, dans des mots à elle.
const REVOKED: &str = "Token has been expired or revoked : autorisation requise, \
                       `/mcp auth agenda` sur Telegram ou `penelope mcp auth agenda`";

async fn services(quiet: &str) -> (tempfile::TempDir, Arc<Services>, Arc<TestClock>) {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock.clone())
            .await
            .unwrap(),
    );
    let quiet = quiet.to_string();
    s.publish_config("test", move |c| {
        c.telegram.quiet_hours = quiet;
        Ok(vec!["telegram.quiet_hours".into()])
    })
    .unwrap();
    (dir, s, clock)
}

fn status(name: &str, state: ServerState, error: Option<&str>) -> ServerStatus {
    ServerStatus {
        name: name.into(),
        state,
        transport: "stdio".into(),
        protocol: None,
        tool_count: 12,
        failures: 0,
        last_error: error.map(str::to_string),
        last_ok: None,
        p50_ms: 0.0,
        p95_ms: 0.0,
        calls: 0,
        errors: 0,
        running: false,
        lazy: true,
        keychain: false,
    }
}

fn revoked(name: &str) -> ServerStatus {
    status(name, ServerState::AuthRequired, Some(REVOKED))
}

/// Le test de l'issue : le jeton est révoqué, dix passes de suite voient le serveur en
/// « autorisation requise », le propriétaire reçoit un seul message, avec la cause et la
/// commande, sans le remède de l'erreur recopié à côté.
#[tokio::test]
async fn ten_passes_over_a_revoked_token_tell_the_owner_once() {
    let (_dir, s, clock) = services("").await;
    let rec = RecordingMessenger::new();
    for _ in 0..10 {
        tick(&s, &[revoked("agenda")], Some(rec.as_ref())).await;
        clock.advance_secs(60);
    }
    let texts = rec.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert_eq!(
        texts[0],
        "🔌 **Serveur MCP hors d'usage**\n\n\
         • `agenda` (12 outil(s)) : autorisation requise. Cause : Token has been expired or \
         revoked. Pour le rebrancher : `/mcp auth agenda` dans la conversation, ou \
         `penelope mcp auth agenda`.\n\n\
         Les tours qui s'en servent échouent en attendant ; le digest du matin le rappelle \
         tant que ça dure."
    );
    let standing = standing(&s).await;
    assert_eq!(standing["agenda"].state, "auth_required");
    assert!(standing["agenda"].told_ms.is_some());
}

/// Un daemon qui redémarre remet ses serveurs au repos, puis les reconnecte : ni l'un ni
/// l'autre ne dit que le jeton est revenu, et l'alerte ne repart pas.
#[tokio::test]
async fn a_daemon_restart_does_not_tell_again() {
    let (_dir, s, clock) = services("").await;
    let rec = RecordingMessenger::new();
    tick(&s, &[revoked("agenda")], Some(rec.as_ref())).await;
    let since = standing(&s).await["agenda"].since_ms;
    for state in [ServerState::Configured, ServerState::Connecting] {
        clock.advance_secs(60);
        tick(&s, &[status("agenda", state, None)], Some(rec.as_ref())).await;
        assert_eq!(standing(&s).await["agenda"].since_ms, since);
    }
    clock.advance_secs(60);
    tick(&s, &[revoked("agenda")], Some(rec.as_ref())).await;
    assert_eq!(rec.texts().len(), 1);
    assert_eq!(standing(&s).await["agenda"].since_ms, since);
}

/// La reconnexion efface l'alerte ; une révocation une semaine plus tard est un fait
/// nouveau, dit à son tour.
#[tokio::test]
async fn reconnecting_clears_it_and_a_later_revocation_is_told_again() {
    let (_dir, s, clock) = services("").await;
    let rec = RecordingMessenger::new();
    tick(&s, &[revoked("agenda")], Some(rec.as_ref())).await;
    assert!(digest_line(&s).await.is_some());

    clock.advance_secs(60);
    let back = [status("agenda", ServerState::Ready, None)];
    tick(&s, &back, Some(rec.as_ref())).await;
    assert!(standing(&s).await.is_empty());
    assert_eq!(digest_line(&s).await, None);

    clock.advance_days(7);
    tick(&s, &back, Some(rec.as_ref())).await;
    assert_eq!(s.kv_get(KEY).await.unwrap(), None, "plus rien à garder");
    tick(&s, &[revoked("agenda")], Some(rec.as_ref())).await;
    assert_eq!(rec.texts().len(), 2);
}

/// Un serveur qui clignote ne sonne pas : la rechute dans le même état, le jour de
/// l'alerte, va au digest seulement. Un autre état est une autre nouvelle.
#[tokio::test]
async fn a_relapse_the_same_day_goes_to_the_digest_only() {
    let (_dir, s, clock) = services("").await;
    let rec = RecordingMessenger::new();
    let down = [status(
        "notes",
        ServerState::Failed,
        Some("connexion refusée"),
    )];
    tick(&s, &down, Some(rec.as_ref())).await;
    clock.advance_hours(1);
    tick(
        &s,
        &[status("notes", ServerState::Degraded, None)],
        Some(rec.as_ref()),
    )
    .await;
    assert!(standing(&s).await.is_empty());
    clock.advance_hours(1);
    tick(&s, &down, Some(rec.as_ref())).await;
    assert_eq!(rec.texts().len(), 1);
    assert_eq!(
        digest_line(&s).await.as_deref(),
        Some(
            "Serveurs MCP : ❌ `notes` (en panne depuis moins d'une heure, \
             `penelope mcp restart notes`)"
        )
    );

    clock.advance_hours(1);
    tick(&s, &[revoked("notes")], Some(rec.as_ref())).await;
    let texts = rec.texts();
    assert_eq!(texts.len(), 2);
    assert!(texts[1].contains("`notes` (12 outil(s)) : autorisation requise"));
}

/// Une panne durable a son remède à elle, et deux serveurs tombés ensemble font un seul
/// message.
#[tokio::test]
async fn two_servers_down_together_make_one_message() {
    let (_dir, s, _clock) = services("").await;
    let rec = RecordingMessenger::new();
    let mut failed = status("notes", ServerState::Failed, Some("connexion refusée."));
    failed.tool_count = 0;
    tick(&s, &[revoked("agenda"), failed], Some(rec.as_ref())).await;
    let texts = rec.texts();
    assert_eq!(texts.len(), 1);
    assert!(texts[0].starts_with("🔌 **2 serveurs MCP hors d'usage**\n\n• `agenda`"));
    assert!(
        texts[0].contains(
            "\n• `notes` : en panne. Cause : connexion refusée. À voir : \
             `penelope mcp logs notes`, puis `penelope mcp restart notes`.\n"
        ),
        "{}",
        texts[0]
    );
}

/// Pendant les heures calmes, l'alerte attend dans la file et n'y entre qu'une fois.
#[tokio::test]
async fn quiet_hours_hold_the_alert_once() {
    // TestClock : 04:00 à La Réunion, dans la plage.
    let (_dir, s, clock) = services("22:00-07:00").await;
    let rec = RecordingMessenger::new();
    for _ in 0..3 {
        tick(&s, &[revoked("agenda")], Some(rec.as_ref())).await;
        clock.advance_secs(60);
    }
    assert!(rec.texts().is_empty());
    let held = crate::quiet::held(&s).await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].kind, "mcp_alert");
    assert!(held[0].text.contains("/mcp auth agenda"));
}

/// Sans canal branché, rien n'est tenu pour dit : l'alerte part à la première passe qui
/// en a un.
#[tokio::test]
async fn without_a_channel_the_alert_waits_for_one() {
    let (_dir, s, clock) = services("").await;
    tick(&s, &[revoked("agenda")], None).await;
    assert_eq!(standing(&s).await["agenda"].told_ms, None);
    clock.advance_secs(60);
    let rec = RecordingMessenger::new();
    tick(&s, &[revoked("agenda")], Some(rec.as_ref())).await;
    assert_eq!(rec.texts().len(), 1);
}

/// Le digest le rappelle tant que ça dure, avec l'âge et la commande.
#[tokio::test]
async fn the_digest_reminds_as_long_as_it_lasts() {
    let (_dir, s, clock) = services("").await;
    assert_eq!(digest_line(&s).await, None);
    tick(&s, &[revoked("agenda")], None).await;
    clock.advance_hours(5);
    assert_eq!(
        digest_line(&s).await.as_deref(),
        Some("Serveurs MCP : ❌ `agenda` (autorisation requise depuis 5 h, `/mcp auth agenda`)")
    );
    clock.advance_days(6);
    assert_eq!(
        digest_line(&s).await.as_deref(),
        Some("Serveurs MCP : ❌ `agenda` (autorisation requise depuis 6 j, `/mcp auth agenda`)")
    );
}

/// Ce qui ferme une entrée sans passer par « prêt » : le serveur retiré de `mcp.d`, le
/// serveur désactivé, le serveur paresseux qui a répondu depuis, l'autorisation refaite.
#[tokio::test]
async fn removed_disabled_answered_or_reauthorised_servers_leave() {
    let (_dir, s, clock) = services("").await;
    let all = ["retire", "coupe", "paresseux", "autorise", "reste"];
    let down: Vec<ServerStatus> = all.iter().map(|n| revoked(n)).collect();
    tick(&s, &down, None).await;
    assert_eq!(standing(&s).await.len(), 5);

    clock.advance_secs(60);
    let mut answered = status("paresseux", ServerState::Configured, None);
    answered.last_ok = Some(s.clock.now_rfc3339());
    forget(&s, "autorise").await;
    forget(&s, "inconnu").await;
    tick(
        &s,
        &[
            status("coupe", ServerState::Disabled, None),
            answered,
            status("autorise", ServerState::Configured, None),
            status("reste", ServerState::Configured, None),
        ],
        None,
    )
    .await;
    let left: Vec<String> = standing(&s).await.into_keys().collect();
    assert_eq!(left, vec!["reste".to_string()]);
}

#[test]
fn the_cause_drops_the_remedy_the_error_carries() {
    assert_eq!(cause(Some(REVOKED)), "Token has been expired or revoked");
    assert_eq!(
        cause(Some(
            "autorisation requise : le renouvellement du jeton de `agenda` est refusé \
             (HTTP 400 invalid_grant) ; `penelope mcp auth agenda`, ou `/mcp auth agenda` \
             dans la conversation"
        )),
        "le renouvellement du jeton de `agenda` est refusé (HTTP 400 invalid_grant)"
    );
    assert_eq!(cause(None), "");
    let long = "é".repeat(300);
    assert_eq!(cause(Some(&long)).chars().count(), CAUSE_CHARS + 1);
}
