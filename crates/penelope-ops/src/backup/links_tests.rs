//! Liens symboliques, plafond de la collecte et archives retouchées (revue de sécurité du
//! lot #327-#330) : rien n'entre de hors des racines déclarées, rien ne sort de la
//! destination à la restauration.

use super::*;

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::new(1_789_516_800_000));
    let s = Arc::new(
        Services::for_tests(dir.path().join("home"), clock)
            .await
            .unwrap(),
    );
    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    (dir, s)
}

/// Un lien vers un fichier hors racine et une boucle de liens sous `workspace` et
/// `mcp-data` : ni suivis ni copiés, nommés au manifeste ; la sauvegarde aboutit.
#[cfg(unix)]
#[tokio::test]
async fn symbolic_links_are_never_followed() {
    use std::os::unix::fs::symlink;
    let (dir, s) = services().await;
    let outside = dir.path().join("hors-racine.txt");
    std::fs::write(&outside, "secret de la machine hors sauvegarde").unwrap();
    let data = s.platform.dirs.data();
    std::fs::create_dir_all(data.join("workspace/projet")).unwrap();
    std::fs::write(data.join("workspace/projet/notes.txt"), "à garder").unwrap();
    symlink(&outside, data.join("workspace/projet/fuite.txt")).unwrap();
    std::fs::create_dir_all(data.join("mcp-data/pont")).unwrap();
    symlink(data.join("mcp-data"), data.join("mcp-data/pont/boucle")).unwrap();
    symlink("/", data.join("mcp-data/racine")).unwrap();

    let (archive, report) = build(&s, false).await.unwrap();
    let skipped: Vec<&str> = report["manifest"]["links_skipped"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for l in [
        "workspace/projet/fuite.txt",
        "mcp-data/pont/boucle",
        "mcp-data/racine",
    ] {
        assert!(skipped.contains(&l), "{l} : {skipped:?}");
    }
    let plain = penelope_platform::archive::open_to_vec(&archive, "phrase").unwrap();
    let names = penelope_platform::process::list_tar_gz_bytes(&plain).unwrap();
    assert!(names.contains(&"penelope/workspace/projet/notes.txt".to_string()));
    assert!(
        !names
            .iter()
            .any(|n| n.contains("fuite") || n.contains("boucle")),
        "{names:?}"
    );
    let out = tempfile::tempdir().unwrap();
    penelope_platform::process::extract_tar_gz_bytes(&plain, out.path()).unwrap();
    let text = String::from_utf8_lossy(&plain).to_string();
    assert!(!text.contains("secret de la machine"));
}

/// Au-delà du plafond, la collecte s'arrête et le dit, avant toute archive.
#[test]
fn the_collection_stops_at_its_cap() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a"), vec![0u8; 600]).unwrap();
    std::fs::write(src.join("b"), vec![0u8; 600]).unwrap();
    let mut c = Collect::bounded(1000);
    let e = c
        .copy_root(&src, &dir.path().join("dst"), "workspace")
        .unwrap_err()
        .to_string();
    assert!(e.contains("plafond"), "{e}");
    let mut c = Collect::bounded(2000);
    c.copy_root(&src, &dir.path().join("dst2"), "workspace")
        .unwrap();
    assert_eq!(c.bytes, 1200);
}

/// Une archive ne s'extrait que si chaque entrée est un fichier ou un dossier sous
/// `penelope/`, sans `..` ni chemin absolu.
#[test]
fn escaping_entries_are_refused() {
    let ok = |k: char, p: &str| restore::check_entries(&[(k, p.to_string())]);
    ok('d', "penelope").unwrap();
    ok('-', "penelope/vault/memoire.md").unwrap();
    ok('-', "./penelope/config.toml").unwrap();
    for (k, p) in [
        ('-', "penelope/../../etc/passwd"),
        ('-', "/etc/passwd"),
        ('-', "autre/fichier"),
        ('-', "../penelope/x"),
        ('l', "penelope/vault/lien"),
        ('h', "penelope/vault/dur"),
    ] {
        let e = ok(k, p).unwrap_err().to_string();
        assert!(e.contains("archive refusée"), "{p} : {e}");
    }
}

/// Une archive retouchée qui porte un lien symbolique est refusée entière : rien n'est
/// extrait ni restauré.
#[cfg(unix)]
#[test]
fn an_archive_with_a_link_is_not_restored() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src/penelope");
    std::fs::create_dir_all(src.join("vault")).unwrap();
    std::fs::write(src.join("MANIFEST.json"), "{}").unwrap();
    std::os::unix::fs::symlink("/etc", src.join("vault/etc")).unwrap();
    let tar = dir.path().join("a.tar.gz");
    penelope_platform::process::create_tar_gz(&tar, &dir.path().join("src"), &["penelope".into()])
        .unwrap();
    let sealed = dir.path().join("penelope-2026-01-01.tar.gz.enc");
    penelope_platform::archive::seal(&tar, &sealed, "phrase").unwrap();
    let home = dir.path().join("neuve");
    let dirs = penelope_platform::RootedDirs::new(&home);
    penelope_platform::Directories::ensure_all(&dirs).unwrap();
    let e = restore::restore_archive(&dirs, &sealed, "phrase", None, false, &|_| true)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("archive refusée") && e.contains("vault/etc"),
        "{e}"
    );
    assert!(std::fs::symlink_metadata(home.join("data/vault/etc")).is_err());
    let left = std::fs::read_dir(home.join("data/backups"))
        .map(|r| r.count())
        .unwrap_or(0);
    assert_eq!(left, 0, "rien d'extrait ne reste");
}
