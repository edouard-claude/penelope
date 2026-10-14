use super::*;
use crate::pipeline::policy::{PolicyStage, Verdict, VerdictLayer};

fn info(name: &str, risk: RiskClass, policy: Option<PolicyDecision>) -> CallInfo {
    CallInfo {
        effective_name: name.into(),
        risk,
        idempotent: false,
        policy,
    }
}

async fn verdict(s: &AgentServices, sid: &str, info: &CallInfo, args: Value) -> Verdict {
    PolicyStage::evaluate(s, &spec(sid), None, info, &args)
        .await
        .unwrap()
}

/// Les huit raisons de `verdict.reason`, à l'octet : c'est la chaîne que la carte
/// affiche et que les tests d'approbation lisent. Chaque couche a sa variante.
#[tokio::test]
async fn the_eight_policy_reasons_are_byte_identical() {
    let (_d, s, _p) = setup().await;
    // Les modes de session en mémoire, gardés pour les fixer plus bas.
    let modes = Arc::new(MemoryModes::new(s.config.clone()));
    let s = AgentServices {
        modes: modes.clone(),
        ..(*s).clone()
    };
    s.config
        .mutate("test", |c| {
            c.tools.shell_allow = vec!["cargo build".into()];
            Ok(vec!["tools.shell_allow".into()])
        })
        .unwrap();
    let sid = session(&s).await;

    // 1. Règle du propriétaire.
    let rule = s
        .policies
        .create_rule(
            penelope_hitl::RuleScope::Tool,
            Some("fs_write"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    let v = verdict(
        &s,
        &sid,
        &info("fs_write", RiskClass::Write, None),
        json!({"path": "a.rs"}),
    )
    .await;
    assert_eq!(
        v.reason,
        format!("règle {} (tool, fenêtre always)", rule.id)
    );
    assert_eq!(
        v.layer,
        VerdictLayer::Rule {
            id: rule.id.clone()
        }
    );
    assert_eq!(v.decision, PolicyDecision::Auto);

    // 2. Déclaration du serveur : un refus.
    let v = verdict(
        &s,
        &sid,
        &info(
            "mcp__srv__drop",
            RiskClass::Write,
            Some(PolicyDecision::Deny),
        ),
        json!({}),
    )
    .await;
    assert_eq!(v.reason, "politique de la déclaration du serveur : `deny`");
    assert_eq!(v.layer, VerdictLayer::ServerDeclaration);
    assert_eq!(v.decision, PolicyDecision::Deny);

    // 3. Déclaration « auto » contre une règle du propriétaire : la règle gagne.
    let ask = s
        .policies
        .create_rule(
            penelope_hitl::RuleScope::Tool,
            Some("mcp__srv__send"),
            Some("srv"),
            None,
            PolicyDecision::Ask,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    let v = verdict(
        &s,
        &sid,
        &info(
            "mcp__srv__send",
            RiskClass::External,
            Some(PolicyDecision::Auto),
        ),
        json!({}),
    )
    .await;
    assert_eq!(v.reason, format!("règle {} (tool, fenêtre always)", ask.id));
    assert_eq!(v.layer, VerdictLayer::Rule { id: ask.id.clone() });
    assert_eq!(v.decision, PolicyDecision::Ask);

    // 4. Autorisation déclarée d'avance.
    let v = verdict(
        &s,
        &sid,
        &info("shell_exec", RiskClass::Write, None),
        json!({"command": "cargo build --release"}),
    )
    .await;
    assert_eq!(
        v.reason,
        "autorisé d'avance : famille(s) « cargo build » de `tools.shell_allow`"
    );
    assert_eq!(v.layer, VerdictLayer::DeclaredAllow);
    assert_eq!(v.decision, PolicyDecision::Auto);

    // 5. Mode « demander tout » : même une lecture du shell attend.
    let asking = session(&s).await;
    modes.set(&asking, Some(ApprovalMode::Ask));
    let v = verdict(
        &s,
        &asking,
        &info("shell_exec", RiskClass::Read, None),
        json!({"command": "ls"}),
    )
    .await;
    assert_eq!(v.reason, "mode « demander tout » de la session");
    assert_eq!(v.layer, VerdictLayer::SessionMode);
    assert_eq!(v.decision, PolicyDecision::Ask);

    // 6. Mode « tout sauf le destructif ».
    let trusting = session(&s).await;
    modes.set(&trusting, Some(ApprovalMode::Auto));
    let v = verdict(
        &s,
        &trusting,
        &info("shell_exec", RiskClass::Write, None),
        json!({"command": "make test"}),
    )
    .await;
    assert_eq!(v.reason, "mode « tout sauf le destructif » de la session");
    assert_eq!(v.layer, VerdictLayer::SessionMode);
    assert_eq!(v.decision, PolicyDecision::Auto);

    // 7. Réseau demandé : la raison le dit, la couche ne change pas.
    let v = verdict(
        &s,
        &sid,
        &info("shell_exec", RiskClass::External, None),
        json!({"command": "curl https://example.org", "network": true}),
    )
    .await;
    assert_eq!(
        v.reason,
        "accès réseau demandé pour cette commande (politique par défaut pour la classe \
         `external`)"
    );
    assert_eq!(v.layer, VerdictLayer::Default);
    assert_eq!(v.decision, PolicyDecision::Ask);

    // 8. Réglage sensible : double confirmation, malgré toute règle.
    s.policies
        .create_rule(
            penelope_hitl::RuleScope::Tool,
            Some("config_set"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    let v = verdict(
        &s,
        &sid,
        &info("config_set", RiskClass::Destructive, None),
        json!({"path": "sandbox.profile", "value": "full"}),
    )
    .await;
    assert_eq!(
        v.reason,
        "réglage sensible : double confirmation à chaque fois"
    );
    assert_eq!(v.layer, VerdictLayer::SensitiveConfig);
    assert_eq!(v.decision, PolicyDecision::AskTwice);
}

/// Le mode d'un run autonome, pour le seul run `r-auto` ; les autres tours suivent leur
/// session.
struct AutonomousRun(Arc<MemoryModes>);

#[async_trait::async_trait]
impl crate::ports::SessionModes for AutonomousRun {
    async fn of_session(&self, session_id: &str) -> ApprovalMode {
        self.0.of_session(session_id).await
    }
    async fn of_turn(&self, session_id: &str, run_id: Option<&str>) -> ApprovalMode {
        if run_id == Some("r-auto") {
            return ApprovalMode::Unattended;
        }
        self.0.of_session(session_id).await
    }
}

/// #346 : dans un run autonome, ce qui demandait passe sans demande (écriture du shell,
/// outil destructif, commande qui peut détruire) ; un refus reste un refus, et un réglage
/// sensible demande toujours deux fois. Hors du run, rien ne change.
#[tokio::test]
async fn an_autonomous_run_asks_nothing_but_keeps_denials_and_sensitive_settings() {
    let (_d, s, _p) = setup().await;
    let s = AgentServices {
        modes: Arc::new(AutonomousRun(Arc::new(MemoryModes::new(s.config.clone())))),
        ..(*s).clone()
    };
    let sid = session(&s).await;
    let in_run = |sid: &str| TurnSpec {
        run_id: Some("r-auto".into()),
        ..spec(sid)
    };
    let eval = |info: CallInfo, args: Value| {
        let s = s.clone();
        let spec = in_run(&sid);
        async move {
            PolicyStage::evaluate(&s, &spec, None, &info, &args)
                .await
                .unwrap()
        }
    };

    for (tool, risk, args) in [
        (
            "shell_exec",
            RiskClass::Write,
            json!({"command": "tailscale status"}),
        ),
        (
            "shell_exec",
            RiskClass::Write,
            json!({"command": "rm -rf build"}),
        ),
        (
            "mailbridge_delete",
            RiskClass::Destructive,
            json!({"uid": 1}),
        ),
    ] {
        let v = eval(info(tool, risk, None), args.clone()).await;
        assert_eq!(v.decision, PolicyDecision::Auto, "{tool} {args}");
        assert_eq!(
            v.reason,
            "autonome (run ou mission) : aucune approbation demandée"
        );
        assert_eq!(v.layer, VerdictLayer::SessionMode);
    }
    // Ce qu'un serveur MCP impose de demander passe aussi ; ce qu'il refuse, non.
    let v = eval(
        info(
            "srv_send",
            RiskClass::External,
            Some(PolicyDecision::AskTwice),
        ),
        json!({}),
    )
    .await;
    assert_eq!(v.decision, PolicyDecision::Auto);
    let v = eval(
        info("srv_send", RiskClass::External, Some(PolicyDecision::Deny)),
        json!({}),
    )
    .await;
    assert_eq!(v.decision, PolicyDecision::Deny);
    // Le run ne change pas lui-même qui le juge.
    let v = eval(
        info("config_set", RiskClass::Write, None),
        json!({"path": "approval.judge", "value": "off"}),
    )
    .await;
    assert_eq!(v.decision, PolicyDecision::AskTwice);
    assert_eq!(v.layer, VerdictLayer::SensitiveConfig);

    // Hors du run : la politique par défaut.
    let v = verdict(
        &s,
        &sid,
        &info("shell_exec", RiskClass::Write, None),
        json!({"command": "tailscale status"}),
    )
    .await;
    assert_eq!(v.decision, PolicyDecision::Ask);
}
