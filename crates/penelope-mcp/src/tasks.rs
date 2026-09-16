//! Tâches MCP longues comme **jobs durables** (§8.4, extension Tasks).
//!
//! Une tâche MCP est suivie hors du tour : elle survit au redémarrage du daemon, notifie
//! à la fin, et son résultat est livré même si le tour d'origine est clos depuis
//! longtemps.

use crate::error::Result;
use penelope_kernel::clock::SharedClock;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Working => "working",
            TaskState::InputRequired => "input_required",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<TaskState> {
        Some(match s {
            "working" | "running" | "in_progress" => TaskState::Working,
            "input_required" => TaskState::InputRequired,
            "completed" | "complete" | "succeeded" => TaskState::Completed,
            "failed" | "error" => TaskState::Failed,
            "cancelled" | "canceled" => TaskState::Cancelled,
            _ => return None,
        })
    }
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTask {
    pub id: String,
    pub server: String,
    pub task_ref: String,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub request: Value,
    pub state: TaskState,
    pub result: Option<Value>,
    pub poll_at: Option<String>,
}

#[derive(Clone)]
pub struct TaskStore {
    store: Store,
    clock: SharedClock,
}

impl TaskStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        TaskStore { store, clock }
    }

    pub async fn create(
        &self,
        server: &str,
        task_ref: &str,
        session_id: Option<&str>,
        run_id: Option<&str>,
        request: &Value,
    ) -> Result<McpTask> {
        let t = McpTask {
            id: format!("mt_{}", penelope_kernel::ids::Ulid::new()),
            server: server.to_string(),
            task_ref: task_ref.to_string(),
            session_id: session_id.map(String::from),
            run_id: run_id.map(String::from),
            request: request.clone(),
            state: TaskState::Working,
            result: None,
            poll_at: Some(self.clock.now_rfc3339()),
        };
        let row = t.clone();
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mcp_tasks(id, server, task_ref, session_id, run_id, request,
                        state, poll_at, created_at, updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,'working',?7,?7,?7)",
                    params![
                        row.id,
                        row.server,
                        row.task_ref,
                        row.session_id,
                        row.run_id,
                        row.request.to_string(),
                        ts
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(t)
    }

    /// Tâches à interroger maintenant.
    pub async fn due(&self, limit: i64) -> Result<Vec<McpTask>> {
        let now = self.clock.now_rfc3339();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, server, task_ref, session_id, run_id, request, state, result,
                            poll_at
                     FROM mcp_tasks
                     WHERE state IN ('working','input_required')
                       AND (poll_at IS NULL OR poll_at <= ?1)
                     ORDER BY poll_at LIMIT ?2",
                )?;
                let rows = st.query_map(params![now, limit], row_to_task)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?)
    }

    /// Met à jour l'état après un `tasks/get`.
    pub async fn update(
        &self,
        id: &str,
        state: TaskState,
        result: Option<Value>,
        next_poll_ms: Option<i64>,
    ) -> Result<()> {
        let now_ms = self.clock.now_ms();
        let (id, ts) = (id.to_string(), self.clock.now_rfc3339());
        let poll_at = next_poll_ms.map(|d| {
            chrono::DateTime::from_timestamp_millis(now_ms + d)
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        });
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE mcp_tasks SET state=?2, result=?3, poll_at=?4, updated_at=?5
                     WHERE id=?1",
                    params![
                        id,
                        state.as_str(),
                        result.map(|r| r.to_string()),
                        poll_at,
                        ts
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<McpTask>> {
        let id = id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, server, task_ref, session_id, run_id, request, state, result,
                            poll_at FROM mcp_tasks WHERE id = ?1",
                )?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_task(r)?)),
                    None => Ok(None),
                }
            })
            .await?)
    }

    /// Au démarrage : les tâches non terminales sont reprises immédiatement (§8, CA 8 :
    /// « tâche MCP longue + `kill -9` : la tâche est retrouvée et son résultat livré »).
    pub async fn recover_on_boot(&self) -> Result<Vec<McpTask>> {
        let now = self.clock.now_rfc3339();
        Ok(self
            .store
            .write(move |tx| {
                tx.execute(
                    "UPDATE mcp_tasks SET poll_at = ?1
                     WHERE state IN ('working','input_required')",
                    params![now],
                )?;
                let mut st = tx.prepare(
                    "SELECT id, server, task_ref, session_id, run_id, request, state, result,
                            poll_at FROM mcp_tasks WHERE state IN ('working','input_required')",
                )?;
                let rows = st.query_map([], row_to_task)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?)
    }

    /// Intervalle de polling : progressif, de 2 s à 60 s.
    pub fn poll_interval_ms(attempt: u32) -> i64 {
        let base = 2_000i64 * (1i64 << attempt.min(5));
        base.min(60_000)
    }
}

fn row_to_task(r: &penelope_store::rusqlite::Row<'_>) -> penelope_store::rusqlite::Result<McpTask> {
    let request: String = r.get(5)?;
    let state: String = r.get(6)?;
    let result: Option<String> = r.get(7)?;
    Ok(McpTask {
        id: r.get(0)?,
        server: r.get(1)?,
        task_ref: r.get(2)?,
        session_id: r.get(3)?,
        run_id: r.get(4)?,
        request: serde_json::from_str(&request).unwrap_or(Value::Null),
        state: TaskState::parse(&state).unwrap_or(TaskState::Working),
        result: result.and_then(|s| serde_json::from_str(&s).ok()),
        poll_at: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use serde_json::json;
    use std::sync::Arc;

    fn tasks(clock: TestClock) -> TaskStore {
        TaskStore::new(Store::open_memory().unwrap(), Arc::new(clock))
    }

    #[tokio::test]
    async fn create_poll_and_complete() {
        let clock = TestClock::default();
        let t = tasks(clock.clone());
        let task = t
            .create(
                "forge",
                "task-42",
                Some("s1"),
                Some("r1"),
                &json!({"tool":"build"}),
            )
            .await
            .unwrap();

        let due = t.due(10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_ref, "task-42");

        t.update(&task.id, TaskState::Working, None, Some(30_000))
            .await
            .unwrap();
        assert!(t.due(10).await.unwrap().is_empty(), "pas encore l'heure");

        clock.advance_ms(31_000);
        assert_eq!(t.due(10).await.unwrap().len(), 1);

        t.update(
            &task.id,
            TaskState::Completed,
            Some(json!({"ok":true})),
            None,
        )
        .await
        .unwrap();
        assert!(t.due(10).await.unwrap().is_empty());
        let got = t.get(&task.id).await.unwrap().unwrap();
        assert_eq!(got.state, TaskState::Completed);
        assert_eq!(got.result.unwrap()["ok"], true);
    }

    /// CA 8 : tâche MCP longue + `kill -9` ⇒ retrouvée et livrée après redémarrage.
    #[tokio::test]
    async fn ca_8_6_tasks_survive_a_restart() {
        let clock = TestClock::default();
        let store = Store::open_memory().unwrap();
        let t = TaskStore::new(store.clone(), Arc::new(clock.clone()));
        let task = t
            .create("forge", "task-99", Some("s1"), None, &json!({}))
            .await
            .unwrap();
        t.update(&task.id, TaskState::Working, None, Some(3_600_000))
            .await
            .unwrap();

        // Redémarrage : le poll est ramené à maintenant, la tâche repart tout de suite.
        let t2 = TaskStore::new(store, Arc::new(clock));
        let recovered = t2.recover_on_boot().await.unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, task.id);
        assert_eq!(t2.due(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn terminal_tasks_are_not_recovered() {
        let store = Store::open_memory().unwrap();
        let t = TaskStore::new(store.clone(), Arc::new(TestClock::default()));
        let task = t.create("s", "x", None, None, &json!({})).await.unwrap();
        t.update(&task.id, TaskState::Failed, None, None)
            .await
            .unwrap();
        assert!(
            TaskStore::new(store, Arc::new(TestClock::default()))
                .recover_on_boot()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn state_parsing_accepts_provider_variants() {
        assert_eq!(TaskState::parse("in_progress"), Some(TaskState::Working));
        assert_eq!(TaskState::parse("succeeded"), Some(TaskState::Completed));
        assert_eq!(TaskState::parse("canceled"), Some(TaskState::Cancelled));
        assert!(TaskState::Completed.is_terminal());
        assert!(!TaskState::InputRequired.is_terminal());
    }

    #[test]
    fn poll_interval_grows_and_caps() {
        assert_eq!(TaskStore::poll_interval_ms(0), 2_000);
        assert_eq!(TaskStore::poll_interval_ms(3), 16_000);
        assert_eq!(TaskStore::poll_interval_ms(10), 60_000);
    }
}
