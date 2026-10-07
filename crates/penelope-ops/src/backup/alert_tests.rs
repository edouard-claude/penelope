//! Rotation du dossier local, alerte à 24 h, `doctor` en rouge et ligne du digest (#330).

use super::*;
use penelope_kernel::clock::TestClock;

async fn services() -> (tempfile::TempDir, Arc<Services>, TestClock) {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    (dir, s, clock)
}

fn use_dir(s: &Services, dir: &str) {
    let d = dir.to_string();
    s.publish_config("test", move |c| {
        c.backup.provider = "dir".into();
        c.backup.dir = d;
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
}

/// Le dossier local ne garde que la dernière archive et le dernier instantané ; quand il
/// est lui-même le dossier du fournisseur, c'est la rotation du fournisseur qui vaut.
#[tokio::test]
async fn the_local_folder_keeps_only_the_latest_archive() {
    let (_dir, s, clock) = services().await;
    let local = s.platform.dirs.data().join("backups");
    use_dir(&s, "{data}/fournisseur");
    for _ in 0..3 {
        run(&s, true, Some(false)).await.unwrap();
        clock.advance_days(1);
    }
    assert_eq!(provider::archive_names(&local).len(), 1);
    let provider_dir = s.platform.dirs.data().join("fournisseur");
    assert_eq!(provider::archive_names(&provider_dir).len(), 3);
    for _ in 0..3 {
        rpc(&s, &json!({"snapshot": true})).await.unwrap();
        clock.advance_hours(1);
    }
    let dbs = std::fs::read_dir(&local)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
        .count();
    assert_eq!(dbs, 1);

    use_dir(&s, "{data}/backups");
    for _ in 0..2 {
        run(&s, true, Some(false)).await.unwrap();
        clock.advance_days(1);
    }
    assert_eq!(
        provider::archive_names(&local).len(),
        3,
        "le dossier du fournisseur garde ses quotidiennes"
    );
}

/// Aucune sauvegarde réussie depuis 24 h : une alerte au foyer, avec la cause et la
/// commande, répétée une fois par jour au plus ; une sauvegarde réussie l'éteint.
#[tokio::test]
async fn a_day_without_a_successful_backup_is_said_once_a_day() {
    let (_dir, s, clock) = services().await;
    let rec = penelope_app::testing::RecordingMessenger::new();
    let m: Arc<dyn Messenger> = rec.clone();
    alert::tick(&s, Some(&m)).await.unwrap();
    clock.advance_hours(23);
    alert::tick(&s, Some(&m)).await.unwrap();
    assert!(rec.texts().is_empty(), "moins de 24 h");
    clock.advance_hours(2);
    alert::tick(&s, Some(&m)).await.unwrap();
    alert::tick(&s, Some(&m)).await.unwrap();
    let texts = rec.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0].contains("Aucune sauvegarde réussie")
            && texts[0].contains("aucun fournisseur")
            && texts[0].contains("`penelope backup setup`"),
        "{texts:?}"
    );

    // Un échec retenu : sa cause est dans l'alerte du lendemain et dans `doctor`.
    use_dir(&s, "/Volumes/penelope-volume-absent-de-test/p");
    run(&s, true, Some(false)).await.unwrap_err();
    clock.advance_hours(25);
    alert::tick(&s, Some(&m)).await.unwrap();
    let texts = rec.texts();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts[1].contains("dernier échec : le volume") && texts[1].contains("`penelope backup`."),
        "{texts:?}"
    );
    let c = doctor_check(&s).await;
    assert!(c.detail.contains("dernier échec : le volume"), "{c:?}");

    // Réussie : plus d'alerte, plus d'échec retenu ; 24 h plus tard, `doctor` en rouge.
    use_dir(&s, "{data}/fournisseur");
    run(&s, true, Some(false)).await.unwrap();
    assert!(s.kv_get(LAST_ERROR_KEY).await.unwrap().is_none());
    clock.advance_hours(23);
    alert::tick(&s, Some(&m)).await.unwrap();
    assert_eq!(rec.texts().len(), 2);
    let c = doctor_check(&s).await;
    assert!(c.ok, "{c:?}");
    clock.advance_hours(1);
    let c = doctor_check(&s).await;
    assert!(!c.ok && c.severity == "error", "{c:?}");
}

/// L'échec de la nuit vaut l'avis du jour : pas d'alerte dans la foulée.
#[tokio::test]
async fn the_failed_night_counts_as_the_alert_of_the_day() {
    let (_dir, s, clock) = services().await;
    let rec = penelope_app::testing::RecordingMessenger::new();
    s.publish_config("test", |c| {
        c.backup.cron = "0 3 * * *".into();
        Ok(vec!["backup.cron".into()])
    })
    .unwrap();
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    clock.advance_days(2);
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    let texts = rec.texts();
    // 48 h sans rien, mais la nuit vient d'échouer : un seul message, le sien.
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0].starts_with("⚠️ La sauvegarde de cette nuit a échoué"),
        "{texts:?}"
    );
    assert!(
        texts[0].ends_with("Relancer : `penelope backup`."),
        "{texts:?}"
    );
    // Une demi-heure plus tard : ni l'heure de la nuit (3 h), ni une alerte en double.
    clock.advance_ms(1_800_000);
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    assert_eq!(rec.texts().len(), 1);
    clock.advance_hours(24);
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    assert_eq!(rec.texts().len(), 2, "la nuit suivante échoue et le dit");
}

/// La ligne du digest : ✅ cette nuit, taille, fournisseur ; ❌ avec la cause sinon ; rien
/// quand aucune sauvegarde n'est attendue.
#[tokio::test]
async fn the_morning_digest_says_how_the_night_went() {
    let (_dir, s, clock) = services().await;
    let line = penelope_app::backup_state::digest_line(&s).await.unwrap();
    assert_eq!(
        line,
        "Sauvegarde : ❌ aucune réussie encore ; aucun fournisseur : `penelope backup setup`. \
         Relancer : `penelope backup`"
    );
    use_dir(&s, "{data}/fournisseur");
    run(&s, true, Some(false)).await.unwrap();
    let line = penelope_app::backup_state::digest_line(&s).await.unwrap();
    assert_eq!(line, "Sauvegarde : ✅ cette nuit, 0 Mo, dossier");
    clock.advance_hours(2);
    use_dir(&s, "/Volumes/penelope-volume-absent-de-test/p");
    run(&s, true, Some(false)).await.unwrap_err();
    let line = penelope_app::backup_state::digest_line(&s).await.unwrap();
    assert!(
        line.starts_with("Sauvegarde : ❌ aucune réussie depuis 2 h ; dernier échec : le volume"),
        "{line}"
    );
    s.publish_config("test", |c| {
        c.backup.cron = String::new();
        c.backup.provider = String::new();
        Ok(vec!["backup.cron".into()])
    })
    .unwrap();
    assert!(penelope_app::backup_state::digest_line(&s).await.is_none());
}
