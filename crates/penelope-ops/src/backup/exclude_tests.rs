//! #341 : ce que la sauvegarde laisse hors du workspace (règles Git des dépôts,
//! `.penelopeignore`, `backup.exclude`, caches reconnus) et ce qu'elle garde.

use super::*;

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// Copie `src` comme le workspace, et rend les chemins copiés et la collecte.
fn collect(src: &Path, patterns: &[String]) -> (Vec<String>, Collect) {
    let dst = tempfile::tempdir().unwrap();
    let mut c = Collect::new(patterns);
    c.copy_root(src, &dst.path().join("w"), "workspace")
        .unwrap();
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().to_string());
            }
        }
    }
    walk(&dst.path().join("w"), &dst.path().join("w"), &mut out);
    out.sort();
    (out, c)
}

fn rule_of<'a>(c: &'a Collect, path: &str) -> Option<&'a str> {
    c.excluded
        .iter()
        .find(|e| e.path == path)
        .map(|e| e.rule.as_str())
}

/// Les règles Git d'un dépôt, et d'un dépôt imbriqué : `target/` ignoré part, un fichier
/// non suivi mais non ignoré reste, une règle négative réinclut ; un `.gitignore` hors
/// dépôt ne vaut rien, comme pour Git ; le `.git` reste entier.
#[test]
fn git_rules_of_each_repository_apply() {
    let ws = tempfile::tempdir().unwrap();
    let w = ws.path();
    write(w, "app/.git/HEAD", "ref: refs/heads/main");
    write(w, "app/.git/info/exclude", "local-only/\n");
    write(w, "app/.gitignore", "/target\n*.log\n!keep.log\n");
    write(w, "app/Cargo.toml", "[package]");
    write(w, "app/target/debug/app", &"x".repeat(5000));
    write(w, "app/src/main.rs", "fn main() {}");
    write(w, "app/nouveau-non-suivi.txt", "à garder");
    write(w, "app/trace.log", "bruit");
    write(w, "app/keep.log", "à garder malgré *.log");
    write(w, "app/local-only/note", "exclu par info/exclude");
    write(w, "app/build/out.txt", "aucune règle : gardé");
    write(w, "app/sous/.git/HEAD", "ref");
    write(w, "app/sous/.gitignore", "build/\n");
    write(w, "app/sous/build/out.txt", "ignoré par le dépôt imbriqué");
    write(w, "notes/.gitignore", "*.md\n");
    write(
        w,
        "notes/idee.md",
        "hors dépôt : le .gitignore ne vaut rien",
    );

    let (kept, c) = collect(w, &[]);
    for k in [
        "app/.git/HEAD",
        "app/.git/info/exclude",
        "app/src/main.rs",
        "app/nouveau-non-suivi.txt",
        "app/keep.log",
        "app/build/out.txt",
        "app/sous/.git/HEAD",
        "notes/idee.md",
    ] {
        assert!(kept.contains(&k.to_string()), "{k} manque : {kept:?}");
    }
    for gone in [
        "app/target/debug/app",
        "app/trace.log",
        "app/local-only/note",
        "app/sous/build/out.txt",
    ] {
        assert!(!kept.contains(&gone.to_string()), "{gone} copié");
    }
    assert_eq!(
        rule_of(&c, "workspace/app/target"),
        Some(".gitignore : /target")
    );
    assert_eq!(
        rule_of(&c, "workspace/app/local-only"),
        Some("exclude : local-only/")
    );
    assert_eq!(
        rule_of(&c, "workspace/app/sous/build"),
        Some(".gitignore : build/")
    );
    let entry = c.excluded_entry().unwrap();
    assert_eq!(entry["name"], "workspace");
    assert_eq!(entry["count"], 4);
    assert_eq!(entry["items"][0]["path"], "workspace/app/target", "{entry}");
    assert_eq!(entry["items"][0]["bytes"], 5000);
}

/// Les caches reconnus partent avec ou sans dépôt Git, même quand le `.gitignore` du
/// dépôt ne les nomme pas (les `.gocache` de l'instance du 11/10) ; un `target/` sans
/// `Cargo.toml` à côté reste ; un dépôt qui réinclut un cache a le dernier mot.
#[test]
fn known_build_caches_go_with_or_without_git() {
    let ws = tempfile::tempdir().unwrap();
    let w = ws.path();
    write(w, "mcp/.git/HEAD", "ref");
    write(w, "mcp/.gitignore", "/mcp-bin\n");
    write(w, "mcp/.gocache/ab/cd", "cache go");
    write(w, "mcp/.gomodcache/mod", "modules");
    write(w, "mcp/main.go", "package main");
    write(w, "helloasso/Cargo.toml", "[package]");
    write(w, "helloasso/target/release/h", "binaire");
    write(w, "front/node_modules/left-pad/index.js", "module");
    write(w, "front/src/app.js", "code");
    write(w, "py/.venv/bin/python", "venv");
    write(w, "py/pkg/__pycache__/m.pyc", "bytecode");
    write(
        w,
        "rapports/target/objectifs.md",
        "pas de Cargo.toml : gardé",
    );
    write(w, "garde/.git/HEAD", "ref");
    write(w, "garde/.gitignore", "!node_modules/\n");
    write(w, "garde/node_modules/vendu.js", "réinclus par le dépôt");

    let (kept, c) = collect(w, &[]);
    for k in [
        "mcp/main.go",
        "front/src/app.js",
        "rapports/target/objectifs.md",
        "garde/node_modules/vendu.js",
    ] {
        assert!(kept.contains(&k.to_string()), "{k} manque : {kept:?}");
    }
    for gone in [
        ".gocache",
        ".gomodcache",
        "target/release",
        "node_modules/left",
        ".venv",
        "__pycache__",
    ] {
        assert!(
            !kept.iter().any(|k| k.contains(gone)),
            "{gone} copié : {kept:?}"
        );
    }
    assert_eq!(
        rule_of(&c, "workspace/mcp/.gocache"),
        Some("cache reconnu : .gocache/")
    );
    assert_eq!(
        rule_of(&c, "workspace/helloasso/target"),
        Some("cache reconnu : target/ à côté d'un Cargo.toml")
    );
}

/// `backup.exclude` et `.penelopeignore` : leurs motifs écartent, leurs `!` réincluent ;
/// un motif illisible est dit avant toute copie.
#[test]
fn configured_patterns_and_penelopeignore_apply() {
    let ws = tempfile::tempdir().unwrap();
    let w = ws.path();
    write(w, "exports/gros.csv", "gros");
    write(w, "exports/petit.csv", "petit");
    write(w, "downloads/video.mp4", "lourd");
    write(w, "downloads/facture.pdf", "à garder");
    write(
        w,
        "downloads/.penelopeignore",
        "*\n!facture.pdf\n!.penelopeignore\n",
    );
    write(w, "tmp/scratch", "jetable");

    let patterns = vec![
        "exports/*.csv".to_string(),
        "!exports/petit.csv".into(),
        "/tmp/".into(),
    ];
    let (kept, c) = collect(w, &patterns);
    assert_eq!(
        kept,
        vec![
            "downloads/.penelopeignore".to_string(),
            "downloads/facture.pdf".into(),
            "exports/petit.csv".into(),
        ]
    );
    assert_eq!(rule_of(&c, "workspace/tmp"), Some("/tmp/"));
    assert_eq!(
        rule_of(&c, "workspace/downloads/video.mp4"),
        Some(".penelopeignore : *")
    );

    let dst = tempfile::tempdir().unwrap();
    let e = Collect::new(&["[z-a]".to_string()])
        .copy_root(w, dst.path(), "workspace")
        .unwrap_err()
        .to_string();
    assert!(e.contains("backup.exclude") && e.contains("[z-a]"), "{e}");
}

/// Hors du workspace, rien n'est écarté : le vault ou `mcp-data` sont copiés entiers.
#[test]
fn other_roots_are_copied_whole() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    write(r, ".git/HEAD", "ref");
    write(r, ".gitignore", "*.md\n");
    write(r, "node_modules/x.js", "gardé hors workspace");
    write(r, "note.md", "gardé hors workspace");
    let dst = tempfile::tempdir().unwrap();
    let mut c = Collect::new(&["note.md".to_string()]);
    c.copy_root(r, &dst.path().join("v"), "mcp-data").unwrap();
    assert!(c.excluded.is_empty());
    assert!(dst.path().join("v/node_modules/x.js").exists());
    assert!(dst.path().join("v/note.md").exists());
}

/// Bout en bout : l'archive n'a pas le `target/` du dépôt, le manifeste dit ce qui est
/// resté dehors, et la restauration le met dans ce qui reste à faire.
#[tokio::test]
async fn the_archive_leaves_regenerable_files_out_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::new(1_789_516_800_000));
    let s = Services::for_tests(dir.path().join("home"), clock)
        .await
        .unwrap();
    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    let ws = s.platform.dirs.data().join("workspace");
    write(&ws, "ent1-mcp/.git/HEAD", "ref");
    write(&ws, "ent1-mcp/.gitignore", "/target\n");
    write(&ws, "ent1-mcp/target/debug/ent1", &"x".repeat(4096));
    write(&ws, "ent1-mcp/src/main.rs", "fn main() {}");

    let (archive, report) = build(&s, false).await.unwrap();
    let names = penelope_platform::process::list_tar_gz_from(
        penelope_platform::archive::open_reader(&archive, "phrase").unwrap(),
    )
    .unwrap();
    assert!(names.contains(&"penelope/workspace/ent1-mcp/src/main.rs".to_string()));
    assert!(!names.iter().any(|n| n.contains("target")), "{names:?}");
    let ws_entry = report["manifest"]["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "workspace")
        .cloned()
        .unwrap();
    assert_eq!(ws_entry["bytes"], 4096, "{ws_entry}");
    assert_eq!(ws_entry["items"][0]["rule"], ".gitignore : /target");
    let todo = restore::todo(&report["manifest"], &[], false, &|_| true);
    assert!(
        todo.iter()
            .any(|t| t.contains("régénérer") && t.contains("1 élément")),
        "{todo:?}"
    );
}
