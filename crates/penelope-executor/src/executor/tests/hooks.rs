//! Hook après édition du dépôt (#366) : seulement en mission ou en run autonome.
use super::*;

#[tokio::test]
async fn the_repository_hook_runs_after_an_edit_only_in_a_mission() {
    let (d, x) = executor().await;
    let ws = d.path().join("ws");
    std::fs::create_dir_all(ws.join(".penelope")).unwrap();
    std::fs::write(
        ws.join(penelope_tools::edit_hooks::FILE),
        "echo \"vérifié : {file}\"; grep -q TODO {file} && { echo 'TODO restant'; exit 3; }; true\n",
    )
    .unwrap();

    // Conversation ordinaire : le fichier du dépôt ne lance rien.
    let r = x
        .execute(
            "fs_write",
            &json!({"path":"src/a.rs","content":"fn a() {}\n"}),
        )
        .await
        .unwrap();
    assert!(!r.text.contains("\"hook\""), "{}", r.text);

    penelope_app::mission::start(
        &x.services,
        "s1",
        penelope_app::mission::Request {
            goal: "écrire a.rs".into(),
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
    assert!(r.text.contains("TODO restant"), "{}", r.text);
    assert!(
        r.text.contains("\"exitCode\":3") || r.text.contains("\"exitCode\": 3"),
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
}
