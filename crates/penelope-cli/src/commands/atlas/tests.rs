use super::*;

/// Un GET contre `handle`, sur une connexion locale.
fn get(vault: &Path, cache: &Arc<Mutex<Cache>>, request: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (v, c) = (vault.to_path_buf(), cache.clone());
    let server = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        handle(s, &v, &c);
    });
    let mut client = TcpStream::connect(addr).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    let mut out = String::new();
    client.read_to_string(&mut out).unwrap();
    server.join().unwrap();
    out
}

fn json_of(resp: &str) -> serde_json::Value {
    serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

/// #428 : la page porte les données du moment ; une note ajoutée paraît au GET suivant,
/// sans redémarrer ; une lecture qui échoue garde le dernier atlas, marqué périmé.
#[test]
fn each_refresh_serves_the_current_vault_and_says_when_it_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(vault.join("concepts")).unwrap();
    std::fs::write(vault.join("concepts/a.md"), "[[b]]\n").unwrap();
    std::fs::write(vault.join("concepts/b.md"), "B\n").unwrap();
    let cache = Arc::new(Mutex::new(Cache::default()));

    let page = get(&vault, &cache, "GET / HTTP/1.1\r\n\r\n");
    assert!(page.starts_with("HTTP/1.1 200 OK"));
    assert!(page.contains("Cache-Control: no-store"));
    assert!(page.contains("\"title\":\"a\"") && !page.contains("__ATLAS_DATA__"));

    std::fs::write(vault.join("concepts/c.md"), "[[a]] </script>\n").unwrap();
    let v = json_of(&get(&vault, &cache, "GET /atlas.json HTTP/1.1\r\n\r\n"));
    assert_eq!(v["stats"]["notes"], 3);
    assert_eq!(v["stats"]["liens"], 2);
    assert_eq!(v["meta"]["stale"], false);
    let fresh_at = v["meta"]["computed_at"].clone();

    std::fs::remove_dir_all(&vault).unwrap();
    let v = json_of(&get(&vault, &cache, "GET /atlas.json HTTP/1.1\r\n\r\n"));
    assert_eq!(v["meta"]["stale"], true);
    assert_eq!(v["meta"]["error"], "lecture du vault impossible");
    assert_eq!(
        v["meta"]["computed_at"], fresh_at,
        "la date du dernier calcul réussi"
    );
    assert_eq!(
        v["stats"]["notes"], 3,
        "le dernier atlas, pas un atlas vide"
    );
}

/// Deux routes, en lecture : rien d'autre, ni fichier du vault ni traversée de chemin.
#[test]
fn only_the_page_and_its_data_are_served() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.md"), "secret\n").unwrap();
    let cache = Arc::new(Mutex::new(Cache::default()));
    for path in [
        "/note.md",
        "/../config.toml",
        "/%2e%2e/etc/passwd",
        "/vault-graph.html",
    ] {
        let r = get(dir.path(), &cache, &format!("GET {path} HTTP/1.1\r\n\r\n"));
        assert!(r.starts_with("HTTP/1.1 404"), "{path} : {r}");
        assert!(!r.contains("secret"));
    }
    let r = get(dir.path(), &cache, "POST / HTTP/1.1\r\n\r\n");
    assert!(r.starts_with("HTTP/1.1 405"));
    let r = get(dir.path(), &cache, "HEAD / HTTP/1.1\r\n\r\n");
    assert!(
        r.starts_with("HTTP/1.1 200") && r.ends_with("\r\n\r\n"),
        "HEAD sans corps"
    );
}
