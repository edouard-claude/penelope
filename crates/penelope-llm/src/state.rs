//! Machine d'état des appels LLM (§4.3).
//!
//! Séquence : corps figé → plan d'envoi enregistré → `dispatching` (CAS) →
//! `response_started` à réception des en-têtes → `completed`.
//!
//! Une coupure **avant** les en-têtes donne `send_unknown` : pas de retry automatique
//! silencieux si le provider peut avoir facturé. La politique est configurable ; par
//! défaut, retry autorisé pour OpenRouter avec le même corps, et coût marqué
//! « possible doublon ».

use penelope_kernel::canonical::{canonical_json, sha256_hex};
use penelope_kernel::clock::SharedClock;
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmState {
    Planned,
    Dispatching,
    ResponseStarted,
    Completed,
    Failed,
    /// Coupure avant en-têtes : le provider a peut-être facturé.
    SendUnknown,
}

impl LlmState {
    pub fn as_str(&self) -> &'static str {
        match self {
            LlmState::Planned => "planned",
            LlmState::Dispatching => "dispatching",
            LlmState::ResponseStarted => "response_started",
            LlmState::Completed => "completed",
            LlmState::Failed => "failed",
            LlmState::SendUnknown => "send_unknown",
        }
    }
    pub fn parse(s: &str) -> Option<LlmState> {
        Some(match s {
            "planned" => LlmState::Planned,
            "dispatching" => LlmState::Dispatching,
            "response_started" => LlmState::ResponseStarted,
            "completed" => LlmState::Completed,
            "failed" => LlmState::Failed,
            "send_unknown" => LlmState::SendUnknown,
            _ => return None,
        })
    }
}

/// Politique de retry après `send_unknown` (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum UnknownSendPolicy {
    /// Retry autorisé avec le même corps ; le coût est marqué « possible doublon ».
    #[default]
    RetryMarkDuplicate,
    /// Aucun retry automatique : demande HITL.
    AskHuman,
}

/// Les trois clés qui rendent une requête retrouvable sans en recopier le corps
/// (issue #205) : le prompt système par son instantané, la liste d'outils et la chaîne
/// des messages par leur empreinte. Elles sont déjà calculées pour le cache de préfixe.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestKeys {
    pub system_hash: Option<String>,
    pub tools_hash: Option<String>,
    pub request_hash: Option<String>,
}

/// Ce qu'il faut savoir d'un appel pour enregistrer son plan d'envoi.
pub struct PlannedCall<'a> {
    pub id: &'a str,
    pub session_id: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub model: &'a str,
    pub provider: &'a str,
    pub keys: RequestKeys,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequestRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub model: String,
    pub provider: String,
    pub state: LlmState,
    pub body_hash: String,
    pub keys: RequestKeys,
    pub maybe_billed: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct LlmStateMachine {
    store: Store,
    clock: SharedClock,
    policy: UnknownSendPolicy,
}

impl LlmStateMachine {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        LlmStateMachine {
            store,
            clock,
            policy: UnknownSendPolicy::default(),
        }
    }

    pub fn with_policy(mut self, p: UnknownSendPolicy) -> Self {
        self.policy = p;
        self
    }

    pub fn policy(&self) -> UnknownSendPolicy {
        self.policy
    }

    /// Fige le corps et enregistre le plan d'envoi, avec les clés qui le rendront
    /// retrouvable (issue #205).
    pub async fn plan(
        &self,
        call: PlannedCall<'_>,
        body: &Value,
    ) -> penelope_store::Result<String> {
        let hash = sha256_hex(canonical_json(body).as_bytes());
        let (id, sid, rid, model, provider, ts) = (
            call.id.to_string(),
            call.session_id.map(String::from),
            call.run_id.map(String::from),
            call.model.to_string(),
            call.provider.to_string(),
            self.clock.now_rfc3339(),
        );
        let (h, keys) = (hash.clone(), call.keys);
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO llm_requests(id, session_id, run_id, model, provider, state,
                        body_hash, created_at, updated_at, system_hash, tools_hash, request_hash)
                     VALUES(?1,?2,?3,?4,?5,'planned',?6,?7,?7,?8,?9,?10)",
                    params![
                        id,
                        sid,
                        rid,
                        model,
                        provider,
                        h,
                        ts,
                        keys.system_hash,
                        keys.tools_hash,
                        keys.request_hash
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(hash)
    }

    /// `planned → dispatching`, en compare-and-swap.
    pub async fn dispatching(&self, id: &str) -> penelope_store::Result<bool> {
        self.cas(id, LlmState::Planned, LlmState::Dispatching).await
    }

    /// `dispatching → response_started` : les en-têtes sont arrivés, l'appel est parti.
    pub async fn response_started(&self, id: &str) -> penelope_store::Result<bool> {
        self.cas(id, LlmState::Dispatching, LlmState::ResponseStarted)
            .await
    }

    pub async fn completed(&self, id: &str) -> penelope_store::Result<bool> {
        let (id, ts) = (id.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE llm_requests SET state='completed', updated_at=?2 WHERE id=?1",
                    params![id, ts],
                )? > 0)
            })
            .await
    }

    pub async fn failed(
        &self,
        id: &str,
        error: &str,
        maybe_billed: bool,
    ) -> penelope_store::Result<()> {
        let (id, err, ts) = (id.to_string(), error.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE llm_requests SET state='failed', error=?2, maybe_billed=?3,
                     updated_at=?4 WHERE id=?1",
                    params![id, err, maybe_billed as i64, ts],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn cas(&self, id: &str, from: LlmState, to: LlmState) -> penelope_store::Result<bool> {
        let (id, f, t, ts) = (
            id.to_string(),
            from.as_str(),
            to.as_str(),
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE llm_requests SET state=?3, updated_at=?4 WHERE id=?1 AND state=?2",
                    params![id, f, t, ts],
                )? > 0)
            })
            .await
    }

    /// Au démarrage : les requêtes restées en `dispatching` deviennent `send_unknown`
    /// (coupure **avant** en-têtes) ; celles en `response_started` sont considérées comme
    /// parties et facturées, donc `failed` avec `maybe_billed`.
    pub async fn recover_on_boot(&self) -> penelope_store::Result<Vec<LlmRequestRecord>> {
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE llm_requests SET state='send_unknown', maybe_billed=1, updated_at=?1
                     WHERE state='dispatching'",
                    params![ts],
                )?;
                tx.execute(
                    "UPDATE llm_requests SET state='failed', maybe_billed=1,
                     error='interrompu après réception des en-têtes', updated_at=?1
                     WHERE state='response_started'",
                    params![ts],
                )?;
                let mut st = tx.prepare(
                    "SELECT id, session_id, run_id, model, provider, state, body_hash,
                            maybe_billed, error, system_hash, tools_hash, request_hash
                     FROM llm_requests WHERE state='send_unknown' ORDER BY updated_at",
                )?;
                let rows = st.query_map([], |r| {
                    let state: String = r.get(5)?;
                    Ok(LlmRequestRecord {
                        id: r.get(0)?,
                        session_id: r.get(1)?,
                        run_id: r.get(2)?,
                        model: r.get(3)?,
                        provider: r.get(4)?,
                        state: LlmState::parse(&state).unwrap_or(LlmState::SendUnknown),
                        body_hash: r.get(6)?,
                        maybe_billed: r.get::<_, i64>(7)? != 0,
                        error: r.get(8)?,
                        keys: RequestKeys {
                            system_hash: r.get(9)?,
                            tools_hash: r.get(10)?,
                            request_hash: r.get(11)?,
                        },
                    })
                })?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    /// Décide si une requête `send_unknown` peut être relancée automatiquement.
    pub fn may_retry(&self, provider: &str) -> bool {
        match self.policy {
            UnknownSendPolicy::AskHuman => false,
            // Le PRD autorise le retry pour OpenRouter, avec coût marqué « possible doublon ».
            UnknownSendPolicy::RetryMarkDuplicate => provider == "openrouter",
        }
    }

    pub async fn get(&self, id: &str) -> penelope_store::Result<Option<LlmRequestRecord>> {
        let id = id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, session_id, run_id, model, provider, state, body_hash,
                            maybe_billed, error, system_hash, tools_hash, request_hash
                     FROM llm_requests WHERE id=?1",
                )?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => {
                        let state: String = r.get(5)?;
                        Ok(Some(LlmRequestRecord {
                            id: r.get(0)?,
                            session_id: r.get(1)?,
                            run_id: r.get(2)?,
                            model: r.get(3)?,
                            provider: r.get(4)?,
                            state: LlmState::parse(&state).unwrap_or(LlmState::Failed),
                            body_hash: r.get(6)?,
                            maybe_billed: r.get::<_, i64>(7)? != 0,
                            error: r.get(8)?,
                            keys: RequestKeys {
                                system_hash: r.get(9)?,
                                tools_hash: r.get(10)?,
                                request_hash: r.get(11)?,
                            },
                        }))
                    }
                    None => Ok(None),
                }
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use serde_json::json;
    use std::sync::Arc;

    fn sm(store: Store) -> LlmStateMachine {
        LlmStateMachine::new(store, Arc::new(TestClock::default()))
    }

    fn planned<'a>(id: &'a str, provider: &'a str) -> PlannedCall<'a> {
        PlannedCall {
            id,
            session_id: None,
            run_id: None,
            model: "m",
            provider,
            keys: RequestKeys::default(),
        }
    }

    #[tokio::test]
    async fn nominal_sequence() {
        let m = sm(Store::open_memory().unwrap());
        m.plan(
            PlannedCall {
                session_id: Some("s1"),
                model: "a/b",
                ..planned("q1", "openrouter")
            },
            &json!({"x":1}),
        )
        .await
        .unwrap();
        assert!(m.dispatching("q1").await.unwrap());
        assert!(m.response_started("q1").await.unwrap());
        assert!(m.completed("q1").await.unwrap());
        assert_eq!(
            m.get("q1").await.unwrap().unwrap().state,
            LlmState::Completed
        );
    }

    /// #205 : la colonne `request` n'a jamais été écrite ; ce sont les trois clés déjà
    /// calculées qui rendent une requête retrouvable, et elles sont relues telles quelles.
    #[tokio::test]
    async fn the_three_keys_make_a_request_findable() {
        let m = sm(Store::open_memory().unwrap());
        let keys = RequestKeys {
            system_hash: Some("sys".into()),
            tools_hash: Some("tools".into()),
            request_hash: Some("req".into()),
        };
        m.plan(
            PlannedCall {
                id: "q1",
                session_id: Some("s1"),
                run_id: None,
                model: "a/b",
                provider: "openrouter",
                keys: keys.clone(),
            },
            &json!({"x":1}),
        )
        .await
        .unwrap();
        assert_eq!(m.get("q1").await.unwrap().unwrap().keys, keys);
    }

    #[tokio::test]
    async fn body_hash_is_order_independent() {
        let m = sm(Store::open_memory().unwrap());
        let h1 = m
            .plan(planned("a", "p"), &json!({"x":1,"y":2}))
            .await
            .unwrap();
        let h2 = m
            .plan(planned("b", "p"), &json!({"y":2,"x":1}))
            .await
            .unwrap();
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn cas_refuses_out_of_order_transitions() {
        let m = sm(Store::open_memory().unwrap());
        m.plan(planned("q", "p"), &json!({})).await.unwrap();
        assert!(
            !m.response_started("q").await.unwrap(),
            "on ne passe pas de planned à response_started"
        );
        assert!(m.dispatching("q").await.unwrap());
        assert!(!m.dispatching("q").await.unwrap(), "un seul envoi");
    }

    #[tokio::test]
    async fn crash_before_headers_gives_send_unknown() {
        let store = Store::open_memory().unwrap();
        let m = sm(store.clone());
        m.plan(planned("q", "openrouter"), &json!({}))
            .await
            .unwrap();
        m.dispatching("q").await.unwrap();

        // « kill -9 » puis redémarrage.
        let m2 = sm(store);
        let pending = m2.recover_on_boot().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, LlmState::SendUnknown);
        assert!(pending[0].maybe_billed, "le provider a pu facturer");
    }

    #[tokio::test]
    async fn crash_after_headers_is_a_failure_not_an_unknown() {
        let store = Store::open_memory().unwrap();
        let m = sm(store.clone());
        m.plan(planned("q", "openrouter"), &json!({}))
            .await
            .unwrap();
        m.dispatching("q").await.unwrap();
        m.response_started("q").await.unwrap();

        let m2 = sm(store);
        let pending = m2.recover_on_boot().await.unwrap();
        assert!(
            pending.is_empty(),
            "rien à décider : l'appel était bien parti"
        );
        let r = m2.get("q").await.unwrap().unwrap();
        assert_eq!(r.state, LlmState::Failed);
        assert!(r.maybe_billed);
    }

    #[test]
    fn retry_policy_follows_the_provider() {
        let store = Store::open_memory().unwrap();
        let m = sm(store.clone());
        assert!(m.may_retry("openrouter"));
        assert!(
            !m.may_retry("openai_compat"),
            "un endpoint quelconque n'est pas réputé sûr pour un retry silencieux"
        );
        let strict = sm(store).with_policy(UnknownSendPolicy::AskHuman);
        assert!(!strict.may_retry("openrouter"));
    }
}
