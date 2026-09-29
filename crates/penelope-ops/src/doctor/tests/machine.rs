//! Contrôles de la machine : foyer Telegram, binaire lancé par le service,
//! planifications en échec.

use super::*;
use serde_json::json;

/// #143 : avec des groupes autorisés, l'absence de foyer est signalée ; un foyer réglé
/// est nommé, sujet compris.
#[tokio::test]
async fn the_telegram_home_is_needed_once_groups_are_allowed() {
    let (_d, s) = services().await;
    let c = home_check(&s);
    assert!(c.ok);
    assert_eq!(c.detail, "chat privé du propriétaire");
    s.publish_config("test", |c| {
        c.telegram.allowed_chats = vec![-1001];
        Ok(vec!["telegram.allowed_chats".into()])
    })
    .unwrap();
    let c = home_check(&s);
    assert!(
        !c.ok && c.fix.as_deref() == Some("dans le sujet voulu : /home"),
        "{c:?}"
    );
    s.publish_config("test", |c| {
        c.telegram.home.chat = -1001;
        c.telegram.home.topic = 12;
        Ok(vec!["telegram.home".into()])
    })
    .unwrap();
    assert_eq!(home_check(&s).detail, "chat -1001, sujet 12");
    s.publish_config("test", |c| {
        c.telegram.home.topic = 0;
        Ok(vec!["telegram.home".into()])
    })
    .unwrap();
    assert_eq!(home_check(&s).detail, "chat -1001");
}

/// #36 : sans service, le mode est seulement décrit ; un service qui lance ce binaire est
/// sain ; un service qui en lance un autre changerait de binaire au redémarrage.
#[test]
fn the_service_must_launch_this_binary() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("bin/penelope");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "").unwrap();
    let exe = std::fs::canonicalize(&exe).unwrap();

    let c = install_mode(&exe, None);
    assert!(c.ok && c.detail.ends_with("service non installé"), "{c:?}");

    let c = install_mode(&exe, Some(exe.to_str().unwrap()));
    assert!(c.ok && c.detail.contains("le service lance"), "{c:?}");

    let other = dir.path().join("ailleurs/penelope");
    let c = install_mode(&exe, Some(other.to_str().unwrap()));
    assert!(!c.ok && c.detail.contains("changerait de binaire"), "{c:?}");
    assert_eq!(
        c.fix.as_deref(),
        Some("penelope uninstall && penelope install")
    );
}

/// #39 : une planification active dont la dernière exécution a échoué est nommée.
#[tokio::test]
async fn a_failing_schedule_is_named() {
    let (_d, s) = services().await;
    let sched = s
        .schedules
        .create(
            serde_json::from_value(json!("interval")).unwrap(),
            json!({"every_ms": 3_600_000}),
            json!({"type": "notify", "template": "Bonjour"}),
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(
        schedules_check(&s).await.detail,
        "1 active(s), aucune en échec"
    );
    s.schedules
        .record_outcome(&sched.id, Some("canal injoignable"))
        .await
        .unwrap();
    let c = schedules_check(&s).await;
    assert!(!c.ok);
    assert_eq!(
        c.detail,
        format!("1 en échec : {} (canal injoignable)", sched.id)
    );
    // #229 : la série qui dure se lit ici, même quand ses alertes se taisent.
    s.schedules
        .record_outcome(&sched.id, Some("canal injoignable"))
        .await
        .unwrap();
    assert_eq!(
        schedules_check(&s).await.detail,
        format!(
            "1 en échec : {} (2 échecs de suite, canal injoignable)",
            sched.id
        )
    );
}

/// #260 : `doctor` résume la carte et dit ce qui a changé depuis la passe précédente,
/// ou, sans changement, quand la machine a bougé pour la dernière fois.
#[test]
fn doctor_says_what_changed_on_the_machine() {
    use crate::environment::{Capability, Changes, Environment, Found};
    let mut env = Environment {
        tools: vec![Found {
            name: "gh".into(),
            source: "brew".into(),
            version: Some("2.63.0".into()),
            ..Default::default()
        }],
        capabilities: vec![Capability {
            kind: "mcp".into(),
            id: "safari".into(),
            name: "Safari".into(),
            via: "safaridriver --mcp".into(),
            origin: "application".into(),
            ..Default::default()
        }],
        checked_at: "2026-09-29T10:00:00Z".into(),
        ..Default::default()
    };
    let checks = environment_checks(&env);
    assert_eq!(checks[0].id, "machine.environment");
    assert!(
        checks[0].detail.contains("1 outil(s) (brew 1)"),
        "{}",
        checks[0].detail
    );
    assert!(
        checks[0].detail.contains("mcp Safari (non branché)"),
        "{}",
        checks[0].detail
    );
    assert!(
        checks[1].detail.contains("première passe"),
        "{}",
        checks[1].detail
    );

    env.changes = Some(Changes {
        since: "2026-09-29T09:00:00Z".into(),
        at: "2026-09-29T10:00:00Z".into(),
        appeared: vec!["ollama (brew)".into()],
        disappeared: vec!["application Numbers".into()],
        updated: vec!["gh (brew) : 2.62.0 → 2.63.0".into()],
    });
    let now = &environment_checks(&env)[1];
    assert_eq!(
        now.detail,
        "depuis la passe du 2026-09-29T09:00:00Z : apparus : ollama (brew) ; disparus : \
         application Numbers ; mis à jour : gh (brew) : 2.62.0 → 2.63.0"
    );

    env.checked_at = "2026-09-29T11:00:00Z".into();
    let later = &environment_checks(&env)[1];
    assert!(
        later
            .detail
            .starts_with("rien depuis la passe du 2026-09-29T10:00:00Z"),
        "{}",
        later.detail
    );
}
