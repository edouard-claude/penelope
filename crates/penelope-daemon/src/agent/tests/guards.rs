use super::*;
use crate::agent::guards::{GuardVerdict, TurnGuard};

/// Garde qui note son passage et rend un verdict fixé d'avance.
struct Recording {
    name: &'static str,
    verdict: GuardVerdict,
    seen: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait::async_trait]
impl TurnGuard for Recording {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn check(&self, _cx: &TurnContext<'_>) -> anyhow::Result<GuardVerdict> {
        self.seen.lock().unwrap().push(self.name);
        Ok(self.verdict.clone())
    }
}

/// L'ordre de la chaîne est fixé dans le code : arrêt, budget, plafond d'appels, palier.
#[test]
fn the_turn_guards_run_in_a_fixed_order() {
    let names: Vec<&str> = default_chain().iter().map(|g| g.name()).collect();
    assert_eq!(names, ["cancel", "budget", "call_cap", "cost_checkpoint"]);
}

/// La première garde qui ne laisse pas passer décide ; les suivantes ne sont pas lues.
#[tokio::test]
async fn the_first_guard_that_objects_decides() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let agent = AgentLoop::new(s.clone(), p.clone());
    let sp = spec(&sid);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let guard = |name, verdict| Recording {
        name,
        verdict,
        seen: seen.clone(),
    };
    let cx = TurnContext {
        agent: &agent,
        spec: &sp,
        sink: &NullSink,
        iteration: 0,
        cost: 0.0,
    };

    let chain = [
        guard("a", GuardVerdict::Proceed),
        guard(
            "b",
            GuardVerdict::Suspend {
                approval_id: "ap_1".into(),
            },
        ),
        guard("c", GuardVerdict::Stop(TurnOutcome::Cancelled)),
    ];
    let chain: Vec<&dyn TurnGuard> = chain.iter().map(|g| g as &dyn TurnGuard).collect();
    let out = run_guards(&chain, &cx).await.unwrap();
    assert_eq!(
        out,
        Some(TurnOutcome::AwaitingApproval {
            approval_id: "ap_1".into()
        })
    );
    assert_eq!(*seen.lock().unwrap(), ["a", "b"]);

    seen.lock().unwrap().clear();
    let all = [
        guard("a", GuardVerdict::Proceed),
        guard("b", GuardVerdict::Proceed),
    ];
    let all: Vec<&dyn TurnGuard> = all.iter().map(|g| g as &dyn TurnGuard).collect();
    assert_eq!(run_guards(&all, &cx).await.unwrap(), None);
    assert_eq!(*seen.lock().unwrap(), ["a", "b"]);
}

/// Un arrêt demandé passe avant le budget : aucune carte de plafond n'est créée pour un
/// tour que le propriétaire a déjà arrêté. Le budget passe avant le plafond d'appels.
#[tokio::test]
async fn cancellation_comes_before_the_budget_and_the_budget_before_the_call_cap() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    for u in spent("t_test", TURN_CALLS as usize, 10.0) {
        s.budget.record(u).await.unwrap();
    }
    let agent = AgentLoop::new(s.clone(), p.clone());
    let sp = spec(&sid);
    sp.cancel.cancel();
    let cx = TurnContext {
        agent: &agent,
        spec: &sp,
        sink: &NullSink,
        iteration: 0,
        cost: 0.0,
    };
    let out = run_guards(&default_chain(), &cx).await.unwrap();
    assert_eq!(out, Some(TurnOutcome::Cancelled));
    assert!(s.approvals.pending(10).await.unwrap().is_empty());

    let sp = spec(&sid);
    let cx = TurnContext { spec: &sp, ..cx };
    match run_guards(&default_chain(), &cx).await.unwrap() {
        Some(TurnOutcome::BudgetExceeded { scope, .. }) => assert_eq!(scope, "jour"),
        other => panic!("le budget d'abord : {other:?}"),
    }
}
