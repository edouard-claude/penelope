//! Exécuteur durable de workflows (§12.7).
//!
//! Chaque run est une session `workflow_run` avec un **ledger d'opérations** (§4.2).
//! Reprise après crash : on repart de l'étape courante, les effets `completed` sont
//! rejoués depuis le ledger, une étape `agent` interrompue reprend avec son historique et
//! un nudge.

use crate::model::{BLOCKED, DONE, StepResult, Workflow};
use penelope_kernel::clock::SharedClock;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Paused,
    Blocked,
    Done,
    Failed,
    Cancelled,
}

impl RunState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Paused => "paused",
            RunState::Blocked => "blocked",
            RunState::Done => "done",
            RunState::Failed => "failed",
            RunState::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<RunState> {
        Some(match s {
            "running" => RunState::Running,
            "paused" => RunState::Paused,
            "blocked" => RunState::Blocked,
            "done" => RunState::Done,
            "failed" => RunState::Failed,
            "cancelled" => RunState::Cancelled,
            _ => return None,
        })
    }
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunState::Done | RunState::Failed | RunState::Cancelled
        )
    }
    /// Un run peut-il avancer ?
    pub fn is_advanceable(&self) -> bool {
        matches!(self, RunState::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub workflow_id: String,
    pub session_id: String,
    pub params: Value,
    pub state: RunState,
    pub current_step: Option<String>,
    pub phase: Option<String>,
    pub iterations: u32,
    pub max_iterations: u32,
    pub step_outputs: Value,
    pub workdir: Option<String>,
    pub spent_usd: f64,
    pub spent_tokens: u64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub parent_run: Option<String>,
    pub depth: u32,
}

/// Opérations de contrôle (§12.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    Cancel,
    RetryStep,
    /// Approbation requise (§12.7).
    SkipStep,
    /// Réservé à l'administration.
    Goto(String),
}

impl Control {
    pub fn parse(s: &str) -> Option<Control> {
        Some(match s {
            "pause" => Control::Pause,
            "resume" => Control::Resume,
            "cancel" => Control::Cancel,
            "retry-step" | "retry_step" => Control::RetryStep,
            "skip-step" | "skip_step" => Control::SkipStep,
            other => Control::Goto(other.strip_prefix("goto:")?.to_string()),
        })
    }
    /// Les opérations qui exigent une approbation du propriétaire.
    pub fn requires_approval(&self) -> bool {
        matches!(self, Control::SkipStep | Control::Goto(_))
    }
}

#[derive(Clone)]
pub struct RunStore {
    store: Store,
    clock: SharedClock,
}

impl RunStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        RunStore { store, clock }
    }

    pub async fn create(
        &self,
        workflow: &Workflow,
        session_id: &str,
        params: Value,
        workdir: Option<&str>,
        parent: Option<&str>,
        depth: u32,
    ) -> penelope_store::Result<Run> {
        let run = Run {
            id: format!("r_{}", penelope_kernel::ids::Ulid::new()),
            workflow_id: workflow.metadata.id.clone(),
            session_id: session_id.to_string(),
            params,
            state: RunState::Running,
            current_step: Some(workflow.entry_step.clone()),
            phase: workflow
                .step(&workflow.entry_step)
                .map(|s| s.phase.as_str().to_string()),
            iterations: 0,
            max_iterations: workflow.settings.max_iterations,
            step_outputs: json!({}),
            workdir: workdir.map(String::from),
            spent_usd: 0.0,
            spent_tokens: 0,
            started_at: self.clock.now_rfc3339(),
            finished_at: None,
            result: None,
            error: None,
            parent_run: parent.map(String::from),
            depth,
        };
        let row = run.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO workflow_runs(id, workflow_id, session_id, params, state,
                        current_step, phase, iterations, max_iterations, step_outputs, workdir,
                        started_at, updated_at, parent_run, depth)
                     VALUES(?1,?2,?3,?4,'running',?5,?6,0,?7,'{}',?8,?9,?9,?10,?11)",
                    params![
                        row.id,
                        row.workflow_id,
                        row.session_id,
                        row.params.to_string(),
                        row.current_step,
                        row.phase,
                        row.max_iterations as i64,
                        row.workdir,
                        row.started_at,
                        row.parent_run,
                        row.depth as i64
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(run)
    }

    pub async fn get(&self, run_id: &str) -> penelope_store::Result<Option<Run>> {
        let id = run_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!("{SELECT} WHERE id = ?1"))?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_run(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    pub async fn list(
        &self,
        state: Option<RunState>,
        limit: i64,
    ) -> penelope_store::Result<Vec<Run>> {
        let s = state.map(|s| s.as_str().to_string());
        self.store
            .read(move |c| {
                let sql = format!(
                    "{SELECT} WHERE (?1 IS NULL OR state = ?1) ORDER BY started_at DESC LIMIT ?2"
                );
                let mut st = c.prepare(&sql)?;
                let rows = st.query_map(params![s, limit], row_to_run)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Enregistre le résultat d'une étape et avance vers la suivante.
    ///
    /// L'incrément d'itérations et la transition sont dans la **même transaction** :
    /// un crash ne peut pas compter une itération sans avancer, ni l'inverse.
    pub async fn advance(
        &self,
        run_id: &str,
        step_id: &str,
        result: &StepResult,
        output: Value,
        next_step: &str,
        next_phase: Option<&str>,
    ) -> penelope_store::Result<Run> {
        let (id, step, res, next, phase, now) = (
            run_id.to_string(),
            step_id.to_string(),
            result.as_str().to_string(),
            next_step.to_string(),
            next_phase.map(String::from),
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                let raw: String = tx.query_row(
                    "SELECT step_outputs FROM workflow_runs WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )?;
                let mut outputs: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                if let Some(o) = outputs.as_object_mut() {
                    o.insert(step.clone(), output.clone());
                    o.insert("__last".into(), output);
                }

                tx.execute(
                    "INSERT INTO workflow_step_log(run_id, step_id, started_at, ended_at, result,
                        output)
                     VALUES(?1,?2,?3,?3,?4,?5)",
                    params![id, step, now, res, outputs[&step].to_string()],
                )?;

                let terminal = next == DONE || next == BLOCKED;
                let state = if next == DONE {
                    "done"
                } else if next == BLOCKED {
                    "blocked"
                } else {
                    "running"
                };
                tx.execute(
                    "UPDATE workflow_runs SET current_step = ?2, phase = ?3,
                        iterations = iterations + 1, step_outputs = ?4, state = ?5,
                        updated_at = ?6, finished_at = CASE WHEN ?7 = 1 AND ?5 = 'done'
                            THEN ?6 ELSE finished_at END
                     WHERE id = ?1",
                    params![
                        id,
                        if terminal { None } else { Some(next.clone()) },
                        phase,
                        outputs.to_string(),
                        state,
                        now,
                        terminal as i64
                    ],
                )?;

                let mut st = tx.prepare(&format!("{SELECT} WHERE id = ?1"))?;
                let mut rows = st.query([&id])?;
                let r = rows
                    .next()?
                    .ok_or_else(|| penelope_store::StoreError::other("run introuvable"))?;
                row_to_run(r).map_err(penelope_store::StoreError::from)
            })
            .await
    }

    /// Répertoire de travail du run, connu une fois le run créé.
    pub async fn set_workdir(&self, run_id: &str, workdir: &str) -> penelope_store::Result<()> {
        let (id, w) = (run_id.to_string(), workdir.to_string());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workflow_runs SET workdir = ?2 WHERE id = ?1",
                    params![id, w],
                )?;
                Ok(())
            })
            .await
    }

    /// Oublie le répertoire de travail d'un run, une fois nettoyé.
    pub async fn forget_workdir(&self, run_id: &str) -> penelope_store::Result<()> {
        let id = run_id.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workflow_runs SET workdir = NULL WHERE id = ?1",
                    params![id],
                )?;
                Ok(())
            })
            .await
    }

    /// Dépense du run, recalculée depuis le ledger d'usage.
    pub async fn set_spent(
        &self,
        run_id: &str,
        usd: f64,
        tokens: u64,
    ) -> penelope_store::Result<()> {
        let id = run_id.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workflow_runs SET spent_usd = ?2, spent_tokens = ?3 WHERE id = ?1",
                    params![id, usd, tokens as i64],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn set_state(
        &self,
        run_id: &str,
        state: RunState,
        error: Option<&str>,
    ) -> penelope_store::Result<()> {
        let (id, s, e, now) = (
            run_id.to_string(),
            state.as_str().to_string(),
            error.map(String::from),
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workflow_runs SET state = ?2, error = COALESCE(?3, error),
                        updated_at = ?4,
                        finished_at = CASE WHEN ?2 IN ('done','failed','cancelled') THEN ?4
                                           ELSE finished_at END
                     WHERE id = ?1",
                    params![id, s, e, now],
                )?;
                Ok(())
            })
            .await
    }

    /// Applique une opération de contrôle.
    pub async fn control(&self, run_id: &str, op: &Control) -> penelope_store::Result<RunState> {
        let state = match op {
            Control::Pause => RunState::Paused,
            Control::Resume => RunState::Running,
            Control::Cancel => RunState::Cancelled,
            Control::RetryStep | Control::SkipStep | Control::Goto(_) => RunState::Running,
        };
        if let Control::Goto(target) = op {
            let (id, t) = (run_id.to_string(), target.clone());
            self.store
                .write(move |tx| {
                    tx.execute(
                        "UPDATE workflow_runs SET current_step = ?2 WHERE id = ?1",
                        params![id, t],
                    )?;
                    Ok(())
                })
                .await?;
        }
        self.set_state(run_id, state, None).await?;
        Ok(state)
    }

    /// Reprise après crash : tout run `running` redevient candidat, à son étape courante.
    pub async fn recover_on_boot(&self) -> penelope_store::Result<Vec<Run>> {
        self.store
            .read(|c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE state IN ('running','blocked') ORDER BY started_at"
                ))?;
                let rows = st.query_map([], row_to_run)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Journal d'un run (`penelope wf trace`).
    pub async fn trace(&self, run_id: &str) -> penelope_store::Result<Vec<Value>> {
        let id = run_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT step_id, attempt, started_at, ended_at, result, output, error
                     FROM workflow_step_log WHERE run_id = ?1 ORDER BY id",
                )?;
                let rows = st.query_map([&id], |r| {
                    Ok(json!({
                        "step": r.get::<_, String>(0)?,
                        "attempt": r.get::<_, i64>(1)?,
                        "startedAt": r.get::<_, String>(2)?,
                        "endedAt": r.get::<_, Option<String>>(3)?,
                        "result": r.get::<_, Option<String>>(4)?,
                        "output": r.get::<_, Option<String>>(5)?
                            .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                        "error": r.get::<_, Option<String>>(6)?,
                    }))
                })?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Nettoyage des workspaces éphémères (§12.7 : 7 jours après `$done`).
    pub async fn expired_workspaces(
        &self,
        retention_days: i64,
    ) -> penelope_store::Result<Vec<(String, String)>> {
        let cutoff = (self.clock.now_utc() - chrono::Duration::days(retention_days))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, workdir FROM workflow_runs
                     WHERE workdir IS NOT NULL AND finished_at IS NOT NULL AND finished_at < ?1",
                )?;
                let rows = st.query_map([cutoff], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Admission d'un nouveau run selon la politique de concurrence (§12.2).
    pub async fn admit(
        &self,
        workflow_id: &str,
        max_concurrent: u32,
        admission: &str,
    ) -> penelope_store::Result<Admission> {
        let id = workflow_id.to_string();
        let active: i64 = self
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM workflow_runs
                     WHERE workflow_id = ?1 AND state IN ('running','paused','blocked')",
                    [id],
                    |r| r.get(0),
                )?)
            })
            .await?;
        if (active as u32) < max_concurrent.max(1) {
            return Ok(Admission::Start);
        }
        Ok(match admission {
            "parallel" => Admission::Start,
            "drop" => Admission::Drop,
            "coalesce" => Admission::Coalesce,
            _ => Admission::Hold,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Start,
    Hold,
    Coalesce,
    Drop,
}

const SELECT: &str = "SELECT id, workflow_id, session_id, params, state, current_step, phase,
     iterations, max_iterations, step_outputs, workdir, spent_usd, spent_tokens, started_at,
     finished_at, result, error, parent_run, depth FROM workflow_runs";

fn row_to_run(r: &penelope_store::rusqlite::Row<'_>) -> penelope_store::rusqlite::Result<Run> {
    let params_s: String = r.get(3)?;
    let state: String = r.get(4)?;
    let outputs: String = r.get(9)?;
    Ok(Run {
        id: r.get(0)?,
        workflow_id: r.get(1)?,
        session_id: r.get(2)?,
        params: serde_json::from_str(&params_s).unwrap_or(json!({})),
        state: RunState::parse(&state).unwrap_or(RunState::Failed),
        current_step: r.get(5)?,
        phase: r.get(6)?,
        iterations: r.get::<_, i64>(7)? as u32,
        max_iterations: r.get::<_, i64>(8)? as u32,
        step_outputs: serde_json::from_str(&outputs).unwrap_or(json!({})),
        workdir: r.get(10)?,
        spent_usd: r.get(11)?,
        spent_tokens: r.get::<_, i64>(12)? as u64,
        started_at: r.get(13)?,
        finished_at: r.get(14)?,
        result: r.get(15)?,
        error: r.get(16)?,
        parent_run: r.get(17)?,
        depth: r.get::<_, i64>(18)? as u32,
    })
}

/// Vérifie les bornes d'un run avant de continuer (§12.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Limit {
    Ok,
    IterationsExhausted,
    BudgetUsd,
    BudgetTokens,
    WallClock,
}

pub fn check_limits(run: &Run, budget: &crate::model::Budget, now_ms: i64) -> Limit {
    if run.iterations >= run.max_iterations {
        return Limit::IterationsExhausted;
    }
    if budget.max_usd > 0.0 && run.spent_usd >= budget.max_usd {
        return Limit::BudgetUsd;
    }
    if budget.max_tokens > 0 && run.spent_tokens >= budget.max_tokens {
        return Limit::BudgetTokens;
    }
    if budget.max_wall_ms > 0 {
        let started = chrono::DateTime::parse_from_rfc3339(&run.started_at)
            .map(|d| d.timestamp_millis())
            .unwrap_or(now_ms);
        if now_ms - started >= budget.max_wall_ms as i64 {
            return Limit::WallClock;
        }
    }
    Limit::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Metadata, Settings, Step, Transition};
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    fn workflow() -> Workflow {
        Workflow {
            metadata: Metadata {
                id: "demo".into(),
                ..Default::default()
            },
            entry_step: "un".into(),
            settings: Settings::default(),
            start_condition: json!({"type":"always"}),
            steps: vec![
                Step {
                    id: "un".into(),
                    kind: "shell".into(),
                    command: json!("echo un"),
                    transitions: vec![Transition::always("deux")],
                    ..Default::default()
                },
                Step {
                    id: "deux".into(),
                    kind: "shell".into(),
                    command: json!("echo deux"),
                    transitions: vec![Transition::always(DONE)],
                    ..Default::default()
                },
            ],
        }
    }

    async fn runs(clock: TestClock) -> RunStore {
        let store = Store::open_memory().unwrap();
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES('s1','workflow_run','t','t')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        RunStore::new(store, Arc::new(clock))
    }

    #[tokio::test]
    async fn create_advance_and_finish() {
        let rs = runs(TestClock::default()).await;
        let w = workflow();
        let run = rs
            .create(&w, "s1", json!({"x":1}), Some("/tmp/r1"), None, 0)
            .await
            .unwrap();
        assert_eq!(run.current_step.as_deref(), Some("un"));
        assert_eq!(run.state, RunState::Running);

        let r = rs
            .advance(
                &run.id,
                "un",
                &StepResult::Success,
                json!({"exitCode":0}),
                "deux",
                Some("build"),
            )
            .await
            .unwrap();
        assert_eq!(r.current_step.as_deref(), Some("deux"));
        assert_eq!(r.iterations, 1);
        assert_eq!(r.step_outputs["un"]["exitCode"], 0);
        assert_eq!(r.step_outputs["__last"]["exitCode"], 0);

        let r = rs
            .advance(&run.id, "deux", &StepResult::Success, json!({}), DONE, None)
            .await
            .unwrap();
        assert_eq!(r.state, RunState::Done);
        assert!(r.finished_at.is_some());
        assert!(r.current_step.is_none());
    }

    #[tokio::test]
    async fn blocked_runs_can_be_resumed() {
        let rs = runs(TestClock::default()).await;
        let run = rs
            .create(&workflow(), "s1", json!({}), None, None, 0)
            .await
            .unwrap();
        let r = rs
            .advance(
                &run.id,
                "un",
                &StepResult::Failure,
                json!({}),
                BLOCKED,
                None,
            )
            .await
            .unwrap();
        assert_eq!(r.state, RunState::Blocked);

        rs.control(&run.id, &Control::Resume).await.unwrap();
        assert_eq!(
            rs.get(&run.id).await.unwrap().unwrap().state,
            RunState::Running
        );
    }

    /// CA 12 : `kill -9` à chaque étape ⇒ reprise sans double effet.
    #[tokio::test]
    async fn ca_12_2_runs_are_recovered_at_their_current_step() {
        let clock = TestClock::default();
        let store = Store::open_memory().unwrap();
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES('s1','workflow_run','t','t')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let rs = RunStore::new(store.clone(), Arc::new(clock.clone()));
        let run = rs
            .create(&workflow(), "s1", json!({}), None, None, 0)
            .await
            .unwrap();
        rs.advance(
            &run.id,
            "un",
            &StepResult::Success,
            json!({"a":1}),
            "deux",
            None,
        )
        .await
        .unwrap();

        // « kill -9 » : nouvel exécuteur sur la même base.
        let rs2 = RunStore::new(store, Arc::new(clock));
        let recovered = rs2.recover_on_boot().await.unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, run.id);
        assert_eq!(
            recovered[0].current_step.as_deref(),
            Some("deux"),
            "la reprise repart de l'étape courante, pas du début"
        );
        assert_eq!(
            recovered[0].step_outputs["un"]["a"], 1,
            "les sorties déjà produites sont conservées"
        );
        assert_eq!(
            recovered[0].iterations, 1,
            "l'itération n'est pas recomptée"
        );
    }

    #[tokio::test]
    async fn control_operations() {
        let rs = runs(TestClock::default()).await;
        let run = rs
            .create(&workflow(), "s1", json!({}), None, None, 0)
            .await
            .unwrap();

        assert_eq!(
            rs.control(&run.id, &Control::Pause).await.unwrap(),
            RunState::Paused
        );
        assert!(
            !rs.get(&run.id)
                .await
                .unwrap()
                .unwrap()
                .state
                .is_advanceable()
        );
        assert_eq!(
            rs.control(&run.id, &Control::Resume).await.unwrap(),
            RunState::Running
        );

        rs.control(&run.id, &Control::Goto("deux".into()))
            .await
            .unwrap();
        assert_eq!(
            rs.get(&run.id)
                .await
                .unwrap()
                .unwrap()
                .current_step
                .as_deref(),
            Some("deux")
        );

        rs.control(&run.id, &Control::Cancel).await.unwrap();
        assert!(rs.get(&run.id).await.unwrap().unwrap().state.is_terminal());
    }

    #[test]
    fn control_parsing_and_approval() {
        assert_eq!(Control::parse("pause"), Some(Control::Pause));
        assert_eq!(Control::parse("retry-step"), Some(Control::RetryStep));
        assert_eq!(
            Control::parse("goto:etape"),
            Some(Control::Goto("etape".into()))
        );
        assert!(Control::parse("inventé").is_none());
        assert!(Control::SkipStep.requires_approval());
        assert!(!Control::Pause.requires_approval());
    }

    #[tokio::test]
    async fn trace_records_every_step() {
        let rs = runs(TestClock::default()).await;
        let run = rs
            .create(&workflow(), "s1", json!({}), None, None, 0)
            .await
            .unwrap();
        rs.advance(
            &run.id,
            "un",
            &StepResult::Success,
            json!({"a":1}),
            "deux",
            None,
        )
        .await
        .unwrap();
        rs.advance(
            &run.id,
            "deux",
            &StepResult::Failure,
            json!({}),
            BLOCKED,
            None,
        )
        .await
        .unwrap();
        let t = rs.trace(&run.id).await.unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["step"], "un");
        assert_eq!(t[1]["result"], "failure");
    }

    #[tokio::test]
    async fn admission_policies() {
        let rs = runs(TestClock::default()).await;
        let w = workflow();
        rs.create(&w, "s1", json!({}), None, None, 0).await.unwrap();

        assert_eq!(rs.admit("demo", 2, "hold").await.unwrap(), Admission::Start);
        assert_eq!(rs.admit("demo", 1, "hold").await.unwrap(), Admission::Hold);
        assert_eq!(rs.admit("demo", 1, "drop").await.unwrap(), Admission::Drop);
        assert_eq!(
            rs.admit("demo", 1, "coalesce").await.unwrap(),
            Admission::Coalesce
        );
        assert_eq!(
            rs.admit("demo", 1, "parallel").await.unwrap(),
            Admission::Start
        );
        assert_eq!(
            rs.admit("autre", 1, "hold").await.unwrap(),
            Admission::Start
        );
    }

    #[tokio::test]
    async fn expired_workspaces_are_listed() {
        let clock = TestClock::default();
        let rs = runs(clock.clone()).await;
        let run = rs
            .create(&workflow(), "s1", json!({}), Some("/tmp/r1"), None, 0)
            .await
            .unwrap();
        rs.advance(&run.id, "un", &StepResult::Success, json!({}), DONE, None)
            .await
            .unwrap();
        assert!(rs.expired_workspaces(7).await.unwrap().is_empty());
        clock.advance_days(8);
        let expired = rs.expired_workspaces(7).await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].1, "/tmp/r1");
    }

    #[test]
    fn limits_are_checked() {
        let budget = crate::model::Budget {
            max_usd: 5.0,
            max_tokens: 1000,
            max_wall_ms: 60_000,
        };
        let base = Run {
            id: "r".into(),
            workflow_id: "w".into(),
            session_id: "s".into(),
            params: json!({}),
            state: RunState::Running,
            current_step: Some("un".into()),
            phase: None,
            iterations: 0,
            max_iterations: 40,
            step_outputs: json!({}),
            workdir: None,
            spent_usd: 0.0,
            spent_tokens: 0,
            started_at: "2026-01-01T00:00:00Z".into(),
            finished_at: None,
            result: None,
            error: None,
            parent_run: None,
            depth: 0,
        };
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(check_limits(&base, &budget, t0), Limit::Ok);
        assert_eq!(
            check_limits(
                &Run {
                    iterations: 40,
                    ..base.clone()
                },
                &budget,
                t0
            ),
            Limit::IterationsExhausted
        );
        assert_eq!(
            check_limits(
                &Run {
                    spent_usd: 6.0,
                    ..base.clone()
                },
                &budget,
                t0
            ),
            Limit::BudgetUsd
        );
        assert_eq!(
            check_limits(
                &Run {
                    spent_tokens: 2000,
                    ..base.clone()
                },
                &budget,
                t0
            ),
            Limit::BudgetTokens
        );
        assert_eq!(check_limits(&base, &budget, t0 + 120_000), Limit::WallClock);
    }
}
