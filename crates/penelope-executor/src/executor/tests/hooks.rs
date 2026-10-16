//! Hook après édition (#366) : la commande `--hook` de la mission, jamais un fichier du
//! dépôt, et jamais pour un chemin qui porte un caractère du shell.
use super::*;

#[tokio::test]
async fn the_mission_hook_runs_after_an_edit_and_a_repository_file_does_not() {
    let (d, x) = executor().await;
    let ws = d.path().join("ws");
    // Un fichier de hook écrit dans le dépôt (par l'agent, par exemple) n'est pas lu.
    std::fs::create_dir_all(ws.join(".penelope")).unwrap();
    std::fs::write(ws.join(".penelope/after-edit"), "touch pwned").unwrap();
    let r = x
        .execute(
            "fs_write",
            &json!({"path":"src/a.rs","content":"fn a() {}\n"}),
        )
        .await
        .unwrap();
    assert!(!r.text.contains("\"hook\""), "{}", r.text);
    assert!(!ws.join("pwned").exists());

    penelope_app::mission::start(
        &x.services,
        "s1",
        penelope_app::mission::Request {
            goal: "écrire a.rs".into(),
            cwd: Some(ws.to_string_lossy().into_owned()),
            hook: Some("grep -q TODO {file} && { echo 'TODO restant'; exit 3; }; true".into()),
            ..Default::default()
        },
        &Origin::Cli,
    )
    .await
    .unwrap();
    let r = x
        .execute(
            "fs_edit",
            &json!({"path":"src/a.rs","old":"fn a() {}","new":"fn a() {} // TODO"}),
        )
        .await
        .unwrap();
    assert!(r.text.contains("\"output\""), "{}", r.text);
    assert!(
        r.text.contains("\"exitCode\": 3") || r.text.contains("\"exitCode\":3"),
        "{}",
        r.text
    );

    let r = x
        .execute(
            "fs_edit",
            &json!({"path":"src/a.rs","old":" // TODO","new":""}),
        )
        .await
        .unwrap();
    assert!(r.text.contains("\"hook\""), "{}", r.text);
    assert!(
        !r.text.contains("\"output\""),
        "un hook vert ne renvoie que son code : {}",
        r.text
    );

    // Un nom de fichier piégé ne lance rien.
    let r = x
        .execute(
            "fs_write",
            &json!({"path":"src/$(touch pwned).rs","content":"x"}),
        )
        .await
        .unwrap();
    assert!(r.text.contains("skipped"), "{}", r.text);
    assert!(!ws.join("pwned").exists());
}

/// #383 : un hook `post_tool` de `hooks.d/` tourne après l'outil qu'il vise, sans shell
/// pour ses arguments ; son verdict `block` fait de l'appel une erreur ; le retirer
/// l'arrête sans redémarrage.
#[tokio::test]
async fn a_post_tool_hook_from_hooks_d_runs_blocks_and_goes_away() {
    let (_d, x) = executor().await;
    let dir = x.services.platform.dirs.hooks_d();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("todo.toml"),
        r#"
on = "post_tool"
match = { tool = "fs_write|fs_edit", path = "**/*.rs" }
command = "sh"
args = ["-c", "grep -q TODO \"$1\" && { echo '{\"verdict\":\"block\",\"reason\":\"TODO restant\"}'; exit 2; }; echo propre", "hook", "{file}"]
timeout = "10s"
"#,
    )
    .unwrap();

    let ok = x
        .execute(
            "fs_write",
            &json!({"path":"src/a.rs","content":"fn a() {}\n"}),
        )
        .await
        .unwrap();
    assert!(!ok.is_error, "{}", ok.text);
    assert!(
        ok.text.contains("[hook todo : continue]") && ok.text.contains("propre"),
        "{}",
        ok.text
    );

    let blocked = x
        .execute(
            "fs_write",
            &json!({"path":"src/b.rs","content":"// TODO\n"}),
        )
        .await
        .unwrap();
    assert!(blocked.is_error, "{}", blocked.text);
    assert!(
        blocked.text.contains("TODO restant") && blocked.text.contains("bloque"),
        "{}",
        blocked.text
    );

    // Un fichier qui n'est pas visé ne lance rien.
    let md = x
        .execute("fs_write", &json!({"path":"NOTES.md","content":"TODO"}))
        .await
        .unwrap();
    assert!(!md.text.contains("[hook"), "{}", md.text);

    std::fs::remove_file(dir.join("todo.toml")).unwrap();
    let gone = x
        .execute(
            "fs_write",
            &json!({"path":"src/c.rs","content":"// TODO\n"}),
        )
        .await
        .unwrap();
    assert!(
        !gone.is_error && !gone.text.contains("[hook"),
        "{}",
        gone.text
    );
}

/// #383 : `hooks.d/` n'est jamais écrit par un outil, même quand un workspace l'englobe.
#[tokio::test]
async fn hooks_d_cannot_be_written_by_a_tool() {
    let (d, x) = executor().await;
    let root = d.path().to_string_lossy().to_string();
    x.services
        .config
        .mutate("test", |cfg| {
            cfg.sandbox.workspaces = vec![root];
            Ok(vec!["sandbox.workspaces".into()])
        })
        .unwrap();
    let target = x.services.platform.dirs.hooks_d().join("x.toml");
    let mut env = x.env.clone();
    env.workspaces = vec![penelope_platform::sandbox::normalise(d.path())];
    let x = NativeToolExecutor::new(x.services.clone(), env);
    let e = x
        .execute(
            "fs_write",
            &json!({"path": target.to_string_lossy(), "content": "on = \"post_tool\""}),
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("hooks.d"), "{e}");
    assert!(!target.exists());
}

/// Revue de sécurité du 16/10 : la commande d'un hook est résolue comme au lancement avant
/// d'être jugée ; un secret passé par `env` ne ressort pas dans le résultat.
#[tokio::test]
async fn a_hook_command_in_a_workspace_is_refused_and_env_secrets_stay_hidden() {
    let (d, x) = executor().await;
    let ws = d.path().join("ws");
    std::fs::write(ws.join("outil.sh"), "#!/bin/sh\necho piégé\n").unwrap();
    let ws_list = vec![penelope_platform::sandbox::normalise(&ws)];
    assert!(
        super::super::post_hooks::command_in_workspace(
            &ws.join("outil.sh").to_string_lossy(),
            &ws_list
        )
        .is_some()
    );
    assert!(super::super::post_hooks::command_in_workspace("sh", &ws_list).is_none());
    // Relative, introuvable : refusées, jamais laissées passer.
    assert!(super::super::post_hooks::command_in_workspace("./outil.sh", &ws_list).is_some());
    assert!(
        super::super::post_hooks::command_in_workspace("commande-qui-n-existe-pas", &ws_list)
            .is_some()
    );

    let dir = x.services.platform.dirs.hooks_d();
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY du test : variable propre à ce processus de test.
    std::fs::write(
        dir.join("jeton.toml"),
        r#"
on = "post_tool"
match = { tool = "fs_write" }
command = "sh"
args = ["-c", "echo \"jeton=$JETON\"; exit 1"]
env = { JETON = "${SECRET:jeton_hook}" }
"#,
    )
    .unwrap();
    x.services
        .platform
        .secrets
        .set("jeton_hook", "valeur-tres-secrete-42")
        .unwrap();
    let r = x
        .execute("fs_write", &json!({"path":"note.txt","content":"x"}))
        .await
        .unwrap();
    assert!(r.text.contains("[hook jeton"), "{}", r.text);
    assert!(!r.text.contains("valeur-tres-secrete-42"), "{}", r.text);
}
