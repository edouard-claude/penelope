//! Contrôles de cohérence : réglages qui s'annulent, heures calmes, bac à sable,
//! conversations Telegram, rétention, horloge.

use super::*;
use serde_json::json;

fn set(s: &Services, f: impl FnOnce(&mut penelope_kernel::config::Config)) {
    s.publish_config("test", move |c| {
        f(c);
        Ok(vec!["test".into()])
    })
    .unwrap();
}

/// #16 : un réglage qui en annule un autre est nommé avec la clé à relire ; un
/// déclencheur qui tombe dans les heures calmes aussi, avec la commande qui le suspend.
#[tokio::test]
async fn contradictions_and_quiet_hour_triggers_are_named() {
    let (_d, s) = services().await;
    let before = coherence_checks(&s).await;
    assert!(
        !before
            .iter()
            .any(|c| c.detail.contains("budget.session_usd") || c.detail.contains("heures calmes")),
        "{before:?}"
    );

    set(&s, |c| {
        c.budget.session_usd = 50.0;
        c.telegram.quiet_hours = "00:00-23:59".into();
    });
    let sched = s
        .schedules
        .create(
            // Le type vient de `penelope-workflow`, que la crate ne nomme pas.
            serde_json::from_value(json!("cron")).unwrap(),
            json!({"expr": "30 9 * * *"}),
            json!({"type": "notify", "template": "Bonjour"}),
            json!({}),
        )
        .await
        .unwrap();
    let checks = coherence_checks(&s).await;
    assert_eq!(checks.len(), before.len() + 2, "{checks:?}");
    let budget = checks
        .iter()
        .find(|c| c.detail.contains("budget.session_usd"))
        .expect("contradiction nommée");
    assert_eq!(
        budget.fix.as_deref(),
        Some("penelope config get budget.session_usd")
    );
    let quiet = checks
        .iter()
        .find(|c| c.detail.contains("heures calmes"))
        .expect("déclencheur en heures calmes");
    assert!(quiet.detail.contains(":30"), "{}", quiet.detail);
    assert_eq!(
        quiet.fix.clone().unwrap(),
        format!("penelope schedule pause {}", sched.id)
    );
}

/// #68 et #106 : sans bac à sable, ou réseau ouvert à toutes les commandes, ou lectures
/// sensibles permises, chaque contrôle le dit.
#[tokio::test]
async fn sandbox_gaps_are_reported() {
    let (_d, s) = services().await;
    set(&s, |c| {
        c.sandbox.default_profile = "workspace-write".into();
        c.sandbox.deny_read = vec!["~/.ssh".into()];
        c.sandbox.shell_network = true;
    });
    let c = sandbox_reads_check(&s);
    assert!(!c.ok);
    assert_eq!(
        c.detail,
        "lisibles par le shell : {data}/secrets.enc, {data}/penelope.db"
    );
    let c = sandbox_network_check(&s).await;
    assert!(!c.ok && c.detail.starts_with("ouvert à toutes"), "{c:?}");

    set(&s, |c| {
        c.sandbox.deny_read.clear();
        c.sandbox.shell_network = false;
    });
    assert!(
        sandbox_reads_check(&s)
            .detail
            .starts_with("aucune lecture refusée")
    );
    let c = sandbox_network_check(&s).await;
    assert!(c.ok && c.detail.contains("0 règle(s)"), "{c:?}");

    set(&s, |c| c.sandbox.default_profile = "full".into());
    assert!(sandbox_reads_check(&s).detail.starts_with("profil `full`"));
    assert!(
        sandbox_network_check(&s)
            .await
            .detail
            .starts_with("profil `full`")
    );
}

/// #113 : un groupe refusé est donné avec son identifiant ; `allow_groups` seul n'ouvre
/// plus rien.
#[tokio::test]
async fn refused_telegram_chats_are_listed_with_their_id() {
    let (_d, s) = services().await;
    s.kv_set(
        penelope_app::helpers::SEEN_CHATS_KEY,
        &json!([
            {"id": -1001, "type": "supergroup", "title": "Équipe", "last_seen": "2026-09-20T10:11:12Z"},
            {"id": -1002, "type": "group", "title": "Famille", "last_seen": "2026-09-21T08:00:00Z"}
        ])
        .to_string(),
    )
    .await
    .unwrap();
    let c = telegram_chats_check(&s).await;
    assert!(c.ok);
    assert!(
        c.detail.starts_with("conversation privée seulement ; refusées récemment : supergroup « Équipe » `-1001` (vu le 2026-09-20T10:11)"),
        "{}",
        c.detail
    );

    set(&s, |c| c.telegram.allow_groups = true);
    let c = telegram_chats_check(&s).await;
    assert!(
        !c.ok && c.detail.contains("n'ouvre plus aucun groupe"),
        "{c:?}"
    );

    set(&s, |c| c.telegram.allowed_chats = vec![-1001]);
    let c = telegram_chats_check(&s).await;
    assert!(c.ok);
    assert!(
        c.detail.starts_with("privée et 1 groupe(s) : `-1001`"),
        "{}",
        c.detail
    );
    assert!(c.detail.contains("Famille") && !c.detail.contains("Équipe"));
}

/// #78 : une rétention désactivée, jamais passée ou en retard, chacune se dit.
#[tokio::test]
async fn retention_says_when_it_last_ran() {
    let (_d, s) = services().await;
    let c = retention_check(&s).await;
    assert!(
        !c.ok && c.detail.starts_with("aucune passe enregistrée"),
        "{c:?}"
    );
    let old = s.clock.now_ms() - 100 * 3_600_000;
    s.kv_set("retention.last", &old.to_string()).await.unwrap();
    let c = retention_check(&s).await;
    assert!(
        !c.ok && c.detail.starts_with("dernière passe il y a 100 h"),
        "{c:?}"
    );
    set(&s, |c| c.retention.days = 0);
    let c = retention_check(&s).await;
    assert!(c.ok && c.detail.starts_with("désactivée"), "{c:?}");
}

/// Une horloge décalée de la machine est une dérive signalée.
#[tokio::test]
async fn a_drifting_clock_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::new(1_000_000));
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    let c = clock_check(&s);
    assert!(
        !c.ok && c.detail.contains("les schedules seront décalés"),
        "{c:?}"
    );
}
