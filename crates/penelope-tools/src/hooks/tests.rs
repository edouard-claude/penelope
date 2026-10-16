use super::*;

fn declared(args: &[&str]) -> Hook {
    serde_json::from_value(serde_json::json!({
        "name": "essai",
        "on": "post_tool",
        "match": {"tool": "fs_write|fs_edit", "path": "**/*.rs"},
        "command": "printf",
        "args": args,
        "timeout": "5s",
    }))
    .unwrap()
}

#[test]
fn matching_follows_the_event_the_tool_and_the_path() {
    let h = declared(&["%s", "{file}"]);
    assert!(h.applies(
        Event::PostTool,
        Some("fs_edit"),
        Some("crates/a/src/lib.rs")
    ));
    assert!(h.applies(Event::PostTool, Some("fs_write"), Some("main.rs")));
    assert!(!h.applies(Event::PreTool, Some("fs_edit"), Some("main.rs")));
    assert!(!h.applies(Event::PostTool, Some("shell_exec"), Some("main.rs")));
    assert!(!h.applies(Event::PostTool, Some("fs_edit"), Some("README.md")));
    assert!(path_matches("src/*.rs", "src/a.rs"));
    assert!(!path_matches("src/*.rs", "src/x/a.rs"));
    assert!(path_matches("**", "n/importe/quoi"));
    assert_eq!(h.timeout, Duration::from_secs(5));
    assert_eq!(h.on_failure, OnFailure::Continue);
}

/// #382 : chaque élément est rendu à part ; une valeur avec un caractère du shell ne
/// lance pas le hook, une valeur avec une espace reste un seul argument.
#[test]
fn values_are_rendered_per_element_and_shell_characters_refuse_the_hook() {
    let h = declared(&["check", "{file}"]);
    let vars = |f: &str| Vars {
        file: Some(f.into()),
        ..Default::default()
    };
    let r = h.render(&vars("a b.rs")).unwrap();
    assert_eq!(r.program, "printf");
    assert_eq!(r.args, vec!["check".to_string(), "a b.rs".to_string()]);
    assert_eq!(h.render(&vars("a;b.rs")).unwrap_err(), UNSAFE_VALUE);
    assert_eq!(h.render(&vars("$(id).rs")).unwrap_err(), UNSAFE_VALUE);
}

/// Hors `--hook` de mission, aucun hook ne passe par `sh`.
#[test]
fn only_the_mission_hook_goes_through_a_shell() {
    let m = Hook::mission("cargo fmt -- 'src/a.rs'");
    assert_eq!(m.command, "sh");
    assert_eq!(
        m.args,
        vec!["-c".to_string(), "cargo fmt -- 'src/a.rs'".to_string()]
    );
    let d = declared(&["{file}"]).render(&Vars::default());
    assert!(d.is_err(), "une valeur absente n'est pas un chemin sûr");
    let d = declared(&["fixe"]).render(&Vars::default()).unwrap();
    assert_ne!(d.program, "sh");
}

#[cfg(unix)]
#[tokio::test]
async fn a_declared_hook_is_spawned_without_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let host = penelope_platform::UnixProcessHost::new(dir.path().join("pids"));
    let r = declared(&["%s|", "{file}"])
        .render(&Vars {
            file: Some("a b.rs".into()),
            ..Default::default()
        })
        .unwrap();
    let out = crate::shell::exec_argv(
        &host,
        &r.program,
        &r.args,
        crate::shell::ExecOptions {
            timeout: Duration::from_secs(5),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Sans shell, l'argument arrive entier : une seule substitution, pas deux mots.
    assert_eq!(out.stdout, "a b.rs|");
    // Une ligne qu'un shell interpréterait reste un texte.
    let out = crate::shell::exec_argv(
        &host,
        "printf",
        &["%s".into(), "$(echo injecté); echo deux".into()],
        crate::shell::ExecOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(out.stdout, "$(echo injecté); echo deux");
}

const FMT: &str = r#"
on      = "post_tool"
match   = { tool = "fs_write|fs_edit", path = "**/*.rs" }
command = "cargo"
args    = ["fmt", "--check", "--", "{file}"]
cwd     = "{workspace}"
timeout = "90s"
sandbox = "workspace-write"
on_failure = "continue"
deliver = "note"
"#;

#[test]
fn a_declaration_is_read_from_its_file_and_its_rules_hold() {
    let h = parse("fmt", FMT).unwrap();
    assert_eq!(h.name, "fmt");
    assert_eq!(h.timeout, Duration::from_secs(90));
    assert_eq!(h.args, vec!["fmt", "--check", "--", "{file}"]);
    assert!(
        parse("fmt", &FMT.replace("post_tool", "pre_tool"))
            .unwrap_err()
            .contains("pas encore servi")
    );
    assert!(
        parse("fmt", &format!("{FMT}\nname = \"autre\""))
            .unwrap_err()
            .contains("nom du fichier")
    );
    let secret = FMT.replace("\"{file}\"]", "\"${SECRET:jeton}\"]");
    assert!(parse("fmt", &secret).unwrap_err().contains("`env`"));
    assert!(parse("mauvais nom", FMT).is_err());
    assert!(parse("fmt", "on = 3").is_err());
}

#[test]
fn the_directory_is_reloaded_when_it_changes_and_a_bad_file_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::new(dir.path().to_path_buf());
    assert!(reg.current().hooks.is_empty());
    std::fs::write(dir.path().join("fmt.toml"), FMT).unwrap();
    std::fs::write(
        dir.path().join("casse.toml"),
        "on = \"post_tool\"\ncommand = ",
    )
    .unwrap();
    let loaded = reg.current();
    assert_eq!(loaded.hooks.len(), 1);
    assert_eq!(loaded.invalid.len(), 1);
    assert_eq!(loaded.invalid[0].0, "casse");
    assert!(reg.get("fmt").is_some());
    std::fs::remove_file(dir.path().join("fmt.toml")).unwrap();
    assert!(reg.current().hooks.is_empty(), "retiré sans redémarrage");
}

#[test]
fn a_field_is_edited_only_into_a_valid_declaration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fmt.toml"), FMT).unwrap();
    let h = edit(dir.path(), "fmt", "args", serde_json::json!("--check")).unwrap();
    assert_eq!(h.args, vec!["--check"]);
    let h = edit(dir.path(), "fmt", "timeout", serde_json::json!("30s")).unwrap();
    assert_eq!(h.timeout, Duration::from_secs(30));
    assert!(edit(dir.path(), "fmt", "on", serde_json::json!("pre_tool")).is_err());
    // Refusé : le fichier n'a pas bougé.
    assert_eq!(
        parse(
            "fmt",
            &std::fs::read_to_string(dir.path().join("fmt.toml")).unwrap()
        )
        .unwrap()
        .on,
        Event::PostTool
    );
    assert!(edit(dir.path(), "fmt", "name", serde_json::json!("x")).is_err());
    assert!(
        edit(dir.path(), "absent", "args", serde_json::json!("x"))
            .unwrap_err()
            .contains("créer")
    );
}

#[test]
fn a_hook_never_runs_wider_than_the_configuration() {
    assert_eq!(Sandbox::ReadOnly.effective("workspace-write"), "readonly");
    assert_eq!(Sandbox::WorkspaceWrite.effective("readonly"), "readonly");
    assert_eq!(
        Sandbox::WorkspaceWrite.effective("workspace-write"),
        "workspace-write"
    );
    if cfg!(target_os = "macos") {
        assert_eq!(Sandbox::WorkspaceWrite.effective("full"), "workspace-write");
    } else {
        assert_eq!(Sandbox::WorkspaceWrite.effective("full"), "full");
    }
}
