use super::*;

use crate::agent::Conversation;

use penelope_kernel::clock::{Clock, TestClock};

use penelope_llm::mock::{MockProvider, Scripted};

use penelope_llm::types::ToolCall;

mod journal;
mod models;
mod rules;
mod turns;

async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    (dir, d, p)
}

async fn claim(d: &Daemon) -> Turn {
    d.services.turns.claim("test").await.unwrap().unwrap()
}

/// Un tour qui fait les appels `calls` (un par réponse du modèle) : son issue, le
/// daemon et l'historique de la session.
async fn tool_turn(
    calls: Vec<(&str, Value)>,
) -> (tempfile::TempDir, Arc<Daemon>, TurnOutcome, String) {
    let (dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    for (i, (name, args)) in calls.into_iter().enumerate() {
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: format!("c{i}"),
                name: name.into(),
                arguments: args,
            }],
        ));
    }
    p.reply("Je m'arrête là.");
    d.enqueue_message(
        &sid,
        "lance la correction de la pagination",
        &Origin::Cli,
        None,
    )
    .await
    .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    (dir, d, out, sid)
}

async fn tool_results(d: &Daemon, sid: &str) -> Vec<String> {
    d.services
        .context
        .history
        .load(sid, 0)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.message.role == penelope_llm::types::Role::Tool)
        .map(|e| e.message.text())
        .collect()
}

/// Un tour qui appelle `shell_exec` avec `command` : son issue et le daemon.
async fn shell_turn(
    command: &str,
    mode: Option<&str>,
    allow: &[&str],
) -> (tempfile::TempDir, Arc<Daemon>, TurnOutcome) {
    let (dir, d, p) = daemon().await;
    let allow: Vec<String> = allow.iter().map(|a| a.to_string()).collect();
    d.publish_config("test", move |c| {
        c.tools.shell_allow = allow;
        Ok(vec!["tools.shell_allow".into()])
    })
    .unwrap();
    // `{ws}` : le workspace de la session (#123).
    let ws = crate::executor::default_workspaces(&d.services)[0].clone();
    let command = command.replace("{ws}", &ws.to_string_lossy());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    if let Some(m) = mode {
        crate::approval_mode::set(
            &d.services,
            &sid,
            crate::approval_mode::ApprovalMode::parse(m),
        )
        .await
        .unwrap();
    }
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": command}),
        }],
    ));
    p.reply("C'est fait.");
    d.enqueue_message(&sid, "vas-y", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    (dir, d, out)
}

fn asked(out: &TurnOutcome) -> bool {
    matches!(out, TurnOutcome::AwaitingApproval { .. })
}

/// Script de la demande de #130, secrets remplacés : préfixe `cd`, heredoc quoté,
/// accolades simples, découpes Python, triple guillemet, astérisque.
const HEREDOC: &str = r#"python3 - <<'PYEOF'
import json, urllib.request

URL = "https://api.example.test/query"
KEY = "abcdefghijklmnopqrstuvwxyz0123*456789abcdefghijklmnopqrstuv"

def gql(query, variables=None, token=None):
    body = {"query": query, "variables": variables or {}}
    req = urllib.request.Request(URL, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "X-API-Key": KEY})
    if token: req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)

# 1. login
r = gql("mutation l($e: String!, $p: String!) { login(data: {email: $e, password: $p}) { access refresh } }",
        {"e": "essai@example.test", "p": "motdepasse"})
tok = r["data"]["login"]["access"]
print("LOGIN OK")
for off in (0, 20):
    q = """query p($o: Int!) { points(offset: $o) { id nb_points point_created_at } }"""
    for x in gql(q, {"o": off}, tok)["data"]["points"]:
        print(f"  {x['point_created_at'][:10]}  {x['nb_points']:>6}  [{x['id'][:8]}]")
PYEOF"#;
