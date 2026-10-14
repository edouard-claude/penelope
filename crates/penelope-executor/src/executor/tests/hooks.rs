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
