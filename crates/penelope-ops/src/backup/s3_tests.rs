//! La destination S3 (#289) contre le faux serveur : envoi vérifié, parties, rétention,
//! restauration, erreurs nommées, `doctor`.

use super::fake_s3::{FakeS3, Mode};
use super::*;

const PREFIX: &str = "sauvegardes/";

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

/// Services dont `[backup.s3]` vise le faux serveur, clés dans le magasin de secrets.
async fn services_with_s3(fake: &FakeS3) -> (tempfile::TempDir, Arc<Services>) {
    let (dir, s) = services().await;
    s.platform
        .secrets
        .set("s3_access_key_id", &fake.creds.access_key)
        .unwrap();
    s.platform
        .secrets
        .set("s3_secret_access_key", &fake.creds.secret_key)
        .unwrap();
    let (url, bucket) = (fake.url.clone(), fake.bucket.clone());
    s.publish_config("test", move |c| {
        c.backup.s3.endpoint = url;
        c.backup.s3.bucket = bucket;
        c.backup.s3.prefix = PREFIX.into();
        Ok(vec!["backup.s3".into()])
    })
    .unwrap();
    (dir, s)
}

/// L'archive et son manifeste arrivent dans le bucket, la taille est relue par `HEAD`, et
/// le rapport comme `backup.last` le disent. `[backup.s3]` renseignée sans
/// `backup.provider` (fichier d'avant #327) vaut le fournisseur `s3`.
#[tokio::test]
async fn an_archive_lands_in_the_bucket_with_its_manifest_and_is_verified() {
    let fake = FakeS3::start("sauvegardes-penelope").await;
    let (_dir, s) = services_with_s3(&fake).await;
    let report = run(&s, true, Some(false)).await.unwrap();
    let pushed = &report["pushed"];
    assert_eq!(pushed["provider"], "s3", "{report}");
    let key = pushed["key"].as_str().unwrap();
    assert!(
        key.starts_with("sauvegardes/penelope-") && key.ends_with(".tar.gz.enc"),
        "{report}"
    );
    assert_eq!(pushed["bytes"], report["bytes"], "{report}");
    assert_eq!(pushed["parts"], 1, "{report}");
    assert_eq!(pushed["bucket"], "sauvegardes-penelope");

    let keys = fake.keys();
    let mut expected = vec![key.to_string(), s3::manifest_key(key)];
    expected.sort();
    assert_eq!(keys, expected, "{keys:?}");
    let manifest: Value =
        serde_json::from_slice(&fake.object(&s3::manifest_key(key)).unwrap()).unwrap();
    assert_eq!(
        manifest["manifest"]["created_at"],
        report["manifest"]["created_at"]
    );
    assert_eq!(
        fake.object(key).unwrap().len() as u64,
        report["bytes"].as_u64().unwrap()
    );
    let requests = fake.requests();
    let put = requests
        .iter()
        .position(|r| r == &format!("PUT /sauvegardes-penelope/{key}"));
    let head = requests
        .iter()
        .position(|r| r == &format!("HEAD /sauvegardes-penelope/{key}"));
    assert!(put.is_some() && head > put, "{requests:?}");
    let last = s.kv_get(LAST_KEY).await.unwrap().unwrap();
    assert!(last.contains("\"pushed\""), "{last}");
}

/// Au-delà du seuil, l'archive part en parties et le serveur la recompose ; une partie
/// refusée abandonne l'envoi, et le serveur ne garde rien.
#[tokio::test]
async fn a_large_file_is_sent_in_parts_and_a_failed_part_aborts_the_upload() {
    let fake = FakeS3::start("b").await;
    let client = fake.client().with_part_sizes(1024, 400);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("grosse.bin");
    let content: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&file, &content).unwrap();

    let up = client.put_file("archives/grosse.bin", &file).await.unwrap();
    assert_eq!((up.bytes, up.parts), (3000, 8), "{up:?}");
    assert_eq!(fake.object("archives/grosse.bin").unwrap(), content);
    let requests = fake.requests();
    assert!(
        requests
            .iter()
            .any(|r| r == "POST /b/archives/grosse.bin?uploads"),
        "{requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("PUT /b/archives/grosse.bin?partNumber="))
            .count(),
        8,
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("POST /b/archives/grosse.bin?uploadId=")),
        "{requests:?}"
    );
    // Le petit fichier reste en un seul envoi.
    let small = dir.path().join("petite.bin");
    std::fs::write(&small, b"petite").unwrap();
    let up = client
        .put_file("archives/petite.bin", &small)
        .await
        .unwrap();
    assert_eq!(up.parts, 1);

    fake.state.lock().unwrap().fail_part = Some(3);
    let err = client
        .put_file("archives/ratee.bin", &file)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("partie 3") && err.contains("500"), "{err}");
    assert!(fake.object("archives/ratee.bin").is_none());
    assert!(
        fake.requests()
            .iter()
            .any(|r| r.starts_with("DELETE /b/archives/ratee.bin?uploadId=")),
        "l'envoi est abandonné"
    );
    assert!(fake.state.lock().unwrap().uploads.is_empty());
}

/// La rétention du dépôt git vaut pour le bucket : les archives en trop partent avec leur
/// manifeste, et la liste se lit page par page.
#[tokio::test]
async fn retention_removes_old_archives_and_their_manifests_from_the_bucket() {
    let fake = FakeS3::start("b").await;
    fake.state.lock().unwrap().page_size = 7;
    // Trente quotidiennes du mois d'avant (l'horloge des tests est au 16 septembre).
    for day in 1..=30 {
        let key = format!("{PREFIX}penelope-2026-08-{day:02}T04-00-00-000Z.tar.gz.enc");
        fake.put(&key, b"x");
        fake.put(&s3::manifest_key(&key), b"{}");
    }
    fake.put(
        "ailleurs/penelope-2026-08-01T04-00-00-000Z.tar.gz.enc",
        b"hors prefixe",
    );
    let (_dir, s) = services_with_s3(&fake).await;
    let report = run(&s, true, Some(false)).await.unwrap();
    let rotated = report["pushed"]["rotated"].as_array().unwrap();
    assert!(!rotated.is_empty(), "{report}");

    let left: Vec<String> = fake
        .keys()
        .into_iter()
        .filter(|k| k.starts_with(PREFIX) && k.ends_with(s3::ARCHIVE_SUFFIX))
        .map(|k| k[PREFIX.len()..].to_string())
        .collect();
    let mut all: Vec<String> = (1..=30)
        .map(|d| format!("penelope-2026-08-{d:02}T04-00-00-000Z.tar.gz.enc"))
        .collect();
    let new = report["pushed"]["key"].as_str().unwrap()[PREFIX.len()..].to_string();
    all.push(new.clone());
    let removed = rotation_plan(&all, &s.config.config().backup);
    let expected: Vec<String> = all
        .iter()
        .filter(|n| !removed.contains(n))
        .cloned()
        .collect();
    assert_eq!(left, expected, "{report}");
    assert!(left.contains(&new));
    // Chaque manifeste a son archive, et l'autre préfixe n'est pas touché.
    for k in fake.keys() {
        if let Some(stem) = k.strip_suffix(s3::MANIFEST_SUFFIX) {
            assert!(
                fake.keys()
                    .contains(&format!("{stem}{}", s3::ARCHIVE_SUFFIX)),
                "{k}"
            );
        }
    }
    assert!(
        fake.object("ailleurs/penelope-2026-08-01T04-00-00-000Z.tar.gz.enc")
            .is_some()
    );
    assert!(
        fake.requests()
            .iter()
            .filter(|r| r.contains("list-type=2"))
            .count()
            >= 2,
        "la liste a été lue en plusieurs pages"
    );
}

/// Ce que `restore-all s3` fait : lister les archives, la plus récente d'abord, et
/// télécharger celle choisie à l'octet près.
#[tokio::test]
async fn restore_lists_the_latest_first_and_downloads_it() {
    let fake = FakeS3::start("b").await;
    for (day, body) in [(1, "une"), (3, "trois"), (2, "deux")] {
        fake.put(
            &format!("{PREFIX}penelope-2026-10-{day:02}T04-00-00-000Z.tar.gz.enc"),
            body.as_bytes(),
        );
    }
    fake.put(
        &format!("{PREFIX}penelope-2026-10-03T04-00-00-000Z.manifest.json"),
        b"{}",
    );
    let client = fake.client();
    let archives = s3::list_archives(&client, PREFIX).await.unwrap();
    let keys: Vec<&str> = archives.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "sauvegardes/penelope-2026-10-03T04-00-00-000Z.tar.gz.enc",
            "sauvegardes/penelope-2026-10-02T04-00-00-000Z.tar.gz.enc",
            "sauvegardes/penelope-2026-10-01T04-00-00-000Z.tar.gz.enc",
        ]
    );
    assert_eq!(archives[0].size, 5);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("restore").join("archive.enc");
    let n = client.get_to_file(&archives[0].key, &dest).await.unwrap();
    assert_eq!(n, 5);
    assert_eq!(std::fs::read(&dest).unwrap(), b"trois");
    assert!(client.head("sauvegardes/absente").await.unwrap().is_none());
    let err = client
        .get_to_file("sauvegardes/absente", &dest)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("introuvable"), "{err}");
}

/// Chaque refus nomme sa cause : droits (403), bucket absent (404), serveur injoignable,
/// clé secrète fausse.
#[tokio::test]
async fn errors_name_their_cause() {
    let fake = FakeS3::start("sauvegardes-penelope").await;
    let (_dir, s) = services_with_s3(&fake).await;

    fake.set_mode(Mode::Forbidden);
    let err = run(&s, true, Some(false)).await.unwrap_err().to_string();
    assert!(
        err.contains("accès refusé (403 AccessDenied)") && err.contains("sauvegardes-penelope"),
        "{err}"
    );
    assert!(err.starts_with("PUT sauvegardes/penelope-"), "{err}");

    fake.set_mode(Mode::NoBucket);
    let err = run(&s, true, Some(false)).await.unwrap_err().to_string();
    assert!(
        err.contains("bucket `sauvegardes-penelope` introuvable")
            && err.contains("404 NoSuchBucket"),
        "{err}"
    );

    fake.set_mode(Mode::Normal);
    let wrong = s3::S3Client::new(
        &fake.url,
        &fake.bucket,
        "us-east-1",
        true,
        sigv4::Credentials {
            access_key: fake.creds.access_key.clone(),
            secret_key: "mauvaise".into(),
        },
    )
    .unwrap();
    let err = wrong.list(PREFIX).await.unwrap_err().to_string();
    assert!(err.contains("403 SignatureDoesNotMatch"), "{err}");
    // `HEAD` n'a pas de corps : le code HTTP seul, sans espace orphelin.
    let err = wrong.head_bucket().await.unwrap_err().to_string();
    assert!(err.contains("accès refusé (403)"), "{err}");

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let unreachable = s3::S3Client::new(
        &format!("http://127.0.0.1:{port}"),
        "b",
        "us-east-1",
        true,
        fake.creds.clone(),
    )
    .unwrap();
    let err = unreachable.head_bucket().await.unwrap_err().to_string();
    assert!(err.contains("injoignable"), "{err}");
}

/// `doctor` : sans S3, un contrôle ; avec, `backup.s3` dit si le bucket répond avec ces
/// clés, puis l'âge de la dernière sauvegarde S3.
#[tokio::test]
async fn doctor_reports_the_bucket_and_the_last_s3_backup() {
    let (_dir, plain) = services().await;
    assert_eq!(doctor_checks(&plain).await.len(), 1);

    let fake = FakeS3::start("sauvegardes-penelope").await;
    let (_dir, s) = services_with_s3(&fake).await;
    let checks = doctor_checks(&s).await;
    assert_eq!(checks.len(), 2, "{checks:?}");
    let c = &checks[1];
    assert_eq!(c.id, "backup.s3");
    assert!(
        !c.ok && c.detail.contains("joignable") && c.detail.contains("aucune sauvegarde S3"),
        "{c:?}"
    );
    assert_eq!(c.fix.as_deref(), Some("penelope backup"));
    assert!(
        checks[0].detail.contains("aucune sauvegarde"),
        "{:?}",
        checks[0]
    );

    run(&s, true, Some(false)).await.unwrap();
    let checks = doctor_checks(&s).await;
    let c = &checks[1];
    assert!(c.ok, "{c:?}");
    assert!(
        c.detail.contains("bucket `sauvegardes-penelope` joignable")
            && c.detail.contains("il y a 0 h")
            && c.detail.contains("sauvegardes/penelope-"),
        "{c:?}"
    );
    assert!(
        checks[0]
            .detail
            .contains("vers S3 `sauvegardes-penelope/sauvegardes/`"),
        "{:?}",
        checks[0]
    );

    fake.set_mode(Mode::Forbidden);
    let c = doctor_checks(&s).await.remove(1);
    assert!(!c.ok && c.detail.contains("403"), "{c:?}");

    s.platform.secrets.delete("s3_secret_access_key").unwrap();
    let c = doctor_checks(&s).await.remove(1);
    assert!(
        !c.ok && c.detail.contains("backup.s3.secret_access_key"),
        "{c:?}"
    );
}

/// #328 : l'essai de mise en place écrit puis efface un objet dans le bucket, avec les
/// clés tapées ; des droits refusés se voient à ce moment, pas la première nuit.
#[tokio::test]
async fn setup_tries_the_bucket_with_the_typed_keys() {
    let fake = FakeS3::start("sauvegardes-penelope").await;
    let (_dir, s) = services_with_s3(&fake).await;
    let cfg = s.config.config().backup.s3.clone();
    let t = provider::Target::S3(cfg);
    let creds = || sigv4::Credentials {
        access_key: fake.creds.access_key.clone(),
        secret_key: fake.creds.secret_key.clone(),
    };
    let at = provider::probe(&t, Some(creds())).await.unwrap();
    assert!(at.contains("sauvegardes-penelope"), "{at}");
    assert!(fake.keys().is_empty(), "l'objet d'essai est effacé");
    fake.set_mode(Mode::Forbidden);
    let e = provider::probe(&t, Some(creds())).await.unwrap_err();
    assert!(e.to_string().contains("403"), "{e}");
}
