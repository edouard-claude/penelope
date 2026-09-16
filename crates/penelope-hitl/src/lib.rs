//! `penelope-hitl` : politiques, demandes d'approbation, audit (§9).
//!
//! Invariants :
//! - une demande en attente **suspend uniquement le run concerné** ;
//! - la première décision gagne (compare-and-swap), l'autre canal est mis à jour ;
//! - toute autorisation « toujours » crée une règle **visible et révocable**.

#![forbid(unsafe_code)]

pub mod policy;

pub use policy::{PolicyEngine, PolicyRule, RuleScope};

use penelope_kernel::clock::SharedClock;
use penelope_kernel::ids::ApprovalId;
use penelope_kernel::risk::{PolicyWindow, RiskClass};
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Types de demandes (§9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    ToolCall,
    McpSampling,
    McpElicitation,
    WorkflowGate,
    PlanProposal,
    EffectUnknown,
    SkillProposal,
    MemoryProposal,
    ConfigChange,
    BudgetExceeded,
    McpAdmin,
}

impl ApprovalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalKind::ToolCall => "tool_call",
            ApprovalKind::McpSampling => "mcp_sampling",
            ApprovalKind::McpElicitation => "mcp_elicitation",
            ApprovalKind::WorkflowGate => "workflow_gate",
            ApprovalKind::PlanProposal => "plan_proposal",
            ApprovalKind::EffectUnknown => "effect_unknown",
            ApprovalKind::SkillProposal => "skill_proposal",
            ApprovalKind::MemoryProposal => "memory_proposal",
            ApprovalKind::ConfigChange => "config_change",
            ApprovalKind::BudgetExceeded => "budget_exceeded",
            ApprovalKind::McpAdmin => "mcp_admin",
        }
    }
    pub fn parse(s: &str) -> Option<ApprovalKind> {
        Some(match s {
            "tool_call" => ApprovalKind::ToolCall,
            "mcp_sampling" => ApprovalKind::McpSampling,
            "mcp_elicitation" => ApprovalKind::McpElicitation,
            "workflow_gate" => ApprovalKind::WorkflowGate,
            "plan_proposal" => ApprovalKind::PlanProposal,
            "effect_unknown" => ApprovalKind::EffectUnknown,
            "skill_proposal" => ApprovalKind::SkillProposal,
            "memory_proposal" => ApprovalKind::MemoryProposal,
            "config_change" => ApprovalKind::ConfigChange,
            "budget_exceeded" => ApprovalKind::BudgetExceeded,
            "mcp_admin" => ApprovalKind::McpAdmin,
            _ => return None,
        })
    }

    /// Template Telegram associé (§14.5).
    pub fn template(&self) -> &'static str {
        match self {
            ApprovalKind::ToolCall => "tool_approval",
            ApprovalKind::McpSampling => "sampling_request",
            ApprovalKind::McpElicitation => "form",
            ApprovalKind::WorkflowGate => "workflow_preview",
            ApprovalKind::PlanProposal => "plan_proposal",
            ApprovalKind::EffectUnknown => "effect_unknown",
            ApprovalKind::SkillProposal => "skill_proposal",
            ApprovalKind::MemoryProposal => "memory_proposal",
            ApprovalKind::ConfigChange => "tool_approval",
            ApprovalKind::BudgetExceeded => "budget_alert",
            ApprovalKind::McpAdmin => "tool_approval",
        }
    }

    /// Une demande urgente n'est jamais reportée par le mode silencieux (§9.2).
    pub fn is_urgent(&self) -> bool {
        matches!(
            self,
            ApprovalKind::EffectUnknown | ApprovalKind::BudgetExceeded
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Expired,
    Cancelled,
}

impl ApprovalState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalState::Pending => "pending",
            ApprovalState::Approved => "approved",
            ApprovalState::Denied => "denied",
            ApprovalState::Expired => "expired",
            ApprovalState::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<ApprovalState> {
        Some(match s {
            "pending" => ApprovalState::Pending,
            "approved" => ApprovalState::Approved,
            "denied" => ApprovalState::Denied,
            "expired" => ApprovalState::Expired,
            "cancelled" => ApprovalState::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub kind: ApprovalKind,
    pub subject: String,
    pub risk: RiskClass,
    pub payload: Value,
    pub choices: Vec<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub state: ApprovalState,
    pub decision: Option<String>,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
    pub decided_via: Option<String>,
    pub decided_at: Option<String>,
    pub rule_created: Option<String>,
}

/// Décision prise par le propriétaire.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub approved: bool,
    pub choice: String,
    pub window: PolicyWindow,
    pub reason: Option<String>,
    /// `telegram` ou `cli`.
    pub via: String,
}

impl Decision {
    pub fn approve_once(via: &str) -> Self {
        Decision {
            approved: true,
            choice: "Autoriser".into(),
            window: PolicyWindow::Once,
            reason: None,
            via: via.into(),
        }
    }
    pub fn approve_always(via: &str) -> Self {
        Decision {
            approved: true,
            choice: "Toujours".into(),
            window: PolicyWindow::Always,
            reason: None,
            via: via.into(),
        }
    }
    pub fn deny(via: &str, reason: Option<String>) -> Self {
        Decision {
            approved: false,
            choice: "Refuser".into(),
            window: PolicyWindow::Once,
            reason,
            via: via.into(),
        }
    }
}

/// Erreur de décision.
#[derive(Debug, thiserror::Error)]
pub enum HitlError {
    #[error("stockage : {0}")]
    Store(#[from] penelope_store::StoreError),
    #[error("demande {0} introuvable")]
    NotFound(String),
    #[error("demande {id} déjà tranchée : {state} par {by} à {at}")]
    AlreadyDecided {
        id: String,
        state: String,
        by: String,
        at: String,
    },
}

pub type Result<T, E = HitlError> = std::result::Result<T, E>;

#[derive(Clone)]
pub struct ApprovalStore {
    store: Store,
    clock: SharedClock,
    default_ttl_ms: i64,
}

impl ApprovalStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        ApprovalStore {
            store,
            clock,
            default_ttl_ms: 24 * 3_600_000,
        }
    }

    pub fn with_ttl_ms(mut self, ms: i64) -> Self {
        self.default_ttl_ms = ms;
        self
    }

    // Chaque paramètre est une colonne du ledger : les regrouper dans une structure
    // ne ferait que déplacer la liste.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        kind: ApprovalKind,
        subject: &str,
        risk: RiskClass,
        payload: Value,
        choices: Vec<String>,
        session_id: Option<&str>,
        run_id: Option<&str>,
        quiet: bool,
    ) -> Result<ApprovalRequest> {
        let now_ms = self.clock.now_ms();
        let req = ApprovalRequest {
            id: ApprovalId::new(),
            kind,
            subject: subject.to_string(),
            risk,
            payload,
            choices,
            session_id: session_id.map(String::from),
            run_id: run_id.map(String::from),
            created_at: self.clock.now_rfc3339(),
            expires_at: ms_to_rfc3339(now_ms + self.default_ttl_ms),
            state: ApprovalState::Pending,
            decision: None,
            reason: None,
            decided_by: None,
            decided_via: None,
            decided_at: None,
            rule_created: None,
        };
        let row = req.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO approval_requests(id, kind, subject, risk, payload, choices,
                        session_id, run_id, created_at, expires_at, state, quiet)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',?11)",
                    params![
                        row.id.as_str(),
                        row.kind.as_str(),
                        row.subject,
                        row.risk.as_str(),
                        serde_json::to_string(&row.payload).unwrap_or_default(),
                        serde_json::to_string(&row.choices).unwrap_or_default(),
                        row.session_id,
                        row.run_id,
                        row.created_at,
                        row.expires_at,
                        quiet as i64
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(req)
    }

    /// Tranche une demande. **La première décision gagne** : un second appel échoue avec
    /// le détail de la décision déjà enregistrée.
    pub async fn decide(&self, id: &str, d: &Decision) -> Result<ApprovalRequest> {
        let (id_s, now) = (id.to_string(), self.clock.now_rfc3339());
        let state = if d.approved {
            ApprovalState::Approved
        } else {
            ApprovalState::Denied
        };
        let (choice, reason, via, window) = (
            d.choice.clone(),
            d.reason.clone(),
            d.via.clone(),
            d.window.as_str().to_string(),
        );

        let updated = self
            .store
            .write(move |tx| {
                let n = tx.execute(
                    "UPDATE approval_requests
                     SET state=?2, decision=?3, reason=?4, decided_by='owner', decided_via=?5,
                         decided_at=?6, rule_created=?7
                     WHERE id=?1 AND state='pending'",
                    params![
                        id_s,
                        state.as_str(),
                        choice,
                        reason,
                        via,
                        now,
                        (window == "always").then_some(window.clone())
                    ],
                )?;
                Ok(n > 0)
            })
            .await?;

        let req = self
            .get(id)
            .await?
            .ok_or_else(|| HitlError::NotFound(id.to_string()))?;
        if !updated {
            return Err(HitlError::AlreadyDecided {
                id: id.to_string(),
                state: req.state.as_str().to_string(),
                by: req.decided_via.clone().unwrap_or_else(|| "?".into()),
                at: req.decided_at.clone().unwrap_or_else(|| "?".into()),
            });
        }
        Ok(req)
    }

    pub async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        let id = id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(&format!("{SELECT} WHERE id = ?1"))?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_request(r)?)),
                    None => Ok(None),
                }
            })
            .await?)
    }

    /// Dernière demande liée à un appel d'outil précis d'une session.
    ///
    /// C'est ce qui permet de **reprendre** un tour suspendu : l'appel reste dans
    /// l'historique, et la décision est retrouvée par l'identifiant de l'appel.
    pub async fn find_for_call(
        &self,
        session_id: &str,
        call_id: &str,
    ) -> Result<Option<ApprovalRequest>> {
        let (sid, cid) = (session_id.to_string(), call_id.to_string());
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE session_id = ?1 AND json_extract(payload, '$.call_id') = ?2
                     ORDER BY created_at DESC LIMIT 1"
                ))?;
                let mut rows = st.query(params![sid, cid])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_request(r)?)),
                    None => Ok(None),
                }
            })
            .await?)
    }

    pub async fn pending(&self, limit: i64) -> Result<Vec<ApprovalRequest>> {
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE state='pending' ORDER BY created_at LIMIT ?1"
                ))?;
                let rows = st.query_map([limit], row_to_request)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?)
    }

    /// Expire les demandes dépassées. Le run correspondant passe en `blocked`, avec
    /// reprise possible par `/resume` (§9.2).
    pub async fn expire_due(&self) -> Result<Vec<ApprovalRequest>> {
        let now = self.clock.now_rfc3339();
        Ok(self
            .store
            .write(move |tx| {
                let mut st = tx.prepare(&format!(
                    "{SELECT} WHERE state='pending' AND expires_at <= ?1"
                ))?;
                let rows = st.query_map([&now], row_to_request)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                drop(st);
                for r in &v {
                    tx.execute(
                        "UPDATE approval_requests SET state='expired', decided_at=?2 WHERE id=?1",
                        params![r.id.as_str(), now],
                    )?;
                }
                Ok(v)
            })
            .await?)
    }

    /// Demandes à relancer (T+1 h puis T+6 h, §9.2).
    pub async fn due_reminders(&self) -> Result<Vec<(ApprovalRequest, u8)>> {
        let now_ms = self.clock.now_ms();
        let now = self.clock.now_rfc3339();
        Ok(self
            .store
            .write(move |tx| {
                let mut st = tx.prepare(
                    "SELECT id, kind, subject, risk, payload, choices, session_id, run_id,
                            created_at, expires_at, state, decision, reason, decided_by,
                            decided_via, decided_at, rule_created, reminded_at
                     FROM approval_requests WHERE state='pending'",
                )?;
                let rows = st.query_map([], |r| {
                    let req = row_to_request(r)?;
                    let reminded: Option<String> = r.get(17)?;
                    Ok((req, reminded))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    let (req, reminded) = r?;
                    let created = rfc3339_to_ms(&req.created_at);
                    let age = now_ms - created;
                    let stage = if age >= 6 * 3_600_000 {
                        2u8
                    } else if age >= 3_600_000 {
                        1
                    } else {
                        0
                    };
                    if stage == 0 {
                        continue;
                    }
                    let already = reminded
                        .as_deref()
                        .map(|s| {
                            let last = rfc3339_to_ms(s);
                            if stage == 1 {
                                last >= created + 3_600_000
                            } else {
                                last >= created + 6 * 3_600_000
                            }
                        })
                        .unwrap_or(false);
                    if !already {
                        out.push((req, stage));
                    }
                }
                drop(st);
                for (r, _) in &out {
                    tx.execute(
                        "UPDATE approval_requests SET reminded_at=?2 WHERE id=?1",
                        params![r.id.as_str(), now],
                    )?;
                }
                Ok(out)
            })
            .await?)
    }

    pub async fn count_pending(&self) -> Result<i64> {
        Ok(self
            .store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM approval_requests WHERE state='pending'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await?)
    }

    /// Demandes groupées pour le digest du matin (mode silencieux, §9.2).
    pub async fn quiet_backlog(&self) -> Result<Vec<ApprovalRequest>> {
        Ok(self
            .store
            .read(|c| {
                let mut st = c.prepare(&format!(
                    "{SELECT} WHERE state='pending' AND quiet=1 ORDER BY created_at"
                ))?;
                let rows = st.query_map([], row_to_request)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await?)
    }
}

const SELECT: &str = "SELECT id, kind, subject, risk, payload, choices, session_id, run_id,
     created_at, expires_at, state, decision, reason, decided_by, decided_via, decided_at,
     rule_created FROM approval_requests";

fn row_to_request(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<ApprovalRequest> {
    let kind: String = r.get(1)?;
    let risk: String = r.get(3)?;
    let payload: String = r.get(4)?;
    let choices: String = r.get(5)?;
    let state: String = r.get(10)?;
    Ok(ApprovalRequest {
        id: ApprovalId(r.get(0)?),
        kind: ApprovalKind::parse(&kind).unwrap_or(ApprovalKind::ToolCall),
        subject: r.get(2)?,
        risk: RiskClass::parse(&risk).unwrap_or(RiskClass::Unknown),
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        choices: serde_json::from_str(&choices).unwrap_or_default(),
        session_id: r.get(6)?,
        run_id: r.get(7)?,
        created_at: r.get(8)?,
        expires_at: r.get(9)?,
        state: ApprovalState::parse(&state).unwrap_or(ApprovalState::Pending),
        decision: r.get(11)?,
        reason: r.get(12)?,
        decided_by: r.get(13)?,
        decided_via: r.get(14)?,
        decided_at: r.get(15)?,
        rule_created: r.get(16)?,
    })
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn rfc3339_to_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use serde_json::json;
    use std::sync::Arc;

    fn approvals(clock: TestClock) -> ApprovalStore {
        ApprovalStore::new(Store::open_memory().unwrap(), Arc::new(clock))
    }

    async fn make(a: &ApprovalStore) -> ApprovalRequest {
        a.create(
            ApprovalKind::ToolCall,
            "mcp__forge__create_pr",
            RiskClass::External,
            json!({"title":"fix"}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some("s1"),
            Some("r1"),
            false,
        )
        .await
        .unwrap()
    }

    /// CA 9 : double clic simultané Telegram + CLI donne une seule décision.
    #[tokio::test]
    async fn ca_9_1_first_decision_wins() {
        let a = approvals(TestClock::default());
        let r = make(&a).await;

        let first = a
            .decide(r.id.as_str(), &Decision::approve_once("telegram"))
            .await
            .unwrap();
        assert_eq!(first.state, ApprovalState::Approved);
        assert_eq!(first.decided_via.as_deref(), Some("telegram"));

        let second = a
            .decide(r.id.as_str(), &Decision::deny("cli", None))
            .await
            .unwrap_err();
        match second {
            HitlError::AlreadyDecided { state, by, .. } => {
                assert_eq!(state, "approved");
                assert_eq!(by, "telegram");
            }
            other => panic!("attendu AlreadyDecided, obtenu {other:?}"),
        }
        assert_eq!(a.count_pending().await.unwrap(), 0);
    }

    /// CA 9 : une expiration bloque le run, puis `/resume` le relance.
    #[tokio::test]
    async fn ca_9_2_expiry_blocks_then_can_resume() {
        let clock = TestClock::default();
        let a = approvals(clock.clone()).with_ttl_ms(3_600_000);
        let r = make(&a).await;

        assert!(a.expire_due().await.unwrap().is_empty());
        clock.advance_hours(2);
        let expired = a.expire_due().await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].run_id.as_deref(), Some("r1"));
        assert_eq!(
            a.get(r.id.as_str()).await.unwrap().unwrap().state,
            ApprovalState::Expired
        );
        // Une demande expirée ne peut plus être tranchée.
        assert!(
            a.decide(r.id.as_str(), &Decision::approve_once("cli"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn reminders_fire_at_one_and_six_hours() {
        let clock = TestClock::default();
        let a = approvals(clock.clone());
        make(&a).await;

        assert!(a.due_reminders().await.unwrap().is_empty());
        clock.advance_hours(1);
        let r1 = a.due_reminders().await.unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].1, 1);
        assert!(
            a.due_reminders().await.unwrap().is_empty(),
            "pas de relance en double"
        );

        clock.advance_hours(5);
        let r2 = a.due_reminders().await.unwrap();
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].1, 2);
    }

    #[tokio::test]
    async fn denial_reason_is_kept_for_the_model() {
        let a = approvals(TestClock::default());
        let r = make(&a).await;
        let d = a
            .decide(
                r.id.as_str(),
                &Decision::deny("telegram", Some("pas sur ce dépôt".into())),
            )
            .await
            .unwrap();
        assert_eq!(d.state, ApprovalState::Denied);
        assert_eq!(d.reason.as_deref(), Some("pas sur ce dépôt"));
    }

    #[tokio::test]
    async fn always_records_a_rule_marker() {
        let a = approvals(TestClock::default());
        let r = make(&a).await;
        let d = a
            .decide(r.id.as_str(), &Decision::approve_always("telegram"))
            .await
            .unwrap();
        assert_eq!(d.rule_created.as_deref(), Some("always"));
    }

    #[tokio::test]
    async fn quiet_requests_are_batched_for_the_digest() {
        let a = approvals(TestClock::default());
        a.create(
            ApprovalKind::ToolCall,
            "outil",
            RiskClass::Write,
            json!({}),
            vec![],
            None,
            None,
            true,
        )
        .await
        .unwrap();
        make(&a).await;
        assert_eq!(a.quiet_backlog().await.unwrap().len(), 1);
        assert_eq!(a.pending(10).await.unwrap().len(), 2);
    }

    #[test]
    fn urgent_kinds_are_never_silenced() {
        assert!(ApprovalKind::EffectUnknown.is_urgent());
        assert!(ApprovalKind::BudgetExceeded.is_urgent());
        assert!(!ApprovalKind::MemoryProposal.is_urgent());
    }

    #[test]
    fn every_kind_maps_to_a_template() {
        for k in [
            ApprovalKind::ToolCall,
            ApprovalKind::McpSampling,
            ApprovalKind::McpElicitation,
            ApprovalKind::WorkflowGate,
            ApprovalKind::PlanProposal,
            ApprovalKind::EffectUnknown,
            ApprovalKind::SkillProposal,
            ApprovalKind::MemoryProposal,
            ApprovalKind::ConfigChange,
            ApprovalKind::BudgetExceeded,
            ApprovalKind::McpAdmin,
        ] {
            assert!(!k.template().is_empty());
            assert_eq!(ApprovalKind::parse(k.as_str()), Some(k));
        }
    }
}
