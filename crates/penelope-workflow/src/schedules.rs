//! Déclencheurs et jobs planifiés (§12.9).
//!
//! Types : `cron`, `interval`, `mcp_poll`, `watch_file`, `event`, `webhook`.
//! Cibles : `prompt`, `workflow`, `notify`.
//!
//! `webhook` (#294) : un service extérieur pousse un corps JSON sur `POST /hook/<jeton>`,
//! signé en HMAC-SHA256 par un secret rangé dans le magasin de secrets ; la spécification
//! ne porte que le chemin et le **nom** du secret (`secret_ref`), jamais sa valeur.
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
    Webhook,
}

impl TriggerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TriggerKind::Cron => "cron",
            TriggerKind::Interval => "interval",
            TriggerKind::McpPoll => "mcp_poll",
            TriggerKind::WatchFile => "watch_file",
            TriggerKind::Event => "event",
            TriggerKind::Webhook => "webhook",
        }
    }
    pub fn parse(s: &str) -> Option<TriggerKind> {
        Some(match s {
            "cron" => TriggerKind::Cron,
            "interval" => TriggerKind::Interval,
            "mcp_poll" => TriggerKind::McpPoll,
            "watch_file" => TriggerKind::WatchFile,
            "event" => TriggerKind::Event,
            "webhook" => TriggerKind::Webhook,
            _ => return None,
        })
    }
}

/// Préfixe des chemins de webhook : `/hook/<jeton>` (#294).
pub const WEBHOOK_PATH_PREFIX: &str = "/hook/";

/// Un jeton de chemin de webhook : 16 caractères au moins parmi `[A-Za-z0-9_-]`, pour
/// qu'il ne se devine pas et ne porte rien qu'une URL ou un journal maltraite.
pub fn webhook_token_ok(token: &str) -> bool {
    token.len() >= 16
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
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
    /// Exécutions ratées à la suite, remises à zéro au premier succès (#229).
    #[serde(default)]
    pub failures_in_a_row: u32,
    /// Motif normalisé de la dernière alerte de la série en cours ; `None` : aucune
    /// alerte n'est partie depuis le dernier succès.
    #[serde(skip)]
    pub alerted_reason: Option<String>,
}

impl Schedule {
    pub fn target_kind(&self) -> Option<TargetKind> {
        self.target
            .get("type")
            .and_then(|t| t.as_str())
            .and_then(TargetKind::parse)
    }

    /// Chemin d'un déclencheur `webhook` (`/hook/<jeton>`), s'il en a un.
    pub fn webhook_path(&self) -> Option<&str> {
        (self.kind == TriggerKind::Webhook)
            .then(|| self.spec.get("path").and_then(|v| v.as_str()))
            .flatten()
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
            TriggerKind::Webhook => {
                let path = self
                    .spec
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or("`path` est obligatoire pour un webhook (attribué à la création)")?;
                if !path
                    .strip_prefix(WEBHOOK_PATH_PREFIX)
                    .is_some_and(webhook_token_ok)
                {
                    return Err(format!(
                        "`path` d'un webhook : `{WEBHOOK_PATH_PREFIX}<jeton>` attendu, reçu `{path}`"
                    ));
                }
                let secret_ref = self.spec.get("secret_ref").and_then(|v| v.as_str()).ok_or(
                    "`secret_ref` est obligatoire pour un webhook (attribué à la création)",
                )?;
                if secret_ref.is_empty()
                    || !secret_ref
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                {
                    return Err(format!(
                        "`secret_ref` d'un webhook : nom de secret invalide `{secret_ref}`"
                    ));
                }
                // Le secret lui-même n'a rien à faire dans le store.
                if self.spec.get("secret").is_some() {
                    return Err("`secret` n'a pas sa place dans la spécification : le secret vit dans le magasin de secrets, sous `secret_ref`".into());
                }
                if self.spec.get("filter").is_some_and(|f| !f.is_object()) {
                    return Err("`filter` d'un webhook : un objet `{chemin: valeur}`".into());
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
            TriggerKind::WatchFile | TriggerKind::Event | TriggerKind::Webhook => None,
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
            failures_in_a_row: 0,
            alerted_reason: None,
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

    /// Marque l'exécution et programme la suivante. Rend, pour un succès, la série alertée
    /// qu'il clôt (voir [`ScheduleStore::record_outcome`]).
    pub async fn mark_run(
        &self,
        id: &str,
        error: Option<&str>,
    ) -> penelope_store::Result<Option<u32>> {
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
                let closed = streak_closed(tx, &id, err.is_none())?;
                tx.execute(
                    &format!(
                        "UPDATE schedules SET last_run = ?2, next_run = ?3, runs = runs + 1,
                            last_error = ?4, updated_at = ?2, {} WHERE id = ?1",
                        streak("?4")
                    ),
                    params![id, now, next, err],
                )?;
                Ok(closed)
            })
            .await
    }

    /// Programme le passage suivant sans compter d'exécution (issue #39) : un prompt planifié
    /// n'est exécuté qu'à la fin de son tour. `error` : le déclenchement lui-même a échoué.
    pub async fn advance(&self, id: &str, error: Option<&str>) -> penelope_store::Result<()> {
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
                // Un déclenchement réussi n'est pas encore une exécution : la série attend
                // l'issue du tour. Un déclenchement en échec en est une, ratée.
                tx.execute(
                    "UPDATE schedules SET next_run = ?2, updated_at = ?3,
                        last_error = COALESCE(?4, last_error),
                        failures_in_a_row = failures_in_a_row + (?4 IS NOT NULL)
                     WHERE id = ?1",
                    params![id, next, now, err],
                )?;
                Ok(())
            })
            .await
    }

    /// Issue d'une exécution menée jusqu'au bout : réussie, elle compte (`runs`, `last_run`)
    /// et efface l'erreur ; sinon seule l'erreur est gardée (issue #39). La série d'échecs
    /// suit (#229) : un échec l'allonge, un succès la clôt. Rend, pour un succès qui clôt
    /// une série déjà alertée, son nombre d'échecs : le propriétaire, prévenu de la panne,
    /// l'est aussi de son retour.
    pub async fn record_outcome(
        &self,
        id: &str,
        error: Option<&str>,
    ) -> penelope_store::Result<Option<u32>> {
        let (id, now, err) = (
            id.to_string(),
            self.clock.now_rfc3339(),
            error.map(String::from),
        );
        self.store
            .write(move |tx| {
                let closed = streak_closed(tx, &id, err.is_none())?;
                match err {
                    None => tx.execute(
                        &format!(
                            "UPDATE schedules SET runs = runs + 1, last_run = ?2,
                                last_error = NULL, updated_at = ?2, {} WHERE id = ?1",
                            streak("NULL")
                        ),
                        params![id, now],
                    )?,
                    Some(e) => tx.execute(
                        &format!(
                            "UPDATE schedules SET last_error = ?3, updated_at = ?2, {}
                             WHERE id = ?1",
                            streak("?3")
                        ),
                        params![id, now, e],
                    )?,
                };
                Ok(closed)
            })
            .await
    }

    /// Décide si l'échec qui vient d'être enregistré mérite une alerte (#229) : le premier
    /// de la série, un motif (normalisé) différent de celui de la dernière alerte, ou un
    /// palier ([`failure_step`]). Sinon il est tu, et compté. L'alerte décidée est notée
    /// dans la même transaction : deux échecs simultanés n'alertent pas deux fois.
    /// Rend la longueur de la série si l'alerte doit partir.
    pub async fn alert_due(&self, id: &str, reason: &str) -> penelope_store::Result<Option<u32>> {
        let (id, motif) = (id.to_string(), failure_motif(reason));
        self.store
            .write(move |tx| {
                let row: Option<(i64, Option<String>)> = tx
                    .query_row(
                        "SELECT failures_in_a_row, alerted_reason FROM schedules WHERE id = ?1",
                        [&id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                let Some((failures, alerted)) = row else {
                    return Ok(Some(1));
                };
                let failures = failures.max(1) as u32;
                let due = alerted.as_deref() != Some(motif.as_str()) || failure_step(failures);
                if due {
                    tx.execute(
                        "UPDATE schedules SET alerted_reason = ?2 WHERE id = ?1",
                        params![id, motif],
                    )?;
                }
                Ok(due.then_some(failures))
            })
            .await
    }

    /// Planifications actives identiques à celle qu'on s'apprête à créer : même déclencheur,
    /// même spécification, même cible (prompt quasi identique) (issue #39).
    pub async fn similar(
        &self,
        kind: TriggerKind,
        spec: &Value,
        target: &Value,
    ) -> penelope_store::Result<Vec<Schedule>> {
        let words = |v: &Value| -> std::collections::BTreeSet<String> {
            v.as_str()
                .unwrap_or_default()
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.chars().count() > 2)
                .map(String::from)
                .collect()
        };
        let same_target = |other: &Value| {
            if other.get("type") != target.get("type") {
                return false;
            }
            match target.get("type").and_then(|t| t.as_str()) {
                Some("prompt") => {
                    let (a, b) = (words(&target["prompt"]), words(&other["prompt"]));
                    let union = a.union(&b).count();
                    union > 0 && a.intersection(&b).count() as f64 / union as f64 >= 0.8
                }
                Some("notify") => other.get("template") == target.get("template"),
                Some("workflow") => {
                    other.get("workflowId") == target.get("workflowId")
                        && other.get("params") == target.get("params")
                }
                _ => false,
            }
        };
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|s| s.state == "active" && s.kind == kind && &s.spec == spec)
            .filter(|s| same_target(&s.target))
            .collect())
    }

    /// Faux si la planification n'existe pas ou a été supprimée (#221).
    pub async fn set_state(&self, id: &str, state: &str) -> penelope_store::Result<bool> {
        let (id, state, now) = (id.to_string(), state.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                let changed = tx.execute(
                    "UPDATE schedules SET state = ?2, updated_at = ?3
                     WHERE id = ?1 AND state != 'deleted'",
                    params![id, state, now],
                )?;
                Ok(changed > 0)
            })
            .await
    }

    /// Change la destination d'une planification (`target.origin`) sans la recréer : son
    /// historique, ses exécutions et son état restent (issue #124). Faux si elle n'existe
    /// pas.
    pub async fn set_origin(&self, id: &str, origin: Value) -> penelope_store::Result<bool> {
        let (id, now) = (id.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                let target: Option<String> = tx
                    .query_row(
                        "SELECT target FROM schedules WHERE id = ?1 AND state != 'deleted'",
                        [&id],
                        |r| r.get(0),
                    )
                    .ok();
                let Some(target) = target else {
                    return Ok(false);
                };
                let mut v: Value = serde_json::from_str(&target).unwrap_or_else(|_| json!({}));
                if !v.is_object() {
                    v = json!({});
                }
                v["origin"] = origin;
                tx.execute(
                    "UPDATE schedules SET target = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, v.to_string(), now],
                )?;
                Ok(true)
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
     last_error, failures_in_a_row, alerted_reason FROM schedules";

/// Tenue de la série dans un `UPDATE` dont le paramètre `error` porte l'erreur (NULL : un
/// succès) : un échec l'allonge, un succès la remet à zéro et oublie la dernière alerte
/// (#229).
fn streak(error: &str) -> String {
    format!(
        "failures_in_a_row = CASE WHEN {error} IS NULL THEN 0 ELSE failures_in_a_row + 1 END,
         alerted_reason = CASE WHEN {error} IS NULL THEN NULL ELSE alerted_reason END"
    )
}

/// Série alertée que clôt un succès, lue avant qu'il la remette à zéro.
fn streak_closed(
    tx: &penelope_store::rusqlite::Transaction<'_>,
    id: &str,
    success: bool,
) -> penelope_store::rusqlite::Result<Option<u32>> {
    if !success {
        return Ok(None);
    }
    let row: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT failures_in_a_row, alerted_reason FROM schedules WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    Ok(match row {
        Some((n, Some(_))) if n > 0 => Some(n as u32),
        _ => None,
    })
}

/// Paliers où une série au même motif est rappelée : 5, 20, 100, puis toutes les 100
/// exécutions ratées. Une planification horaire cassée depuis des semaines ne se tait
/// jamais tout à fait (#39), sans répéter vingt-quatre fois par jour la même chose.
pub fn failure_step(failures: u32) -> bool {
    matches!(failures, 5 | 20) || (failures >= 100 && failures.is_multiple_of(100))
}

/// Motif d'un échec, ce qui compte pour dire « c'est la même panne » (#229) : l'erreur
/// sans ce qui change d'une exécution à l'autre. Un mot de huit caractères ou plus qui
/// contient un chiffre est un identifiant, un horodatage ou une empreinte
/// (`gen-1759000001-a1b2c3`, `2026-09-27T08:00:00Z`, `sch_01K5…`) : il devient `#`. Un
/// code court (`429`, `HTTP 500`) reste, pour qu'un changement de cause se voie.
pub fn failure_motif(reason: &str) -> String {
    reason
        .split_whitespace()
        .map(|w| {
            let core = w.trim_matches(|c: char| !c.is_alphanumeric());
            if core.chars().count() >= 8 && core.chars().any(|c| c.is_ascii_digit()) {
                w.replacen(core, "#", 1)
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

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
        failures_in_a_row: r.get::<_, i64>(10)?.max(0) as u32,
        alerted_reason: r.get(11)?,
    })
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests;
