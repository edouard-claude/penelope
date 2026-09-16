//! CA 12 : `ticket-to-deploy` de bout en bout contre des mocks.
//!
//! ```text
//!  tracker MCP (redmine) ─► dépôt git local (origin.git) ─► agent (mock) ─► tests shell
//!        ▲                                                                      │
//!        └── commentaire ◄── déploiement `make` ◄── PR (forge MCP) ◄── push ◄───┘
//! ```
//!
//! Les approbations et les choix passent par le mock Telegram. Le daemon est reconstruit
//! avant chaque passage du pilote, comme après un `kill -9` : la reprise ne doit ni pousser
//! deux fois, ni ouvrir deux PR, ni commenter deux fois, ni déployer deux fois.

use crate::bus::Origin;
use crate::mcp::testing::{FakeConnector, declare, server, tool};
use crate::runtime::{Daemon, Services};
use crate::telegram::TelegramGateway;
use penelope_kernel::clock::TestClock;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ToolCall;
use penelope_telegram::mock::{MockTransport, updates};
use penelope_workflow::runs::RunState;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const OWNER: i64 = 42;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Essai",
            "-c",
            "user.email=essai@example.org",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} : {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Dépôt d'origine : un Makefile qui teste, déploie et vérifie, et `.penelope/deploy.toml`.
fn origin_repo(root: &Path) -> PathBuf {
    let source = root.join("source");
    std::fs::create_dir_all(source.join(".penelope")).unwrap();
    std::fs::write(
        source.join("Makefile"),
        "test:\n\t@test -f fixed.txt && echo tests verts\n\
         lint:\n\t@echo lint propre\n\
         deploy:\n\t@echo \"deploy $(ENV)\" >> deployed.txt\n\
         smoke:\n\t@test -f deployed.txt && echo en ligne\n\
         rollback:\n\t@echo rollback >> rolledback.txt\n",
    )
    .unwrap();
    std::fs::write(
        source.join(".penelope/deploy.toml"),
        "[environments.prod]\ndeploy = \"make deploy ENV=prod\"\n",
    )
    .unwrap();
    std::fs::write(source.join("README.md"), "Service à corriger\n").unwrap();
    git(&source, &["init", "-q", "-b", "main"]);
    git(&source, &["add", "."]);
    git(&source, &["commit", "-q", "-m", "initial"]);
    let bare = root.join("origin.git");
    git(
        root,
        &[
            "clone",
            "-q",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    bare
}

fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

struct World {
    _dir: tempfile::TempDir,
    home: PathBuf,
    clock: TestClock,
    p: Arc<MockProvider>,
    fake: Arc<FakeConnector>,
    t: Arc<MockTransport>,
    comments: Arc<Mutex<Vec<Value>>>,
    prs: Arc<AtomicUsize>,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let comments: Arc<Mutex<Vec<Value>>> = Arc::default();
        let prs: Arc<AtomicUsize> = Arc::default();
        let fake = Arc::new(FakeConnector::default());

        let tracker = server(Arc::new(Mutex::new(vec![
            tool("get_issue", json!({"readOnlyHint": true})),
            tool(
                "update_issue",
                json!({"readOnlyHint": false, "destructiveHint": false, "openWorldHint": true}),
            ),
        ])));
        let seen = comments.clone();
        fake.serve(
            "redmine",
            Arc::new(move |m, p| match (m, p["name"].as_str()) {
                ("tools/call", Some("get_issue")) => Ok(json!({
                    "content": [{"type": "text", "text": "#42 Le service renvoie 500"}],
                    "structuredContent": {"id": 42, "subject": "Le service renvoie 500"}
                })),
                ("tools/call", Some("update_issue")) => {
                    seen.lock().unwrap().push(p["arguments"].clone());
                    Ok(json!({"content": [{"type": "text", "text": "commentaire ajouté"}]}))
                }
                _ => tracker(m, p),
            }),
        );
        let forge = server(Arc::new(Mutex::new(vec![tool(
            "create_pull_request",
            json!({"readOnlyHint": false, "destructiveHint": false, "openWorldHint": true}),
        )])));
        let opened = prs.clone();
        fake.serve(
            "github",
            Arc::new(move |m, p| match (m, p["name"].as_str()) {
                ("tools/call", Some("create_pull_request")) => {
                    opened.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({
                        "content": [{"type": "text", "text": "PR #12 ouverte"}],
                        "structuredContent": {"number": 12}
                    }))
                }
                _ => forge(m, p),
            }),
        );
        World {
            home,
            clock: TestClock::new(1_789_516_800_000),
            p: Arc::new(MockProvider::new()),
            fake,
            t: MockTransport::new(),
            comments,
            prs,
            _dir: dir,
        }
    }

    /// Un daemon neuf sur le même état : ce que voit un redémarrage après `kill -9`.
    async fn boot(&self) -> (Arc<Daemon>, Arc<TelegramGateway>) {
        let shared: penelope_kernel::clock::SharedClock = Arc::new(self.clock.clone());
        // La vie précédente libère la base de façon asynchrone (fil écrivain).
        let mut attempt = 0;
        let s = loop {
            match Services::for_tests(self.home.clone(), shared.clone()).await {
                Ok(s) => break Arc::new(s),
                Err(e) if attempt < 100 => {
                    attempt += 1;
                    let _ = e;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => panic!("redémarrage impossible : {e}"),
            }
        };
        let d = Arc::new(Daemon::from_services(s));
        d.publish_config("test", |c| {
            c.telegram.rate_per_chat_per_s = 1_000.0;
            Ok(vec!["telegram.rate_per_chat_per_s".into()])
        })
        .unwrap();
        d.set_provider_override(self.p.clone());
        let g = TelegramGateway::with_transport(d.clone(), self.t.clone());
        g.register();
        d.hooks
            .set_orchestrator(Arc::new(crate::workflow::WorkflowOrchestrator {
                daemon: d.clone(),
            }));
        let sup = crate::mcp::McpSupervisor::new(d.services.clone(), self.fake.clone());
        declare(&sup, "redmine", "");
        declare(&sup, "github", "");
        d.hooks.set_mcp(sup.clone());
        sup.reload().await;
        d.recover().await.unwrap();
        (d, g)
    }
}

/// Fin brutale d'une vie : plus rien ne tient le daemon (les hooks forment des cycles
/// avec la passerelle et l'orchestrateur), la base se libère.
fn kill(d: Arc<Daemon>, g: Arc<TelegramGateway>) {
    *d.hooks.messenger.write().unwrap() = None;
    *d.hooks.telegram.write().unwrap() = None;
    *d.hooks.orchestrator.write().unwrap() = None;
    *d.hooks.mcp.write().unwrap() = None;
    *d.hooks.mcp_supervisor.write().unwrap() = None;
    d.handle.shutdown();
    drop(g);
    drop(d);
}

/// Pilote chaque run actif jusqu'à ce qu'il attende (parent, puis sous-run).
async fn drive_everything(d: &Arc<Daemon>) {
    for _ in 0..3 {
        for r in d
            .services
            .runs
            .list(Some(RunState::Running), 50)
            .await
            .unwrap()
        {
            crate::workflow::drive(d, &r.id).await.unwrap();
        }
    }
}

/// Clique le bouton `label` le plus récent qui n'a pas encore été cliqué.
async fn click(
    g: &TelegramGateway,
    t: &MockTransport,
    label: &str,
    clicked: &mut BTreeSet<String>,
    update: &mut i64,
) -> bool {
    let calls = t.calls().await;
    let token = calls.iter().rev().find_map(|(_, body)| {
        body["reply_markup"]["inline_keyboard"]
            .as_array()?
            .iter()
            .flat_map(|row| row.as_array().cloned().unwrap_or_default())
            .find_map(|b| {
                let data = b["callback_data"].as_str()?.to_string();
                (b["text"].as_str()? == label && !clicked.contains(&data)).then_some(data)
            })
    });
    let Some(token) = token else {
        return false;
    };
    clicked.insert(token.clone());
    *update += 1;
    g.process_update(&updates::callback(*update, OWNER, &token, 7000 + *update))
        .await
        .unwrap();
    true
}

#[tokio::test]
async fn ca_12_1_ticket_to_deploy_runs_end_to_end_and_survives_restarts() {
    let w = World::new();
    let origin = origin_repo(w._dir.path());

    // Réponses du modèle, dans l'ordre où le workflow les demande.
    w.p.reply(&format!(
        "```json\n{{\"forge\": \"github\", \"repo\": \"{}\", \"base_branch\": \"main\"}}\n```",
        origin.display()
    ));
    let criteria = |status: &str| json!([{"id": "c1", "text": "Le service ne renvoie plus 500", "status": status}]);
    w.p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call(
                "a1",
                "session_metadata",
                json!({"op": "set", "key": "criteria", "entry": criteria("pending")}),
            ),
            call(
                "a2",
                "return_value",
                json!({"result": "completed", "content": "Créer fixed.txt"}),
            ),
            call("a3", "step_done", json!({})),
        ],
    ));
    w.p.reply("Plan prêt.");
    w.p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call(
                "i1",
                "fs_write",
                json!({"path": "repo/fixed.txt", "content": "corrigé\n"}),
            ),
            call(
                "i2",
                "git_commit",
                json!({"cwd": "repo", "message": "Corrige le ticket 42"}),
            ),
            call(
                "i3",
                "session_metadata",
                json!({"op": "set", "key": "criteria", "entry": criteria("passed")}),
            ),
            call("i4", "step_done", json!({})),
        ],
    ));
    w.p.reply("Correctif appliqué.");
    w.p.reply("RAS : critères remplis.");
    w.p.reply("RAS : critères remplis.");

    let (d, g) = w.boot().await;
    let chat = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let run = crate::workflow::start_run(
        &d,
        "ticket-to-deploy",
        json!({"ticket_url": "https://redmine.example/issues/42", "ticket_id": "42"}),
        &chat,
        None,
        0,
    )
    .await
    .unwrap();
    kill(d, g);

    let mut clicked = BTreeSet::new();
    let mut update = 0;
    let mut identity_set = false;
    let mut answered_plan = false;
    let mut state = RunState::Running;
    for _ in 0..60 {
        let (d, g) = w.boot().await;
        drive_everything(&d).await;
        g.flush_outbox().await.unwrap();
        let current = d.services.runs.get(&run.id).await.unwrap().unwrap();
        state = current.state;
        if state != RunState::Running {
            kill(d, g);
            break;
        }
        // Le dépôt cloné reçoit une identité git avant que l'agent ne committe.
        if !identity_set
            && let Some(dir) = current
                .workdir
                .as_deref()
                .map(|w| Path::new(w).join("repo"))
            && dir.join(".git").exists()
        {
            git(&dir, &["config", "user.name", "Pénélope"]);
            git(&dir, &["config", "user.email", "penelope@example.org"]);
            identity_set = true;
        }
        let mut acted = false;
        for label in ["✅ Autoriser", "Déployer"] {
            while click(&g, &w.t, label, &mut clicked, &mut update).await {
                acted = true;
            }
        }
        if click(&g, &w.t, "Appliquer", &mut clicked, &mut update).await {
            update += 1;
            g.process_update(&updates::text_message(update, OWNER, OWNER, "vas-y"))
                .await
                .unwrap();
            answered_plan = true;
            acted = true;
        }
        g.flush_outbox().await.unwrap();
        if !acted {
            // Rien à cliquer : les attentes (sous-run, sondages) avancent avec le temps.
            w.clock.advance_secs(5);
        }
        kill(d, g);
    }

    let (d, _g) = w.boot().await;
    let run = d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(state, RunState::Done, "run : {run:?}");
    assert!(answered_plan, "le plan a été approuvé par Telegram");

    // Push unique : la branche existe sur l'origine, avec le commit de l'agent.
    let out = Command::new("git")
        .args(["log", "--oneline", "penelope/42"])
        .current_dir(&origin)
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(log.contains("Corrige le ticket 42"), "{log}");

    assert_eq!(w.prs.load(Ordering::SeqCst), 1, "une seule PR");
    let comments = w.comments.lock().unwrap().clone();
    assert_eq!(comments.len(), 1, "un seul commentaire : {comments:?}");
    assert_eq!(comments[0]["status"], "resolved");

    let repo = Path::new(run.workdir.as_deref().unwrap()).join("repo");
    let deployed = std::fs::read_to_string(repo.join("deployed.txt")).unwrap();
    assert_eq!(deployed, "deploy prod\n", "un seul déploiement");
    assert!(!repo.join("rolledback.txt").exists());

    // Les effets externes passés par le ledger sont chacun complétés une fois.
    let pushes: i64 = d
        .services
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM effects WHERE tool = 'git_push' AND state = 'completed'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(pushes, 1);
}
