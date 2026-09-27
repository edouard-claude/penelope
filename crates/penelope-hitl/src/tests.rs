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

#[tokio::test]
async fn approval_lifecycle_is_in_the_runtime_log() {
    let store = Store::open_memory().unwrap();
    let clock = Arc::new(TestClock::default());
    let events = penelope_kernel::event::EventLog::new(store.clone(), clock.clone());
    let approvals = ApprovalStore::new(store, clock).with_events(events.clone());
    let request = make(&approvals).await;
    approvals
        .decide(request.id.as_str(), &Decision::approve_once("cli"))
        .await
        .unwrap();
    let logged = events.range(0, 10).await.unwrap();
    assert_eq!(logged.len(), 2);
    assert_eq!(logged[0].kind, "runtime.approval.requested");
    assert_eq!(logged[1].kind, "runtime.approval.decided");
    assert_eq!(logged[0].payload["approval_id"], request.id.as_str());
    assert_eq!(logged[1].payload["approved"], true);
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

/// Chaque sorte de demande et chaque état se relisent depuis leur nom stocké ; seules
/// les demandes d'effet inconnu et de budget dépassé sont urgentes (§9.2).
#[test]
fn kinds_and_states_round_trip() {
    let kinds = [
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
    ];
    for k in kinds {
        assert_eq!(ApprovalKind::parse(k.as_str()), Some(k));
        assert!(!k.template().is_empty());
    }
    let urgent: Vec<_> = kinds.into_iter().filter(|k| k.is_urgent()).collect();
    assert_eq!(
        urgent,
        [ApprovalKind::EffectUnknown, ApprovalKind::BudgetExceeded]
    );
    assert_eq!(ApprovalKind::parse("inconnue"), None);
    for s in [
        ApprovalState::Pending,
        ApprovalState::Approved,
        ApprovalState::Denied,
        ApprovalState::Expired,
        ApprovalState::Cancelled,
    ] {
        assert_eq!(ApprovalState::parse(s.as_str()), Some(s));
    }
    assert_eq!(ApprovalState::parse("perdue"), None);
}
