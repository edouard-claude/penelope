//! #465 : les dossiers de travail de `backups/`, leur balayage et ce que `doctor` en dit.

use super::super::*;
use super::*;

fn aged(path: &Path, by: Duration) {
    let file = std::fs::File::open(path).unwrap();
    file.set_modified(SystemTime::now() - by).unwrap();
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn names(found: &[(String, u64)]) -> Vec<&str> {
    found.iter().map(|(n, _)| n.as_str()).collect()
}

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    s.platform
        .secrets
        .set(PASSPHRASE_SECRET, "phrase de passe de sauvegarde")
        .unwrap();
    (dir, Arc::new(s))
}

/// Le reste du 13/10 : un dossier de travail sans marque, avec une copie de la base.
fn leftover(backups: &Path) -> PathBuf {
    let old = backups.join("work-2026-10-13T00-00-26-046Z");
    std::fs::create_dir_all(old.join("penelope")).unwrap();
    std::fs::write(old.join("penelope/penelope.db"), vec![0u8; 4096]).unwrap();
    aged(&old, Duration::from_secs(3600));
    old
}

#[test]
fn a_scratch_is_private_marked_and_removed_with_its_owner() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("backups");
    let path = backups.join("work-2026-11-11T04-00-00-000Z");
    // Un reste du même nom ne gêne pas.
    std::fs::create_dir_all(path.join("penelope")).unwrap();
    let scratch = Scratch::create(path.clone()).unwrap();
    assert!(!path.join("penelope").exists());
    assert_eq!(
        std::fs::read_to_string(path.join(OWNER)).unwrap(),
        std::process::id().to_string()
    );
    assert_eq!(mode(&path), 0o700);
    assert_eq!(mode(&path.join(OWNER)), 0o600);
    write_private(&path.join("secrets.enc"), "scellé").unwrap();
    assert_eq!(mode(&path.join("secrets.enc")), 0o600);
    std::fs::write(path.join("copie.db"), "base").unwrap();
    restrict_file(&path.join("copie.db")).unwrap();
    assert_eq!(mode(&path.join("copie.db")), 0o600);

    // Tant que son propriétaire travaille : ni balayé, ni montré à `doctor`.
    assert!(sweep(&backups).is_empty());
    assert!(unexpected(&backups).is_empty());
    assert!(scratch.path().is_dir());
    drop(scratch);
    assert!(!path.exists());
}

/// Ce qui est retiré : un dossier sans marque passé le délai, celui d'un processus mort,
/// celui d'un pid réattribué, celui de ce processus que plus rien ne tient. Ce qui reste :
/// un dossier tout neuf, celui d'un processus vivant, et tout ce qui n'est pas un dossier
/// de travail.
#[test]
fn only_an_abandoned_scratch_is_swept() {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path();
    let old = leftover(b);
    let fresh = b.join("restauration-20261111T100000");
    std::fs::create_dir_all(&fresh).unwrap();
    // Un autre programme, vivant : une restauration lancée à la main pendant le démarrage.
    let mut other = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let busy = b.join("restauration-20261111T100001");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join(OWNER), other.id().to_string()).unwrap();
    let reused = b.join("telechargement-20261101T000000");
    std::fs::create_dir_all(&reused).unwrap();
    std::fs::write(reused.join(OWNER), other.id().to_string()).unwrap();
    aged(&reused.join(OWNER), OWNER_MAX_AGE + Duration::from_secs(60));
    let ours = b.join("work-2026-11-10T04-00-00-000Z");
    std::fs::create_dir_all(&ours).unwrap();
    std::fs::write(ours.join(OWNER), std::process::id().to_string()).unwrap();
    // Ce que le code ne connaît plus, ce qu'il range, et un lien au nom trompeur.
    std::fs::create_dir_all(b.join("repo/objects")).unwrap();
    std::fs::write(b.join("repo/objects/pack"), vec![0u8; 2048]).unwrap();
    aged(&b.join("repo"), Duration::from_secs(3600));
    std::fs::write(b.join("penelope-2026-11-10T04-00-00-000Z.tar.gz.enc"), "x").unwrap();
    std::fs::write(b.join("avant-restauration-20261101T000000.db"), "x").unwrap();
    std::os::unix::fs::symlink(b.join("repo"), b.join("work-lien")).unwrap();

    let swept = sweep(b);
    assert_eq!(
        names(&swept),
        [
            "telechargement-20261101T000000",
            "work-2026-10-13T00-00-26-046Z",
            "work-2026-11-10T04-00-00-000Z"
        ]
    );
    assert_eq!(swept[1].1, 4096, "le poids retiré est dit");
    assert!(!old.exists() && !reused.exists() && !ours.exists());
    assert!(fresh.is_dir() && busy.is_dir());
    assert!(
        b.join("repo/objects/pack").is_file(),
        "le lien n'est pas suivi"
    );
    let odd = unexpected(b);
    assert_eq!(names(&odd), ["repo", "work-lien"]);
    assert_eq!(odd[0].1, 2048);

    // Le programme s'arrête : son dossier n'est plus à personne.
    other.kill().unwrap();
    other.wait().unwrap();
    assert_eq!(names(&sweep(b)), ["restauration-20261111T100001"]);
    // Le délai passé, le dossier sans marque non plus.
    aged(&fresh, UNMARKED_GRACE);
    assert_eq!(names(&sweep(b)), ["restauration-20261111T100000"]);
    assert_eq!(names(&unexpected(b)), ["repo", "work-lien"]);
}

#[test]
fn sizes_read_like_the_owner_writes_them() {
    assert_eq!(human(3_328_599_654), "3,1 Gio");
    assert_eq!(human(724 * 1024 * 1024), "724 Mo");
    assert_eq!(human(12 * 1024), "12 Ko");
    assert_eq!(human(512), "512 o");
}

/// La sauvegarde suivante retire le dossier de travail d'une sauvegarde interrompue, et
/// ne laisse pas le sien. L'ancienne destination git, que le code ne connaît plus, n'est
/// pas retirée : `doctor` la montre, avec son poids et la commande.
#[tokio::test]
async fn a_backup_removes_what_an_interrupted_one_left() {
    let (_dir, s) = services().await;
    let backups = s.platform.dirs.data().join("backups");
    let old = leftover(&backups);
    std::fs::create_dir_all(backups.join("repo/objects")).unwrap();
    std::fs::write(backups.join("repo/objects/pack"), vec![0u8; 2048]).unwrap();

    let (archive, _report) = build(&s, false).await.unwrap();
    assert!(archive.is_file());
    assert!(!old.exists(), "le reste du 13/10 est retiré");
    let left: Vec<String> = std::fs::read_dir(&backups)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("work-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(backups.join("repo/objects/pack").is_file());

    let checks = doctor_checks(&s).await;
    let check = checks
        .iter()
        .find(|c| c.id == "backup.leftovers")
        .expect("contrôle attendu");
    assert!(!check.ok);
    assert!(check.detail.contains("`repo` (2 Ko)"), "{}", check.detail);
    let fix = check.fix.as_deref().unwrap_or_default();
    assert_eq!(fix, format!("rm -rf '{}'", backups.join("repo").display()));
    std::fs::remove_dir_all(backups.join("repo")).unwrap();
    let checks = doctor_checks(&s).await;
    assert!(checks.iter().all(|c| c.id != "backup.leftovers"));
}

/// Sans sauvegarde programmée, la passe de maintenance du daemon balaie quand même : la
/// première suit le démarrage.
#[tokio::test]
async fn the_maintenance_pass_sweeps_without_a_backup() {
    let (_dir, s) = services().await;
    s.config
        .mutate("test", |c| {
            c.backup.cron = String::new();
            Ok(vec!["backup.cron".into()])
        })
        .unwrap();
    let backups = s.platform.dirs.data().join("backups");
    let old = leftover(&backups);
    // Une sauvegarde en cours dans ce processus : elle n'est pas touchée.
    let running = Scratch::create(backups.join("work-2026-11-11T04-00-00-000Z")).unwrap();
    nightly_tick(&s, None).await.unwrap();
    assert!(!old.exists());
    assert!(running.path().is_dir());
}

/// Ce que la sauvegarde pose en clair dans son dossier de travail (instantané de la base,
/// secrets, manifeste, archive avant chiffrement) n'est lisible que du propriétaire.
#[tokio::test]
async fn the_work_files_of_a_backup_are_private() {
    let (_dir, s) = services().await;
    let job = BuildJob::of(&s, false);
    let work = Scratch::create(job.out_dir.join(format!("work-{}", job.stamp))).unwrap();
    let tar = work.path().join(format!("penelope-{}.tar.gz", job.stamp));
    let (root, dir) = (work.path().join("penelope"), work.path().to_path_buf());
    tokio::task::spawn_blocking(move || job.assemble(&dir, "phrase de passe de sauvegarde"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mode(work.path()), 0o700);
    for file in [
        root.join("penelope.db"),
        root.join(secrets::FILE),
        root.join("MANIFEST.json"),
        tar,
    ] {
        assert_eq!(mode(&file), 0o600, "{}", file.display());
    }
}
