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
