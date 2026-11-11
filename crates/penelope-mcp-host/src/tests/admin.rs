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

/// L'entretien comme en production : un passage toutes les quinze secondes.
async fn tick(clock: &TestClock, sup: &McpSupervisor, ms: i64) {
    for _ in 0..ms / 15_000 {
        clock.advance_ms(15_000);
        sup.maintenance().await;
    }
}

async fn running(sup: &McpSupervisor) -> Vec<String> {
    sup.statuses()
        .await
        .into_iter()
        .filter(|s| s.running)
        .map(|s| s.name)
        .collect()
}

async fn status(sup: &McpSupervisor, name: &str) -> ServerStatus {
    sup.statuses()
        .await
        .into_iter()
        .find(|s| s.name == name)
        .unwrap()
}

async fn pings(fake: &FakeConnector, name: &str) -> usize {
    fake.last_transport(name)
        .call_log()
        .await
        .iter()
        .filter(|(m, _)| m == "ping")
        .count()
}

async fn use_once(sup: &McpSupervisor, name: &str) {
    sup.call(
        &format!("mcp__{name}__list_issues"),
        &json!({}),
        Default::default(),
    )
    .await
    .unwrap();
}

/// #457 : chaque sonde rendue remettait l'inactivité à zéro, et aucun serveur paresseux
/// ne s'arrêtait jamais (cinq vivaient depuis deux à six jours). L'ancien test sautait
/// onze minutes d'un coup, ce que la production ne fait pas : ici l'horloge avance par
/// pas de quinze secondes. Le serveur répond à ses sondes et s'arrête quand même au bout
/// de `idle_timeout` ; `"0"` et `eager_schemas` le gardent ; l'appel suivant le relance.
#[tokio::test]
async fn an_idle_lazy_server_stops_although_it_answers_the_probe() {
    let (_d, _s, clock, fake, sup) = setup().await;
    for name in ["expose", "garde", "lent"] {
        fake.serve(name, server(two_tools()));
    }
    declare(
        &sup,
        "expose",
        "idle_timeout = \"10m\"\neager_schemas = true\n",
    );
    declare(&sup, "garde", "");
    declare(&sup, "lent", "idle_timeout = \"10m\"\n");
    sup.reload().await;
    // `penelope mcp edit garde idle_timeout 0` : le zéro arrive en nombre.
    sup.edit("garde", &json!({"idle_timeout": 0}))
        .await
        .unwrap();
    for name in ["expose", "garde", "lent"] {
        use_once(&sup, name).await;
    }
    let kept = fake.opened("garde");

    tick(&clock, &sup, 9 * 60_000 + 45_000).await;
    assert_eq!(running(&sup).await, vec!["expose", "garde", "lent"]);
    assert_eq!(pings(&fake, "lent").await, 9, "une sonde par minute");

    tick(&clock, &sup, 15_000).await;
    assert_eq!(running(&sup).await, vec!["expose", "garde"]);
    let lent = status(&sup, "lent").await;
    assert_eq!(lent.state, ServerState::Configured, "{lent:?}");
    assert!(lent.last_error.is_none(), "{lent:?}");

    use_once(&sup, "lent").await;
    assert_eq!(fake.opened("lent"), 2, "relancé à la demande");
    tick(&clock, &sup, 20 * 60_000).await;
    assert_eq!(running(&sup).await, vec!["expose", "garde"]);
    assert_eq!(fake.opened("garde"), kept, "jamais relancé");
}

/// Ce que la sonde faisait déjà : un serveur vivant qui ne répond plus est perdu, avec
/// son erreur, bien avant son délai d'inactivité.
#[tokio::test]
async fn a_mute_server_is_lost_at_its_first_probe() {
    let (_d, _s, clock, fake, sup) = setup().await;
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
    declare(&sup, "sonde", "idle_timeout = \"1h\"\n");
    sup.reload().await;
    use_once(&sup, "sonde").await;

    tick(&clock, &sup, 2 * 60_000).await;
    assert_eq!(running(&sup).await, vec!["sonde"], "sondé, toujours là");
    assert_eq!(pings(&fake, "sonde").await, 2);

    alive.store(false, Ordering::SeqCst);
    tick(&clock, &sup, 60_000).await;
    assert!(running(&sup).await.is_empty());
    let sonde = status(&sup, "sonde").await;
    assert!(sonde.last_error.is_some(), "{sonde:?}");
}

/// #457 : `last_used` n'est écrit qu'à la fin d'un appel. Un outil qui travaille trois
/// minutes passait pour un serveur muet : sondé en plein travail, il perdait sa connexion
/// s'il ne rendait pas le `ping` en dix secondes, et un `idle_timeout` plus court que
/// l'appel l'aurait arrêté. Tant que l'appel tourne : ni sonde, ni arrêt ; l'inactivité
/// compte de sa fin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_in_progress_is_neither_probed_nor_stopped() {
    let (_d, _s, clock, fake, sup) = setup().await;
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let working = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let base = server(two_tools());
    fake.serve(
        "long",
        Arc::new(move |m, p| {
            if m == "tools/call" {
                working.store(true, Ordering::SeqCst);
                let _ = entered_tx.send(());
                let _ = release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(30));
                working.store(false, Ordering::SeqCst);
            }
            // Un serveur à un seul fil ne rend pas de `ping` pendant qu'il travaille.
            if m == "ping" && working.load(Ordering::SeqCst) {
                return Err(McpError::Timeout {
                    method: "ping".into(),
                    ms: 10_000,
                });
            }
            base(m, p)
        }),
    );
    declare(&sup, "long", "idle_timeout = \"2m\"\n");
    sup.reload().await;

    let call = tokio::spawn({
        let sup = sup.clone();
        async move {
            sup.call("mcp__long__list_issues", &json!({}), Default::default())
                .await
        }
    });
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(10)))
        .await
        .unwrap()
        .expect("l'appel est parti");

    tick(&clock, &sup, 3 * 60_000).await;
    let long = status(&sup, "long").await;
    assert!(long.running && long.last_error.is_none(), "{long:?}");
    assert_eq!(
        pings(&fake, "long").await,
        0,
        "pas de sonde pendant l'appel"
    );
    assert_eq!(fake.opened("long"), 1);

    release_tx.send(()).unwrap();
    call.await.unwrap().expect("l'appel aboutit");
    tick(&clock, &sup, 105_000).await;
    assert_eq!(
        running(&sup).await,
        vec!["long"],
        "deux minutes après sa fin"
    );
    assert_eq!(pings(&fake, "long").await, 1);
    tick(&clock, &sup, 15_000).await;
    assert!(running(&sup).await.is_empty());
}

/// Le serveur MCP de Slack répond `-32601 Method not found` à `ping` (#276). La sonde
/// d'entretien comptait ce refus comme une panne : à chaque minute de silence le serveur
/// repassait en `connecting` avec cette erreur, et `doctor` avertissait alors que ses
/// outils répondaient. Il reste `ready`, et `ping` n'est envoyé qu'une fois par session.
#[tokio::test]
async fn a_server_that_refuses_ping_stays_ready_through_maintenance() {
    let (_d, _s, clock, fake, sup) = setup().await;
    let base = server(two_tools());
    fake.serve(
        "slack",
        Arc::new(move |m, p| {
            if m == "ping" {
                return Err(McpError::Rpc {
                    code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                    message: "Method not found: ping".into(),
                    data: None,
                });
            }
            base(m, p)
        }),
    );
    declare(&sup, "slack", "idle_timeout = \"1h\"\n");
    sup.reload().await;
    sup.call("mcp__slack__list_issues", &json!({}), Default::default())
        .await
        .unwrap();

    for _ in 0..2 {
        clock.advance_ms(2 * 60_000);
        sup.maintenance().await;
    }
    let st = sup
        .statuses()
        .await
        .into_iter()
        .find(|s| s.name == "slack")
        .unwrap();
    assert_eq!(st.state, ServerState::Ready, "{st:?}");
    assert!(st.running && st.last_error.is_none(), "{st:?}");
    let pings = fake
        .last_transport("slack")
        .call_log()
        .await
        .iter()
        .filter(|(m, _)| m == "ping")
        .count();
    assert_eq!(pings, 1, "le refus est retenu pour la session");
    sup.call("mcp__slack__list_issues", &json!({}), Default::default())
        .await
        .unwrap();
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
