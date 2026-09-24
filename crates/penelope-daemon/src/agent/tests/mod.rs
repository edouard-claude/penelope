use super::*;

use super::decisions::decide_approval;
use super::turn_log::close_unopened;
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

async fn setup() -> (tempfile::TempDir, Arc<AgentServices>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(AgentServices::for_tests(dir.path(), clock).unwrap());
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

async fn session(s: &AgentServices) -> String {
    penelope_kernel::session::SessionStore::new(s.store.clone(), s.clock.clone())
        .with_events(s.events.clone())
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string()
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
