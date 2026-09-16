//! Budgets par jour, session et run (§10.3 point 6).
//!
//! Deux seuils : `alert_ratio` (notification) et 100 % (demande HITL
//! `budget_exceeded`, choix « augmenter » ou « arrêter »).

use crate::clock::SharedClock;
use crate::error::Result;
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    Daily,
    Session,
    Run,
}

impl BudgetScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetScope::Daily => "jour",
            BudgetScope::Session => "session",
            BudgetScope::Run => "run",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub scope: BudgetScope,
    pub spent_usd: f64,
    pub limit_usd: f64,
    pub ratio: f64,
    pub alerting: bool,
    pub exceeded: bool,
}

impl BudgetStatus {
    pub fn compute(scope: BudgetScope, spent: f64, limit: f64, alert_ratio: f64) -> Self {
        let ratio = if limit > 0.0 { spent / limit } else { 0.0 };
        BudgetStatus {
            scope,
            spent_usd: spent,
            limit_usd: limit,
            ratio,
            alerting: limit > 0.0 && ratio >= alert_ratio && ratio < 1.0,
            exceeded: limit > 0.0 && ratio >= 1.0,
        }
    }
}

/// Une consommation à enregistrer dans le ledger de coûts (§16).
#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub model: String,
    pub provider: String,
    pub role: Option<String>,
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub reasoning: u64,
    pub cost_usd: f64,
    pub estimated: bool,
    pub maybe_duplicate: bool,
}

#[derive(Clone)]
pub struct BudgetLedger {
    store: Store,
    clock: SharedClock,
}

impl BudgetLedger {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        BudgetLedger { store, clock }
    }

    pub async fn record(&self, u: UsageRecord) -> Result<()> {
        let ts = self.clock.now_rfc3339();
        let day = ts.chars().take(10).collect::<String>();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO usage(ts, day, session_id, run_id, model, provider, role,
                        prompt, completion, cached, reasoning, cost_usd, estimated, maybe_dup)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                    params![
                        ts,
                        day,
                        u.session_id,
                        u.run_id,
                        u.model,
                        u.provider,
                        u.role,
                        u.prompt as i64,
                        u.completion as i64,
                        u.cached as i64,
                        u.reasoning as i64,
                        u.cost_usd,
                        u.estimated as i64,
                        u.maybe_duplicate as i64
                    ],
                )?;
                if let Some(sid) = &u.session_id {
                    tx.execute(
                        "UPDATE sessions SET spent_usd = spent_usd + ?2 WHERE id = ?1",
                        params![sid, u.cost_usd],
                    )?;
                }
                if let Some(rid) = &u.run_id {
                    tx.execute(
                        "UPDATE workflow_runs SET spent_usd = spent_usd + ?2,
                         spent_tokens = spent_tokens + ?3 WHERE id = ?1",
                        params![rid, u.cost_usd, (u.prompt + u.completion) as i64],
                    )?;
                }
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn spent_today(&self) -> Result<f64> {
        let day = self
            .clock
            .now_rfc3339()
            .chars()
            .take(10)
            .collect::<String>();
        Ok(self
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(cost_usd), 0) FROM usage WHERE day = ?1",
                    [day],
                    |r| r.get(0),
                )?)
            })
            .await?)
    }

    pub async fn spent_session(&self, session_id: &str) -> Result<f64> {
        let sid = session_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(cost_usd), 0) FROM usage WHERE session_id = ?1",
                    [sid],
                    |r| r.get(0),
                )?)
            })
            .await?)
    }

    pub async fn spent_run(&self, run_id: &str) -> Result<f64> {
        let rid = run_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(cost_usd), 0) FROM usage WHERE run_id = ?1",
                    [rid],
                    |r| r.get(0),
                )?)
            })
            .await?)
    }

    /// Statuts de tous les périmètres pertinents pour un tour.
    pub async fn status(
        &self,
        cfg: &crate::config::Budget,
        session_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<BudgetStatus>> {
        let mut out = vec![BudgetStatus::compute(
            BudgetScope::Daily,
            self.spent_today().await?,
            cfg.daily_usd,
            cfg.alert_ratio,
        )];
        if let Some(s) = session_id {
            out.push(BudgetStatus::compute(
                BudgetScope::Session,
                self.spent_session(s).await?,
                cfg.session_usd,
                cfg.alert_ratio,
            ));
        }
        if let Some(r) = run_id {
            out.push(BudgetStatus::compute(
                BudgetScope::Run,
                self.spent_run(r).await?,
                cfg.run_usd,
                cfg.alert_ratio,
            ));
        }
        Ok(out)
    }

    /// Répartition des coûts (`penelope usage --by model|day|run`).
    pub async fn breakdown(&self, by: &str, limit: i64) -> Result<Vec<(String, f64, i64)>> {
        let column = match by {
            "model" => "model",
            "day" => "day",
            "run" => "COALESCE(run_id, '')",
            "session" => "COALESCE(session_id, '')",
            "provider" => "provider",
            _ => "model",
        };
        let sql = format!(
            "SELECT {column} AS k, SUM(cost_usd), SUM(prompt + completion)
             FROM usage GROUP BY k ORDER BY SUM(cost_usd) DESC LIMIT ?1"
        );
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(&sql)?;
                let rows = st.query_map([limit], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?;
                let mut v = Vec::new();
                for row in rows {
                    v.push(row?);
                }
                Ok(v)
            })
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use std::sync::Arc;

    #[test]
    fn status_thresholds() {
        let s = BudgetStatus::compute(BudgetScope::Daily, 16.0, 20.0, 0.8);
        assert!(s.alerting && !s.exceeded);
        let s = BudgetStatus::compute(BudgetScope::Daily, 20.0, 20.0, 0.8);
        assert!(s.exceeded && !s.alerting);
        let s = BudgetStatus::compute(BudgetScope::Daily, 1.0, 20.0, 0.8);
        assert!(!s.alerting && !s.exceeded);
    }

    #[test]
    fn no_limit_never_alerts() {
        let s = BudgetStatus::compute(BudgetScope::Run, 999.0, 0.0, 0.8);
        assert!(!s.alerting && !s.exceeded);
    }

    #[tokio::test]
    async fn records_and_sums() {
        let store = Store::open_memory().unwrap();
        let l = BudgetLedger::new(store, Arc::new(TestClock::default()));
        for i in 0..3 {
            l.record(UsageRecord {
                session_id: Some("s1".into()),
                model: "m".into(),
                provider: "openrouter".into(),
                prompt: 100,
                completion: 10,
                cost_usd: 0.5 * (i as f64 + 1.0),
                ..Default::default()
            })
            .await
            .unwrap();
        }
        assert!((l.spent_today().await.unwrap() - 3.0).abs() < 1e-9);
        assert!((l.spent_session("s1").await.unwrap() - 3.0).abs() < 1e-9);

        let b = l.breakdown("model", 10).await.unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].0, "m");
        assert_eq!(b[0].2, 330);
    }

    /// CA 10 : le budget journalier dépassé déclenche la demande HITL.
    #[tokio::test]
    async fn ca_10_4_daily_budget_exceeded_is_detected() {
        let store = Store::open_memory().unwrap();
        let l = BudgetLedger::new(store, Arc::new(TestClock::default()));
        l.record(UsageRecord {
            model: "m".into(),
            provider: "p".into(),
            cost_usd: 25.0,
            ..Default::default()
        })
        .await
        .unwrap();
        let cfg = crate::config::Budget::default();
        let st = l.status(&cfg, None, None).await.unwrap();
        assert!(st[0].exceeded, "{st:?}");
    }
}
