use super::*;

use penelope_kernel::clock::TestClock;

use penelope_llm::mock::{MockProvider, Scripted};

use std::sync::atomic::{AtomicUsize, Ordering};

mod approvals;
mod attempts;
mod call_guards;
mod effects;
mod fallback;
mod guards;
mod policy;
mod run_loop;
mod turn_bounds;

struct CountingExecutor {
    calls: AtomicUsize,
    fail: bool,
}

#[async_trait::async_trait]
impl ToolExecutor for CountingExecutor {
    async fn execute(
        &self,
        name: &str,
        _args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(penelope_tools::ToolError::Io("disque plein".into()));
        }
        Ok(ToolOutcome::ok(json!({"tool": name, "ok": true})))
    }
}

/// Exécuteur lent qui date le début et la fin de chaque appel (issue #85).
struct TimedExecutor {
    delay: std::time::Duration,
    log: Mutex<Vec<(String, std::time::Instant, std::time::Instant)>>,
}

impl TimedExecutor {
    fn new(ms: u64) -> Self {
        TimedExecutor {
            delay: std::time::Duration::from_millis(ms),
            log: Mutex::new(Vec::new()),
        }
    }
    fn span(&self, key: &str) -> (std::time::Instant, std::time::Instant) {
        let log = self.log.lock().unwrap();
        let (_, a, b) = log.iter().find(|(k, _, _)| k == key).expect(key);
        (*a, *b)
    }
}

#[async_trait::async_trait]
impl ToolExecutor for TimedExecutor {
    async fn execute(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        self.execute_cancellable(name, args, &CancelToken::new())
            .await
    }

    async fn execute_cancellable(
        &self,
        name: &str,
        args: &Value,
        cancel: &CancelToken,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        let key = args["path"]
            .as_str()
            .or(args["command"].as_str())
            .unwrap_or(name)
            .to_string();
        let start = std::time::Instant::now();
        let deadline = start + self.delay;
        while std::time::Instant::now() < deadline {
            if cancel.is_cancelled() {
                self.log
                    .lock()
                    .unwrap()
                    .push((key, start, std::time::Instant::now()));
                return Err(penelope_tools::ToolError::Io("interrompu".into()));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        self.log
            .lock()
            .unwrap()
            .push((key.clone(), start, std::time::Instant::now()));
        if key == "casse.rs" {
            return Err(penelope_tools::ToolError::Io("fichier illisible".into()));
        }
        Ok(ToolOutcome::ok(json!({"lu": key})))
    }
}

fn exec(fail: bool) -> CountingExecutor {
    CountingExecutor {
        calls: AtomicUsize::new(0),
        fail,
    }
}

async fn setup() -> (tempfile::TempDir, Arc<Services>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let p = Arc::new(MockProvider::new());
    (dir, s, p)
}

fn request(session_id: &str) -> TurnRequest {
    TurnRequest {
        session_id: session_id.to_string(),
        run_id: None,
        user_text: "corrige le bug".into(),
        model_id: "mock/model".into(),
        tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
        system_prompt: "Tu es Pénélope.".into(),
        allowed_tools: vec![],
        cancel: CancelToken::new(),
    }
}

fn spec(session_id: &str) -> TurnSpec {
    TurnSpec {
        session_id: session_id.to_string(),
        run_id: None,
        turn_id: Some("t_test".into()),
        model_id: "mock/model".into(),
        fallback_models: vec![],
        tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
        allowed_tools: vec![],
        cancel: CancelToken::new(),
    }
}

fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

async fn session(s: &Services) -> String {
    s.sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string()
}

/// Un `git push` était en vol quand le daemon est tombé : l'effet est `dispatching`,
/// l'appel n'a pas de résultat. Le « redémarrage » le passe en `unknown`.
async fn crashed_push() -> (
    tempfile::TempDir,
    Arc<Services>,
    Arc<MockProvider>,
    String,
    MemoryConversation,
    String,
) {
    let (d, s, p) = setup().await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "pousse la branche");
    let args = json!({"command": "git push origin main"});
    conv.record(
        &ChatMessage::assistant("").with_tool_calls(vec![call("c1", "shell_exec", args.clone())]),
        false,
    )
    .await
    .unwrap();
    let id = match s
        .effects
        .plan(
            EffectSpec::new(effect_kind("shell_exec"), "shell_exec", args)
                .session(&sid)
                .step("c1"),
        )
        .await
        .unwrap()
    {
        Planned::Fresh(id) => id,
        o => panic!("{o:?}"),
    };
    s.effects.dispatching(&id).await.unwrap();
    let daemon = crate::runtime::Daemon::from_services(s.clone());
    daemon.recover().await.unwrap();
    // Un second redémarrage ne crée pas de seconde demande.
    daemon.recover().await.unwrap();
    let pending = s.approvals.pending(10).await.unwrap();
    assert_eq!(pending.len(), 1, "une demande par effet : {pending:?}");
    assert_eq!(pending[0].kind, penelope_hitl::ApprovalKind::EffectUnknown);
    let approval = pending[0].id.0.clone();
    (d, s, p, sid, conv, approval)
}

fn spent(turn: &str, calls: usize, each: f64) -> Vec<penelope_kernel::budget::UsageRecord> {
    (0..calls)
        .map(|_| penelope_kernel::budget::UsageRecord {
            turn_id: Some(turn.into()),
            model: "m".into(),
            provider: "mock".into(),
            role: Some("chat".into()),
            prompt: 100_000,
            cost_usd: each,
            ..Default::default()
        })
        .collect()
}
