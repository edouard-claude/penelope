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

/// Une racine `data/workspace` qui est elle-même un lien (vers `/`, ici vers un dossier
/// hors du home) n'est pas suivie : seule la configuration peut désigner une racine.
#[cfg(unix)]
#[tokio::test]
async fn a_linked_root_is_not_followed() {
    let (dir, s) = services().await;
    let elsewhere = dir.path().join("ailleurs");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("prive.txt"), "hors racine").unwrap();
    let ws = s.platform.dirs.data().join("workspace");
    let _ = std::fs::remove_dir_all(&ws);
    std::os::unix::fs::symlink(&elsewhere, &ws).unwrap();
    let (archive, report) = build(&s, false).await.unwrap();
    let skipped = report["manifest"]["links_skipped"].to_string();
    assert!(skipped.contains("\"workspace\""), "{skipped}");
    let plain = penelope_platform::archive::open_to_vec(&archive, "phrase").unwrap();
    let names = penelope_platform::process::list_tar_gz_bytes(&plain).unwrap();
    assert!(!names.iter().any(|n| n.contains("prive")), "{names:?}");
}

/// Un en-tête tar (ustar) pour un fichier `name` de `len` octets.
fn ustar_header(name: &str, len: usize) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(format!("{len:011o}").as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| *b as u32).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    h
}

/// Une archive malveillante, forgée à la main avec une entrée `penelope/../../evade.txt` :
/// refusée entière, rien n'est écrit hors de la destination ni dans le home.
#[test]
fn a_malicious_archive_with_dot_dot_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let body = b"je sors de la destination";
    let mut tar = Vec::new();
    tar.extend_from_slice(&ustar_header("penelope/MANIFEST.json", 2));
    let mut block = [0u8; 512];
    block[..2].copy_from_slice(b"{}");
    tar.extend_from_slice(&block);
    tar.extend_from_slice(&ustar_header("penelope/../../evade.txt", body.len()));
    let mut block = [0u8; 512];
    block[..body.len()].copy_from_slice(body);
    tar.extend_from_slice(&block);
    tar.extend_from_slice(&[0u8; 1024]);
    let raw = dir.path().join("a.tar");
    std::fs::write(&raw, &tar).unwrap();
    let gz = std::process::Command::new("gzip")
        .args(["-kf"])
        .arg(&raw)
        .status()
        .unwrap();
    assert!(gz.success());
    let tar_gz = dir.path().join("a.tar.gz");
    let entries =
        penelope_platform::process::tar_gz_entries(&std::fs::read(&tar_gz).unwrap()).unwrap();
    assert!(entries.iter().any(|(_, p)| p.contains("..")), "{entries:?}");
    let sealed = dir.path().join("penelope-2026-01-01.tar.gz.enc");
    penelope_platform::archive::seal(&tar_gz, &sealed, "phrase").unwrap();

    let home = dir.path().join("h/neuve");
    let dirs = penelope_platform::RootedDirs::new(&home);
    penelope_platform::Directories::ensure_all(&dirs).unwrap();
    let e = restore::restore_archive(&dirs, &sealed, "phrase", None, false, &|_| true)
        .unwrap_err()
        .to_string();
    assert!(e.contains("archive refusée") && e.contains(".."), "{e}");
    for escaped in [dir.path().join("evade.txt"), dir.path().join("h/evade.txt")] {
        assert!(!escaped.exists(), "{}", escaped.display());
    }
    let walk = |p: &Path| std::fs::read_dir(p).map(|r| r.count()).unwrap_or(0);
    assert_eq!(
        walk(&home.join("data/backups")),
        0,
        "rien d'extrait ne reste"
    );
    // Fichiers spéciaux : refusés comme les liens.
    for k in ['p', 'c', 'b'] {
        assert!(restore::check_entries(&[(k, "penelope/x".into())]).is_err());
    }
}
