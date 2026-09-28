use super::*;
use crate::{ApprovalKind, ApprovalStore, Decision};
use penelope_kernel::clock::TestClock;
use penelope_kernel::risk::RiskClass;
use std::sync::Arc;

struct Bench {
    store: Store,
    clock: TestClock,
    samples: ApprovalSamples,
    approvals: ApprovalStore,
}

fn bench() -> Bench {
    let store = Store::open_memory().unwrap();
    let clock = TestClock::default();
    Bench {
        samples: ApprovalSamples::new(store.clone(), Arc::new(clock.clone())),
        approvals: ApprovalStore::new(store.clone(), Arc::new(clock.clone())),
        store,
        clock,
    }
}

fn draft(call_id: &str) -> SampleDraft {
    SampleDraft {
        session_id: "s1".into(),
        turn_id: Some("t1".into()),
        call_id: call_id.into(),
        command_sha: "0123456789abcdef".into(),
        input: json!({"command": "ls; pwd", "cwd": "/ws", "workspaces": ["/ws"], "network": false}),
        floors: json!({"policy": "ask", "risk": "write", "rule": null, "sans_motif": true}),
        judge: None,
        approval_id: None,
        outcome: None,
    }
}

impl Bench {
    async fn exported(&self) -> Vec<Value> {
        self.store.read(|c| Ok(export(c, None)?)).await.unwrap()
    }

    async fn card(&self) -> String {
        self.approvals
            .create(
                ApprovalKind::ToolCall,
                "shell_exec",
                RiskClass::Write,
                json!({}),
                vec![],
                Some("s1"),
                None,
                false,
            )
            .await
            .unwrap()
            .id
            .0
    }
}

/// Une carte tranchée complète l'échantillon qui la cite : issue, canal, délai. Une
/// seconde décision (refusée par la carte) ne le réécrit pas.
#[tokio::test]
async fn a_decided_card_settles_its_sample_once() {
    let b = bench();
    let id = b.card().await;
    assert!(
        b.samples
            .record(SampleDraft {
                approval_id: Some(id.clone()),
                ..draft("c1")
            })
            .await
            .unwrap()
    );
    assert_eq!(b.exported().await[0]["outcome"], Value::Null);
    b.clock.advance_secs(42);
    b.approvals
        .decide(&id, &Decision::approve_once("telegram"))
        .await
        .unwrap();
    assert!(
        b.approvals
            .decide(&id, &Decision::deny("cli", None))
            .await
            .is_err()
    );
    let line = &b.exported().await[0];
    assert_eq!(line["outcome"], "approved");
    assert_eq!(line["via"], "telegram");
    assert_eq!(line["decision_ms"], 42_000);
    assert_eq!(line["v"], 1);
}

/// Une carte expirée donne l'issue `expired`.
#[tokio::test]
async fn an_expired_card_settles_as_expired() {
    let b = bench();
    let id = b.card().await;
    b.samples
        .record(SampleDraft {
            approval_id: Some(id),
            ..draft("c1")
        })
        .await
        .unwrap();
    b.clock.advance_days(2);
    assert_eq!(b.approvals.expire_due().await.unwrap().len(), 1);
    assert_eq!(b.exported().await[0]["outcome"], "expired");
}

/// Un échantillon est écrit une fois par appel ; l'exécution le complète une fois.
#[tokio::test]
async fn a_sample_is_completed_never_rewritten() {
    let b = bench();
    let immediate = SampleDraft {
        outcome: Some(("auto".into(), "regle".into())),
        ..draft("c1")
    };
    assert!(b.samples.record(immediate.clone()).await.unwrap());
    assert!(!b.samples.record(immediate).await.unwrap());
    b.samples
        .executed("s1", "c1", Some(0), Some(12))
        .await
        .unwrap();
    b.samples
        .executed("s1", "c1", Some(1), Some(99))
        .await
        .unwrap();
    let all = b.exported().await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["outcome"], "auto");
    assert_eq!(all[0]["via"], "regle");
    assert_eq!(all[0]["exit_code"], 0);
    assert_eq!(all[0]["exec_ms"], 12);
}

/// L'export repasse le rédacteur : une ligne écrite avant qu'une règle ne reconnaisse un
/// secret ne sort pas en clair (#148, #134).
#[tokio::test]
async fn export_masks_what_the_table_kept_in_clear() {
    let b = bench();
    let key = "sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef";
    b.samples
        .record(SampleDraft {
            input: json!({"command": format!("curl -H 'Authorization: Bearer {key}' x")}),
            ..draft("c1")
        })
        .await
        .unwrap();
    let line = b.exported().await[0].to_string();
    assert!(!line.contains(key), "{line}");
    assert!(
        line.contains("0123456789abcdef"),
        "l'empreinte reste : {line}"
    );
}

/// `doctor` : volume, plus ancien, issues.
#[tokio::test]
async fn stats_count_outcomes_and_the_oldest_sample() {
    let b = bench();
    b.samples
        .record(SampleDraft {
            outcome: Some(("auto".into(), "regle".into())),
            ..draft("c1")
        })
        .await
        .unwrap();
    b.clock.advance_secs(1);
    b.samples.record(draft("c2")).await.unwrap();
    let st = b.store.read(|c| Ok(stats(c)?)).await.unwrap();
    assert_eq!(st.total, 2);
    assert!(st.oldest.is_some());
    assert_eq!(
        st.outcomes,
        vec![("auto".to_string(), 1), ("en_attente".to_string(), 1)]
    );
}
