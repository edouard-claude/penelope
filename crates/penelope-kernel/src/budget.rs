//! Budgets par jour, session et run (§10.3 point 6).
//!
//! Deux seuils : `alert_ratio` (notification) et 100 % (demande HITL
//! `budget_exceeded`, choix « augmenter » ou « arrêter »).

use crate::clock::SharedClock;
use crate::error::Result;
use crate::event::{EventDraft, EventLog};
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

/// Fuseau du propriétaire, relu à chaque calcul : un changement à chaud de
/// `owner.timezone` s'applique aux consommations suivantes (issue #79).
type Timezone = std::sync::Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
pub struct BudgetLedger {
    store: Store,
    clock: SharedClock,
    watcher: Watcher,
    timezone: Option<Timezone>,
    events: Option<EventLog>,
}

impl BudgetLedger {
    /// Sans fuseau, les journées sont celles d'UTC (tests, outils hors daemon).
    pub fn new(store: Store, clock: SharedClock) -> Self {
        BudgetLedger {
            store,
            clock,
            watcher: Watcher::default(),
            timezone: None,
            events: None,
        }
    }

    /// Journée budgétaire dans le fuseau du propriétaire (#79) : le plafond du jour, son
    /// relèvement, `/usage` et les regroupements par jour suivent minuit **local**.
    pub fn with_timezone(mut self, tz: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.timezone = Some(std::sync::Arc::new(tz));
        self
    }

    /// Tous les appels LLM enregistrés par le ledger alimentent le même flux durable.
    pub fn with_events(mut self, events: EventLog) -> Self {
        self.events = Some(events);
        self
    }

    /// Jour du propriétaire à l'instant `ms` (`AAAA-MM-JJ`).
    pub fn day_at(&self, ms: i64) -> String {
        let utc = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
        match self
            .timezone
            .as_ref()
            .and_then(|f| f().parse::<chrono_tz::Tz>().ok())
        {
            Some(tz) => utc.with_timezone(&tz).format("%Y-%m-%d").to_string(),
            None => utc.format("%Y-%m-%d").to_string(),
        }
    }

    /// Jour budgétaire courant, clé des sommes « aujourd'hui ».
    pub fn today(&self) -> String {
        self.day_at(self.clock.now_ms())
    }

    /// Consommations des dernières 48 h dont le jour enregistré n'est pas celui que donne
    /// le fuseau actuel : changement de `owner.timezone`, ou lignes d'une version qui
    /// comptait en UTC. Le total du jour peut alors être décalé (`doctor`, #79).
    pub async fn mixed_days(&self) -> Result<u64> {
        let since = chrono::DateTime::from_timestamp_millis(self.clock.now_ms() - 48 * 3_600_000)
            .unwrap_or_default()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let rows: Vec<(String, String)> = self
            .store
            .read(move |c| {
                let mut st = c.prepare("SELECT ts, day FROM usage WHERE ts >= ?1")?;
                let rows = st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await?;
        Ok(rows
            .iter()
            .filter(|(ts, day)| {
                chrono::DateTime::parse_from_rfc3339(ts)
                    .map(|t| self.day_at(t.timestamp_millis()) != *day)
                    .unwrap_or(false)
            })
            .count() as u64)
    }

    /// Branche l'observateur des consommations (un seul).
    pub fn watch(&self, watcher: std::sync::Arc<dyn UsageWatcher>) {
        if let Ok(mut g) = self.watcher.write() {
            *g = Some(watcher);
        }
    }

    pub async fn record(&self, u: UsageRecord) -> Result<()> {
        let (session, run) = (u.session_id.clone(), u.run_id.clone());
        self.insert(u.clone()).await?;
        if let Some(events) = &self.events {
            let mut event = EventDraft::new(
                "runtime.llm",
                serde_json::json!({
                    "model": u.model,
                    "provider": u.provider,
                    "role": u.role,
                    "turn_id": u.turn_id,
                    "generation_id": u.generation_id,
                    "prompt_tokens": u.prompt,
                    "completion_tokens": u.completion,
                    "cached_tokens": u.cached,
                    "reasoning_tokens": u.reasoning,
                    "cost_usd": u.cost_usd,
                    "cost_estimated": u.estimated,
                }),
            );
            if let Some(session_id) = &session {
                event = event.session(session_id);
            }
            if let Some(run_id) = &run {
                event = event.run(run_id);
            }
            if let Err(error) = events.append(event).await {
                tracing::warn!(%error, "consommation LLM sans événement runtime");
            }
        }
        let watcher = self.watcher.read().ok().and_then(|g| g.clone());
        if let Some(w) = watcher {
            w.recorded(session.as_deref(), run.as_deref());
        }
        Ok(())
    }

    async fn insert(&self, u: UsageRecord) -> Result<()> {
        let ts = self.clock.now_rfc3339();
        let day = self.today();
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
        let day = self.today();
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

    async fn kv_limit(&self, key: String) -> Result<Option<f64>> {
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare("SELECT v FROM kv WHERE k = ?1")?;
                let mut rows = st.query([key])?;
                Ok(match rows.next()? {
                    Some(r) => r.get::<_, String>(0)?.parse::<f64>().ok(),
                    None => None,
                })
            })
            .await?)
    }

    async fn set_kv_limit(&self, key: String, usd: f64) -> Result<()> {
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO kv(k, v, ts) VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
                    params![key, usd.to_string()],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Plafonds effectifs : relèvement du jour, plafond propre à la session, relèvement du
    /// run, sinon la configuration (issue #32).
    pub async fn limits(
        &self,
        cfg: &crate::config::Budget,
        session_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(f64, f64, f64)> {
        let daily = self
            .kv_limit(format!("budget.daily.{}", self.today()))
            .await?
            .unwrap_or(cfg.daily_usd);
        let session = match session_id {
            Some(sid) => {
                let sid = sid.to_string();
                self.store
                    .read(move |c| {
                        Ok(c.query_row(
                            "SELECT budget_usd FROM sessions WHERE id = ?1",
                            [sid],
                            |r| r.get::<_, Option<f64>>(0),
                        )
                        .ok()
                        .flatten())
                    })
                    .await?
                    .filter(|u| *u > 0.0)
                    .unwrap_or(cfg.session_usd)
            }
            None => cfg.session_usd,
        };
        let run = match run_id {
            Some(r) => self
                .kv_limit(format!("budget.run.{r}"))
                .await?
                .unwrap_or(cfg.run_usd),
            None => cfg.run_usd,
        };
        Ok((daily, session, run))
    }

    /// Relève le plafond du jour, pour aujourd'hui seulement.
    pub async fn raise_daily(&self, usd: f64) -> Result<()> {
        self.set_kv_limit(format!("budget.daily.{}", self.today()), usd)
            .await
    }

    /// Relève le plafond d'un run.
    pub async fn raise_run(&self, run_id: &str, usd: f64) -> Result<()> {
        self.set_kv_limit(format!("budget.run.{run_id}"), usd).await
    }

    /// Statuts de tous les périmètres pertinents pour un tour.
    pub async fn status(
        &self,
        cfg: &crate::config::Budget,
        session_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<BudgetStatus>> {
        let (daily, session, run) = self.limits(cfg, session_id, run_id).await?;
        let mut out = vec![BudgetStatus::compute(
            BudgetScope::Daily,
            self.spent_today().await?,
            daily,
            cfg.alert_ratio,
        )];
        if let Some(s) = session_id {
            out.push(BudgetStatus::compute(
                BudgetScope::Session,
                self.spent_session(s).await?,
                session,
                cfg.alert_ratio,
            ));
        }
        if let Some(r) = run_id {
            out.push(BudgetStatus::compute(
                BudgetScope::Run,
                self.spent_run(r).await?,
                run,
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
                        "miss" => Some(miss_label(&row.key)),
                        _ => None,
                    };
                }
                Ok(rows)
            })
            .await?)
    }
}

/// « 16,10 $ »
pub fn usd(x: f64) -> String {
    format!("{x:.2} $").replace('.', ",")
}

/// Libellé d'une cause de raté de cache.
///
/// La cause « préfixe » porte, quand l'instantané du prompt le permet (issue #205), les
/// tuiles qui ont bougé : `prefixe:T1`, `prefixe:T1+T2`. Le libellé les nomme en clair.
pub fn miss_label(cause: &str) -> String {
    if let Some(tiles) = cause.strip_prefix("prefixe:") {
        let named: Vec<&str> = tiles.split('+').map(tile_label).collect();
        return format!("message système modifié : {}", named.join(", "));
    }
    match cause {
        "" => "cache servi, ou prompt trop court pour compter",
        "premier_appel" => "premier appel de la session",
        "pause" => "pause de plus de 5 min, cache expiré",
        "prefixe" => "message système modifié (T0 à T2), tuile inconnue",
        "outils" => "liste d'outils modifiée",
        "modele" => "autre modèle que l'appel précédent",
        "historique" => "historique réécrit avant le dernier message",
        "fournisseur" => "autre fournisseur amont que l'appel précédent",
        _ => "préfixe intact, cache non servi par le fournisseur",
    }
    .to_string()
}

/// Ce que nomme une tuile du préfixe, en clair (issue #205).
pub fn tile_label(name: &str) -> &'static str {
    match name {
        "T0" => "T0 identité et règles du harnais",
        "T1" => "T1 index des capacités (skills, workflows, serveurs MCP, machine)",
        "T2" => "T2 contexte du workspace et instantanés mémoire",
        _ => "tuile inconnue",
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
    async fn every_recorded_model_usage_has_a_runtime_event() {
        let store = Store::open_memory().unwrap();
        let clock = Arc::new(TestClock::default());
        let events = crate::event::EventLog::new(store.clone(), clock.clone());
        let ledger = BudgetLedger::new(store, clock).with_events(events.clone());
        ledger
            .record(UsageRecord {
                session_id: Some("s1".into()),
                model: "model-a".into(),
                provider: "openrouter".into(),
                prompt: 12,
                completion: 3,
                cost_usd: 0.02,
                estimated: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let logged = events.range(0, 10).await.unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].kind, "runtime.llm");
        assert_eq!(logged[0].payload["prompt_tokens"], 12);
        assert_eq!(logged[0].payload["cost_usd"], 0.02);
    }

    fn conso(cost: f64) -> UsageRecord {
        UsageRecord {
            model: "m".into(),
            provider: "openrouter".into(),
            cost_usd: cost,
            ..Default::default()
        }
    }

    /// #79 : à La Réunion (UTC+4), 00:30 le 2 janvier est encore le 1er en UTC. La
    /// consommation compte pour le 2, le relèvement du jour vaut jusqu'à 23:59 locale.
    #[tokio::test]
    async fn the_budget_day_is_the_owners_day() {
        let store = Store::open_memory().unwrap();
        // 2026-01-01T20:30:00Z = 2026-01-02T00:30+04:00
        let clock = Arc::new(TestClock::new(1_767_299_400_000));
        let tz = Arc::new(std::sync::RwLock::new("Indian/Reunion".to_string()));
        let l = BudgetLedger::new(store.clone(), clock.clone()).with_timezone({
            let tz = tz.clone();
            move || tz.read().unwrap().clone()
        });
        assert_eq!(l.today(), "2026-01-02");
        l.record(conso(1.5)).await.unwrap();
        assert!((l.spent_today().await.unwrap() - 1.5).abs() < 1e-9);
        let day: String = store
            .read(|c| Ok(c.query_row("SELECT day FROM usage", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(day, "2026-01-02");

        let cfg = crate::config::Budget::default();
        l.raise_daily(99.0).await.unwrap();
        let key: i64 = store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM kv WHERE k = 'budget.daily.2026-01-02'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(key, 1);
        // 23:59 locale : toujours relevé.
        clock.set_ms(1_767_383_940_000); // 2026-01-02T19:59:00Z
        assert_eq!(l.limits(&cfg, None, None).await.unwrap().0, 99.0);
        // Minuit local : un nouveau jour, plafond de la configuration, compteur à zéro.
        clock.set_ms(1_767_384_000_000); // 2026-01-02T20:00:00Z
        assert_eq!(l.today(), "2026-01-03");
        assert_eq!(l.limits(&cfg, None, None).await.unwrap().0, cfg.daily_usd);
        assert_eq!(l.spent_today().await.unwrap(), 0.0);
        l.record(conso(0.5)).await.unwrap();

        let days = l.report("day", None, None, 10).await.unwrap();
        let mut keys: Vec<String> = days.iter().map(|r| r.key.clone()).collect();
        keys.sort();
        assert_eq!(keys, vec!["2026-01-02", "2026-01-03"]);
        assert_eq!(l.mixed_days().await.unwrap(), 0);

        // Changement de fuseau à chaud : les lignes suivantes le suivent, et les lignes
        // récentes comptées dans l'ancien sont signalées.
        *tz.write().unwrap() = "UTC".into();
        assert_eq!(l.today(), "2026-01-02");
        assert_eq!(l.mixed_days().await.unwrap(), 2);
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
