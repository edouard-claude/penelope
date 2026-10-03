use super::*;
use crate::pipeline::decide::{
    DescribedCall, GuardContext, GuardStop, Refusal, Suspension, call_chain, run_call_guards,
};
use crate::pipeline::{Decided, Step, Terminal};

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
        context: CallContext::root("c1"),
    };

    let mut restricted = spec(&sid);
    restricted.allowed_tools = vec!["fs_read".into()];
    let mut cx = GuardContext {
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
    let mut cx = GuardContext {
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

/// Couture PTC (décision 0016) : un appel imbriqué dont la politique demande une
/// approbation est refusé sans carte ; le même appel, émis par le modèle, pose la sienne.
#[tokio::test]
async fn a_nested_call_that_asks_is_refused_without_a_card() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let agent = AgentLoop::new(s.clone(), p.clone());
    let e = exec(false);
    let conv = MemoryConversation::new("Tu es Pénélope.", "compile");
    let turn = spec(&sid);
    let args = json!({"command": "cargo build"});
    let program = CallContext::root("c1");
    let nested = program.child("c1:ptc:1");
    assert_eq!(nested.parent, Some(CallId("c1".into())));
    assert_eq!(nested.root, CallId("c1".into()));

    let mut detector = LoopDetector::new(3);
    let mut cx = GuardContext {
        agent: &agent,
        spec: &turn,
        execute: &e,
        detector: &mut detector,
    };
    let described = DescribedCall {
        call: call("c1:ptc:1", "shell_exec", args.clone()),
        info: e.describe_call("shell_exec", &args).await,
        context: nested,
    };
    let decided = agent
        .decide_call(&turn, &conv, &NullSink, &mut cx, described)
        .await
        .unwrap();
    let Decided::Step(Step::Record(refused, text)) = decided else {
        panic!("l'appel imbriqué n'est pas refusé");
    };
    assert_eq!(refused.id, "c1:ptc:1");
    assert!(
        text.starts_with("Non exécuté : un appel imbriqué ne peut pas demander d'approbation"),
        "{text}"
    );
    assert!(
        s.approvals.pending(10).await.unwrap().is_empty(),
        "aucune carte"
    );

    let described = DescribedCall {
        call: call("c2", "shell_exec", args.clone()),
        info: e.describe_call("shell_exec", &args).await,
        context: CallContext::root("c2"),
    };
    let decided = agent
        .decide_call(&turn, &conv, &NullSink, &mut cx, described)
        .await
        .unwrap();
    assert!(matches!(
        decided,
        Decided::Stop(Terminal::Stop(TurnOutcome::AwaitingApproval { .. }))
    ));
    assert_eq!(s.approvals.pending(10).await.unwrap().len(), 1);
    assert_eq!(e.calls.load(Ordering::SeqCst), 0);
}

/// Exécuteur dont le précontrôle refuse `workflow_start`, comme le gate « vas-y » d'un
/// canal (#302).
struct GateExecutor;

#[async_trait::async_trait]
impl ToolExecutor for GateExecutor {
    async fn precheck(&self, name: &str, _args: &Value) -> Result<(), penelope_tools::ToolError> {
        if name == "workflow_start" {
            return Err(penelope_tools::ToolError::Denied(
                "le plan v1 est déjà lancé : run `r_plan_1`, en pause".into(),
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        name: &str,
        _args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        Ok(ToolOutcome::ok(json!({"tool": name})))
    }
}

/// #302 : un appel refusé par l'exécuteur avant toute carte revient au modèle avec
/// l'état ; le deuxième refus du même outil dans le tour arrête le tour, sans carte ni
/// autre appel au modèle, et le propriétaire reçoit cet état.
#[tokio::test]
async fn a_second_executor_refusal_in_a_turn_halts_it_with_the_state() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let args = json!({"id": "build-verify"});
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "workflow_start", args.clone())],
    ));
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c2", "workflow_start", args.clone())],
    ));
    p.reply("jamais appelé");
    let conv = MemoryConversation::new("Tu es Pénélope.", "lance-le");
    let mut spec = request(&sid);
    spec.tools = vec![ToolDef::new(
        "workflow_start",
        "lancer",
        json!({"type":"object"}),
    )];
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec, &conv, &GateExecutor, &NullSink)
        .await
        .unwrap();
    match out {
        TurnOutcome::Answered { text, .. } => {
            assert!(
                text.contains("Tour arrêté") && text.contains("run `r_plan_1`"),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(p.call_count(), 2, "aucun appel au modèle après l'arrêt");
    assert!(
        s.approvals.pending(10).await.unwrap().is_empty(),
        "aucune carte"
    );
    let tail = conv.tail().await.unwrap();
    let results: Vec<String> = tail
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.text())
        .collect();
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(
        results
            .iter()
            .all(|r| r.starts_with("Refusé : le plan v1 est déjà lancé")),
        "{results:?}"
    );
    assert_eq!(tail.last().unwrap().role, Role::Assistant);
    assert!(tail.last().unwrap().text().contains("Tour arrêté"));
    let halted = s
        .events
        .session_events_of_kind(&sid, "turn.halted")
        .await
        .unwrap();
    assert_eq!(halted.len(), 1);
    assert_eq!(halted[0].payload["tool"], "workflow_start");
}
