//! Hooks `pre_tool` qui réécrivent les arguments (#446) : ce que la carte montre, ce qui
//! part après l'accord, et ce que le modèle en apprend.

use super::*;
use penelope_app::hook_gate::{HookGate, PreToolGate};

/// Hook qui remplace `from` par `to` dans les arguments, tant que `to` est posé, et
/// compte ses passages.
struct Rewriter {
    from: &'static str,
    to: Mutex<Option<&'static str>>,
    runs: AtomicUsize,
}

impl Rewriter {
    fn new(from: &'static str, to: &'static str) -> Arc<Self> {
        Arc::new(Rewriter {
            from,
            to: Mutex::new(Some(to)),
            runs: AtomicUsize::new(0),
        })
    }
    fn set(&self, to: Option<&'static str>) {
        *self.to.lock().unwrap() = to;
    }
}

#[async_trait::async_trait]
impl HookGate for Rewriter {
    async fn pre_tool(
        &self,
        _session_id: &str,
        _run_id: Option<&str>,
        _tool: &str,
        args: &Value,
    ) -> PreToolGate {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let text = args.to_string();
        match *self.to.lock().unwrap() {
            Some(to) if text.contains(self.from) => PreToolGate::Args {
                hook: "garde-git".into(),
                args: serde_json::from_str(&text.replace(self.from, to)).unwrap(),
            },
            _ => PreToolGate::Continue,
        }
    }
}

/// Exécuteur qui garde ce qu'il a reçu.
#[derive(Default)]
struct Recorder(Mutex<Vec<Value>>);

impl Recorder {
    fn seen(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ToolExecutor for Recorder {
    async fn execute(
        &self,
        _name: &str,
        args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        self.0.lock().unwrap().push(args.clone());
        Ok(ToolOutcome::ok(json!({"ok": true})))
    }
}

async fn with_hook(
    hook: Arc<Rewriter>,
) -> (tempfile::TempDir, Arc<AgentServices>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let mut s = AgentServices::for_tests(dir.path(), clock).unwrap();
    s.hooks = hook;
    (dir, Arc::new(s), Arc::new(MockProvider::new()))
}

const FORCE: &str = "git push --force origin feat";
const LEASE: &str = "git push --force-with-lease origin feat";

/// Le tour qui pose la carte d'un `git push --force` réécrit par le hook.
async fn force_card(
    loop_: &AgentLoop,
    p: &MockProvider,
    sid: &str,
    conv: &MemoryConversation,
    e: &Recorder,
) -> String {
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "shell_exec", json!({"command": FORCE}))],
    ));
    match loop_
        .run_conversation(&spec(sid), conv, e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    }
}

/// #446 : la carte montrait `--force-with-lease`, l'accord donné c'est `--force` qui
/// partait, parce que la reprise relisait l'appel dans le transcript et retrouvait
/// l'accord par le seul identifiant de l'appel. Ce qui part est maintenant ce que la carte
/// a montré, sans seconde carte, et le résultat le dit au modèle.
#[tokio::test]
async fn an_approved_rewritten_call_runs_as_its_card_showed() {
    let hook = Rewriter::new("--force ", "--force-with-lease ");
    let (_d, s, p) = with_hook(hook.clone()).await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "pousse la branche");
    let e = Recorder::default();
    let loop_ = AgentLoop::new(s.clone(), p.clone());

    let id = force_card(&loop_, &p, &sid, &conv, &e).await;
    let card = s.approvals.get(&id).await.unwrap().unwrap();
    assert_eq!(card.payload["arguments"]["command"], LEASE);
    assert!(
        card.payload["args_digest"].is_string(),
        "{:#}",
        card.payload
    );
    assert!(e.seen().is_empty(), "rien ne part avant l'accord");

    loop_
        .decide_approval(&id, &Decision::approve_once("telegram"))
        .await
        .unwrap();
    p.reply("poussé");
    let out = loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(
        e.seen(),
        [json!({"command": LEASE})],
        "ce que la carte a montré, une fois"
    );
    assert!(
        s.approvals.pending(10).await.unwrap().is_empty(),
        "l'accord donné n'est pas redemandé"
    );

    // Le transcript garde l'appel émis ; le résultat dit ce qui est parti à sa place.
    let msgs = conv.messages();
    assert_eq!(msgs[1].tool_calls[0].arguments["command"], FORCE);
    let result = msgs[2].text();
    assert!(
        result.starts_with("[hook `garde-git`] Arguments modifiés avant l'exécution"),
        "{result}"
    );
    assert!(result.contains(LEASE), "{result}");
}

/// Un accord ne vaut que pour les arguments de sa carte : si, à la reprise, l'appel n'est
/// plus réécrit, ou l'est autrement, une carte nouvelle montre ce qui partirait, et rien
/// ne part sans elle.
#[tokio::test]
async fn an_approval_does_not_cover_other_arguments() {
    let hook = Rewriter::new("--force ", "--force-with-lease ");
    let (_d, s, p) = with_hook(hook.clone()).await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "pousse la branche");
    let e = Recorder::default();
    let loop_ = AgentLoop::new(s.clone(), p.clone());

    let first = force_card(&loop_, &p, &sid, &conv, &e).await;
    loop_
        .decide_approval(&first, &Decision::approve_once("telegram"))
        .await
        .unwrap();

    // Le hook réécrit autrement : l'accord portait sur `--force-with-lease`.
    hook.set(Some("--force-if-includes "));
    let second = match loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    assert_ne!(second, first);
    assert!(
        e.seen().is_empty(),
        "rien ne part sous un accord donné à autre chose"
    );
    let card = s.approvals.get(&second).await.unwrap().unwrap();
    assert_eq!(
        card.payload["arguments"]["command"],
        "git push --force-if-includes origin feat"
    );
    loop_
        .decide_approval(&second, &Decision::approve_once("telegram"))
        .await
        .unwrap();

    // Le hook ne réécrit plus : l'appel d'origine n'a jamais été montré.
    hook.set(None);
    let third = match loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    assert!(e.seen().is_empty());
    let card = s.approvals.get(&third).await.unwrap().unwrap();
    assert_eq!(card.payload["arguments"]["command"], FORCE);

    // Cette carte-là approuvée, c'est bien l'appel d'origine qui part, sans note.
    loop_
        .decide_approval(&third, &Decision::approve_once("telegram"))
        .await
        .unwrap();
    p.reply("poussé");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(e.seen(), [json!({"command": FORCE})]);
    assert_eq!(
        conv.messages()[2].text(),
        ToolOutcome::ok(json!({"ok": true})).text
    );
}

/// Sans carte aussi, le modèle apprend ce qui est parti : l'appel réécrit s'exécutait
/// déjà, mais le transcript ne montrait que l'appel émis.
#[tokio::test]
async fn a_rewritten_call_without_a_card_tells_the_model_what_ran() {
    let hook = Rewriter::new("secrets.env", "secrets.env.example");
    let (_d, s, p) = with_hook(hook.clone()).await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis la configuration");
    let e = Recorder::default();
    let loop_ = AgentLoop::new(s.clone(), p.clone());

    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("c1", "fs_read", json!({"path": "secrets.env"})),
            call("c2", "fs_read", json!({"path": "README.md"})),
        ],
    ));
    p.reply("lu");
    let out = loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(
        e.seen(),
        [
            json!({"path": "secrets.env.example"}),
            json!({"path": "README.md"})
        ]
    );
    let msgs = conv.messages();
    assert!(
        msgs[2]
            .text()
            .starts_with("[hook `garde-git`] Arguments modifiés avant l'exécution. Exécuté : "),
        "{}",
        msgs[2].text()
    );
    assert!(msgs[2].text().contains("secrets.env.example"));
    assert!(!msgs[3].text().contains("[hook"), "{}", msgs[3].text());
    assert_eq!(hook.runs.load(Ordering::SeqCst), 2, "un passage par appel");
}

/// Une demande posée avant la 1.0.100 n'a pas d'empreinte : approuvée après la mise à
/// jour, elle vaut comme avant, une fois, et l'appel du transcript part.
#[tokio::test]
async fn an_approval_without_a_digest_still_counts_once() {
    let hook = Rewriter::new("--force ", "--force-with-lease ");
    let (_d, s, p) = with_hook(hook.clone()).await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "pousse la branche");
    let e = Recorder::default();
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    conv.record(
        &ChatMessage::assistant("").with_tool_calls(vec![call(
            "c1",
            "shell_exec",
            json!({"command": FORCE}),
        )]),
        false,
    )
    .await
    .unwrap();
    let old = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            RiskClass::Write,
            json!({"tool": "shell_exec", "arguments": {"command": LEASE}, "call_id": "c1"}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&sid),
            None,
            false,
        )
        .await
        .unwrap();
    loop_
        .decide_approval(&old.id.0, &Decision::approve_once("telegram"))
        .await
        .unwrap();
    p.reply("poussé");
    let out = loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(e.seen(), [json!({"command": FORCE})]);
    assert!(s.approvals.pending(10).await.unwrap().is_empty());
    assert_eq!(hook.runs.load(Ordering::SeqCst), 0, "décision déjà prise");
}
