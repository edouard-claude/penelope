use super::*;
use crate::agent::pipeline::decide::{
    CallContext, DescribedCall, GuardStop, Refusal, Suspension, call_chain, run_call_guards,
};

/// L'ordre de la chaîne est fixé dans le code.
#[test]
fn the_call_guards_run_in_a_fixed_order() {
    let names: Vec<&str> = call_chain().iter().map(|g| g.name()).collect();
    assert_eq!(
        names,
        ["allowlist", "prior_decision", "loop_guard", "precheck"]
    );
}

/// Les textes renvoyés au modèle sont ceux d'avant les gardes typées.
#[test]
fn refusal_texts_are_unchanged() {
    let cases = [
        (
            Refusal::Allowlist {
                tool: "shell_exec".into(),
            },
            "Refusé : `shell_exec` n'est pas autorisé ici.",
        ),
        (
            Refusal::Denied {
                reason: Some("pas maintenant".into()),
            },
            "Non exécuté : le propriétaire a refusé : pas maintenant. Propose une autre \
             approche ou demande.",
        ),
        (
            Refusal::Denied { reason: None },
            "Non exécuté : le propriétaire a refusé. Propose une autre approche ou demande.",
        ),
        (
            Refusal::Expired,
            "Non exécuté : la demande a expiré sans réponse. Propose une autre approche ou \
             demande.",
        ),
        (
            Refusal::LoopWarn("trois fois".into()),
            "[avertissement du harnais] trois fois",
        ),
        (
            Refusal::Invalid("chemin manquant".into()),
            "chemin manquant",
        ),
    ];
    for (refusal, text) in cases {
        assert_eq!(refusal.text(), text, "{refusal:?}");
    }
}

/// La liste blanche passe avant la décision antérieure : un appel interdit à l'étape est
/// refusé, même si une demande l'attend.
#[tokio::test]
async fn the_allowlist_comes_before_a_prior_decision() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let pending = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            RiskClass::Write,
            json!({"call_id": "c1"}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&sid),
            None,
            false,
        )
        .await
        .unwrap();
    let agent = AgentLoop::new(s.clone(), p.clone());
    let e = exec(false);
    let mut detector = LoopDetector::new(3);
    let described = DescribedCall {
        call: call("c1", "shell_exec", json!({"command": "cargo build"})),
        info: e
            .describe_call("shell_exec", &json!({"command": "cargo build"}))
            .await,
    };

    let mut restricted = spec(&sid);
    restricted.allowed_tools = vec!["fs_read".into()];
    let mut cx = CallContext {
        agent: &agent,
        spec: &restricted,
        execute: &e,
        detector: &mut detector,
    };
    assert_eq!(
        run_call_guards(&call_chain(), &described, &mut cx)
            .await
            .unwrap(),
        Some(GuardStop::Refuse(Refusal::Allowlist {
            tool: "shell_exec".into()
        }))
    );

    let open = spec(&sid);
    let mut cx = CallContext {
        agent: &agent,
        spec: &open,
        execute: &e,
        detector: &mut detector,
    };
    assert_eq!(
        run_call_guards(&call_chain(), &described, &mut cx)
            .await
            .unwrap(),
        Some(GuardStop::Suspend(Suspension::Prior {
            approval_id: pending.id.0.clone()
        }))
    );
}
