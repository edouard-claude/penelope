/// #66 : un lien symbolique déposé dans le workspace n'ouvre pas le reste du disque,
/// même pour un fichier qui n'existe pas encore sous le lien. Un lien interne reste
/// accepté, et un workspace lui-même lien (cas de `/tmp` sur macOS) marche.
#[test]
#[cfg(unix)]
fn a_symlink_out_of_the_workspace_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    let outside = dir.path().join("dehors");
    std::fs::create_dir_all(ws.join("interne")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "contenu hors workspace").unwrap();
    std::fs::write(ws.join("interne/note.txt"), "dedans").unwrap();
    std::os::unix::fs::symlink(&outside, ws.join("lien")).unwrap();
    std::os::unix::fs::symlink(ws.join("interne"), ws.join("lien-interne")).unwrap();
    let workspaces = vec![ws.clone()];

    for p in [
        "lien/secret.txt",
        "lien/pas-encore-la.txt",
        "lien/sous/dossier/x.txt",
    ] {
        let e = resolve(p, &workspaces).unwrap_err();
        assert!(
            matches!(e, ToolError::Denied(_)),
            "`{p}` doit être refusé : {e}"
        );
    }
    assert!(resolve("lien-interne/note.txt", &workspaces).is_ok());
    assert!(resolve("interne/nouveau.txt", &workspaces).is_ok());

    // Workspace donné par un chemin qui est lui-même un lien : accepté.
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&ws, &alias).unwrap();
    assert!(resolve("interne/note.txt", &[alias]).is_ok());
}
use super::*;

fn ws() -> (tempfile::TempDir, Vec<PathBuf>) {
    let d = tempfile::tempdir().unwrap();
    let roots = vec![normalise(d.path())];
    (d, roots)
}

#[test]
fn resolve_rejects_paths_outside_the_workspace() {
    let (d, roots) = ws();
    assert!(resolve("src/main.rs", &roots).is_ok());
    assert!(resolve(&d.path().join("a.rs").display().to_string(), &roots).is_ok());
    let e = resolve("../../etc/passwd", &roots).unwrap_err();
    assert!(e.to_string().contains("hors des workspaces"));
    assert!(resolve("/etc/passwd", &roots).is_err());
}

/// #164 : la casse suit le volume. Sur APFS usuel, les deux écritures sont le
/// même dossier ; sur Linux ou APFS sensible, elles peuvent désigner deux dossiers.
#[test]
fn resolve_uses_the_filesystems_case_rules() {
    let d = tempfile::tempdir().unwrap();
    let actual = d.path().join("penelope");
    let typed = d.path().join("Penelope");
    std::fs::create_dir(&actual).unwrap();
    let roots = vec![actual.clone()];
    let requested = typed.join("new.txt");
    if std::fs::canonicalize(&typed).is_ok() {
        assert_eq!(
            resolve(requested.to_str().unwrap(), &roots).unwrap(),
            actual.canonicalize().unwrap().join("new.txt")
        );
    } else {
        std::fs::create_dir(&typed).unwrap();
        assert!(resolve(requested.to_str().unwrap(), &roots).is_err());
    }
}

#[test]
fn resolve_without_workspace_denies_everything() {
    assert!(resolve("a.rs", &[]).is_err());
}

#[test]
fn read_paginates_and_numbers_lines() {
    let (d, _) = ws();
    let p = d.path().join("a.txt");
    std::fs::write(
        &p,
        (1..=100)
            .map(|i| format!("ligne {i}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let r = read(&p, 10, 5).unwrap();
    assert_eq!(r["lines"], 100);
    assert_eq!(r["from"], 11);
    assert_eq!(r["to"], 15);
    assert_eq!(r["truncated"], true);
    let c = r["content"].as_str().unwrap();
    assert!(c.contains("ligne 11"));
    assert!(!c.contains("ligne 16"));
}

fn small_caps() -> ReadCaps {
    ReadCaps {
        count_total_below: 1024 * 1024,
        max_skip_bytes: 1024 * 1024,
        max_line_bytes: 1024,
        max_search_file_bytes: 1024 * 1024,
    }
}

/// Fichier de ~4 Mio, 200 000 lignes.
fn big_log(dir: &Path) -> PathBuf {
    let p = dir.join("gros.log");
    let mut s = String::with_capacity(4 * 1024 * 1024);
    for i in 0..200_000 {
        s.push_str(&format!("{i:08} ligne de journal assez ordinaire\n"));
    }
    std::fs::write(&p, s).unwrap();
    p
}

/// #94 : 50 lignes d'un gros fichier se lisent sans le parcourir : le total n'est
/// pas compté (et c'est dit), la suite est signalée.
#[test]
fn the_head_of_a_big_file_is_read_without_scanning_it() {
    let (d, _) = ws();
    let p = big_log(d.path());
    let t = std::time::Instant::now();
    let r = read_with(&p, 0, 50, small_caps()).unwrap();
    assert!(
        t.elapsed() < std::time::Duration::from_millis(200),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(r["lines"], Value::Null);
    assert_eq!(r["truncated"], true);
    assert_eq!(r["to"], 50);
    assert!(
        r["remarque"].as_str().unwrap().contains("pas compté"),
        "{r}"
    );
    assert!(
        r["content"]
            .as_str()
            .unwrap()
            .ends_with("00000049 ligne de journal assez ordinaire")
    );
}

/// #94 : un `offset` au-delà de ce qu'on accepte de parcourir est refusé avec la
/// marche à suivre ; au-delà de la fin d'un petit fichier, le contenu est vide.
#[test]
fn a_far_offset_is_bounded() {
    let (d, _) = ws();
    let p = big_log(d.path());
    let e = read_with(&p, 190_000, 10, small_caps()).unwrap_err();
    assert!(e.to_string().contains("tail -n"), "{e}");

    let small = d.path().join("petit.txt");
    std::fs::write(&small, "a\nb\n").unwrap();
    let r = read_with(&small, 10, 5, small_caps()).unwrap();
    assert_eq!(r["content"], "");
    assert_eq!(r["lines"], 2);
    assert_eq!(r["truncated"], false);
}

/// #94 : une ligne unique sans fin (JSON minifié, binaire déguisé) est tronquée, et
/// c'est dit.
#[test]
fn a_single_huge_line_is_truncated() {
    let (d, _) = ws();
    let p = d.path().join("minifie.json");
    std::fs::write(&p, "x".repeat(64 * 1024)).unwrap();
    let r = read_with(&p, 0, 10, small_caps()).unwrap();
    assert!(r["content"].as_str().unwrap().len() < 2 * 1024);
    assert!(r["remarque"].as_str().unwrap().contains("tronquée"), "{r}");
}

/// #94 : `fs_search` ignore un fichier au-delà du plafond et le nomme ; les autres
/// donnent les mêmes résultats qu'avant.
#[test]
fn search_skips_and_names_oversized_files() {
    let (d, _) = ws();
    big_log(d.path());
    std::fs::write(d.path().join("notes.log"), "rien\nerreur fatale ici\n").unwrap();
    let r = search_with(d.path(), "erreur", Some("*.log"), 10, small_caps()).unwrap();
    assert_eq!(r["hits"], 1);
    assert_eq!(r["results"][0]["line"], 2);
    assert!(
        r["ignorés"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("gros.log"),
        "{r}"
    );
}

#[test]
fn read_refuses_binary_files() {
    let (d, _) = ws();
    let p = d.path().join("bin");
    std::fs::write(&p, [0u8, 1, 2, 0, 3]).unwrap();
    assert!(read(&p, 0, 10).unwrap_err().to_string().contains("binaire"));
}

#[test]
fn list_is_sorted_and_bounded() {
    let (d, _) = ws();
    for i in 0..30 {
        std::fs::write(d.path().join(format!("f{i:02}.txt")), "x").unwrap();
    }
    let r = list(d.path(), false, 10).unwrap();
    assert_eq!(r["entries"], 10);
    let items = r["items"].as_array().unwrap();
    let first = items[0]["name"].as_str().unwrap();
    let second = items[1]["name"].as_str().unwrap();
    assert!(first < second, "la liste doit être triée");
}

#[test]
fn search_finds_matches_with_line_numbers() {
    let (d, _) = ws();
    std::fs::write(
        d.path().join("a.rs"),
        "fn main() {\n    let tva = 0.20;\n}\n",
    )
    .unwrap();
    std::fs::write(d.path().join("b.md"), "documentation\n").unwrap();
    let r = search(d.path(), r"tva", Some("*.rs"), 10).unwrap();
    assert_eq!(r["hits"], 1);
    assert_eq!(r["results"][0]["line"], 2);
    // Le glob exclut le markdown.
    let r = search(d.path(), "documentation", Some("*.rs"), 10).unwrap();
    assert_eq!(r["hits"], 0);
}

#[test]
fn search_rejects_invalid_regex() {
    let (d, _) = ws();
    assert!(search(d.path(), "[invalide", None, 10).is_err());
}

#[test]
fn glob_matching() {
    assert!(matches_glob("a.rs", Some("*.rs")));
    assert!(!matches_glob("a.md", Some("*.rs")));
    assert!(matches_glob("mod_test.rs", Some("*test*")));
    assert!(matches_glob("quoi que ce soit", None));
    assert!(matches_glob("x", Some("*")));
}

#[test]
fn write_creates_then_reports_a_diff() {
    let (d, _) = ws();
    let p = d.path().join("a.txt");
    let r = write(&p, "une\ndeux\n").unwrap();
    assert_eq!(r["created"], true);
    let r = write(&p, "une\ntrois\n").unwrap();
    assert_eq!(r["created"], false);
    let diff = r["diff"].as_str().unwrap();
    assert!(diff.contains("-deux"));
    assert!(diff.contains("+trois"));
    let (plus, minus) = diff_stats(diff);
    assert_eq!((plus, minus), (1, 1));
}

#[test]
fn edit_requires_a_unique_match() {
    let (d, _) = ws();
    let p = d.path().join("a.rs");
    std::fs::write(&p, "let x = 1;\nlet x = 1;\n").unwrap();

    let e = edit(&p, "let x = 1;", "let x = 2;", false).unwrap_err();
    assert!(e.to_string().contains("2 fois"), "{e}");

    let r = edit(&p, "let x = 1;", "let x = 2;", true).unwrap();
    assert_eq!(r["replaced"], 2);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "let x = 2;\nlet x = 2;\n"
    );
}

#[test]
fn edit_reports_a_missing_portion() {
    let (d, _) = ws();
    let p = d.path().join("a.rs");
    std::fs::write(&p, "contenu\n").unwrap();
    assert!(
        edit(&p, "absent", "x", false)
            .unwrap_err()
            .to_string()
            .contains("absente")
    );
    assert!(edit(&p, "", "x", false).is_err());
}

#[tokio::test]
async fn file_locks_serialise_writes_to_the_same_path() {
    let locks = std::sync::Arc::new(FileLocks::new());
    let p = PathBuf::from("/tmp/ws/a.rs");
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut hs = Vec::new();
    for _ in 0..8 {
        let (l, p, c, m) = (locks.clone(), p.clone(), counter.clone(), max.clone());
        hs.push(tokio::spawn(async move {
            let lock = l.for_path(&p);
            let _g = lock.lock().await;
            let now = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            m.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            c.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
    assert_eq!(
        max.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "une seule écriture à la fois sur un même fichier"
    );
    assert_eq!(locks.tracked(), 1);
}

#[test]
fn diff_handles_insertions_and_deletions() {
    let d = unified_diff("a\nb\nc\n", "a\nc\n", "f");
    assert!(d.contains("-b"));
    assert_eq!(diff_stats(&d), (0, 1), "une suppression, aucun ajout : {d}");

    let d = unified_diff("a\nc\n", "a\nb\nc\n", "f");
    assert!(d.contains("+b"));
    assert_eq!(diff_stats(&d), (1, 0));
}

#[test]
fn files_are_written_in_lf() {
    let (d, _) = ws();
    let p = d.path().join("a.txt");
    write(&p, "une\r\ndeux\r\n").unwrap();
    let raw = std::fs::read(&p).unwrap();
    assert!(!raw.windows(2).any(|w| w == b"\r\n"));
}
