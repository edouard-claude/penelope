//! File de tours durable avec lease et heartbeat (§3.3).
//!
//! Chaque entrée est un enregistrement durable. Un runner **réclame** un tour (lease avec
//! expiration), le traite en envoyant des heartbeats, puis le clôt. Un tour interrompu par
//! un crash est réclamé à nouveau après expiration du lease : rien n'est perdu, rien n'est
//! exécuté deux fois (l'idempotence des effets est assurée par le ledger, §4.2).
//!
//! Le lease porte un **jeton de clôture** (issue #43) : `heartbeat` et `finish` n'écrivent
//! que si le lease appartient encore au holder qui l'a réclamé. Un runner évincé apprend
//! qu'il a perdu la main ([`crate::error::KernelError::LeaseLost`]) au lieu de clore le
//! tour d'un autre.
//!
//! ```text
//!  claim("r1") ─► leases(turn:A, holder=r1)   écrivain figé 90 s, aucun battement
//!                                │
//!    claim("r2") ────────────────┤ r1 est vivant dans ce processus : pas de reprise
//!                                │ (lease expiré = écrivain occupé, pas runner mort)
//!    r1 termine ─► finish(holder=r1) ✓
//!
//!  processus tué ─► leases(turn:A, holder=r1) expire, aucun runner vivant
//!    claim("r2") ─► reprise de A, holder=r2
//!    r1 ressuscité ─► heartbeat/finish(holder=r1) ─► LeaseLost : ne livre rien
//! ```

use crate::clock::SharedClock;
use crate::error::{KernelError, Result};
use crate::ids::TurnId;
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    Message,
    Trigger,
    Resume,
    Nudge,
}

impl TurnKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TurnKind::Message => "message",
            TurnKind::Trigger => "trigger",
            TurnKind::Resume => "resume",
            TurnKind::Nudge => "nudge",
        }
    }
    pub fn parse(s: &str) -> Option<TurnKind> {
        Some(match s {
            "message" => TurnKind::Message,
            "trigger" => TurnKind::Trigger,
            "resume" => TurnKind::Resume,
            "nudge" => TurnKind::Nudge,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub session_id: String,
    pub kind: TurnKind,
    pub payload: Value,
    pub attempts: i64,
    pub enqueued_at: String,
    /// Runner qui tient le lease : jeton de clôture de `heartbeat` et `finish` (#43).
    #[serde(default)]
    pub holder: String,
    /// Messages originaux absorbés, dans l'ordre d'arrivée. La ligne porteuse garde
    /// son propre `payload` ; chaque ligne conserve son ID et sa clé de déduplication.
    #[serde(default)]
    pub merged_messages: Vec<TurnMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMessage {
    pub id: TurnId,
    pub payload: Value,
    pub enqueued_at: String,
}

/// L'identité du canal ignore le message Telegram lui-même : deux messages du
/// même chat et du même sujet partagent un tour, mais jamais deux sujets.
fn origin_key(payload: &Value) -> Option<Value> {
    let origin = payload.get("origin")?;
    if payload.get("text")?.as_str()?.trim().is_empty() {
        return None;
    }
    if payload
        .get("images")
        .and_then(Value::as_array)
        .is_some_and(|images| !images.is_empty())
    {
        return None;
    }
    match origin.get("channel")?.as_str()? {
        "telegram" => Some(serde_json::json!({
            "channel": "telegram",
            "chat_id": origin.get("chat_id")?.as_i64()?,
            "topic_id": origin.get("topic_id").filter(|v| !v.is_null()).cloned(),
        })),
        "cli" => Some(serde_json::json!({"channel": "cli"})),
        "internal" => Some(serde_json::json!({
            "channel": "internal", "source": origin.get("source")?.as_str()?
        })),
        _ => None,
    }
}

#[derive(Clone)]
pub struct TurnQueue {
    store: Store,
    clock: SharedClock,
    lease_ttl_ms: i64,
    /// Runners vivants de **ce** processus : holder vers le tour qu'il tient. Un lease
    /// expiré dont le holder est là n'est pas repris : l'écrivain était occupé (sauvegarde,
    /// réindexation, veille du Mac), le runner n'est pas mort (#43).
    live: Arc<Mutex<HashMap<String, String>>>,
}

impl TurnQueue {
    pub fn new(store: Store, clock: SharedClock, lease_ttl_ms: i64) -> Self {
        TurnQueue {
            store,
            clock,
            lease_ttl_ms,
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn live_holders(&self) -> Vec<String> {
        lock(&self.live).keys().cloned().collect()
    }

    /// Ce runner tient ce tour dans ce processus.
    fn remember(&self, holder: &str, turn_id: &str) {
        lock(&self.live).insert(holder.to_string(), turn_id.to_string());
    }

    /// Ce runner n'a plus de tour : son lease peut être repris s'il traîne.
    fn forget(&self, holder: &str, turn_id: &str) {
        let mut g = lock(&self.live);
        if g.get(holder).map(|t| t == turn_id).unwrap_or(false) {
            g.remove(holder);
        }
    }

    /// Ajoute un tour. `dedup_key` rend l'ajout idempotent (un `update_id` Telegram
    /// rejoué ne crée pas un second tour).
    pub async fn enqueue(
        &self,
        session_id: &str,
        kind: TurnKind,
        payload: Value,
        dedup_key: Option<String>,
        priority: i64,
    ) -> Result<Option<TurnId>> {
        let id = TurnId::new();
        let (sid, now) = (session_id.to_string(), self.clock.now_rfc3339());
        let idc = id.clone();
        let inserted = self
            .store
            .write(move |tx| {
                let n = tx.execute(
                    "INSERT OR IGNORE INTO turn_queue(id, session_id, kind, payload, state,
                        priority, enqueued_at, dedup_key)
                     VALUES(?1,?2,?3,?4,'pending',?5,?6,?7)",
                    params![
                        idc.as_str(),
                        sid,
                        kind.as_str(),
                        serde_json::to_string(&payload).unwrap_or_else(|_| "null".into()),
                        priority,
                        now,
                        dedup_key
                    ],
                )?;
                Ok(n > 0)
            })
            .await?;
        Ok(inserted.then_some(id))
    }

    /// Réclame le prochain tour disponible.
    ///
    /// **Verrou de session** : une seule exécution active par session (§3.3). Un tour
    /// dont la session a déjà un lease actif n'est pas réclamé.
    pub async fn claim(&self, holder: &str) -> Result<Option<Turn>> {
        let holder = holder.to_string();
        let mine = holder.clone();
        let now_ms = self.clock.now_ms();
        let now = self.clock.now_rfc3339();
        let expires = ms_to_rfc3339(now_ms + self.lease_ttl_ms);
        let live = self.live_holders();

        let claimed = self
            .store
            .write(move |tx| {
                // 1. Libère les leases expirés : un tour dont le runner a disparu
                //    redevient `pending`, avec un compteur de tentatives incrémenté. Un
                //    holder vivant de ce processus garde le sien (#43).
                let expired: Vec<(String, String)> = {
                    let mut st =
                        tx.prepare("SELECT resource, holder FROM leases WHERE expires_at <= ?1")?;
                    let rows = st.query_map([&now], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?;
                    let mut v = Vec::new();
                    for r in rows {
                        v.push(r?);
                    }
                    v
                };
                for (res, owner) in &expired {
                    if live.iter().any(|h| h == owner) {
                        continue;
                    }
                    if let Some(turn_id) = res.strip_prefix("turn:") {
                        tx.execute(
                            "UPDATE turn_queue SET state='pending' WHERE id=?1 AND state='leased'",
                            [turn_id],
                        )?;
                    }
                    tx.execute("DELETE FROM leases WHERE resource=?1", [res])?;
                }

                // 2. Les tours d'une session fermée ne répondront jamais (issue #10).
                tx.execute(
                    "UPDATE turn_queue SET state = 'cancelled', finished_at = ?1,
                        last_error = 'session fermée'
                     WHERE state = 'pending' AND session_id IN
                       (SELECT id FROM sessions WHERE state IN ('closed', 'deleted'))",
                    [&now],
                )?;

                // 3. Sélectionne le prochain tour dont la session est libre.
                let candidate: Option<(String, String, String, String, i64, String)> = tx
                    .query_row(
                        "SELECT q.id, q.session_id, q.kind, q.payload, q.attempts, q.enqueued_at
                         FROM turn_queue q
                         WHERE q.state = 'pending'
                           AND NOT EXISTS (SELECT 1 FROM leases l
                                           WHERE l.resource = 'session:' || q.session_id)
                           AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.id = q.session_id
                                           AND s.state IN ('closed', 'deleted'))
                         ORDER BY q.priority DESC, q.enqueued_at, q.rowid
                         LIMIT 1",
                        [],
                        |r| {
                            Ok((
                                r.get(0)?,
                                r.get(1)?,
                                r.get(2)?,
                                r.get(3)?,
                                r.get(4)?,
                                r.get(5)?,
                            ))
                        },
                    )
                    .ok();

                let Some((id, session_id, kind, payload, attempts, enqueued_at)) = candidate else {
                    return Ok(None);
                };

                let payload_value: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                let mut merged_messages = Vec::new();
                if kind == "message"
                    && let Some(key) = origin_key(&payload_value)
                {
                    let candidates: Vec<(String, String, String, bool)> = {
                        let mut st = tx.prepare(
                            "SELECT id, payload, enqueued_at, state='merged'
                             FROM turn_queue
                             WHERE session_id=?1 AND kind='message'
                               AND ((state='pending' AND id<>?2)
                                    OR (state='merged' AND merged_into=?2))
                             ORDER BY enqueued_at, rowid",
                        )?;
                        let rows = st.query_map(params![session_id, id], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                        })?;
                        rows.collect::<penelope_store::rusqlite::Result<Vec<_>>>()?
                    };
                    for (message_id, body, arrived, was_merged) in candidates {
                        let message_payload: Value =
                            serde_json::from_str(&body).unwrap_or(Value::Null);
                        if !was_merged && origin_key(&message_payload) != Some(key.clone()) {
                            continue;
                        }
                        if !was_merged {
                            tx.execute(
                                "UPDATE turn_queue SET state='merged', merged_into=?2
                                 WHERE id=?1 AND state='pending'",
                                params![message_id, id],
                            )?;
                        }
                        merged_messages.push(TurnMessage {
                            id: TurnId(message_id),
                            payload: message_payload,
                            enqueued_at: arrived,
                        });
                    }
                }

                tx.execute(
                    "UPDATE turn_queue SET state='leased', started_at=?2, attempts=attempts+1
                     WHERE id=?1",
                    params![id, now],
                )?;
                for resource in [format!("turn:{id}"), format!("session:{session_id}")] {
                    tx.execute(
                        "INSERT OR REPLACE INTO leases(resource, holder, acquired_at, expires_at,
                            heartbeat_at) VALUES(?1,?2,?3,?4,?3)",
                        params![resource, holder, now, expires],
                    )?;
                }

                Ok(Some(Turn {
                    id: TurnId(id),
                    session_id,
                    kind: TurnKind::parse(&kind).unwrap_or(TurnKind::Message),
                    payload: payload_value,
                    attempts: attempts + 1,
                    enqueued_at,
                    holder: mine,
                    merged_messages,
                }))
            })
            .await?;
        if let Some(t) = &claimed {
            self.remember(&t.holder, t.id.as_str());
        }
        Ok(claimed)
    }

    /// Absorbe les nouveaux messages arrivés pendant l'exécution, sous le lease du
    /// tour porteur. Un autre runner ne peut ni les voler ni les marquer lus.
    pub async fn absorb_pending(&self, turn: &Turn) -> Result<Vec<TurnMessage>> {
        if turn.kind != TurnKind::Message {
            return Ok(Vec::new());
        }
        let Some(origin) = origin_key(&turn.payload) else {
            return Ok(Vec::new());
        };
        let (id, sid, holder) = (
            turn.id.to_string(),
            turn.session_id.clone(),
            turn.holder.clone(),
        );
        let (held, absorbed) = self
            .store
            .write(move |tx| {
                let held: i64 = tx.query_row(
                    "SELECT count(*) FROM leases WHERE resource=?1 AND holder=?2",
                    params![format!("turn:{id}"), holder],
                    |r| r.get(0),
                )?;
                if held == 0 {
                    return Ok((false, Vec::new()));
                }
                let candidates: Vec<(String, String, String)> = {
                    let mut st = tx.prepare(
                        "SELECT id, payload, enqueued_at FROM turn_queue
                         WHERE session_id=?1 AND kind='message' AND state='pending'
                         ORDER BY enqueued_at, rowid",
                    )?;
                    let rows = st.query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                    rows.collect::<penelope_store::rusqlite::Result<Vec<_>>>()?
                };
                let mut absorbed = Vec::new();
                for (message_id, body, arrived) in candidates {
                    let payload: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    if origin_key(&payload) != Some(origin.clone()) {
                        continue;
                    }
                    tx.execute(
                        "UPDATE turn_queue SET state='merged', merged_into=?2
                         WHERE id=?1 AND state='pending'",
                        params![message_id, id],
                    )?;
                    absorbed.push(TurnMessage {
                        id: TurnId(message_id),
                        payload,
                        enqueued_at: arrived,
                    });
                }
                Ok((true, absorbed))
            })
            .await?;
        if !held {
            return Err(KernelError::LeaseLost(format!(
                "tour {} : absorption refusée au runner évincé",
                turn.id
            )));
        }
        Ok(absorbed)
    }

    /// Dernière origine absorbée, y compris les messages arrivés pendant le tour.
    pub async fn last_merged_payload(&self, turn_id: &str) -> Result<Option<Value>> {
        let id = turn_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let payload: Option<String> = c
                    .query_row(
                        "SELECT payload FROM turn_queue WHERE merged_into=?1 AND state='merged'
                         ORDER BY enqueued_at DESC, rowid DESC LIMIT 1",
                        [&id],
                        |r| r.get(0),
                    )
                    .ok();
                Ok(payload.and_then(|raw| serde_json::from_str(&raw).ok()))
            })
            .await?)
    }

    /// Toutes les lignes absorbées par ce porteur, dans leur ordre de réception.
    pub async fn merged_messages(&self, turn_id: &str) -> Result<Vec<TurnMessage>> {
        let id = turn_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, payload, enqueued_at FROM turn_queue
                     WHERE merged_into=?1 AND state='merged' ORDER BY enqueued_at, rowid",
                )?;
                let rows = st.query_map([id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?;
                let mut messages = Vec::new();
                for row in rows {
                    let (id, payload, enqueued_at) = row?;
                    messages.push(TurnMessage {
                        id: TurnId(id),
                        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                        enqueued_at,
                    });
                }
                Ok(messages)
            })
            .await?)
    }

    /// Prolonge le lease d'un tour en cours.
    ///
    /// Renvoie [`KernelError::LeaseLost`] si le lease appartient désormais à un autre
    /// runner : l'appelant doit abandonner le tour sans rien livrer (#43).
    pub async fn heartbeat(&self, turn: &Turn) -> Result<()> {
        let now = self.clock.now_rfc3339();
        let expires = ms_to_rfc3339(self.clock.now_ms() + self.lease_ttl_ms);
        let holder = turn.holder.clone();
        let resources = [
            format!("turn:{}", turn.id),
            format!("session:{}", turn.session_id),
        ];
        let kept = self
            .store
            .write(move |tx| {
                let mut kept = false;
                for (i, r) in resources.iter().enumerate() {
                    let n = tx.execute(
                        "UPDATE leases SET heartbeat_at=?2, expires_at=?3
                         WHERE resource=?1 AND holder=?4",
                        params![r, now, expires, holder],
                    )?;
                    if i == 0 {
                        kept = n > 0;
                    }
                }
                Ok(kept)
            })
            .await?;
        if !kept {
            self.forget(&turn.holder, turn.id.as_str());
            return Err(KernelError::LeaseLost(format!(
                "tour {} : lease repris par un autre runner",
                turn.id
            )));
        }
        Ok(())
    }

    pub async fn complete(&self, turn: &Turn) -> Result<()> {
        self.finish(turn, "done", None).await
    }

    pub async fn cancel_leased(&self, turn: &Turn) -> Result<()> {
        self.finish(turn, "cancelled", None).await
    }

    pub async fn fail(&self, turn: &Turn, error: &str) -> Result<()> {
        self.finish(turn, "failed", Some(error.to_string())).await
    }

    /// Annule les tours en attente d'une session (fermée ou détachée de son chat). Renvoie
    /// leur nombre.
    pub async fn cancel_pending(&self, session_id: &str, reason: &str) -> Result<usize> {
        let (sid, reason, now) = (
            session_id.to_string(),
            reason.to_string(),
            self.clock.now_rfc3339(),
        );
        Ok(self
            .store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE turn_queue SET state = 'cancelled', finished_at = ?2, last_error = ?3
                     WHERE session_id = ?1 AND state = 'pending'",
                    params![sid, now, reason],
                )?)
            })
            .await?)
    }

    /// Annule un tour s'il attend encore d'être réclamé ; un tour déjà parti reste à
    /// son runner (qui s'arrête par son jeton d'annulation). Vrai s'il a été annulé.
    pub async fn cancel_if_pending(&self, turn_id: &str) -> Result<bool> {
        let (id, now) = (turn_id.to_string(), self.clock.now_rfc3339());
        Ok(self
            .store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE turn_queue SET state='cancelled', finished_at=?2,
                        last_error='client parti'
                     WHERE id=?1 AND state='pending'",
                    params![id, now],
                )? > 0)
            })
            .await?)
    }

    pub async fn cancel(&self, turn_id: &str) -> Result<()> {
        let (id, now) = (turn_id.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE turn_queue SET state='cancelled', finished_at=?2 WHERE id=?1",
                    params![id, now],
                )?;
                tx.execute(
                    "UPDATE turn_queue SET state='cancelled', finished_at=?2
                     WHERE merged_into=?1 AND state='merged'",
                    params![id, now],
                )?;
                tx.execute(
                    "DELETE FROM leases WHERE resource=?1",
                    [format!("turn:{id}")],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Clôt un tour. N'écrit que si le lease est encore au holder : un runner évincé
    /// repart en [`KernelError::LeaseLost`] sans toucher au tour de son successeur (#43).
    async fn finish(&self, turn: &Turn, state: &str, error: Option<String>) -> Result<()> {
        let (id, sid, holder, now, state) = (
            turn.id.0.clone(),
            turn.session_id.clone(),
            turn.holder.clone(),
            self.clock.now_rfc3339(),
            state.to_string(),
        );
        let held = self
            .store
            .write(move |tx| {
                let (turn_res, session_res) = (format!("turn:{id}"), format!("session:{sid}"));
                let owner: Option<String> = tx
                    .query_row(
                        "SELECT holder FROM leases WHERE resource=?1",
                        [&turn_res],
                        |r| r.get(0),
                    )
                    .ok();
                if matches!(&owner, Some(o) if o != &holder) {
                    return Ok(false);
                }
                // `state='leased'` : un tour annulé entre-temps garde son état.
                tx.execute(
                    "UPDATE turn_queue SET state=?2, finished_at=?3, last_error=?4
                     WHERE id=?1 AND state='leased'",
                    params![id, state, now, error],
                )?;
                if state == "cancelled" {
                    tx.execute(
                        "UPDATE turn_queue SET state='cancelled', finished_at=?2
                         WHERE merged_into=?1 AND state='merged'",
                        params![id, now],
                    )?;
                }
                tx.execute(
                    "DELETE FROM leases WHERE resource IN (?1, ?2) AND holder=?3",
                    params![turn_res, session_res, holder],
                )?;
                Ok(true)
            })
            .await?;
        self.forget(&turn.holder, turn.id.as_str());
        if !held {
            return Err(KernelError::LeaseLost(format!(
                "tour {} : clôture refusée, le lease est à un autre runner",
                turn.id
            )));
        }
        Ok(())
    }

    /// Tours d'une session en file ou en cours.
    pub async fn queued_for(&self, session_id: &str) -> Result<i64> {
        let sid = session_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM turn_queue
                     WHERE session_id = ?1 AND state IN ('pending','leased')",
                    [&sid],
                    |r| r.get(0),
                )?)
            })
            .await?)
    }

    pub async fn pending_count(&self) -> Result<i64> {
        Ok(self
            .store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM turn_queue WHERE state IN ('pending','leased')",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await?)
    }

    /// Au démarrage : tout lease est mort, tout tour `leased` redevient `pending`.
    pub async fn recover_on_boot(&self) -> Result<i64> {
        lock(&self.live).clear();
        Ok(self
            .store
            .write(|tx| {
                tx.execute("DELETE FROM leases", [])?;
                let n = tx.execute(
                    "UPDATE turn_queue SET state='pending' WHERE state='leased'",
                    [],
                )?;
                Ok(n as i64)
            })
            .await?)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use serde_json::json;
    use std::sync::Arc;

    fn queue(store: Store, clock: TestClock) -> TurnQueue {
        TurnQueue::new(store, Arc::new(clock), 60_000)
    }

    #[tokio::test]
    async fn enqueue_claim_complete() {
        let q = queue(Store::open_memory().unwrap(), TestClock::default());
        q.enqueue("s1", TurnKind::Message, json!({"t":"salut"}), None, 0)
            .await
            .unwrap();
        let t = q.claim("runner-1").await.unwrap().unwrap();
        assert_eq!(t.session_id, "s1");
        assert_eq!(t.attempts, 1);
        assert!(
            q.claim("runner-2").await.unwrap().is_none(),
            "session verrouillée"
        );
        q.complete(&t).await.unwrap();
        assert_eq!(q.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn dedup_key_prevents_double_enqueue() {
        let q = queue(Store::open_memory().unwrap(), TestClock::default());
        let a = q
            .enqueue("s1", TurnKind::Message, json!({}), Some("u42".into()), 0)
            .await
            .unwrap();
        let b = q
            .enqueue("s1", TurnKind::Message, json!({}), Some("u42".into()), 0)
            .await
            .unwrap();
        assert!(a.is_some());
        assert!(b.is_none(), "un update rejoué ne crée pas un second tour");
    }

    /// #161 : les messages Telegram distincts d'un même sujet forment un tour, sans
    /// perdre leur identité, leur ordre ni leur clé de déduplication.
    #[tokio::test]
    async fn claim_merges_pending_messages_of_one_origin() {
        let clock = TestClock::default();
        let store = Store::open_memory().unwrap();
        let q = queue(store.clone(), clock.clone());
        let origin = |message_id| {
            json!({
                "channel": "telegram", "chat_id": 10, "topic_id": 7, "message_id": message_id
            })
        };
        let first = q
            .enqueue(
                "s1",
                TurnKind::Message,
                json!({"text":"un", "origin":origin(1)}),
                Some("tg:1".into()),
                0,
            )
            .await
            .unwrap()
            .unwrap();
        clock.advance_ms(1);
        let second = q
            .enqueue(
                "s1",
                TurnKind::Message,
                json!({"text":"deux", "origin":origin(2)}),
                Some("tg:2".into()),
                0,
            )
            .await
            .unwrap()
            .unwrap();
        clock.advance_ms(1);
        let third = q
            .enqueue(
                "s1",
                TurnKind::Message,
                json!({"text":"trois", "origin":origin(3)}),
                Some("tg:3".into()),
                0,
            )
            .await
            .unwrap()
            .unwrap();

        let turn = q.claim("r1").await.unwrap().unwrap();
        assert_eq!(turn.id, first);
        assert_eq!(
            turn.merged_messages
                .iter()
                .map(|m| m.payload["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["deux", "trois"]
        );
        for (id, key) in [(second, "tg:2"), (third, "tg:3")] {
            let id = id.to_string();
            let row: (String, Option<String>, String) = store
                .read(move |c| {
                    Ok(c.query_row(
                        "SELECT state, merged_into, dedup_key FROM turn_queue WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?)
                })
                .await
                .unwrap();
            assert_eq!(row, ("merged".into(), Some(first.to_string()), key.into()));
        }
        assert_eq!(
            q.last_merged_payload(first.as_str())
                .await
                .unwrap()
                .unwrap()["origin"]["message_id"],
            3
        );
        assert!(
            q.enqueue("s1", TurnKind::Message, json!({}), Some("tg:2".into()), 0)
                .await
                .unwrap()
                .is_none()
        );
        q.complete(&turn).await.unwrap();
        assert!(q.claim("r2").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn claim_keeps_sessions_origins_and_resume_priority_separate() {
        let q = queue(Store::open_memory().unwrap(), TestClock::default());
        let telegram = |id| {
            json!({"text":format!("m{id}"), "origin":{
                "channel":"telegram", "chat_id":10, "message_id":id
            }})
        };
        q.enqueue("s1", TurnKind::Message, telegram(1), None, 0)
            .await
            .unwrap();
        q.enqueue("s1", TurnKind::Message, telegram(2), None, 0)
            .await
            .unwrap();
        q.enqueue(
            "s1",
            TurnKind::Message,
            json!({"text":"cli", "origin":{"channel":"cli"}}),
            None,
            0,
        )
        .await
        .unwrap();
        q.enqueue("s2", TurnKind::Message, telegram(3), None, 0)
            .await
            .unwrap();
        q.enqueue("s2", TurnKind::Message, telegram(4), None, 0)
            .await
            .unwrap();
        q.enqueue(
            "s1",
            TurnKind::Resume,
            json!({"origin":{"channel":"telegram","chat_id":10}}),
            None,
            10,
        )
        .await
        .unwrap();

        let resume = q.claim("r1").await.unwrap().unwrap();
        assert_eq!(resume.kind, TurnKind::Resume);
        assert!(resume.merged_messages.is_empty());
        q.complete(&resume).await.unwrap();
        let a = q.claim("r1").await.unwrap().unwrap();
        assert_eq!(a.session_id, "s1");
        assert_eq!(a.merged_messages.len(), 1);
        q.complete(&a).await.unwrap();
        let b = q.claim("r1").await.unwrap().unwrap();
        assert_eq!(b.session_id, "s1");
        assert_eq!(b.payload["text"], "cli");
        assert!(b.merged_messages.is_empty());
        q.complete(&b).await.unwrap();
        let c = q.claim("r1").await.unwrap().unwrap();
        assert_eq!(c.session_id, "s2");
        assert_eq!(c.merged_messages.len(), 1);
    }

    #[tokio::test]
    async fn a_live_turn_absorbs_pending_messages_and_recovery_keeps_them() {
        let store = Store::open_memory().unwrap();
        let clock = TestClock::default();
        let q = queue(store.clone(), clock.clone());
        let payload = |text| json!({"text":text, "origin":{"channel":"cli"}});
        q.enqueue("s1", TurnKind::Message, payload("premier"), None, 0)
            .await
            .unwrap();
        let turn = q.claim("r1").await.unwrap().unwrap();
        q.enqueue(
            "s1",
            TurnKind::Message,
            payload("ajout"),
            Some("u2".into()),
            0,
        )
        .await
        .unwrap();
        let absorbed = q.absorb_pending(&turn).await.unwrap();
        assert_eq!(absorbed.len(), 1);
        assert_eq!(absorbed[0].payload["text"], "ajout");
        assert!(q.absorb_pending(&turn).await.unwrap().is_empty());

        let restarted = queue(store, clock);
        restarted.recover_on_boot().await.unwrap();
        let replayed = restarted.claim("r2").await.unwrap().unwrap();
        assert_eq!(replayed.id, turn.id);
        assert_eq!(replayed.merged_messages.len(), 1);
        assert_eq!(replayed.merged_messages[0].id, absorbed[0].id);
    }

    #[tokio::test]
    async fn a_message_after_completion_remains_a_separate_turn() {
        let q = queue(Store::open_memory().unwrap(), TestClock::default());
        let payload = |text| json!({"text":text, "origin":{"channel":"cli"}});
        q.enqueue("s1", TurnKind::Message, payload("premier"), None, 0)
            .await
            .unwrap();
        let first = q.claim("r1").await.unwrap().unwrap();
        q.complete(&first).await.unwrap();
        q.enqueue("s1", TurnKind::Message, payload("après"), None, 0)
            .await
            .unwrap();
        let second = q.claim("r1").await.unwrap().unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(second.payload["text"], "après");
        assert!(second.merged_messages.is_empty());
    }

    #[tokio::test]
    async fn a_photo_message_is_not_absorbed_as_text() {
        let q = queue(Store::open_memory().unwrap(), TestClock::default());
        let origin = json!({"channel":"telegram", "chat_id":10});
        q.enqueue(
            "s1",
            TurnKind::Message,
            json!({"text":"question", "origin":origin}),
            None,
            0,
        )
        .await
        .unwrap();
        q.enqueue(
            "s1",
            TurnKind::Message,
            json!({"text":"regarde", "images":["photo.jpg"], "origin":origin}),
            None,
            0,
        )
        .await
        .unwrap();
        let first = q.claim("r1").await.unwrap().unwrap();
        assert!(first.merged_messages.is_empty());
        q.complete(&first).await.unwrap();
        let second = q.claim("r1").await.unwrap().unwrap();
        assert_eq!(second.payload["images"][0], "photo.jpg");
    }

    #[tokio::test]
    async fn cancelling_a_merged_turn_cancels_every_original_message() {
        let store = Store::open_memory().unwrap();
        let q = queue(store.clone(), TestClock::default());
        let payload = |text| json!({"text":text, "origin":{"channel":"cli"}});
        q.enqueue("s1", TurnKind::Message, payload("un"), None, 0)
            .await
            .unwrap();
        q.enqueue("s1", TurnKind::Message, payload("deux"), None, 0)
            .await
            .unwrap();
        let turn = q.claim("r1").await.unwrap().unwrap();
        assert_eq!(turn.merged_messages.len(), 1);
        q.cancel_leased(&turn).await.unwrap();
        let count: i64 = store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM turn_queue WHERE state='cancelled'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(count, 2);
        assert!(q.claim("r2").await.unwrap().is_none());
    }

    /// CA 3 : un tour interrompu par un crash est réclamé à nouveau après expiration
    /// du lease. Le « kill -9 » est un autre processus, donc une autre file (le runner
    /// d'origine n'est plus vivant nulle part, #43).
    #[tokio::test]
    async fn ca_3_3_expired_lease_is_reclaimed() {
        let clock = TestClock::default();
        let store = Store::open_memory().unwrap();
        let mort = queue(store.clone(), clock.clone());
        mort.enqueue("s1", TurnKind::Message, json!({}), None, 0)
            .await
            .unwrap();

        let t1 = mort.claim("runner-1").await.unwrap().unwrap();
        assert!(mort.claim("runner-2").await.unwrap().is_none());

        // « kill -9 » du processus : personne n'envoie plus de heartbeat.
        let vivant = queue(store, clock.clone());
        clock.advance_ms(61_000);
        let t2 = vivant.claim("runner-2").await.unwrap().unwrap();
        assert_eq!(t2.id, t1.id);
        assert_eq!(t2.attempts, 2, "le compteur de tentatives doit progresser");
    }

    /// #43 : l'écrivain a gelé 90 s (sauvegarde, réindexation, veille du Mac), aucun
    /// battement n'est passé, mais r1 est vivant : son tour n'est pas repris et lui seul
    /// le clôt.
    #[tokio::test]
    async fn a_live_runner_keeps_its_turn_when_the_writer_was_frozen() {
        let clock = TestClock::default();
        let q = queue(Store::open_memory().unwrap(), clock.clone());
        q.enqueue("s1", TurnKind::Message, json!({"t":"premier"}), None, 0)
            .await
            .unwrap();
        q.enqueue("s1", TurnKind::Message, json!({"t":"second"}), None, 0)
            .await
            .unwrap();

        let a = q.claim("r1").await.unwrap().unwrap();
        clock.advance_ms(91_000);
        assert!(
            q.claim("r2").await.unwrap().is_none(),
            "un runner vivant garde son tour même sans battement"
        );
        // L'écrivain repart : le battement retrouve son lease.
        q.heartbeat(&a).await.unwrap();
        q.complete(&a).await.unwrap();

        let b = q.claim("r2").await.unwrap().unwrap();
        assert_ne!(b.id, a.id, "le tour suivant de la session vient après");
    }

    /// #43 : un runner évincé (processus figé puis réveillé, tour repris ailleurs) ne
    /// clôt pas le tour de son successeur et n'efface pas son lease.
    #[tokio::test]
    async fn an_evicted_runner_loses_its_lease_and_writes_nothing() {
        let clock = TestClock::default();
        let store = Store::open_memory().unwrap();
        let fige = queue(store.clone(), clock.clone());
        let repris = queue(store.clone(), clock.clone());
        fige.enqueue("s1", TurnKind::Message, json!({"t":"premier"}), None, 0)
            .await
            .unwrap();
        fige.enqueue("s1", TurnKind::Message, json!({"t":"second"}), None, 0)
            .await
            .unwrap();

        let a_r1 = fige.claim("r1").await.unwrap().unwrap();
        clock.advance_ms(61_000);
        let a_r2 = repris.claim("r2").await.unwrap().unwrap();
        assert_eq!(a_r1.id, a_r2.id, "r2 reprend le tour laissé sans battement");

        let e = fige.complete(&a_r1).await.unwrap_err();
        assert!(e.is_lease_lost(), "clôture refusée à l'évincé : {e}");
        let e = fige.heartbeat(&a_r1).await.unwrap_err();
        assert!(e.is_lease_lost(), "battement refusé à l'évincé : {e}");

        // Le verrou de session tient : le second tour attend la fin du premier.
        assert!(
            repris.claim("r3").await.unwrap().is_none(),
            "aucun autre tour de la session pendant que r2 exécute"
        );
        let id = a_r1.id.0.clone();
        let state: String = store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT state FROM turn_queue WHERE id=?1",
                    [id.as_str()],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(state, "leased", "le tour reste à r2");

        // r2 le termine : la session se libère pour le second tour.
        repris.complete(&a_r2).await.unwrap();
        assert!(repris.claim("r3").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn heartbeat_prevents_reclaim() {
        let clock = TestClock::default();
        let q = queue(Store::open_memory().unwrap(), clock.clone());
        q.enqueue("s1", TurnKind::Message, json!({}), None, 0)
            .await
            .unwrap();
        let t = q.claim("r1").await.unwrap().unwrap();
        for _ in 0..5 {
            clock.advance_ms(30_000);
            q.heartbeat(&t).await.unwrap();
        }
        assert!(
            q.claim("r2").await.unwrap().is_none(),
            "un runner vivant garde son tour"
        );
    }

    /// CA 3 : 4 sessions concurrentes s'exécutent sans interblocage.
    #[tokio::test]
    async fn ca_3_2_four_sessions_run_concurrently() {
        let q = queue(Store::open_memory().unwrap(), TestClock::default());
        for i in 0..4 {
            q.enqueue(&format!("s{i}"), TurnKind::Message, json!({}), None, 0)
                .await
                .unwrap();
        }
        let mut claimed = Vec::new();
        for i in 0..4 {
            let t = q.claim(&format!("runner-{i}")).await.unwrap();
            assert!(t.is_some(), "le runner {i} doit obtenir un tour");
            claimed.push(t.unwrap());
        }
        let mut sids: Vec<_> = claimed.iter().map(|t| t.session_id.clone()).collect();
        sids.sort();
        assert_eq!(sids, vec!["s0", "s1", "s2", "s3"]);
        assert!(q.claim("runner-5").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn priority_is_respected() {
        let q = queue(Store::open_memory().unwrap(), TestClock::default());
        q.enqueue("a", TurnKind::Message, json!({"p":0}), None, 0)
            .await
            .unwrap();
        q.enqueue("b", TurnKind::Resume, json!({"p":9}), None, 9)
            .await
            .unwrap();
        let t = q.claim("r").await.unwrap().unwrap();
        assert_eq!(t.session_id, "b");
    }

    #[tokio::test]
    async fn recover_on_boot_releases_everything() {
        let store = Store::open_memory().unwrap();
        let q = queue(store.clone(), TestClock::default());
        q.enqueue("s1", TurnKind::Message, json!({}), None, 0)
            .await
            .unwrap();
        q.claim("r1").await.unwrap().unwrap();

        let q2 = queue(store, TestClock::default());
        assert_eq!(q2.recover_on_boot().await.unwrap(), 1);
        assert!(q2.claim("r2").await.unwrap().is_some());
    }
}
