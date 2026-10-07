//! Le fournisseur unique (#327) : dossier, iCloud Drive, absence de fournisseur, médias,
//! clé retirée, et la sauvegarde nocturne.

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

fn use_dir(s: &Services, dir: &Path) {
    let d = dir.display().to_string();
    s.publish_config("test", move |c| {
        c.backup.provider = "dir".into();
        c.backup.dir = d;
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
}

/// Sans fournisseur, rien n'est construit et la commande qui le règle est donnée ; avec un
/// dossier, l'archive et son manifeste y arrivent, vérifiés, et la rotation s'y applique.
#[tokio::test]
async fn an_archive_lands_in_the_provider_folder_with_its_manifest() {
    let (dir, s) = services().await;
    let err = run(&s, true, Some(false)).await.unwrap_err();
    assert!(err.to_string().contains("penelope backup setup"), "{err}");
    let local = s.platform.dirs.data().join("backups");
    assert!(
        provider::archive_names(&local).is_empty(),
        "rien n'est construit sans fournisseur"
    );

    let target = dir.path().join("nas/penelope");
    use_dir(&s, &target);
    // Quatre semaines d'archives du mois dernier : au-delà des quotas.
    std::fs::create_dir_all(&target).unwrap();
    for day in 1..=28 {
        let old = format!("penelope-2026-08-{day:02}T04-00-00-000Z.tar.gz.enc");
        std::fs::write(target.join(&old), b"x").unwrap();
        std::fs::write(target.join(s3::manifest_key(&old)), b"{}").unwrap();
    }
    let report = run(&s, true, Some(false)).await.unwrap();
    let pushed = &report["pushed"];
    assert_eq!(pushed["provider"], "dir", "{report}");
    assert_eq!(pushed["location"], target.display().to_string());
    let name = pushed["key"].as_str().unwrap();
    let copied = std::fs::read(target.join(name)).unwrap();
    assert_eq!(copied.len() as u64, report["bytes"].as_u64().unwrap());
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(target.join(s3::manifest_key(name))).unwrap())
            .unwrap();
    assert_eq!(manifest["sha256"], report["sha256"]);
    let rotated = pushed["rotated"].as_array().unwrap();
    assert!(!rotated.is_empty(), "{report}");
    for r in rotated {
        let r = r.as_str().unwrap();
        assert!(!target.join(r).exists() && !target.join(s3::manifest_key(r)).exists());
    }
    // Aucun reste de copie provisoire.
    let partial = std::fs::read_dir(&target)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().ends_with(".partiel"));
    assert!(!partial);
    let st = status(&s).await;
    assert_eq!(st["provider"], "dir");
    let c = doctor_check(&s).await;
    assert!(c.ok && c.detail.contains("nas/penelope"), "{c:?}");
}

/// Un volume absent n'est pas remplacé par un dossier du disque interne.
#[test]
fn an_unmounted_volume_is_refused() {
    let t = provider::Target::Dir {
        path: PathBuf::from("/Volumes/penelope-volume-absent-de-test/sauvegardes"),
        icloud: false,
    };
    let e = t.ready_dir().unwrap_err().to_string();
    assert!(e.contains("n'est pas monté"), "{e}");
    assert!(!Path::new("/Volumes/penelope-volume-absent-de-test").exists());
}

/// iCloud Drive est un dossier sous le répertoire personnel : désactivé, il est dit ;
/// activé, l'archive arrive dans `Penelope/` (ou le sous-dossier de `backup.dir`).
#[tokio::test]
async fn icloud_is_a_folder_under_the_home_directory() {
    let (_dir, s) = services().await;
    let home = tempfile::tempdir().unwrap();
    let mut cfg = penelope_kernel::config::Backup {
        provider: "icloud".into(),
        ..Default::default()
    };
    let t = provider::Target::resolve(&cfg, s.platform.dirs.as_ref(), Some(home.path())).unwrap();
    assert_eq!(t.label(), "icloud");
    let e = t.ready_dir().unwrap_err().to_string();
    assert!(e.contains("iCloud Drive n'est pas activé"), "{e}");

    let drive = home.path().join(provider::ICLOUD_DRIVE);
    std::fs::create_dir_all(&drive).unwrap();
    let ready = t.ready_dir().unwrap();
    assert_eq!(ready, drive.join("Penelope"));
    cfg.dir = "Sauvegardes/Pénélope".into();
    let t = provider::Target::resolve(&cfg, s.platform.dirs.as_ref(), Some(home.path())).unwrap();
    assert_eq!(t.ready_dir().unwrap(), drive.join("Sauvegardes/Pénélope"));

    let (archive, report) = build(&s, false).await.unwrap();
    let pushed = provider::push(&s, &t, &archive, &report).await.unwrap();
    assert_eq!(pushed["provider"], "icloud");
    let name = pushed["key"].as_str().unwrap();
    assert!(drive.join("Sauvegardes/Pénélope").join(name).is_file());
}

/// `backup.dir` relatif est refusé à la validation ; `~/` part du répertoire personnel.
#[test]
fn the_provider_folder_is_absolute() {
    let mut c = penelope_kernel::config::Config::sample(1);
    c.backup.provider = "dir".into();
    c.backup.dir = "sauvegardes".into();
    assert!(c.validate().unwrap_err().to_string().contains("absolu"));
    c.backup.dir = "~/Sauvegardes".into();
    c.validate().unwrap();
    let dirs = penelope_platform::RootedDirs::new("/srv/p");
    let t = provider::Target::resolve(&c.backup, &dirs, Some(Path::new("/Users/moi"))).unwrap();
    assert_eq!(t.location(), "/Users/moi/Sauvegardes");
    c.backup.provider = "github".into();
    let e = c.validate().unwrap_err().to_string();
    assert!(e.contains("GitHub n'est plus"), "{e}");
    c.backup.provider = "s3".into();
    assert!(c.validate().unwrap_err().to_string().contains("backup.s3"));
}

/// `backup.include_media` vaut en manuel comme en nocturne : la CLI n'envoie `media` que
/// si `--media` est donné (#327, `route.rs` envoyait toujours `false`).
#[tokio::test]
async fn include_media_is_honoured_when_the_request_does_not_say() {
    let (_dir, s) = services().await;
    std::fs::create_dir_all(s.platform.dirs.data().join("media")).unwrap();
    std::fs::write(s.platform.dirs.data().join("media/photo.jpg"), b"jpg").unwrap();
    s.publish_config("test", |c| {
        c.backup.include_media = true;
        Ok(vec!["backup.include_media".into()])
    })
    .unwrap();
    let report = rpc(&s, &json!({"push": false, "media": null}))
        .await
        .unwrap();
    assert_eq!(report["manifest"]["media_included"], true, "{report}");
    let report = rpc(&s, &json!({"push": false, "media": false}))
        .await
        .unwrap();
    assert_eq!(report["manifest"]["media_included"], false, "{report}");
    let snap = rpc(&s, &json!({"snapshot": true})).await.unwrap();
    assert!(snap["path"].as_str().unwrap().ends_with(".db"), "{snap}");
}

/// Un `config.toml` qui porte encore `backup.git_remote` se lit sans erreur ; la clé est
/// ignorée, `doctor` le dit, et rien ne part vers ce dépôt ni vers celui du vault.
#[tokio::test]
async fn a_leftover_git_remote_is_ignored_and_said() {
    let (_dir, s) = services().await;
    let raw = "[owner]\ntelegram_user_id = 1\n\n[memory]\n\
               vault_git_remote = \"git@github.com:moi/vault.git\"\n\n[backup]\n\
               git_remote = \"git@github.com:moi/penelope-backups.git\"\n\
               max_push_bytes = 104857600\n";
    let (cfg, unknown) = penelope_kernel::config::Config::parse(raw).unwrap();
    assert!(unknown.is_empty(), "{unknown:?}");
    assert_eq!(cfg.backup.effective_provider(), None);
    assert!(
        !cfg.memory.vault_git_push,
        "le vault n'est plus poussé par défaut"
    );
    let why = penelope_kernel::config::retired("backup.git_remote").unwrap();
    assert!(why.contains("penelope backup setup"), "{why}");

    std::fs::write(s.platform.dirs.config_file(), raw).unwrap();
    let checks = doctor_checks(&s).await;
    let c = checks.iter().find(|c| c.id == "backup.git_remote").unwrap();
    assert!(
        c.detail.contains("ignorée") && c.detail.contains("Aucun fournisseur"),
        "{c:?}"
    );
    // Envoi demandé : refusé faute de fournisseur, jamais vers un dépôt git.
    let err = run(&s, true, Some(false)).await.unwrap_err().to_string();
    assert!(err.contains("aucun fournisseur"), "{err}");
    assert_eq!(
        doctor::retired_remote("[backup]\ngit_remote = \"\"\n"),
        None
    );
    assert_eq!(
        doctor::retired_remote("backup.git_remote = \"x\"\n").as_deref(),
        Some("x")
    );
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
        texts[0].starts_with("⚠️ La sauvegarde de cette nuit a échoué : aucun fournisseur"),
        "{texts:?}"
    );
}
