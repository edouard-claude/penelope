use super::*;
use penelope_store::Store;

/// Deux échantillons tels que la boucle les écrit, l'un d'eux avec une clé que le
/// rédacteur de l'époque aurait laissée passer.
fn seed(store: &Store) {
    store
        .write_blocking(|tx| {
            for (call, day, command) in [
                ("c1", "2026-09-01", "ls; pwd"),
                (
                    "c2",
                    "2026-09-20",
                    "curl -H 'x-api-key: sk-proj-Abcdefghijklmnop0123' x; true",
                ),
            ] {
                tx.execute(
                    "INSERT INTO approval_samples(created_at, created_ms, session_id, call_id,
                        command_sha, input, floors, outcome, via)
                     VALUES(?1, 0, 's1', ?2, '0123456789abcdef', ?3,
                        '{\"policy\":\"ask\"}', 'approved', 'telegram')",
                    rusqlite::params![
                        format!("{day}T10:00:00.000Z"),
                        call,
                        json!({"command": command}).to_string()
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

/// Les fichiers de la base et leur empreinte (`-shm` exclu : SQLite le touche en
/// lecture).
fn fingerprint(db: &Path) -> Vec<(String, String)> {
    let dir = db.parent().unwrap();
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| !p.to_string_lossy().ends_with("-shm"))
        .map(|p| {
            let digest = penelope_kernel::canonical::sha256_hex(&std::fs::read(&p).unwrap());
            (p.to_string_lossy().into_owned(), digest)
        })
        .collect();
    out.sort();
    out
}

/// JSONL relisible, `"v": 1`, fichier en `0600`, base intacte, aucun secret en clair.
#[test]
fn export_writes_readable_private_jsonl_from_a_read_only_base() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("base").join("penelope.db");
    let store = Store::open(&db).unwrap();
    seed(&store);
    let out = dir.path().join("jeu.jsonl");
    // Un fichier déjà là, lisible par tous : il est remplacé, et refermé.
    std::fs::write(&out, "ancien").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    let before = fingerprint(&db);
    let conn = approval_stats::open_read_only(&db).unwrap();
    assert_eq!(export_approvals(&conn, None, &out).unwrap(), 2);
    drop(conn);
    assert_eq!(fingerprint(&db), before, "la base n'a pas bougé");

    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l["v"] == 1));
    assert_eq!(lines[0]["call_id"], "c1");
    assert_eq!(lines[0]["input"]["command"], "ls; pwd");
    assert_eq!(lines[0]["floors"]["policy"], "ask");
    assert_eq!(lines[0]["outcome"], "approved");
    assert_eq!(lines[0]["command_sha"], "0123456789abcdef");
    assert!(!text.contains("sk-proj-Abcdefghijklmnop0123"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // `--since` : le premier jour gardé.
    let conn = approval_stats::open_read_only(&db).unwrap();
    assert_eq!(
        export_approvals(&conn, Some("2026-09-02"), &out).unwrap(),
        1
    );
    assert_eq!(
        export_approvals(&conn, Some("2026-09-20"), &out).unwrap(),
        1
    );
    store.close();
}

#[test]
fn since_must_be_a_date() {
    let cli = Cli::try_parse_from([
        "penelope",
        "dataset",
        "export",
        "--kind",
        "approvals",
        "--since",
        "hier",
        "--out",
        "x.jsonl",
    ])
    .unwrap();
    let Command::Dataset(cmd) = &cli.command else {
        panic!("{:?}", cli.command);
    };
    assert!(matches!(run(&cli, cmd), Err(CliError::Usage(_))));
    assert!(
        Cli::try_parse_from([
            "penelope", "dataset", "export", "--kind", "autre", "--out", "x"
        ])
        .is_err()
    );
}
