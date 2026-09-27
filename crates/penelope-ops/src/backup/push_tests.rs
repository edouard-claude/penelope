//! Envoi d'une sauvegarde vers un dépôt, et sauvegarde nocturne.

use super::*;

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::new(1_789_516_800_000));
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    (dir, s)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} : {out:?}");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// #42 : l'archive part dans le dépôt avec son manifeste, en un commit ; sans dépôt
/// configuré, la commande qui le règle est donnée.
#[tokio::test]
async fn an_archive_is_pushed_with_its_manifest() {
    let (dir, s) = services().await;
    let err = run(&s, true, Some(false)).await.unwrap_err();
    assert!(err.to_string().contains("backup.git_remote"), "{err}");

    let bare = dir.path().join("depot.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--quiet"]);
    let remote = bare.display().to_string();
    s.publish_config("test", move |c| {
        c.backup.git_remote = remote.clone();
        Ok(vec!["backup.git_remote".into()])
    })
    .unwrap();
    let report = run(&s, true, Some(false)).await.unwrap();
    let pushed = &report["pushed"];
    let archive = pushed["archive"].as_str().unwrap();
    assert!(archive.ends_with(".tar.gz.enc"), "{report}");
    assert_eq!(pushed["remote"], bare.display().to_string());

    let files = git(&bare, &["ls-tree", "--name-only", "HEAD"]);
    assert!(files.lines().any(|f| f == archive), "{files}");
    assert!(files.lines().any(|f| f == "MANIFEST.json"), "{files}");
    let log = git(&bare, &["log", "--format=%s"]);
    assert_eq!(log.trim(), format!("Sauvegarde {archive}"));
    let last = s.kv_get(LAST_KEY).await.unwrap().unwrap();
    assert!(last.contains(archive), "{last}");
}

/// #39 : la sauvegarde de la nuit ne part qu'à l'heure de `backup.cron`, jamais au
/// premier passage ; un échec est dit au propriétaire.
#[tokio::test]
async fn a_failed_nightly_backup_is_said() {
    let (_dir, s) = services().await;
    let rec = penelope_app::testing::RecordingMessenger::new();
    s.publish_config("test", |c| {
        c.backup.cron = String::new();
        Ok(vec!["backup.cron".into()])
    })
    .unwrap();
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    assert!(s.kv_get("backup.cron.last").await.unwrap().is_none());

    s.publish_config("test", |c| {
        c.backup.cron = "0 3 * * *".into();
        c.backup.git_remote = String::new();
        c.memory.vault_git_remote = String::new();
        Ok(vec!["backup.cron".into()])
    })
    .unwrap();
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    assert!(
        s.kv_get("backup.cron.last").await.unwrap().is_some(),
        "premier passage : l'instant est retenu"
    );
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    assert!(rec.texts().is_empty(), "pas encore l'heure");

    // Dernier passage il y a un jour : l'heure de `backup.cron` est passée depuis.
    s.kv_set(
        "backup.cron.last",
        &(s.clock.now_ms() - 86_400_000).to_string(),
    )
    .await
    .unwrap();
    nightly_tick(&s, Some(rec.clone())).await.unwrap();
    let texts = rec.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0].starts_with("⚠️ La sauvegarde de cette nuit a échoué : aucun dépôt"),
        "{texts:?}"
    );
}
