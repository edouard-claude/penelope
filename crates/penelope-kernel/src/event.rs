//! Event log append-only à chaîne de hachage (§4.1).
//!
//! `hash = sha256(prev_hash || canonical_json(event))`. `penelope audit verify` rejoue la
//! chaîne entière et détecte toute altération, y compris une réécriture de `payload`.
//!
//! La purge RGPD (`penelope purge --session`) efface physiquement le contenu mais
//! conserve la ligne et le hash d'origine dans `event_purges`, afin que la chaîne reste
//! vérifiable après effacement.

use crate::canonical::{canonical_json, sha256_hex};
use crate::clock::SharedClock;
use crate::error::{KernelError, Result};
use penelope_store::Store;
use penelope_store::rusqlite::{self, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Événement tel qu'écrit dans le log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub id: i64,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub seq: i64,
    pub ts: String,
    pub kind: String,
    pub payload: Value,
    pub hash: String,
    pub prev_hash: String,
}

/// Événement à insérer (l'identifiant, le hash et l'horodatage sont attribués par le log).
#[derive(Debug, Clone)]
pub struct EventDraft {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub kind: String,
    pub payload: Value,
}

impl EventDraft {
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        EventDraft {
            session_id: None,
            run_id: None,
            kind: kind.into(),
            payload,
        }
    }
    pub fn session(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }
    pub fn run(mut self, id: impl Into<String>) -> Self {
        self.run_id = Some(id.into());
        self
    }
}

/// Corps hashé : identique à l'écriture et à la vérification.
fn digest_body(
    session_id: Option<&str>,
    run_id: Option<&str>,
    seq: i64,
    ts: &str,
    kind: &str,
    payload: &Value,
) -> Value {
    json!({
        "session_id": session_id,
        "run_id": run_id,
        "seq": seq,
        "ts": ts,
        "kind": kind,
        "payload": payload,
    })
}

pub fn compute_hash(
    prev_hash: &str,
    session_id: Option<&str>,
    run_id: Option<&str>,
    seq: i64,
    ts: &str,
    kind: &str,
    payload: &Value,
) -> String {
    let body = digest_body(session_id, run_id, seq, ts, kind, payload);
    let mut data = String::with_capacity(prev_hash.len() + 256);
    data.push_str(prev_hash);
    data.push_str(&canonical_json(&body));
    sha256_hex(data.as_bytes())
}

/// Même hash que [`compute_hash`], calculé à partir du **texte** canonique du payload tel
/// qu'il est stocké, sans le relire en JSON.
///
/// Relire puis re-sérialiser n'est pas neutre : l'analyseur de `serde_json` arrondit
/// certains flottants (`123456789.12345679` redevient `123456789.1234568`) et refuse un
/// entier de plus de 20 chiffres. La vérification signalait alors un « hash altéré » sur
/// un événement intact. Le corps canonique est donc reconstruit octet pour octet : clés
/// triées (`kind`, `payload`, `run_id`, `seq`, `session_id`, `ts`), payload inséré tel quel.
pub fn compute_hash_from_text(
    prev_hash: &str,
    session_id: Option<&str>,
    run_id: Option<&str>,
    seq: i64,
    ts: &str,
    kind: &str,
    payload_canonical: &str,
) -> String {
    let text = |v: Option<&str>| match v {
        Some(s) => canonical_json(&Value::String(s.to_string())),
        None => "null".to_string(),
    };
    let body = format!(
        "{{\"kind\":{},\"payload\":{payload_canonical},\"run_id\":{},\"seq\":{seq},\"session_id\":{},\"ts\":{}}}",
        text(Some(kind)),
        text(run_id),
        text(session_id),
        text(Some(ts)),
    );
    let mut data = String::with_capacity(prev_hash.len() + body.len());
    data.push_str(prev_hash);
    data.push_str(&body);
    sha256_hex(data.as_bytes())
}

#[derive(Clone)]
pub struct EventLog {
    store: Store,
    clock: SharedClock,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerifyReport {
    pub checked: u64,
    pub purged: u64,
    pub ok: bool,
    pub first_broken: Option<i64>,
    pub detail: Option<String>,
}

impl EventLog {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        EventLog { store, clock }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Ajoute un événement. Le calcul du hash et l'insertion sont dans la même
    /// transaction, sur l'unique thread écrivain : la chaîne ne peut pas diverger.
    pub async fn append(&self, draft: EventDraft) -> Result<Event> {
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                // Journal vide : GENESIS. Toute autre erreur de lecture remonte : un
                // maillon chaîné sur GENESIS au milieu du journal serait indiscernable
                // d'une altération, et la chaîne ne se répare pas (issue #47).
                let prev_hash: String = tx
                    .query_row(
                        "SELECT hash FROM events ORDER BY id DESC LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or_else(|| GENESIS.to_string());

                let seq: i64 = match draft.session_id.as_deref() {
                    Some(sid) => tx.query_row(
                        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1",
                        [sid],
                        |r| r.get(0),
                    )?,
                    None => tx.query_row(
                        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id IS NULL",
                        [],
                        |r| r.get(0),
                    )?,
                };

                let payload_s = canonical_json(&draft.payload);
                let hash = compute_hash(
                    &prev_hash,
                    draft.session_id.as_deref(),
                    draft.run_id.as_deref(),
                    seq,
                    &ts,
                    &draft.kind,
                    &draft.payload,
                );

                tx.execute(
                    "INSERT INTO events(session_id, run_id, seq, ts, kind, payload, hash, prev_hash)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        draft.session_id,
                        draft.run_id,
                        seq,
                        ts,
                        draft.kind,
                        payload_s,
                        hash,
                        prev_hash
                    ],
                )?;
                let id = tx.last_insert_rowid();

                Ok(Event {
                    id,
                    session_id: draft.session_id,
                    run_id: draft.run_id,
                    seq,
                    ts,
                    kind: draft.kind,
                    payload: draft.payload,
                    hash,
                    prev_hash,
                })
            })
            .await
            .map_err(KernelError::from)
    }

    /// Lit les événements d'une session à partir d'un numéro de séquence.
    pub async fn session_events(&self, session_id: &str, from_seq: i64) -> Result<Vec<Event>> {
        let sid = session_id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, session_id, run_id, seq, ts, kind, payload, hash, prev_hash
                     FROM events WHERE session_id = ?1 AND seq >= ?2 ORDER BY seq",
                )?;
                let rows = st.query_map(params![sid, from_seq], row_to_event)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await?)
    }

    /// Lit une tranche du log global (utilisé par `penelope tail` et le rejeu).
    pub async fn range(&self, after_id: i64, limit: i64) -> Result<Vec<Event>> {
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, session_id, run_id, seq, ts, kind, payload, hash, prev_hash
                     FROM events WHERE id > ?1 ORDER BY id LIMIT ?2",
                )?;
                let rows = st.query_map(params![after_id, limit], row_to_event)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await?)
    }

    /// Rejoue la chaîne entière (§4.1, CA 4).
    pub async fn verify(&self) -> Result<VerifyReport> {
        self.store
            .read(|c| {
                let mut purged_ids = std::collections::HashMap::new();
                {
                    let mut st = c.prepare("SELECT event_id, original_hash FROM event_purges")?;
                    let rows =
                        st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                    for r in rows {
                        let (k, v) = r?;
                        purged_ids.insert(k, v);
                    }
                }

                let mut st = c.prepare(
                    "SELECT id, session_id, run_id, seq, ts, kind, payload, hash, prev_hash
                     FROM events ORDER BY id",
                )?;
                let mut rows = st.query([])?;
                let mut prev = GENESIS.to_string();
                let mut report = VerifyReport {
                    ok: true,
                    ..Default::default()
                };

                while let Some(row) = rows.next()? {
                    let ev = row_to_event(row)?;
                    // Le texte stocké fait foi : il n'est jamais relu puis réécrit.
                    let payload_text: String = row.get(6)?;
                    report.checked += 1;

                    if ev.prev_hash != prev {
                        report.ok = false;
                        report.first_broken = Some(ev.id);
                        report.detail = Some(format!(
                            "chaînage rompu : prev_hash={} attendu {}",
                            ev.prev_hash, prev
                        ));
                        return Ok(report);
                    }

                    let expected = match purged_ids.get(&ev.id) {
                        Some(original) => {
                            report.purged += 1;
                            original.clone()
                        }
                        None => compute_hash_from_text(
                            &ev.prev_hash,
                            ev.session_id.as_deref(),
                            ev.run_id.as_deref(),
                            ev.seq,
                            &ev.ts,
                            &ev.kind,
                            &payload_text,
                        ),
                    };

                    if expected != ev.hash {
                        report.ok = false;
                        report.first_broken = Some(ev.id);
                        report.detail = Some(format!(
                            "hash altéré : stocké {} calculé {expected}",
                            ev.hash
                        ));
                        return Ok(report);
                    }
                    prev = ev.hash;
                }
                Ok(report)
            })
            .await
            .map_err(KernelError::from)
    }

    /// Purge RGPD : efface le contenu, conserve la ligne et le hash d'origine.
    pub async fn purge_session(&self, session_id: &str, reason: &str) -> Result<u64> {
        let sid = session_id.to_string();
        let reason = reason.to_string();
        let reason_for_write = reason.clone();
        let ts = self.clock.now_rfc3339();
        let n = self
            .store
            .write(move |tx| {
                let reason = reason_for_write;
                let ids: Vec<(i64, String)> = {
                    let mut st = tx.prepare("SELECT id, hash FROM events WHERE session_id = ?1")?;
                    let rows =
                        st.query_map([&sid], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                    let mut v = Vec::new();
                    for r in rows {
                        v.push(r?);
                    }
                    v
                };
                for (id, hash) in &ids {
                    tx.execute(
                        "INSERT OR IGNORE INTO event_purges(event_id, purged_at, original_hash, reason)
                         VALUES(?1,?2,?3,?4)",
                        params![id, ts, hash, reason],
                    )?;
                    tx.execute(
                        "UPDATE events SET payload = '{\"purged\":true}' WHERE id = ?1",
                        params![id],
                    )?;
                }
                Ok(ids.len() as u64)
            })
            .await?;
        self.append(EventDraft::new(
            "audit.purge",
            json!({"session_id": session_id, "events": n, "reason": reason}),
        ))
        .await?;
        Ok(n)
    }

    pub async fn count(&self) -> Result<i64> {
        Ok(self
            .store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?))
            .await?)
    }
}

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let payload_s: String = r.get(6)?;
    // Un payload illisible est dit, pas remplacé par `null` en silence (issue #47). Un
    // événement purgé garde son marqueur, qui est du JSON valide.
    let payload = serde_json::from_str(&payload_s)
        .unwrap_or_else(|e| json!({"payload_illisible": e.to_string(), "brut": payload_s}));
    Ok(Event {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        seq: r.get(3)?,
        ts: r.get(4)?,
        kind: r.get(5)?,
        payload,
        hash: r.get(7)?,
        prev_hash: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use std::sync::Arc;

    async fn log() -> EventLog {
        let store = Store::open_memory().unwrap();
        EventLog::new(store, Arc::new(TestClock::default()))
    }

    #[tokio::test]
    async fn chain_is_linked_and_verifies() {
        let l = log().await;
        let a = l
            .append(EventDraft::new("turn.started", json!({"n":1})).session("s1"))
            .await
            .unwrap();
        let b = l
            .append(EventDraft::new("turn.finished", json!({"n":2})).session("s1"))
            .await
            .unwrap();

        assert_eq!(a.prev_hash, GENESIS);
        assert_eq!(b.prev_hash, a.hash);
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);

        let r = l.verify().await.unwrap();
        assert!(r.ok, "{r:?}");
        assert_eq!(r.checked, 2);
    }

    /// #47 : une erreur de lecture pendant `append` fait échouer l'écriture. Un maillon
    /// chaîné sur GENESIS au milieu du journal serait indiscernable d'une altération.
    #[tokio::test]
    async fn a_read_error_fails_the_append_instead_of_forging_a_genesis_link() {
        let l = log().await;
        l.append(EventDraft::new("turn.started", json!({"n":1})).session("s1"))
            .await
            .unwrap();

        // La table disparaît sous les pieds de l'écriture : lecture en erreur.
        l.store()
            .write(|tx| {
                tx.execute_batch("ALTER TABLE events RENAME TO events_absent;")?;
                Ok(())
            })
            .await
            .unwrap();
        let e = l
            .append(EventDraft::new("turn.finished", json!({"n":2})).session("s1"))
            .await
            .unwrap_err();
        assert!(
            matches!(e, KernelError::Store(_)),
            "l'erreur doit remonter : {e}"
        );

        l.store()
            .write(|tx| {
                tx.execute_batch("ALTER TABLE events_absent RENAME TO events;")?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(l.count().await.unwrap(), 1, "rien n'a été écrit");
        assert!(l.verify().await.unwrap().ok, "la chaîne reste saine");
    }

    /// #47 : deux événements d'une session ne peuvent pas porter le même `seq`.
    #[tokio::test]
    async fn a_duplicate_seq_is_refused_by_the_database() {
        let l = log().await;
        l.append(EventDraft::new("turn.started", json!({})).session("s1"))
            .await
            .unwrap();
        let refused = l
            .store()
            .write(|tx| {
                tx.execute(
                    "INSERT INTO events(session_id, run_id, seq, ts, kind, payload, hash, prev_hash)
                     VALUES('s1',NULL,1,'2026-01-01T00:00:00Z','triche','{}','h','p')",
                    [],
                )?;
                Ok(())
            })
            .await;
        assert!(
            refused.is_err(),
            "un doublon (session, seq) doit être refusé"
        );
    }

    /// CA 4 : `penelope audit verify` détecte toute altération d'un événement.
    #[tokio::test]
    async fn ca_4_1_detects_tampering() {
        let l = log().await;
        for i in 0..5 {
            l.append(EventDraft::new("x", json!({ "i": i })).session("s1"))
                .await
                .unwrap();
        }
        assert!(l.verify().await.unwrap().ok);

        l.store()
            .write(|tx| {
                tx.execute("UPDATE events SET payload = '{\"i\":999}' WHERE id = 3", [])?;
                Ok(())
            })
            .await
            .unwrap();

        let r = l.verify().await.unwrap();
        assert!(!r.ok);
        assert_eq!(r.first_broken, Some(3));
    }

    #[tokio::test]
    async fn seq_is_per_session() {
        let l = log().await;
        l.append(EventDraft::new("a", json!({})).session("s1"))
            .await
            .unwrap();
        let b = l
            .append(EventDraft::new("a", json!({})).session("s2"))
            .await
            .unwrap();
        let c = l
            .append(EventDraft::new("a", json!({})).session("s1"))
            .await
            .unwrap();
        assert_eq!(b.seq, 1);
        assert_eq!(c.seq, 2);
    }

    #[tokio::test]
    async fn purge_erases_content_but_keeps_chain_verifiable() {
        let l = log().await;
        l.append(EventDraft::new("msg", json!({"texte":"secret personnel"})).session("s1"))
            .await
            .unwrap();
        l.append(EventDraft::new("msg", json!({"texte":"autre"})).session("s2"))
            .await
            .unwrap();

        let n = l.purge_session("s1", "rgpd").await.unwrap();
        assert_eq!(n, 1);

        let evs = l.session_events("s1", 0).await.unwrap();
        assert_eq!(evs[0].payload, json!({"purged": true}));

        let r = l.verify().await.unwrap();
        assert!(r.ok, "la chaîne doit rester vérifiable après purge : {r:?}");
        assert_eq!(r.purged, 1);
    }

    #[tokio::test]
    async fn concurrent_appends_keep_a_single_chain() {
        let l = log().await;
        let mut hs = Vec::new();
        for i in 0..40 {
            let l2 = l.clone();
            hs.push(tokio::spawn(async move {
                l2.append(EventDraft::new("x", json!({ "i": i })).session("s"))
                    .await
                    .unwrap()
            }));
        }
        for h in hs {
            h.await.unwrap();
        }
        let r = l.verify().await.unwrap();
        assert!(r.ok, "{r:?}");
        assert_eq!(r.checked, 40);
    }
}

#[cfg(test)]
mod float_payloads {
    use super::*;
    use crate::clock::TestClock;
    use serde_json::json;
    use std::sync::Arc;

    /// Des flottants que `serde_json` n'aurait pas relus à l'identique ne cassent plus la
    /// vérification de la chaîne (régression constatée en production).
    #[tokio::test]
    async fn events_with_any_float_verify_after_storage() {
        let payloads = [
            json!({"cost_usd": 0.000123, "x": 0.1 + 0.2}),
            json!({"f": 123_456_789.123_456_79, "neg": -0.0, "odd": 9_007_199_254_740_992.0}),
            json!({"near": 999999999999999.9, "tiny": 1e-300, "max": f64::MAX}),
            json!({"s": "émoji 🧠 «guillemets» \u{7f} \u{2028}", "u": u64::MAX, "i": i64::MIN}),
            json!({"arr": [1, 2.5, "x", null, {"b": 1, "a": 2}]}),
        ];
        let log = EventLog::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        );
        for (i, p) in payloads.iter().enumerate() {
            let draft = if i % 2 == 0 {
                EventDraft::new("probe", p.clone()).session("s1")
            } else {
                EventDraft::new("probe", p.clone()).run("r1")
            };
            let ev = log.append(draft).await.unwrap();
            assert_eq!(
                ev.hash,
                compute_hash_from_text(
                    &ev.prev_hash,
                    ev.session_id.as_deref(),
                    ev.run_id.as_deref(),
                    ev.seq,
                    &ev.ts,
                    &ev.kind,
                    &canonical_json(p),
                ),
                "le hash depuis le texte est celui de l'écriture"
            );
        }
        let report = log.verify().await.unwrap();
        assert!(report.ok, "{:?}", report.detail);
        assert_eq!(report.checked, payloads.len() as u64);
    }
}
