use super::testing::*;
use super::*;

mod admin;
mod calls;
mod sandbox;

use penelope_app::ports::McpGateway;
use penelope_kernel::clock::TestClock;
use penelope_kernel::risk::RiskClass;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

async fn setup() -> (
    tempfile::TempDir,
    Arc<Services>,
    Arc<TestClock>,
    Arc<FakeConnector>,
    Arc<McpSupervisor>,
) {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock.clone())
            .await
            .unwrap(),
    );
    let fake = Arc::new(FakeConnector::default());
    let sup = McpSupervisor::new(s.clone(), fake.clone());
    (dir, s, clock, fake, sup)
}

fn two_tools() -> Arc<Mutex<Vec<Value>>> {
    Arc::new(Mutex::new(vec![
        tool("list_issues", json!({"readOnlyHint": true})),
        tool("create_issue", json!({})),
    ]))
}

#[tokio::test]
async fn servers_are_discovered_and_their_tools_answer_calls() {
    let (_d, s, _c, fake, sup) = setup().await;
    fake.serve("redmine", server(two_tools()));
    declare(
        &sup,
        "redmine",
        "[tool_risk]\ncreate_issue = \"destructive\"\n",
    );

    let report = sup.reload().await;
    assert_eq!(report.added, vec!["redmine"]);

    let list = s
        .mcp_tools
        .get("mcp__redmine__list_issues")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(list.risk, RiskClass::Read);
    let create = s
        .mcp_tools
        .get("mcp__redmine__create_issue")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        create.risk,
        RiskClass::Destructive,
        "surcharge de la déclaration"
    );
    assert_eq!(sup.server_lines().await, vec!["redmine : 2 outils"]);

    let v = sup
        .call(
            "mcp__redmine__list_issues",
            &json!({"project": "penelope"}),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(
        v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("list_issues")
    );
    assert_eq!(v["isError"], false);
    assert_eq!(
        fake.opened("redmine"),
        1,
        "la connexion de découverte sert à l'appel"
    );

    let st = &sup.statuses().await[0];
    assert_eq!(st.state, ServerState::Ready);
    assert_eq!((st.calls, st.tool_count, st.running), (1, 2, true));
    assert_eq!(st.protocol.as_deref(), Some("2025-06-18"));

    let (state, tools): (String, i64) = s
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT state, tool_count FROM mcp_servers WHERE name = 'redmine'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!((state.as_str(), tools), ("ready", 2));
}

#[tokio::test]
async fn known_tools_do_not_start_a_lazy_server_at_boot() {
    let (_d, s, _c, fake, sup) = setup().await;
    fake.serve("redmine", server(two_tools()));
    declare(&sup, "redmine", "");
    sup.reload().await;
    sup.stop_all().await;

    // Redémarrage du daemon : même base, nouveau superviseur.
    let fake2 = Arc::new(FakeConnector::default());
    fake2.serve("redmine", server(two_tools()));
    let sup2 = McpSupervisor::new(s.clone(), fake2.clone());
    sup2.reload().await;
    assert_eq!(
        fake2.opened("redmine"),
        0,
        "outils connus, serveur lazy : rien ne démarre"
    );
    assert_eq!(sup2.server_lines().await, vec!["redmine : 2 outils"]);
    assert_eq!(sup2.statuses().await[0].state, ServerState::Configured);

    sup2.call("mcp__redmine__create_issue", &json!({}), Default::default())
        .await
        .unwrap();
    assert_eq!(fake2.opened("redmine"), 1, "démarrage au premier appel");
}

#[tokio::test]
async fn a_broken_server_backs_off_then_fails_until_restarted() {
    let (_d, _s, clock, fake, sup) = setup().await;
    fake.serve("forge", server(two_tools()));
    fake.fail_open.lock().unwrap().insert(
        "forge".into(),
        "exécutable introuvable dans PATH : forge".into(),
    );
    declare(&sup, "forge", "");
    sup.reload().await;

    let st = &sup.statuses().await[0];
    assert_eq!((st.state, st.failures), (ServerState::Connecting, 1));
    assert!(st.last_error.as_deref().unwrap().contains("introuvable"));

    let slot = sup.slot("forge").await.unwrap();
    let e = sup.ensure_live(&slot).await.err().expect("échec attendu");
    assert!(e.contains("nouvel essai dans"), "{e}");
    assert_eq!(
        fake.opened("forge"),
        1,
        "le backoff évite de relancer aussitôt"
    );

    for _ in 0..7 {
        clock.advance_ms(301_000);
        let _ = sup.ensure_live(&slot).await;
    }
    assert_eq!(sup.statuses().await[0].state, ServerState::Failed);
    clock.advance_ms(301_000);
    let e = sup.ensure_live(&slot).await.err().expect("échec attendu");
    assert!(e.contains("penelope mcp restart forge"), "{e}");

    fake.fail_open.lock().unwrap().clear();
    let st = sup.restart("forge").await.unwrap();
    assert_eq!(
        (st.state, st.failures, st.tool_count),
        (ServerState::Ready, 0, 2)
    );
}

#[tokio::test]
async fn a_server_confused_by_the_probe_is_retried_with_initialize() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve("vieux", server(two_tools()));
    fake.dies_on_probe.lock().unwrap().push("vieux".into());
    declare(&sup, "vieux", "");
    sup.reload().await;

    let st = &sup.statuses().await[0];
    assert_eq!((st.state, st.tool_count), (ServerState::Ready, 2));
    assert_eq!(fake.opened("vieux"), 2, "un second processus, sans sonde");

    // Redémarré, il ne repasse pas par la sonde qui le fait tomber.
    sup.restart("vieux").await.unwrap();
    assert_eq!(fake.opened("vieux"), 3);
}

#[tokio::test]
async fn changes_in_mcp_d_are_picked_up_live() {
    let (_d, s, _c, fake, sup) = setup().await;
    fake.serve("a", server(two_tools()));
    fake.serve(
        "b",
        server(Arc::new(Mutex::new(vec![tool("ping_b", json!({}))]))),
    );
    declare(&sup, "a", "");
    sup.reload().await;
    assert_eq!(fake.opened("a"), 1);

    declare(&sup, "b", "");
    sup.maintenance().await;
    assert!(s.mcp_tools.get("mcp__b__ping_b").await.unwrap().is_some());

    declare(&sup, "a", "args = [\"--verbose\"]\n");
    sup.maintenance().await;
    assert_eq!(fake.opened("a"), 2, "configuration changée : redémarré");

    std::fs::remove_file(sup.dir().join("a.toml")).unwrap();
    sup.maintenance().await;
    assert!(
        s.mcp_tools
            .get("mcp__a__list_issues")
            .await
            .unwrap()
            .is_none()
    );
    let names: Vec<String> = sup.statuses().await.into_iter().map(|s| s.name).collect();
    assert_eq!(names, vec!["b"]);
    let rows: i64 = s
        .store
        .read(|c| {
            Ok(
                c.query_row("SELECT count(*) FROM mcp_servers WHERE name='a'", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn administration_rewrites_the_declaration() {
    let (_d, s, _c, fake, sup) = setup().await;
    fake.serve("a", server(two_tools()));
    fake.serve("c", server(two_tools()));
    declare(&sup, "a", "");
    sup.reload().await;

    sup.set_enabled("a", false).await.unwrap();
    let raw = std::fs::read_to_string(sup.dir().join("a.toml")).unwrap();
    assert!(raw.contains("enabled = false"), "{raw}");
    assert_eq!(sup.statuses().await[0].state, ServerState::Disabled);
    assert!(
        s.mcp_tools
            .get("mcp__a__list_issues")
            .await
            .unwrap()
            .is_none()
    );
    assert!(sup.server_lines().await.is_empty());

    sup.set_enabled("a", true).await.unwrap();
    assert!(
        s.mcp_tools
            .get("mcp__a__list_issues")
            .await
            .unwrap()
            .is_some()
    );

    sup.edit("a", &json!({"timeout": "60s"})).await.unwrap();
    assert_eq!(sup.config_of("a").await.unwrap().timeout, "60s");
    assert!(
        sup.edit("a", &json!({"couleur": "bleu"}))
            .await
            .unwrap_err()
            .contains("champ inconnu")
    );
    assert!(sup.edit("a", &json!({"timeout": "bientôt"})).await.is_err());

    let dup = ServerConfig::stdio("a", "/opt/mcp/a", &[]);
    assert!(
        sup.add(dup, false)
            .await
            .unwrap_err()
            .contains("existe déjà")
    );
    let report = sup
        .add(ServerConfig::stdio("c", "/opt/mcp/c", &[]), false)
        .await
        .unwrap();
    assert_eq!(report.added, vec!["c"]);
    sup.remove("c").await.unwrap();
    assert!(!sup.dir().join("c.toml").exists());
    assert!(sup.config_of("c").await.is_none());
}

#[tokio::test]
async fn server_requests_and_list_changes_are_handled() {
    let (_d, s, _c, fake, sup) = setup().await;
    let tools = two_tools();
    fake.serve("a", server(tools.clone()));
    declare(&sup, "a", "roots = [\"~/projets/penelope\"]\n");
    sup.reload().await;
    let t = fake.last_transport("a");

    t.push_server_request(7, "roots/list", json!({}));
    t.push_server_request(
        8,
        "elicitation/create",
        json!({"message": "mot de passe ?"}),
    );
    t.push_server_request(9, "sampling/createMessage", json!({}));
    tools
        .lock()
        .unwrap()
        .push(tool("close_issue", json!({"destructiveHint": true})));
    t.push_notification("notifications/tools/list_changed", json!({}));

    for _ in 0..50 {
        if t.responses.lock().await.len() == 3
            && s.mcp_tools
                .get("mcp__a__close_issue")
                .await
                .unwrap()
                .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let responses = t.responses.lock().await.clone();
    let by_id = |id: u64| {
        responses
            .iter()
            .find(|(i, _)| *i == json!(id))
            .map(|(_, r)| r.clone())
            .unwrap()
    };
    let roots = by_id(7).unwrap();
    assert!(
        roots["roots"][0]["uri"]
            .as_str()
            .unwrap()
            .ends_with("/projets/penelope")
    );
    // Sans propriétaire joignable : rien d'annoncé, et une demande quand même reçue est
    // annulée sans que personne ait refusé (issue #12, parcours Telegram dans
    // `telegram::tests::mcp_elicitation_is_answered_from_telegram`).
    let log = t.call_log().await;
    let (_, init) = log.iter().find(|(m, _)| m == "initialize").unwrap();
    assert!(init["capabilities"].get("elicitation").is_none(), "{init}");
    assert!(init["capabilities"].get("sampling").is_none(), "{init}");
    assert_eq!(by_id(8).unwrap()["action"], "cancel");
    assert!(by_id(9).is_err());
    let close = s
        .mcp_tools
        .get("mcp__a__close_issue")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(close.risk, RiskClass::Destructive);
}

/// #92 : une description qui glisse une consigne est encadrée et signalée ; un
/// changement silencieux de description révoque le « Toujours » de l'outil et prévient
/// le propriétaire ; une liste identique ne change rien.
#[tokio::test]
async fn a_lost_connection_is_counted_and_the_next_call_reconnects() {
    let (_d, _s, clock, fake, sup) = setup().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let base = server(two_tools());
    let n = calls.clone();
    fake.serve(
        "a",
        Arc::new(move |m, p| {
            if m == "tools/call" && n.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(McpError::Transport(
                    "le serveur a fermé la connexion".into(),
                ));
            }
            base(m, p)
        }),
    );
    declare(&sup, "a", "");
    sup.reload().await;

    let e = sup
        .call("mcp__a__list_issues", &json!({}), Default::default())
        .await
        .unwrap_err();
    assert!(e.contains("fermé la connexion"), "{e}");
    let st = &sup.statuses().await[0];
    assert_eq!(
        (st.state, st.failures, st.running),
        (ServerState::Connecting, 1, false)
    );

    clock.advance_ms(5_000);
    sup.call("mcp__a__list_issues", &json!({}), Default::default())
        .await
        .unwrap();
    assert_eq!(fake.opened("a"), 2);
    assert_eq!(sup.statuses().await[0].state, ServerState::Ready);
}

#[tokio::test]
async fn tool_policy_and_eager_schemas_come_from_the_declaration() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve("a", server(two_tools()));
    declare(
        &sup,
        "a",
        "eager_schemas = true\n[tool_policy]\ncreate_issue = \"deny\"\n",
    );
    sup.reload().await;
    assert_eq!(
        sup.tool_policy("mcp__a__create_issue").await.as_deref(),
        Some("deny")
    );
    assert_eq!(sup.tool_policy("mcp__a__list_issues").await, None);
    let eager: Vec<String> = sup
        .eager_tools()
        .await
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(eager, vec!["mcp__a__create_issue", "mcp__a__list_issues"]);
}

/// T25 : le superviseur est à la fois la passerelle des outils MCP (`McpGateway`) et
/// l'administration des serveurs (`McpAdmin`) : le daemon ne connaît que ces deux ports.
#[test]
fn the_supervisor_is_both_gateway_and_admin() {
    fn ports<T: McpGateway + penelope_app::ports::McpAdmin>() {}
    ports::<McpSupervisor>();
}

#[test]
fn binary_content_never_enters_the_transcript() {
    let r = ToolResult {
        content: vec![
            ContentBlock::Text { text: "ok".into() },
            ContentBlock::Image {
                data: "QUJD".repeat(1000),
                mime_type: "image/png".into(),
            },
            ContentBlock::Resource {
                uri: "file:///a.md".into(),
                text: Some("# titre".into()),
                blob: None,
                mime_type: Some("text/markdown".into()),
            },
        ],
        ..Default::default()
    };
    let v = result_json(&r);
    assert_eq!(v["content"][0]["text"], "ok");
    let img = v["content"][1]["text"].as_str().unwrap();
    assert!(img.contains("image/png") && !img.contains("QUJD"), "{img}");
    assert_eq!(v["content"][2]["text"], "# titre");
}

/// #114 : un serveur stdio qui meurt au démarrage laisse dans son état la cause de sa
/// mort (code, durée de vie, sortie d'erreur vide dite), et `mcp test` rend la même.
#[tokio::test]
async fn a_stdio_server_dead_at_start_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    // Profil `full`, explicitement autorisé : le bac à sable `mcp-stdio` n'existe que
    // sur macOS (#102), et ce test porte sur la mort du processus, pas sur lui.
    s.config
        .mutate("test", |c| {
            c.sandbox.allow_full_for = vec!["pont".into()];
            Ok(vec!["sandbox.allow_full_for".into()])
        })
        .unwrap();
    let sup = McpSupervisor::new(s.clone(), Arc::new(ProcessConnector::new(s.clone())));
    std::fs::create_dir_all(sup.dir()).unwrap();
    std::fs::write(
        sup.dir().join("pont.toml"),
        "command = \"/bin/sh\"\nargs = [\"-c\", \"exit 7\"]\ntimeout = \"10s\"\n\
             sandbox_profile = \"full\"\n",
    )
    .unwrap();
    sup.reload().await;
    let st = &sup.statuses().await[0];
    let err = st.last_error.clone().unwrap_or_default();
    assert!(err.contains("sorti avec le code 7 après"), "{err}");
    assert!(err.contains("sans rien écrire"), "{err}");
    let shown = sup.show("pont").await.unwrap();
    assert!(
        shown["status"]["last_error"]
            .as_str()
            .unwrap_or_default()
            .contains("code 7"),
        "{shown}"
    );
    let cfg = sup.config_of("pont").await.unwrap();
    let tested = sup.test(&cfg).await;
    assert_eq!(tested["ok"], false);
    assert!(
        tested["error"].as_str().unwrap().contains("code 7"),
        "{tested}"
    );
    let logs = tested["logs"].as_array().unwrap();
    assert!(!logs.is_empty(), "une sortie vide est dite");
    assert!(
        logs[0]
            .as_str()
            .unwrap()
            .contains("rien sur la sortie d'erreur")
    );
}

/// Un vrai serveur stdio (script Python), lancé sous le profil `mcp-stdio`. Seatbelt    /// Un vrai serveur stdio (script Python), lancé sous le profil `mcp-stdio`. Seatbelt
/// n'existe que sur macOS (issue #102).
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_real_stdio_server_runs_under_the_sandbox() {
    let Some(python) = penelope_platform::which("python3") else {
        eprintln!("python3 absent : test stdio réel ignoré");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let script = dir.path().join("fake_mcp.py");
    std::fs::write(&script, FAKE_PY).unwrap();
    let sup = McpSupervisor::new(s.clone(), Arc::new(ProcessConnector::new(s.clone())));
    std::fs::create_dir_all(sup.dir()).unwrap();
    std::fs::write(
        sup.dir().join("pyfake.toml"),
        format!(
            "command = \"{}\"\nargs = [\"{}\"]\ntimeout = \"10s\"\n[env]\nFAKE_GREETING = \"bonjour\"\n",
            python.display(),
            script.display()
        ),
    )
    .unwrap();

    sup.reload().await;
    let st = &sup.statuses().await[0];
    assert_eq!(st.state, ServerState::Ready, "{:?}", st.last_error);
    assert_eq!(st.tool_count, 1);

    let v = sup
        .call(
            "mcp__pyfake__echo",
            &json!({"text": "salut"}),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(v["content"][0]["text"], "bonjour salut");
    let logs = sup.logs("pyfake", 10).await.unwrap();
    assert!(logs.iter().any(|l| l.contains("pyfake prêt")), "{logs:?}");

    let pid_file = s.platform.dirs.pid_dir().join("mcp-pyfake.pid");
    assert!(pid_file.exists());
    sup.stop_all().await;
    assert!(!pid_file.exists(), "processus arrêté, fichier PID retiré");
}

/// Essai contre un vrai serveur installé : poignée de main et liste des outils,
/// aucun appel d'outil. `PENELOPE_REAL_MCP=/chemin/du/serveur cargo test -p
/// penelope-daemon real_mcp_server -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn real_mcp_server_handshake() {
    let Ok(command) = std::env::var("PENELOPE_REAL_MCP") else {
        eprintln!("PENELOPE_REAL_MCP absent");
        return;
    };
    let profile = std::env::var("PENELOPE_REAL_MCP_PROFILE").unwrap_or("mcp-stdio".into());
    // `CLE=valeur,CLE2=valeur2` : variables d'environnement du serveur.
    let env: String = std::env::var("PENELOPE_REAL_MCP_ENV")
        .unwrap_or_default()
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| format!("{k} = \"{v}\"\n"))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let sup = McpSupervisor::new(s.clone(), Arc::new(ProcessConnector::new(s.clone())));
    std::fs::create_dir_all(sup.dir()).unwrap();
    std::fs::write(
        sup.dir().join("reel.toml"),
        format!(
            "command = \"{command}\"\nsandbox_profile = \"{profile}\"\ntimeout = \"20s\"\n\
                 [env]\n{env}"
        ),
    )
    .unwrap();
    let started = std::time::Instant::now();
    sup.reload().await;
    let st = sup.statuses().await.remove(0);
    eprintln!(
        "état {:?}, protocole {:?}, {} outils en {} ms, sonde évitée : {}",
        st.state,
        st.protocol,
        st.tool_count,
        started.elapsed().as_millis(),
        sup.slot("reel").await.unwrap().info(|i| i.skip_probe)
    );
    eprintln!("erreur : {:?}", st.last_error);
    eprintln!("stderr : {:?}", sup.logs("reel", 20).await.unwrap());
    let tools = s.mcp_tools.list_server("reel").await.unwrap();
    for t in tools.iter().take(10) {
        eprintln!("  {} ({})", t.qualified, t.risk.as_str());
    }
    sup.stop_all().await;
    assert_eq!(st.state, ServerState::Ready);
}

#[cfg(target_os = "macos")]
const FAKE_PY: &str = r#"
import json, os, sys
print("pyfake prêt", file=sys.stderr, flush=True)
greeting = os.environ.get("FAKE_GREETING", "?")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    mid = msg.get("id")
    method = msg.get("method")
    if mid is None:
        continue
    if method == "initialize":
        res = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
               "serverInfo": {"name": "pyfake", "version": "1"}}
    elif method == "tools/list":
        res = {"tools": [{"name": "echo", "description": "Renvoie le texte",
                          "inputSchema": {"type": "object",
                                          "properties": {"text": {"type": "string"}},
                                          "required": ["text"]},
                          "annotations": {"readOnlyHint": True}}]}
    elif method == "tools/call":
        text = msg["params"]["arguments"]["text"]
        res = {"content": [{"type": "text", "text": greeting + " " + text}]}
    elif method == "ping":
        res = {}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": mid,
                          "error": {"code": -32601, "message": "Method not found"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": mid, "result": res}), flush=True)
"#;
