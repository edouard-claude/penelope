//! Déclencheurs et jobs planifiés (§12.9).
//!
//! Types : `cron`, `interval`, `mcp_poll`, `watch_file`, `event`.
//! Cibles : `prompt`, `workflow`, `notify`.
//!
//! Déduplication `mcp_poll` : un élément nouveau **ou modifié** (empreinte) déclenche la
//! cible, une seule fois par élément. À la création, les éléments existants sont marqués
//! vus sans déclenchement, sauf option `backfill`.

use penelope_kernel::clock::SharedClock;
use penelope_kernel::cron::Cron;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Cron,
    Interval,
    McpPoll,
    WatchFile,
    Event,
}

impl TriggerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TriggerKind::Cron => "cron",
            TriggerKind::Interval => "interval",
            TriggerKind::McpPoll => "mcp_poll",
            TriggerKind::WatchFile => "watch_file",
            TriggerKind::Event => "event",
        }
    }
    pub fn parse(s: &str) -> Option<TriggerKind> {
        Some(match s {
            "cron" => TriggerKind::Cron,
            "interval" => TriggerKind::Interval,
            "mcp_poll" => TriggerKind::McpPoll,
            "watch_file" => TriggerKind::WatchFile,
            "event" => TriggerKind::Event,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Prompt,
    Workflow,
    Notify,
}

impl TargetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetKind::Prompt => "prompt",
            TargetKind::Workflow => "workflow",
            TargetKind::Notify => "notify",
        }
    }
    pub fn parse(s: &str) -> Option<TargetKind> {
        Some(match s {
            "prompt" => TargetKind::Prompt,
            "workflow" => TargetKind::Workflow,
            "notify" => TargetKind::Notify,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub kind: TriggerKind,
    pub spec: Value,
    pub target: Value,
    pub dedup: Value,
    pub state: String,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    pub runs: u64,
    pub last_error: Option<String>,
}

impl Schedule {
    pub fn target_kind(&self) -> Option<TargetKind> {
        self.target
            .get("type")
            .and_then(|t| t.as_str())
            .and_then(TargetKind::parse)
    }

    /// Validation d'une spécification avant activation (§12.9, aperçu puis HITL).
    pub fn validate(&self, tz: &str) -> Result<(), String> {
        match self.kind {
            TriggerKind::Cron => {
                let expr = self
                    .spec
                    .get("expr")
                    .and_then(|e| e.as_str())
                    .ok_or("`expr` est obligatoire pour un cron")?;
                Cron::parse(expr).map_err(|e| e.to_string())?;
                let zone = self.spec.get("tz").and_then(|t| t.as_str()).unwrap_or(tz);
                zone.parse::<chrono_tz::Tz>()
                    .map_err(|_| format!("fuseau inconnu : {zone}"))?;
            }
            TriggerKind::Interval => {
                let ms = self
                    .spec
                    .get("every_ms")
                    .and_then(|v| v.as_u64())
                    .ok_or("`every_ms` est obligatoire pour un interval")?;
                if ms < 1000 {
                    return Err("intervalle minimal : 1000 ms".into());
                }
            }
            TriggerKind::McpPoll => {
                for k in ["server", "tool", "item_path", "id_path"] {
                    if self.spec.get(k).and_then(|v| v.as_str()).is_none() {
                        return Err(format!("`{k}` est obligatoire pour un mcp_poll"));
                    }
                }
                let ms = self
                    .spec
                    .get("every_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if ms < 60_000 {
                    return Err("intervalle minimal d'un mcp_poll : 60 000 ms".into());
                }
            }
            TriggerKind::WatchFile => {
                if self.spec.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err("`path` est obligatoire pour un watch_file".into());
                }
            }
            TriggerKind::Event => {
                if self.spec.get("event").and_then(|v| v.as_str()).is_none() {
                    return Err("`event` est obligatoire".into());
                }
            }
        }

        match self.target_kind() {
            Some(TargetKind::Workflow) => {
                if self
                    .target
                    .get("workflowId")
                    .and_then(|v| v.as_str())
                    .is_none()
                {
                    return Err("`workflowId` est obligatoire pour une cible workflow".into());
                }
            }
            Some(TargetKind::Prompt) => {
                if self.target.get("prompt").and_then(|v| v.as_str()).is_none() {
                    return Err("`prompt` est obligatoire".into());
                }
            }
            Some(TargetKind::Notify) => {
                if self
                    .target
                    .get("template")
                    .and_then(|v| v.as_str())
                    .is_none()
                {
                    return Err("`template` est obligatoire pour une notification".into());
                }
            }
            None => return Err("`target.type` inconnu".into()),
        }
        Ok(())
    }

    /// Prochain déclenchement, en millisecondes epoch.
    pub fn next_after(&self, now_ms: i64, tz: &str) -> Option<i64> {
        match self.kind {
            TriggerKind::Cron => {
                let expr = self.spec.get("expr")?.as_str()?;
                let zone = self.spec.get("tz").and_then(|t| t.as_str()).unwrap_or(tz);
                Cron::parse(expr).ok()?.next_after_ms(now_ms, zone)
            }
            TriggerKind::Interval | TriggerKind::McpPoll => {
                let ms = self.spec.get("every_ms")?.as_u64()? as i64;
                Some(now_ms + ms)
            }
            // Ces déclencheurs sont poussés par un événement externe.
            TriggerKind::WatchFile | TriggerKind::Event => None,
        }
    }
}

/// Élément extrait par un `mcp_poll`.
#[derive(Debug, Clone, PartialEq)]
pub struct PolledItem {
    pub id: String,
    pub fingerprint: String,
    pub value: Value,
}

/// Extrait les éléments d'une réponse d'outil MCP.
pub fn extract_items(result: &Value, item_path: &str, id_path: &str) -> Vec<PolledItem> {
    let container = crate::conditions::json_path(result, item_path).unwrap_or(Value::Null);
    let array = match container {
        Value::Array(a) => a,
        Value::Null => return Vec::new(),
        other => vec![other],
    };
    array
        .into_iter()
        .filter_map(|v| {
            let id = crate::conditions::json_path(&v, id_path).map(|x| match x {
                Value::String(s) => s,
                other => other.to_string(),
            })?;
            let fingerprint = penelope_kernel::canonical::canonical_hash(&v);
            Some(PolledItem {
                id,
                fingerprint,
                value: v,
            })
        })
        .collect()
}

/// Filtre optionnel sur les éléments (§12.9).
pub fn passes_filter(item: &Value, filter: Option<&Value>) -> bool {
    let Some(f) = filter else { return true };
    let Some(obj) = f.as_object() else {
        return true;
    };
    obj.iter().all(|(path, expected)| {
        crate::conditions::json_path(item, path)
            .map(|actual| &actual == expected)
            .unwrap_or(false)
    })
}

#[derive(Clone)]
pub struct ScheduleStore {
    store: Store,
    clock: SharedClock,
    timezone: String,
}

impl ScheduleStore {
    pub fn new(store: Store, clock: SharedClock, timezone: &str) -> Self {
        ScheduleStore {
            store,
            clock,
            timezone: timezone.to_string(),
        }
    }

    pub async fn create(
        &self,
        kind: TriggerKind,
        spec: Value,
        target: Value,
        dedup: Value,
    ) -> Result<Schedule, String> {
        let s = Schedule {
            id: format!("sch_{}", penelope_kernel::ids::Ulid::new()),
            kind,
            spec,
            target,
            dedup,
            state: "active".into(),
            last_run: None,
            next_run: None,
            runs: 0,
            last_error: None,
        };
        s.validate(&self.timezone)?;
        let next = s
            .next_after(self.clock.now_ms(), &self.timezone)
            .map(ms_to_rfc3339);
        let row = Schedule {
            next_run: next.clone(),
            ..s.clone()
        };
        let now = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO schedules(id, kind, spec, target, dedup, state, next_run,
                        created_at, updated_at)
                     VALUES(?1,?2,?3,?4,?5,'active',?6,?7,?7)",
                    params![
                        row.id,
                        row.kind.as_str(),
                        row.spec.to_string(),
                        row.target.to_string(),
                        row.dedup.to_string(),
                        row.next_run,
                        now
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(Schedule {
            next_run: next,
            ..s
        })
    }

    pub async fn due(&self) -> penelope_store::Result<Vec<Schedule>> {
        let now = self.clock.now_rfc3339();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE state = 'active' AND next_run IS NOT NULL AND next_run <= ?1
                     ORDER BY next_run"
                ))?;
                let rows = st.query_map([now], row_to_schedule)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    pub async fn list(&self) -> penelope_store::Result<Vec<Schedule>> {
        self.store
            .read(|c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE state != 'deleted' ORDER BY created_at"
                ))?;
                let rows = st.query_map([], row_to_schedule)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    pub async fn get(&self, id: &str) -> penelope_store::Result<Option<Schedule>> {
        let id = id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!("{SELECT} WHERE id = ?1"))?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_schedule(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    /// Marque l'exécution et programme la suivante.
    pub async fn mark_run(&self, id: &str, error: Option<&str>) -> penelope_store::Result<()> {
        let sched = self.get(id).await?;
        let next = sched
            .as_ref()
            .and_then(|s| s.next_after(self.clock.now_ms(), &self.timezone))
            .map(ms_to_rfc3339);
        let (id, now, err) = (
            id.to_string(),
            self.clock.now_rfc3339(),
            error.map(String::from),
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE schedules SET last_run = ?2, next_run = ?3, runs = runs + 1,
                        last_error = ?4, updated_at = ?2 WHERE id = ?1",
                    params![id, now, next, err],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn set_state(&self, id: &str, state: &str) -> penelope_store::Result<()> {
        let (id, state, now) = (id.to_string(), state.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE schedules SET state = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, state, now],
                )?;
                Ok(())
            })
            .await
    }

    /// Amorçage : marque les éléments existants comme vus **sans déclencher**, sauf
    /// `backfill` (§12.9).
    pub async fn seed(
        &self,
        schedule_id: &str,
        items: &[PolledItem],
        backfill: bool,
    ) -> penelope_store::Result<usize> {
        if backfill {
            return Ok(0);
        }
        let (id, now) = (schedule_id.to_string(), self.clock.now_rfc3339());
        let rows: Vec<(String, String)> = items
            .iter()
            .map(|i| (i.id.clone(), i.fingerprint.clone()))
            .collect();
        self.store
            .write(move |tx| {
                let mut n = 0;
                for (item_id, fp) in &rows {
                    n += tx.execute(
                        "INSERT OR IGNORE INTO seen_items(schedule_id, item_id, first_seen,
                            last_seen, fingerprint, fired)
                         VALUES(?1,?2,?3,?3,?4,0)",
                        params![id, item_id, now, fp],
                    )?;
                }
                Ok(n)
            })
            .await
    }

    /// Filtre les éléments à déclencher : nouveaux, ou modifiés si `retrigger_on_change`.
    pub async fn new_or_changed(
        &self,
        schedule_id: &str,
        items: &[PolledItem],
        retrigger_on_change: bool,
    ) -> penelope_store::Result<Vec<PolledItem>> {
        let id = schedule_id.to_string();
        let now = self.clock.now_rfc3339();
        let incoming: Vec<(String, String)> = items
            .iter()
            .map(|i| (i.id.clone(), i.fingerprint.clone()))
            .collect();

        let to_fire: Vec<String> = self
            .store
            .write(move |tx| {
                let mut out = Vec::new();
                for (item_id, fp) in &incoming {
                    let existing: Option<(String, i64)> = tx
                        .query_row(
                            "SELECT fingerprint, fired FROM seen_items
                             WHERE schedule_id = ?1 AND item_id = ?2",
                            params![id, item_id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .ok();
                    let fire = match &existing {
                        None => true,
                        Some((old_fp, _)) => retrigger_on_change && old_fp != fp,
                    };
                    tx.execute(
                        "INSERT INTO seen_items(schedule_id, item_id, first_seen, last_seen,
                            fingerprint, fired)
                         VALUES(?1,?2,?3,?3,?4,?5)
                         ON CONFLICT(schedule_id, item_id) DO UPDATE SET
                            last_seen = ?3, fingerprint = ?4,
                            fired = CASE WHEN ?5 = 1 THEN 1 ELSE fired END",
                        params![id, item_id, now, fp, fire as i64],
                    )?;
                    if fire {
                        out.push(item_id.clone());
                    }
                }
                Ok(out)
            })
            .await?;

        Ok(items
            .iter()
            .filter(|i| to_fire.contains(&i.id))
            .cloned()
            .collect())
    }

    /// Coalescence : plusieurs éléments détectés dans le même tick donnent une seule
    /// notification groupée si la cible est `notify` (§12.9).
    pub fn coalesce(&self, schedule: &Schedule, items: &[PolledItem]) -> Vec<Vec<PolledItem>> {
        if items.is_empty() {
            return Vec::new();
        }
        match schedule.target_kind() {
            Some(TargetKind::Notify) => vec![items.to_vec()],
            _ => items.iter().map(|i| vec![i.clone()]).collect(),
        }
    }
}

const SELECT: &str = "SELECT id, kind, spec, target, dedup, state, last_run, next_run, runs,
     last_error FROM schedules";

fn row_to_schedule(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<Schedule> {
    let kind: String = r.get(1)?;
    let spec: String = r.get(2)?;
    let target: String = r.get(3)?;
    let dedup: String = r.get(4)?;
    Ok(Schedule {
        id: r.get(0)?,
        kind: TriggerKind::parse(&kind).unwrap_or(TriggerKind::Event),
        spec: serde_json::from_str(&spec).unwrap_or(json!({})),
        target: serde_json::from_str(&target).unwrap_or(json!({})),
        dedup: serde_json::from_str(&dedup).unwrap_or(json!({})),
        state: r.get(5)?,
        last_run: r.get(6)?,
        next_run: r.get(7)?,
        runs: r.get::<_, i64>(8)? as u64,
        last_error: r.get(9)?,
    })
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    fn schedules(clock: TestClock) -> ScheduleStore {
        ScheduleStore::new(
            Store::open_memory().unwrap(),
            Arc::new(clock),
            "Indian/Reunion",
        )
    }

    fn poll_spec() -> Value {
        json!({
            "server":"redmine",
            "tool":"mcp__redmine__search_issues",
            "args":{"assigned_to":"me"},
            "every_ms":900000,
            "item_path":"issues",
            "id_path":"id"
        })
    }

    fn workflow_target() -> Value {
        json!({"type":"workflow","workflowId":"ticket-to-deploy","params":{}})
    }

    #[tokio::test]
    async fn cron_schedules_compute_their_next_run() {
        let clock = TestClock::new(
            chrono::DateTime::parse_from_rfc3339("2026-09-16T10:00:00Z")
                .unwrap()
                .timestamp_millis(),
        );
        let s = schedules(clock);
        let sched = s
            .create(
                TriggerKind::Cron,
                json!({"expr":"30 3 * * *"}),
                json!({"type":"prompt","prompt":"consolide"}),
                json!({}),
            )
            .await
            .unwrap();
        // 03:30 à La Réunion = 23:30 UTC la veille.
        assert_eq!(sched.next_run.as_deref(), Some("2026-09-16T23:30:00.000Z"));
    }

    #[tokio::test]
    async fn due_returns_only_ripe_schedules() {
        let clock = TestClock::default();
        let s = schedules(clock.clone());
        s.create(
            TriggerKind::Interval,
            json!({"every_ms": 3600000}),
            json!({"type":"notify","template":"heartbeat"}),
            json!({}),
        )
        .await
        .unwrap();
        assert!(s.due().await.unwrap().is_empty());
        clock.advance_hours(2);
        assert_eq!(s.due().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn validation_rejects_bad_specs() {
        let s = schedules(TestClock::default());
        assert!(
            s.create(
                TriggerKind::Cron,
                json!({"expr":"pas du cron"}),
                json!({"type":"prompt","prompt":"x"}),
                json!({})
            )
            .await
            .is_err()
        );
        assert!(
            s.create(
                TriggerKind::Interval,
                json!({"every_ms": 10}),
                json!({"type":"prompt","prompt":"x"}),
                json!({})
            )
            .await
            .unwrap_err()
            .contains("1000 ms")
        );
        assert!(
            s.create(
                TriggerKind::McpPoll,
                json!({"server":"x"}),
                workflow_target(),
                json!({})
            )
            .await
            .is_err()
        );
        assert!(
            s.create(
                TriggerKind::Cron,
                json!({"expr":"0 8 * * *"}),
                json!({"type":"workflow"}),
                json!({})
            )
            .await
            .unwrap_err()
            .contains("workflowId")
        );
    }

    #[test]
    fn items_are_extracted_with_stable_fingerprints() {
        let result = json!({"issues":[
            {"id": 4312, "subject":"TVA", "updated_on":"2026-09-16"},
            {"id": 4313, "subject":"Facture"}
        ]});
        let items = extract_items(&result, "issues", "id");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "4312");

        // La même donnée donne la même empreinte, l'ordre des clés ne compte pas.
        let reordered = json!({"issues":[
            {"subject":"TVA","id": 4312,"updated_on":"2026-09-16"}
        ]});
        assert_eq!(
            extract_items(&reordered, "issues", "id")[0].fingerprint,
            items[0].fingerprint
        );

        // Un changement de contenu change l'empreinte.
        let changed = json!({"issues":[{"id":4312,"subject":"TVA corrigée"}]});
        assert_ne!(
            extract_items(&changed, "issues", "id")[0].fingerprint,
            items[0].fingerprint
        );
    }

    #[test]
    fn nested_item_paths_and_filters() {
        let result = json!({"data":{"items":[{"ticket":{"id":"T-1"},"priority":"haute"}]}});
        let items = extract_items(&result, "data.items", "ticket.id");
        assert_eq!(items[0].id, "T-1");
        assert!(passes_filter(
            &items[0].value,
            Some(&json!({"priority":"haute"}))
        ));
        assert!(!passes_filter(
            &items[0].value,
            Some(&json!({"priority":"basse"}))
        ));
        assert!(passes_filter(&items[0].value, None));
    }

    /// CA 12 : le schedule `mcp_poll` ne déclenche qu'une fois par ticket, et à nouveau
    /// si le ticket est modifié lorsque `retrigger_on_change = true`.
    #[tokio::test]
    async fn ca_12_4_poll_fires_once_per_item() {
        let s = schedules(TestClock::default());
        let sched = s
            .create(
                TriggerKind::McpPoll,
                poll_spec(),
                workflow_target(),
                json!({"retrigger_on_change": true}),
            )
            .await
            .unwrap();

        let first = extract_items(
            &json!({"issues":[{"id":4312,"subject":"TVA"},{"id":4313,"subject":"Facture"}]}),
            "issues",
            "id",
        );
        let fired = s.new_or_changed(&sched.id, &first, true).await.unwrap();
        assert_eq!(fired.len(), 2, "premier passage : les deux tickets");

        // Deuxième passage, rien n'a changé.
        let fired = s.new_or_changed(&sched.id, &first, true).await.unwrap();
        assert!(fired.is_empty(), "aucun redéclenchement sans changement");

        // Le ticket 4312 est modifié.
        let changed = extract_items(
            &json!({"issues":[{"id":4312,"subject":"TVA corrigée"},{"id":4313,"subject":"Facture"}]}),
            "issues",
            "id",
        );
        let fired = s.new_or_changed(&sched.id, &changed, true).await.unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].id, "4312");

        // Sans `retrigger_on_change`, une modification ne redéclenche pas.
        let changed2 = extract_items(
            &json!({"issues":[{"id":4312,"subject":"encore autre chose"}]}),
            "issues",
            "id",
        );
        assert!(
            s.new_or_changed(&sched.id, &changed2, false)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn seeding_marks_existing_items_without_firing() {
        let s = schedules(TestClock::default());
        let sched = s
            .create(
                TriggerKind::McpPoll,
                poll_spec(),
                workflow_target(),
                json!({}),
            )
            .await
            .unwrap();
        let existing = extract_items(
            &json!({"issues":[{"id":1},{"id":2},{"id":3}]}),
            "issues",
            "id",
        );
        assert_eq!(s.seed(&sched.id, &existing, false).await.unwrap(), 3);
        assert!(
            s.new_or_changed(&sched.id, &existing, true)
                .await
                .unwrap()
                .is_empty(),
            "les éléments amorcés ne déclenchent pas"
        );

        // Avec `backfill`, rien n'est amorcé : tout déclenche.
        let sched2 = s
            .create(
                TriggerKind::McpPoll,
                poll_spec(),
                workflow_target(),
                json!({}),
            )
            .await
            .unwrap();
        assert_eq!(s.seed(&sched2.id, &existing, true).await.unwrap(), 0);
        assert_eq!(
            s.new_or_changed(&sched2.id, &existing, true)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn coalescing_groups_notifications_only() {
        let s = schedules(TestClock::default());
        let notify = s
            .create(
                TriggerKind::McpPoll,
                poll_spec(),
                json!({"type":"notify","template":"ticket_detected"}),
                json!({}),
            )
            .await
            .unwrap();
        let wf = s
            .create(
                TriggerKind::McpPoll,
                poll_spec(),
                workflow_target(),
                json!({}),
            )
            .await
            .unwrap();
        let items = extract_items(&json!({"issues":[{"id":1},{"id":2}]}), "issues", "id");

        assert_eq!(
            s.coalesce(&notify, &items).len(),
            1,
            "une seule notification"
        );
        assert_eq!(s.coalesce(&wf, &items).len(), 2, "un run par élément");
        assert!(s.coalesce(&notify, &[]).is_empty());
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let clock = TestClock::default();
        let s = schedules(clock.clone());
        let sched = s
            .create(
                TriggerKind::Interval,
                json!({"every_ms": 60000}),
                json!({"type":"notify","template":"heartbeat"}),
                json!({}),
            )
            .await
            .unwrap();
        clock.advance_ms(120_000);
        assert_eq!(s.due().await.unwrap().len(), 1);

        s.set_state(&sched.id, "paused").await.unwrap();
        assert!(s.due().await.unwrap().is_empty());
        s.set_state(&sched.id, "active").await.unwrap();
        assert_eq!(s.due().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn mark_run_schedules_the_next_occurrence() {
        let clock = TestClock::default();
        let s = schedules(clock.clone());
        let sched = s
            .create(
                TriggerKind::Interval,
                json!({"every_ms": 60000}),
                json!({"type":"notify","template":"heartbeat"}),
                json!({}),
            )
            .await
            .unwrap();
        clock.advance_ms(120_000);
        s.mark_run(&sched.id, None).await.unwrap();
        let after = s.get(&sched.id).await.unwrap().unwrap();
        assert_eq!(after.runs, 1);
        assert!(after.last_run.is_some());
        assert!(s.due().await.unwrap().is_empty(), "reprogrammé plus tard");
    }

    #[test]
    fn event_and_watch_triggers_have_no_schedule() {
        let s = Schedule {
            id: "x".into(),
            kind: TriggerKind::Event,
            spec: json!({"event":"run.done"}),
            target: json!({"type":"notify","template":"run_done"}),
            dedup: json!({}),
            state: "active".into(),
            last_run: None,
            next_run: None,
            runs: 0,
            last_error: None,
        };
        assert!(s.next_after(0, "UTC").is_none());
        s.validate("UTC").unwrap();
    }
}
