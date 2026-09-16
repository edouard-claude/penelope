//! File de tours durable avec lease et heartbeat (§3.3).
//!
//! Chaque entrée est un enregistrement durable. Un runner **réclame** un tour (lease avec
//! expiration), le traite en envoyant des heartbeats, puis le clôt. Un tour interrompu par
//! un crash est réclamé à nouveau après expiration du lease : rien n'est perdu, rien n'est
//! exécuté deux fois (l'idempotence des effets est assurée par le ledger, §4.2).

use crate::clock::SharedClock;
use crate::error::Result;
use crate::ids::TurnId;
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}

#[derive(Clone)]
pub struct TurnQueue {
    store: Store,
    clock: SharedClock,
    lease_ttl_ms: i64,
}

impl TurnQueue {
    pub fn new(store: Store, clock: SharedClock, lease_ttl_ms: i64) -> Self {
        TurnQueue {
            store,
            clock,
            lease_ttl_ms,
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
        let now_ms = self.clock.now_ms();
        let now = self.clock.now_rfc3339();
        let expires = ms_to_rfc3339(now_ms + self.lease_ttl_ms);

        Ok(self
            .store
            .write(move |tx| {
                // 1. Libère les leases expirés : un tour dont le runner a disparu
                //    redevient `pending`, avec un compteur de tentatives incrémenté.
                let expired: Vec<String> = {
                    let mut st = tx.prepare(
                        "SELECT resource FROM leases WHERE expires_at <= ?1 AND resource LIKE 'turn:%'",
                    )?;
                    let rows = st.query_map([&now], |r| r.get::<_, String>(0))?;
                    let mut v = Vec::new();
                    for r in rows {
                        v.push(r?);
                    }
                    v
                };
                for res in &expired {
                    let turn_id = res.trim_start_matches("turn:");
                    tx.execute(
                        "UPDATE turn_queue SET state='pending' WHERE id=?1 AND state='leased'",
                        [turn_id],
                    )?;
                    tx.execute("DELETE FROM leases WHERE resource=?1", [res])?;
                }
                tx.execute(
                    "DELETE FROM leases WHERE expires_at <= ?1 AND resource LIKE 'session:%'",
                    [&now],
                )?;

                // 2. Sélectionne le prochain tour dont la session est libre.
                let candidate: Option<(String, String, String, String, i64, String)> = tx
                    .query_row(
                        "SELECT q.id, q.session_id, q.kind, q.payload, q.attempts, q.enqueued_at
                         FROM turn_queue q
                         WHERE q.state = 'pending'
                           AND NOT EXISTS (SELECT 1 FROM leases l
                                           WHERE l.resource = 'session:' || q.session_id)
                         ORDER BY q.priority DESC, q.enqueued_at
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
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                    attempts: attempts + 1,
                    enqueued_at,
                }))
            })
            .await?)
    }

    /// Prolonge le lease d'un tour en cours.
    pub async fn heartbeat(&self, turn: &Turn) -> Result<()> {
        let now = self.clock.now_rfc3339();
        let expires = ms_to_rfc3339(self.clock.now_ms() + self.lease_ttl_ms);
        let resources = vec![
            format!("turn:{}", turn.id),
            format!("session:{}", turn.session_id),
        ];
        self.store
            .write(move |tx| {
                for r in &resources {
                    tx.execute(
                        "UPDATE leases SET heartbeat_at=?2, expires_at=?3 WHERE resource=?1",
                        params![r, now, expires],
                    )?;
                }
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn complete(&self, turn: &Turn) -> Result<()> {
        self.finish(turn, "done", None).await
    }

    pub async fn fail(&self, turn: &Turn, error: &str) -> Result<()> {
        self.finish(turn, "failed", Some(error.to_string())).await
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
                    "DELETE FROM leases WHERE resource=?1",
                    [format!("turn:{id}")],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn finish(&self, turn: &Turn, state: &str, error: Option<String>) -> Result<()> {
        let (id, sid, now, state) = (
            turn.id.0.clone(),
            turn.session_id.clone(),
            self.clock.now_rfc3339(),
            state.to_string(),
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE turn_queue SET state=?2, finished_at=?3, last_error=?4 WHERE id=?1",
                    params![id, state, now, error],
                )?;
                tx.execute(
                    "DELETE FROM leases WHERE resource IN (?1, ?2)",
                    params![format!("turn:{id}"), format!("session:{sid}")],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
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

    /// CA 3 : un tour interrompu par un crash est réclamé à nouveau après expiration
    /// du lease.
    #[tokio::test]
    async fn ca_3_3_expired_lease_is_reclaimed() {
        let clock = TestClock::default();
        let store = Store::open_memory().unwrap();
        let q = queue(store, clock.clone());
        q.enqueue("s1", TurnKind::Message, json!({}), None, 0)
            .await
            .unwrap();

        let t1 = q.claim("runner-1").await.unwrap().unwrap();
        assert!(q.claim("runner-2").await.unwrap().is_none());

        // « kill -9 » du runner 1 : personne n'envoie plus de heartbeat.
        clock.advance_ms(61_000);
        let t2 = q.claim("runner-2").await.unwrap().unwrap();
        assert_eq!(t2.id, t1.id);
        assert_eq!(t2.attempts, 2, "le compteur de tentatives doit progresser");
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
