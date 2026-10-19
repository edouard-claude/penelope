//! `penelope plugin list|show` (#393) : le vrai binaire, sans daemon.

#[test]
fn plugin_list_and_show_read_the_plugins_without_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    let paths = std::process::Command::new(env!("CARGO_BIN_EXE_penelope"))
        .args(["--home", home.path().to_str().unwrap(), "--json", "paths"])
        .output()
        .unwrap();
    let paths: serde_json::Value = serde_json::from_slice(&paths.stdout).unwrap();
    let root = std::path::Path::new(paths["data"].as_str().unwrap()).join("plugins/notes");
    std::fs::create_dir_all(root.join("skills/resume")).unwrap();
    std::fs::write(
        root.join("skills/resume/SKILL.md"),
        "---\nname: resume\ndescription: Résumer\n---\nRésume.\n",
    )
    .unwrap();
    std::fs::write(root.join(".mcp.json"), r#"{"index":{"command":"idx"}}"#).unwrap();
    let run = |args: &[&str]| {
        let mut all = vec!["--home", home.path().to_str().unwrap()];
        all.extend_from_slice(args);
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_penelope"))
            .args(all)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let (ok, list) = run(&["plugin", "list"]);
    assert!(ok);
    assert!(
        list.contains("notes") && list.contains("1 skill(s)") && list.contains("1 refusé(s)"),
        "{list}"
    );
    let (ok, show) = run(&["plugin", "show", "notes"]);
    assert!(ok);
    assert!(show.contains("notes/index"), "{show}");
    assert!(!run(&["plugin", "show", "absent"]).0);
    let src = home.path().join("source-plugin");
    std::fs::create_dir_all(src.join("agents")).unwrap();
    std::fs::write(
        src.join("agents/lecteur.md"),
        "---\nname: lecteur\ndescription: Lit\n---\nTu lis.\n",
    )
    .unwrap();
    let (ok, _) = run(&["plugin", "add", src.to_str().unwrap(), "--name", "lecteur"]);
    assert!(ok);
    assert!(run(&["plugin", "list"]).1.contains("lecteur"));
    assert!(run(&["plugin", "remove", "lecteur"]).0);
    assert!(!run(&["plugin", "list"]).1.contains("lecteur "));

    // Une marketplace au format Claude Code, puis un plugin installé depuis elle.
    let market = home.path().join("place");
    std::fs::create_dir_all(market.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(market.join("p/outil/skills/o")).unwrap();
    std::fs::write(
        market.join("p/outil/skills/o/SKILL.md"),
        "---\nname: o\ndescription: d\n---\nb\n",
    )
    .unwrap();
    std::fs::write(
        market.join(".claude-plugin/marketplace.json"),
        r#"{"name":"atelier","plugins":[{"name":"outil","source":"./p/outil","description":"Un outil"}]}"#,
    )
    .unwrap();
    assert!(run(&["plugin", "marketplace", "add", market.to_str().unwrap()]).0);
    assert!(
        run(&["plugin", "marketplace", "list"])
            .1
            .contains("outil@atelier")
    );
    assert!(run(&["plugin", "install", "outil@atelier"]).0);
    assert!(run(&["plugin", "list"]).1.contains("outil"));
}
