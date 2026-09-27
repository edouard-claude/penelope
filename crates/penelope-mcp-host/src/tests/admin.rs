//! Entretien et administration : inactifs arrêtés, santé, plafond de processus,
//! prompts, refus de l'édition.

use super::*;
use penelope_app::ports::McpAdmin;

fn with_prompts(tools: Arc<Mutex<Vec<Value>>>) -> Handler {
    let base = server(tools);
    Arc::new(move |m, p| match m {
        "prompts/list" => Ok(json!({"prompts": [{"name": "resume", "description": "Résumé"}]})),
        "prompts/get" => Ok(json!({"messages": [{"role": "user",
            "content": {"type": "text", "text": format!("résume {}", p["arguments"]["sujet"])}}]})),
        _ => base(m, p),
    })
}

/// Un serveur lazy inactif au-delà de son délai est arrêté ; un serveur vivant muet
/// depuis une minute est sondé, et perdu s'il ne répond plus.
#[tokio::test]
async fn maintenance_stops_idle_servers_and_checks_health() {
    let (_d, _s, clock, fake, sup) = setup().await;
    fake.serve("lent", server(two_tools()));
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let a2 = alive.clone();
    let base = server(two_tools());
    fake.serve(
        "sonde",
        Arc::new(move |m, p| {
            if m == "ping" && !a2.load(Ordering::SeqCst) {
                return Err(McpError::Transport("plus personne".into()));
            }
            base(m, p)
        }),
    );
    declare(&sup, "lent", "idle_timeout = \"10m\"\n");
    declare(&sup, "sonde", "idle_timeout = \"1h\"\n");
    sup.reload().await;
    for name in ["lent", "sonde"] {
        sup.call(
            &format!("mcp__{name}__list_issues"),
            &json!({}),
            Default::default(),
        )
        .await
        .unwrap();
    }
    let running = |sup: Arc<McpSupervisor>| async move {
        sup.statuses()
            .await
            .into_iter()
            .filter(|s| s.running)
            .map(|s| s.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(running(sup.clone()).await, vec!["lent", "sonde"]);

    clock.advance_ms(2 * 60_000);
    sup.maintenance().await;
    assert_eq!(
        running(sup.clone()).await,
        vec!["lent", "sonde"],
        "sondés, toujours là"
    );
    let log = fake.last_transport("sonde").call_log().await;
    assert!(log.iter().any(|(m, _)| m == "ping"), "{log:?}");

    alive.store(false, Ordering::SeqCst);
    clock.advance_ms(11 * 60_000);
    sup.maintenance().await;
    assert!(running(sup.clone()).await.is_empty());
    let sonde = sup
        .statuses()
        .await
        .into_iter()
        .find(|s| s.name == "sonde")
        .unwrap();
    assert!(sonde.last_error.is_some(), "{sonde:?}");
}

/// Au-delà de `mcp.max_processes`, les serveurs lazy les moins récemment utilisés
/// s'arrêtent.
#[tokio::test]
async fn the_process_cap_stops_the_least_recently_used() {
    let (_d, s, clock, fake, sup) = setup().await;
    s.config
        .mutate("test", |c| {
            c.mcp.max_processes = 1;
            Ok(vec!["mcp.max_processes".into()])
        })
        .unwrap();
    for name in ["ancien", "recent"] {
        fake.serve(name, server(two_tools()));
        declare(&sup, name, "");
    }
    sup.reload().await;
    for name in ["ancien", "recent"] {
        sup.call(
            &format!("mcp__{name}__list_issues"),
            &json!({}),
            Default::default(),
        )
        .await
        .unwrap();
        clock.advance_ms(1_000);
    }
    sup.maintenance().await;
    let running: Vec<String> = sup
        .statuses()
        .await
        .into_iter()
        .filter(|s| s.running)
        .map(|s| s.name)
        .collect();
    assert_eq!(running, vec!["recent"]);
}

/// `/p` : prompts listés et rendus par leur serveur, via le port d'administration.
#[tokio::test]
async fn prompts_are_listed_and_rendered_through_the_admin_port() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve("docs", with_prompts(two_tools()));
    declare(&sup, "docs", "");
    sup.reload().await;
    let admin: Arc<dyn McpAdmin> = sup.clone();
    let prompts = admin.prompts("docs").await.unwrap();
    assert_eq!(prompts[0]["name"], "resume");
    let rendered = admin
        .get_prompt("docs", "resume", json!({"sujet": "la veille"}))
        .await
        .unwrap();
    assert!(
        rendered.to_string().contains("résume \\\"la veille\\\""),
        "{rendered}"
    );
    for err in [
        admin.prompts("absent").await.unwrap_err(),
        admin
            .get_prompt("absent", "x", json!({}))
            .await
            .unwrap_err(),
        admin.logs("absent", 5).await.unwrap_err(),
    ] {
        assert!(err.contains("penelope mcp list"), "{err}");
    }
}

/// Les modifications refusées le disent sans toucher à la déclaration.
#[tokio::test]
async fn invalid_edits_and_removals_are_refused() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve("a", server(two_tools()));
    declare(&sup, "a", "");
    sup.reload().await;
    let before = std::fs::read_to_string(sup.dir().join("a.toml")).unwrap();
    for (patch, why) in [
        (json!(["enabled"]), "objet JSON"),
        (json!({"name": "b"}), "ne change pas"),
        (json!({"couleur": "bleu"}), "champ inconnu"),
    ] {
        let err = sup.edit("a", &patch).await.unwrap_err();
        assert!(err.contains(why), "{patch} : {err}");
    }
    assert_eq!(
        std::fs::read_to_string(sup.dir().join("a.toml")).unwrap(),
        before
    );
    assert!(
        sup.remove("absent")
            .await
            .unwrap_err()
            .contains("penelope mcp list")
    );

    sup.set_enabled("a", false).await.unwrap();
    let err = sup.restart("a").await.unwrap_err();
    assert!(err.contains("penelope mcp enable a"), "{err}");
}

/// Une déclaration partagée avec d'autres serveurs ne se réécrit ni ne se retire par
/// nom : il faut la modifier à la main.
#[tokio::test]
async fn a_server_from_a_shared_file_is_edited_by_hand() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve("x", server(two_tools()));
    fake.serve("y", server(two_tools()));
    std::fs::create_dir_all(sup.dir()).unwrap();
    std::fs::write(
        sup.dir().join("commun.toml"),
        "[servers.x]\ncommand = \"/opt/mcp/x\"\n\n[servers.y]\ncommand = \"/opt/mcp/y\"\n",
    )
    .unwrap();
    let report = sup.reload().await;
    assert_eq!(report.added, vec!["x", "y"], "{report:?}");
    let err = sup.remove("x").await.unwrap_err();
    assert!(err.contains("plusieurs"), "{err}");
    let err = sup.edit("x", &json!({"timeout": "60s"})).await.unwrap_err();
    assert!(err.contains("plusieurs"), "{err}");
}
