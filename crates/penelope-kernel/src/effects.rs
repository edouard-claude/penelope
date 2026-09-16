//! Ledger d'effets de bord et idempotence (§4.2).
//!
//! Tout effet non `readOnly` est **enregistré avant** d'être exécuté. Au redémarrage :
//! - `planned` : ré-exécutable tel quel ;
//! - `dispatching` : devient `unknown`, **aucun retry automatique**, une demande HITL
//!   `effect_unknown` est émise (sauf outil déclaré idempotent) ;
//! - `completed` : rejoué depuis le ledger, jamais ré-exécuté.

use crate::canonical::{canonical_json, sha256_hex};
use crate::clock::SharedClock;
use crate::error::{KernelError, Result};
use crate::ids::EffectId;
use penelope_store::Store;
use penelope_store::rusqlite::{self, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Planned,
    Dispatching,
    Completed,
    Failed,
    Unknown,
}

impl EffectState {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffectState::Planned => "planned",
            EffectState::Dispatching => "dispatching",
            EffectState::Completed => "completed",
            EffectState::Failed => "failed",
            EffectState::Unknown => "unknown",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "planned" => EffectState::Planned,
            "dispatching" => EffectState::Dispatching,
            "completed" => EffectState::Completed,
            "failed" => EffectState::Failed,
            "unknown" => EffectState::Unknown,
            _ => return None,
        })
    }
}

impl fmt::Display for EffectState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Nature de l'effet, utilisée pour le routage HITL et l'affichage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Tool,
    Mcp,
    Shell,
    Telegram,
    Fs,
    Git,
    Http,
}

impl EffectKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffectKind::Tool => "tool",
            EffectKind::Mcp => "mcp",
            EffectKind::Shell => "shell",
            EffectKind::Telegram => "telegram",
            EffectKind::Fs => "fs",
            EffectKind::Git => "git",
            EffectKind::Http => "http",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub id: EffectId,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub step_id: Option<String>,
    pub idem_key: String,
    pub kind: EffectKind,
    pub tool: Option<String>,
    pub request: Value,
    pub state: EffectState,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub attempts: i64,
    pub idempotent: bool,
}

/// Description d'un effet à planifier.
#[derive(Debug, Clone)]
pub struct EffectSpec {
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub step_id: Option<String>,
    pub kind: EffectKind,
    pub tool: Option<String>,
    pub request: Value,
    /// Tentative logique : incrémenter force une nouvelle clé d'idempotence, donc un
    /// nouvel effet (retry volontaire décidé par le harnais ou par l'humain).
    pub attempt: u32,
    /// L'outil est annoncé idempotent : un `unknown` peut être relancé sans HITL.
    pub idempotent: bool,
}

impl EffectSpec {
    pub fn new(kind: EffectKind, tool: impl Into<String>, request: Value) -> Self {
        EffectSpec {
            run_id: None,
            session_id: None,
            step_id: None,
            kind,
            tool: Some(tool.into()),
            request,
            attempt: 0,
            idempotent: false,
        }
    }
    pub fn run(mut self, id: impl Into<String>) -> Self {
        self.run_id = Some(id.into());
        self
    }
    pub fn session(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }
    pub fn step(mut self, id: impl Into<String>) -> Self {
        self.step_id = Some(id.into());
        self
    }
    pub fn attempt(mut self, n: u32) -> Self {
        self.attempt = n;
        self
    }
    pub fn idempotent(mut self, yes: bool) -> Self {
        self.idempotent = yes;
        self
    }

    /// `idem_key = hash(run_id, step_id, tool, args canoniques, tentative logique)`.
    pub fn idem_key(&self) -> String {
        let body = serde_json::json!({
            "run": self.run_id,
            "session": self.session_id,
            "step": self.step_id,
            "tool": self.tool,
            "kind": self.kind.as_str(),
            "args": self.request,
            "attempt": self.attempt,
        });
        sha256_hex(canonical_json(&body).as_bytes())
    }
}

/// Résultat d'une planification.
#[derive(Debug, Clone)]
pub enum Planned {
    /// Effet nouveau : à exécuter.
    Fresh(EffectId),
    /// Effet déjà terminé dans une vie antérieure du processus : rejoué, pas ré-exécuté.
    Replayed(Value),
    /// Effet retrouvé en `dispatching` puis passé en `unknown` : exige une décision
    /// humaine avant toute relance (§4.2).
    NeedsDecision(EffectId),
    /// Effet déjà en cours dans ce processus (double planification).
    InFlight(EffectId),
}

#[derive(Clone)]
pub struct EffectLedger {
    store: Store,
    clock: SharedClock,
}

impl EffectLedger {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        EffectLedger { store, clock }
    }

    /// Enregistre l'intention d'exécuter un effet, **avant** exécution.
    pub async fn plan(&self, spec: EffectSpec) -> Result<Planned> {
        let key = spec.idem_key();
        let ts = self.clock.now_rfc3339();
        let id = EffectId::new();

        self.store
            .write(move |tx| {
                let existing: Option<(String, String, Option<String>, i64)> = tx
                    .query_row(
                        "SELECT id, state, result, attempts FROM effects WHERE idem_key = ?1",
                        [&key],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .ok();

                if let Some((eid, state, result, _attempts)) = existing {
                    let state = EffectState::parse(&state).unwrap_or(EffectState::Unknown);
                    return Ok(match state {
                        EffectState::Completed => {
                            let v = result
                                .as_deref()
                                .and_then(|s| serde_json::from_str(s).ok())
                                .unwrap_or(Value::Null);
                            Planned::Replayed(v)
                        }
                        EffectState::Unknown => Planned::NeedsDecision(EffectId(eid)),
                        EffectState::Dispatching => Planned::InFlight(EffectId(eid)),
                        EffectState::Planned | EffectState::Failed => {
                            tx.execute(
                                "UPDATE effects SET state='planned', updated_at=?2 WHERE id=?1",
                                params![eid, ts],
                            )?;
                            Planned::Fresh(EffectId(eid))
                        }
                    });
                }

                tx.execute(
                    "INSERT INTO effects(id, run_id, session_id, step_id, idem_key, kind, tool,
                        request, state, attempts, idempotent, created_at, updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'planned',0,?9,?10,?10)",
                    params![
                        id.as_str(),
                        spec.run_id,
                        spec.session_id,
                        spec.step_id,
                        key,
                        spec.kind.as_str(),
                        spec.tool,
                        canonical_json(&spec.request),
                        spec.idempotent as i64,
                        ts
                    ],
                )?;
                Ok(Planned::Fresh(id))
            })
            .await
            .map_err(KernelError::from)
    }

    /// Transition `planned → dispatching` en compare-and-swap.
    pub async fn dispatching(&self, id: &EffectId) -> Result<()> {
        let id = id.0.clone();
        let ts = self.clock.now_rfc3339();
        let n = self
            .store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE effects SET state='dispatching', attempts = attempts + 1, updated_at=?2
                     WHERE id=?1 AND state='planned'",
                    params![id, ts],
                )?)
            })
            .await?;
        if n == 0 {
            return Err(KernelError::Conflict(
                "l'effet n'est plus dans l'état 'planned'".into(),
            ));
        }
        Ok(())
    }

    pub async fn complete(&self, id: &EffectId, result: Value) -> Result<()> {
        let id = id.0.clone();
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE effects SET state='completed', result=?2, error=NULL, updated_at=?3
                     WHERE id=?1",
                    params![id, canonical_json(&result), ts],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn fail(&self, id: &EffectId, error: impl Into<String>) -> Result<()> {
        let id = id.0.clone();
        let err = error.into();
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE effects SET state='failed', error=?2, updated_at=?3 WHERE id=?1",
                    params![id, err, ts],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Au démarrage : tout `dispatching` devient `unknown`. Renvoie les effets qui
    /// exigent une décision humaine (les idempotents sont directement replanifiés).
    pub async fn recover_on_boot(&self) -> Result<Vec<Effect>> {
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE effects SET state='planned', updated_at=?1
                     WHERE state='dispatching' AND idempotent=1",
                    params![ts],
                )?;
                tx.execute(
                    "UPDATE effects SET state='unknown', updated_at=?1
                     WHERE state='dispatching' AND idempotent=0",
                    params![ts],
                )?;
                let mut st = tx.prepare(
                    "SELECT id, run_id, session_id, step_id, idem_key, kind, tool, request,
                            state, result, error, attempts, idempotent
                     FROM effects WHERE state='unknown' ORDER BY updated_at",
                )?;
                let rows = st.query_map([], row_to_effect)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
            .map_err(KernelError::from)
    }

    /// Décision humaine sur un effet `unknown`.
    pub async fn resolve_unknown(&self, id: &EffectId, decision: UnknownDecision) -> Result<()> {
        let id_s = id.0.clone();
        let ts = self.clock.now_rfc3339();
        match decision {
            UnknownDecision::Retry => {
                self.store
                    .write(move |tx| {
                        tx.execute(
                            "UPDATE effects SET state='planned', updated_at=?2
                             WHERE id=?1 AND state='unknown'",
                            params![id_s, ts],
                        )?;
                        Ok(())
                    })
                    .await?;
            }
            UnknownDecision::Ignore => {
                self.store
                    .write(move |tx| {
                        tx.execute(
                            "UPDATE effects SET state='failed', error='ignoré par le propriétaire',
                             updated_at=?2 WHERE id=?1 AND state='unknown'",
                            params![id_s, ts],
                        )?;
                        Ok(())
                    })
                    .await?;
            }
            UnknownDecision::MarkCompleted(v) => {
                self.complete(id, v).await?;
            }
        }
        Ok(())
    }

    pub async fn get(&self, id: &EffectId) -> Result<Option<Effect>> {
        let id = id.0.clone();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, run_id, session_id, step_id, idem_key, kind, tool, request,
                            state, result, error, attempts, idempotent
                     FROM effects WHERE id = ?1",
                )?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_effect(r)?)),
                    None => Ok(None),
                }
            })
            .await?)
    }

    pub async fn count_by_state(&self, state: EffectState) -> Result<i64> {
        let s = state.as_str();
        Ok(self
            .store
            .read(move |c| {
                Ok(
                    c.query_row("SELECT count(*) FROM effects WHERE state = ?1", [s], |r| {
                        r.get(0)
                    })?,
                )
            })
            .await?)
    }
}

#[derive(Debug, Clone)]
pub enum UnknownDecision {
    /// « Relancer » : l'humain a vérifié que l'effet n'a pas eu lieu.
    Retry,
    /// « Ignorer » : on n'en parle plus, l'étape échoue.
    Ignore,
    /// « Vérifier » a montré que l'effet a bien eu lieu : on enregistre son résultat.
    MarkCompleted(Value),
}

fn row_to_effect(r: &rusqlite::Row<'_>) -> rusqlite::Result<Effect> {
    let kind: String = r.get(5)?;
    let request: String = r.get(7)?;
    let state: String = r.get(8)?;
    let result: Option<String> = r.get(9)?;
    Ok(Effect {
        id: EffectId(r.get(0)?),
        run_id: r.get(1)?,
        session_id: r.get(2)?,
        step_id: r.get(3)?,
        idem_key: r.get(4)?,
        kind: match kind.as_str() {
            "mcp" => EffectKind::Mcp,
            "shell" => EffectKind::Shell,
            "telegram" => EffectKind::Telegram,
            "fs" => EffectKind::Fs,
            "git" => EffectKind::Git,
            "http" => EffectKind::Http,
            _ => EffectKind::Tool,
        },
        tool: r.get(6)?,
        request: serde_json::from_str(&request).unwrap_or(Value::Null),
        state: EffectState::parse(&state).unwrap_or(EffectState::Unknown),
        result: result.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
        error: r.get(10)?,
        attempts: r.get(11)?,
        idempotent: r.get::<_, i64>(12)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use serde_json::json;
    use std::sync::Arc;

    fn ledger(store: Store) -> EffectLedger {
        EffectLedger::new(store, Arc::new(TestClock::default()))
    }

    fn spec() -> EffectSpec {
        EffectSpec::new(
            EffectKind::Mcp,
            "mcp__forge__create_pr",
            json!({"title":"fix"}),
        )
        .run("r1")
        .step("open_pr")
    }

    #[test]
    fn idem_key_is_argument_order_independent() {
        let a = EffectSpec::new(EffectKind::Tool, "t", json!({"a":1,"b":2}));
        let b = EffectSpec::new(EffectKind::Tool, "t", json!({"b":2,"a":1}));
        assert_eq!(a.idem_key(), b.idem_key());
        let c = EffectSpec::new(EffectKind::Tool, "t", json!({"a":1,"b":3}));
        assert_ne!(a.idem_key(), c.idem_key());
    }

    #[test]
    fn logical_attempt_changes_the_key() {
        let a = spec();
        let b = spec().attempt(1);
        assert_ne!(a.idem_key(), b.idem_key());
    }

    #[tokio::test]
    async fn completed_effect_is_replayed_not_reexecuted() {
        let l = ledger(Store::open_memory().unwrap());
        let id = match l.plan(spec()).await.unwrap() {
            Planned::Fresh(id) => id,
            other => panic!("attendu Fresh, obtenu {other:?}"),
        };
        l.dispatching(&id).await.unwrap();
        l.complete(&id, json!({"pr": 42})).await.unwrap();

        match l.plan(spec()).await.unwrap() {
            Planned::Replayed(v) => assert_eq!(v, json!({"pr": 42})),
            other => panic!("attendu Replayed, obtenu {other:?}"),
        }
    }

    /// CA 4 : un `kill -9` pendant un effet `dispatching` produit une demande HITL et
    /// aucune ré-exécution.
    #[tokio::test]
    async fn ca_4_2_dispatching_becomes_unknown_without_retry() {
        let store = Store::open_memory().unwrap();
        let l = ledger(store.clone());
        let id = match l.plan(spec()).await.unwrap() {
            Planned::Fresh(id) => id,
            o => panic!("{o:?}"),
        };
        l.dispatching(&id).await.unwrap();
        // « kill -9 » : on repart d'un ledger neuf sur la même base.
        let l2 = ledger(store);
        let pending = l2.recover_on_boot().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, EffectState::Unknown);

        // Une replanification ne relance PAS : elle exige une décision.
        match l2.plan(spec()).await.unwrap() {
            Planned::NeedsDecision(eid) => assert_eq!(eid, id),
            o => panic!("attendu NeedsDecision, obtenu {o:?}"),
        }
    }

    #[tokio::test]
    async fn idempotent_effects_are_replanned_without_hitl() {
        let store = Store::open_memory().unwrap();
        let l = ledger(store.clone());
        let s = spec().idempotent(true);
        let id = match l.plan(s.clone()).await.unwrap() {
            Planned::Fresh(id) => id,
            o => panic!("{o:?}"),
        };
        l.dispatching(&id).await.unwrap();

        let l2 = ledger(store);
        let pending = l2.recover_on_boot().await.unwrap();
        assert!(pending.is_empty(), "un effet idempotent ne demande rien");
        match l2.plan(s).await.unwrap() {
            Planned::Fresh(_) => {}
            o => panic!("attendu Fresh, obtenu {o:?}"),
        }
    }

    #[tokio::test]
    async fn dispatching_twice_is_a_conflict() {
        let l = ledger(Store::open_memory().unwrap());
        let id = match l.plan(spec()).await.unwrap() {
            Planned::Fresh(id) => id,
            o => panic!("{o:?}"),
        };
        l.dispatching(&id).await.unwrap();
        assert!(l.dispatching(&id).await.is_err());
    }

    #[tokio::test]
    async fn unknown_resolution_paths() {
        let store = Store::open_memory().unwrap();
        let l = ledger(store.clone());
        let id = match l.plan(spec()).await.unwrap() {
            Planned::Fresh(id) => id,
            o => panic!("{o:?}"),
        };
        l.dispatching(&id).await.unwrap();
        ledger(store.clone()).recover_on_boot().await.unwrap();

        l.resolve_unknown(&id, UnknownDecision::MarkCompleted(json!({"pr": 7})))
            .await
            .unwrap();
        match l.plan(spec()).await.unwrap() {
            Planned::Replayed(v) => assert_eq!(v, json!({"pr": 7})),
            o => panic!("{o:?}"),
        }
    }
}
