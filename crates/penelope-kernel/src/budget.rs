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
    /// Tour d'origine : la requête du propriétaire à laquelle l'appel répond, reprises
    /// après approbation comprises.
    pub turn_id: Option<String>,
    pub model: String,
    pub provider: String,
    /// Usage de l'appel : `chat`, `classifier`, `compaction`…
    pub role: Option<String>,
    /// Identifiant de génération du provider (`gen-…` chez OpenRouter).
    pub generation_id: Option<String>,
    /// Provider amont qui a servi l'appel.
    pub upstream: Option<String>,
    pub finish: Option<String>,
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub cost_usd: f64,
    /// Coût calculé du catalogue faute de coût facturé annoncé.
    pub estimated: bool,
    pub maybe_duplicate: bool,
    /// Empreinte de la requête (issue #17) : nombre de messages, hachage chaîné des
    /// messages, du message système et des outils.
    pub msg_count: Option<i64>,
    pub request_hash: Option<String>,
    pub system_hash: Option<String>,
    pub tools_hash: Option<String>,
    /// Cause probable d'un raté de cache, `None` quand le cache a servi.
    pub miss_cause: Option<String>,
}

/// Une ligne de rapport de consommation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    pub key: String,
    /// Libellé lisible : titre ou premier message d'une session, texte d'une requête.
    pub label: Option<String>,
    pub cost_usd: f64,
    pub tokens: i64,
    pub calls: i64,
    /// Appels dont le coût est estimé, non facturé tel quel.
    pub estimated: i64,
    pub last_ts: String,
    /// Tokens d'entrée, dont lus en cache, et de sortie.
    #[serde(default)]
    pub prompt: i64,
    #[serde(default)]
    pub cached: i64,
    #[serde(default)]
    pub completion: i64,
}

impl UsageRow {
    /// Part des tokens d'entrée servis par le cache du fournisseur.
    pub fn cache_ratio(&self) -> f64 {
        if self.prompt > 0 {
            self.cached as f64 / self.prompt as f64
        } else {
            0.0
        }
    }
}

/// Prévenu après chaque consommation enregistrée : alerte de seuil (issue #20).
pub trait UsageWatcher: Send + Sync {
    fn recorded(&self, session_id: Option<&str>, run_id: Option<&str>);
}

/// Axes de regroupement acceptés par [`BudgetLedger::report`].
pub const USAGE_AXES: &[&str] = &[
    "session", "turn", "model", "day", "role", "provider", "upstream", "run", "miss",
];

type Watcher = std::sync::Arc<std::sync::RwLock<Option<std::sync::Arc<dyn UsageWatcher>>>>;

#[derive(Clone)]
pub struct BudgetLedger {
    store: Store,
    clock: SharedClock,
    watcher: Watcher,
}

impl BudgetLedger {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        BudgetLedger {
            store,
            clock,
            watcher: Watcher::default(),
        }
    }

    /// Branche l'observateur des consommations (un seul).
    pub fn watch(&self, watcher: std::sync::Arc<dyn UsageWatcher>) {
        if let Ok(mut g) = self.watcher.write() {
            *g = Some(watcher);
        }
    }

    pub async fn record(&self, u: UsageRecord) -> Result<()> {
        let (session, run) = (u.session_id.clone(), u.run_id.clone());
        self.insert(u).await?;
        let watcher = self.watcher.read().ok().and_then(|g| g.clone());
        if let Some(w) = watcher {
            w.recorded(session.as_deref(), run.as_deref());
        }
        Ok(())
    }

    async fn insert(&self, u: UsageRecord) -> Result<()> {
        let ts = self.clock.now_rfc3339();
        let day = ts.chars().take(10).collect::<String>();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO usage(ts, day, session_id, run_id, model, provider, role,
                        prompt, completion, cached, reasoning, cost_usd, estimated, maybe_dup,
                        turn_id, generation_id, upstream, finish, cache_write, msg_count,
                        request_hash, system_hash, tools_hash, miss_cause)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,
                            ?20,?21,?22,?23,?24)",
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
                        u.maybe_duplicate as i64,
                        u.turn_id,
                        u.generation_id,
                        u.upstream,
                        u.finish,
                        u.cache_write as i64,
                        u.msg_count,
                        u.request_hash,
                        u.system_hash,
                        u.tools_hash,
                        u.miss_cause
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

    /// Appels de conversation et coût total d'un tour, reprises après approbation
    /// comprises.
    pub async fn turn_totals(&self, turn_id: &str) -> Result<(i64, f64)> {
        let tid = turn_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(CASE WHEN COALESCE(role, 'chat') = 'chat' THEN 1 END), 0),
                            COALESCE(SUM(cost_usd), 0)
                     FROM usage WHERE turn_id = ?1",
                    [tid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
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
        Ok(self
            .report(by, None, None, limit)
            .await?
            .into_iter()
            .map(|r| (r.key, r.cost_usd, r.tokens))
            .collect())
    }

    /// Rapport de consommation, du plus cher au moins cher.
    ///
    /// `by` : un des [`USAGE_AXES`]. `session` restreint à une session, `since` (date
    /// `AAAA-MM-JJ`) aux jours suivants. Sessions et requêtes reçoivent un libellé.
    pub async fn report(
        &self,
        by: &str,
        session: Option<&str>,
        since: Option<&str>,
        limit: i64,
    ) -> Result<Vec<UsageRow>> {
        self.report_run(by, session, None, since, limit).await
    }

    /// [`Self::report`], restreint en plus à un run.
    pub async fn report_run(
        &self,
        by: &str,
        session: Option<&str>,
        run: Option<&str>,
        since: Option<&str>,
        limit: i64,
    ) -> Result<Vec<UsageRow>> {
        // Colonnes fixes : jamais d'entrée utilisateur dans le SQL.
        let column = match by {
            "session" => "COALESCE(session_id, '')",
            "turn" => "COALESCE(turn_id, '')",
            "day" => "day",
            "role" => "COALESCE(role, '')",
            "provider" => "provider",
            "upstream" => "COALESCE(upstream, '')",
            "run" => "COALESCE(run_id, '')",
            "miss" => "COALESCE(miss_cause, '')",
            _ => "model",
        };
        let by = by.to_string();
        let sql = format!(
            "SELECT {column} AS k, SUM(cost_usd), SUM(prompt + completion), COUNT(*),
                    SUM(estimated), MAX(ts), SUM(prompt), SUM(cached), SUM(completion)
             FROM usage
             WHERE (?1 IS NULL OR session_id = ?1) AND (?2 IS NULL OR day >= ?2)
               AND (?4 IS NULL OR run_id = ?4)
             GROUP BY k ORDER BY SUM(cost_usd) DESC, MAX(ts) DESC LIMIT ?3"
        );
        let session = session.map(String::from);
        let since = since.map(String::from);
        let run = run.map(String::from);
        Ok(self
            .store
            .read(move |c| {
                let mut rows = Vec::new();
                {
                    let mut st = c.prepare(&sql)?;
                    let it = st.query_map(params![session, since, limit, run], |r| {
                        Ok(UsageRow {
                            key: r.get::<_, String>(0)?,
                            label: None,
                            cost_usd: r.get::<_, f64>(1)?,
                            tokens: r.get::<_, i64>(2)?,
                            calls: r.get::<_, i64>(3)?,
                            estimated: r.get::<_, i64>(4)?,
                            last_ts: r.get::<_, String>(5)?,
                            prompt: r.get::<_, i64>(6)?,
                            cached: r.get::<_, i64>(7)?,
                            completion: r.get::<_, i64>(8)?,
                        })
                    })?;
                    for row in it {
                        rows.push(row?);
                    }
                }
                for row in &mut rows {
                    row.label = match by.as_str() {
                        "session" => session_label(c, &row.key),
                        "turn" => turn_label(c, &row.key),
                        "miss" => Some(miss_label(&row.key).to_string()),
                        _ => None,
                    };
                }
                Ok(rows)
            })
            .await?)
    }
}

/// Libellé d'une cause de raté de cache.
pub fn miss_label(cause: &str) -> &'static str {
    match cause {
        "" => "cache servi, ou prompt trop court pour compter",
        "premier_appel" => "premier appel de la session",
        "pause" => "pause de plus de 5 min, cache expiré",
        "prefixe" => "message système modifié (T0 à T2)",
        "outils" => "liste d'outils modifiée",
        "modele" => "autre modèle que l'appel précédent",
        "historique" => "historique réécrit avant le dernier message",
        "fournisseur" => "autre fournisseur amont que l'appel précédent",
        _ => "préfixe intact, cache non servi par le fournisseur",
    }
}

/// Titre d'une session, sinon son premier message.
fn session_label(c: &penelope_store::rusqlite::Connection, id: &str) -> Option<String> {
    use penelope_store::rusqlite::OptionalExtension;
    if id.is_empty() {
        return Some("(hors session)".into());
    }
    let title: Option<String> = c
        .query_row("SELECT title FROM sessions WHERE id = ?1", [id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()
        .ok()
        .flatten()
        .flatten()
        .filter(|t| !t.trim().is_empty());
    if title.is_some() {
        return title.map(|t| short_label(&t));
    }
    c.query_row(
        "SELECT json_extract(content, '$.blocks[0].text') FROM messages
         WHERE session_id = ?1 AND role = 'user' ORDER BY seq LIMIT 1",
        [id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
    .map(|t| short_label(&t))
}

/// Texte de la requête qui a ouvert un tour.
fn turn_label(c: &penelope_store::rusqlite::Connection, id: &str) -> Option<String> {
    use penelope_store::rusqlite::OptionalExtension;
    if id.is_empty() {
        return Some("(hors tour)".into());
    }
    c.query_row(
        "SELECT kind, json_extract(payload, '$.text') FROM turn_queue WHERE id = ?1",
        [id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
    .map(|(kind, text)| match text.filter(|t| !t.trim().is_empty()) {
        Some(t) => short_label(&t),
        None => format!("({kind})"),
    })
}

/// Une ligne, 60 caractères au plus.
fn short_label(t: &str) -> String {
    let one_line = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 60 {
        format!("{}…", one_line.chars().take(59).collect::<String>())
    } else {
        one_line
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

    #[tokio::test]
    async fn costs_are_attributed_to_sessions_and_requests() {
        let store = Store::open_memory().unwrap();
        let clock = Arc::new(TestClock::default());
        let l = BudgetLedger::new(store.clone(), clock.clone());
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id, kind, title, created_at, updated_at)
                     VALUES('s1', 'chat', NULL, 'x', 'x'), ('s2', 'chat', 'Refonte', 'x', 'x')",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO messages(session_id, seq, role, content, ts)
                     VALUES('s1', 1, 'user', '{\"blocks\":[{\"type\":\"text\",\"text\":\"Liste mes repo  qui contiennent mcp\"}]}', 'x')",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO turn_queue(id, session_id, kind, payload, state, enqueued_at)
                     VALUES('t1', 's1', 'message', '{\"text\":\"Liste mes repo qui contiennent mcp\"}', 'done', 'x'),
                           ('t2', 's1', 'resume', '{\"approval_id\":\"a1\"}', 'done', 'x')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let call =
            |session: &str, turn: &str, role: &str, cost: f64, estimated: bool| UsageRecord {
                session_id: Some(session.into()),
                turn_id: Some(turn.into()),
                role: Some(role.into()),
                model: "z-ai/glm-5.3".into(),
                provider: "openrouter".into(),
                generation_id: Some("gen-1".into()),
                upstream: Some("Z.AI".into()),
                prompt: 1000,
                completion: 100,
                cost_usd: cost,
                estimated,
                ..Default::default()
            };
        l.record(call("s1", "t1", "classifier", 0.0001, false))
            .await
            .unwrap();
        l.record(call("s1", "t1", "chat", 0.03, false))
            .await
            .unwrap();
        l.record(call("s1", "t1", "chat", 0.02, true))
            .await
            .unwrap();
        l.record(call("s2", "t3", "chat", 0.01, false))
            .await
            .unwrap();

        let by_session = l.report("session", None, None, 10).await.unwrap();
        assert_eq!(by_session[0].key, "s1");
        assert_eq!(by_session[0].calls, 3);
        assert_eq!(by_session[0].estimated, 1);
        assert_eq!(
            by_session[0].label.as_deref(),
            Some("Liste mes repo qui contiennent mcp")
        );
        assert_eq!(by_session[1].label.as_deref(), Some("Refonte"));

        let by_turn = l.report("turn", Some("s1"), None, 10).await.unwrap();
        assert_eq!(
            by_turn.len(),
            1,
            "la reprise est rattachée au tour d'origine"
        );
        assert!((by_turn[0].cost_usd - 0.0501).abs() < 1e-9);
        assert_eq!(
            by_turn[0].label.as_deref(),
            Some("Liste mes repo qui contiennent mcp")
        );

        let by_role = l
            .report("role", None, Some("1970-01-01"), 10)
            .await
            .unwrap();
        assert_eq!(by_role[0].key, "chat");
        assert!(
            l.report("day", None, Some("2999-01-01"), 10)
                .await
                .unwrap()
                .is_empty()
        );
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
