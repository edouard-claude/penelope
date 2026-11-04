use super::*;

/// #397 : un terminal partagé se lit nettoyé et cité comme donnée ; fermé, il ne se lit
/// plus ; sans désignation, le seul ouvert est lu, plusieurs donnent la liste.
#[test]
fn a_shared_terminal_is_read_clean_and_quoted_until_it_closes() {
    let state = tempfile::tempdir().unwrap();
    let st = state.path();
    assert_eq!(tool(st, &json!({})).unwrap()["terminals"], json!([]));

    let log = register(st, "t1", "zsh", "2026-11-04T10:00:00Z").unwrap();
    std::fs::write(&log, "$ cargo build\r\n\x1b[31merror\x1b[0m: E0425\r\n$ ").unwrap();
    let v = tool(st, &json!({"lines": 2})).unwrap();
    assert_eq!(v["terminal"], "t1");
    let screen = v["screen"].as_str().unwrap();
    assert!(screen.contains("error: E0425"), "{screen}");
    assert!(
        !screen.contains("cargo build"),
        "deux lignes seulement : {screen}"
    );
    assert!(!screen.contains('\x1b'));
    assert!(screen.starts_with("<<<DONNÉES NON FIABLES"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir(st)), 0o700);
        assert_eq!(mode(&log), 0o600);
    }

    register(st, "t2", "bash", "2026-11-04T11:00:00Z").unwrap();
    let both = tool(st, &json!({})).unwrap();
    assert_eq!(both["terminals"][0]["id"], "t2", "le plus récent d'abord");
    assert!(tool(st, &json!({"terminal": "t9"})).is_err());

    unregister(st, "t1");
    assert!(!log.exists());
    assert!(
        tool(st, &json!({"terminal": "t1"})).is_err(),
        "fermé : illisible"
    );
    assert!(log_path(st, "../x").is_none());
}
