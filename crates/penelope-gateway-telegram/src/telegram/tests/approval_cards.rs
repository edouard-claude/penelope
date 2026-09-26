//! Cartes d'approbation particulières : point de contrôle de coût, alertes de la carte
//! d'outil, lancement de workflow, autorisation OAuth impossible.

use super::*;

async fn approval(
    g: &TelegramGateway,
    kind: penelope_hitl::ApprovalKind,
    subject: &str,
    payload: Value,
) -> penelope_hitl::ApprovalRequest {
    g.daemon
        .services
        .approvals
        .create(
            kind,
            subject,
            penelope_kernel::risk::RiskClass::Write,
            payload,
            vec!["Autoriser".into(), "Refuser".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap()
}

async fn card(g: &TelegramGateway, t: &MockTransport, a: &penelope_hitl::ApprovalRequest) -> Value {
    g.send_approval_card(OWNER, None, a).await.unwrap();
    g.flush_outbox().await.unwrap();
    t.calls_to(tg::SEND_MESSAGE)
        .await
        .pop()
        .expect("carte envoyée")
}

/// #19 : le point de contrôle de coût demande de continuer ou d'arrêter ; « Arrêter »
/// refuse la demande.
#[tokio::test]
async fn a_cost_checkpoint_is_continued_or_stopped() {
    let (_d, g, t, _p) = gateway().await;
    let a = approval(
        &g,
        penelope_hitl::ApprovalKind::ToolCall,
        "checkpoint",
        json!({"checkpoint": true, "reason": "Ce tour a coûté 2 $, je continue ?"}),
    )
    .await;
    let sent = card(&g, &t, &a).await;
    assert!(
        sent["text"]
            .as_str()
            .unwrap()
            .starts_with("💸 Ce tour a coûté 2 $"),
        "{sent}"
    );
    let buttons = inline_buttons(&sent);
    assert_eq!(
        buttons.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
        ["▶️ Continuer", "⏹ Arrêter"]
    );
    press(&g, &buttons[1].1).await;
    let after = g
        .daemon
        .services
        .approvals
        .get(a.id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, penelope_hitl::ApprovalState::Denied);
}

/// #106 : une commande réseau et une seconde confirmation s'annoncent sur la carte.
#[tokio::test]
async fn a_tool_card_warns_about_network_and_second_confirmation() {
    let (_d, g, t, _p) = gateway().await;
    let a = approval(
        &g,
        penelope_hitl::ApprovalKind::ToolCall,
        "shell_exec",
        json!({"double": true, "reason": "commande réseau",
               "arguments": {"command": "curl https://exemple.org", "network": true}}),
    )
    .await;
    let text = card(&g, &t, &a).await["text"].as_str().unwrap().to_string();
    assert!(text.contains("Seconde confirmation demandée."), "{text}");
    assert!(text.contains("🌐 Accès au réseau demandé."), "{text}");
}

/// La carte de lancement d'un workflow donne ses paramètres et son brief, coupé au-delà
/// de 1 200 caractères ; un workflow inconnu est dit.
#[tokio::test]
async fn a_workflow_launch_card_shows_parameters_and_brief() {
    let (_d, g, t, _p) = gateway().await;
    let brief = "Relire la PR. ".repeat(100);
    let a = approval(
        &g,
        penelope_hitl::ApprovalKind::ToolCall,
        "workflow_start",
        json!({"arguments": {"id": "absent", "params": {"ticket": 7, "branche": "main"},
                             "brief": brief}}),
    )
    .await;
    let text = card(&g, &t, &a).await["text"].as_str().unwrap().to_string();
    assert!(text.contains("Lancer « absent » ?"), "{text}");
    assert!(text.contains("Workflow introuvable."), "{text}");
    assert!(text.contains("<code>ticket</code> : 7"), "{text}");
    assert!(text.contains("Relire la PR. Relire la PR."), "{text}");
    assert!(text.trim_end().ends_with('…'), "brief coupé : {text}");
}

/// Sans serveur déclaré, ou pour un serveur en clair, l'autorisation OAuth est refusée
/// avec la raison.
#[tokio::test]
async fn an_impossible_oauth_authorization_is_explained() {
    use penelope_mcp_host::testing::{FakeConnector, declare};
    let (_d, g, t, _p) = gateway().await;
    g.send_oauth_card(OWNER, None, "absent").await.unwrap();
    let sup = penelope_mcp_host::testing::supervisor(
        g.daemon.services.clone(),
        Arc::new(FakeConnector::default()),
    );
    std::fs::create_dir_all(sup.dir()).unwrap();
    std::fs::write(
        sup.dir().join("clair.toml"),
        "transport = \"http\"\nurl = \"http://mcp.exemple.org/mcp\"\n",
    )
    .unwrap();
    let _ = declare;
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());
    g.send_oauth_card(OWNER, None, "absent").await.unwrap();
    g.send_oauth_card(OWNER, None, "clair").await.unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(
        sent.iter()
            .filter(|x| x.contains("Serveur MCP <code>absent</code> inconnu"))
            .count(),
        2,
        "{sent:?}"
    );
    assert!(
        sent.iter()
            .any(|x| x.starts_with("🔐 Autorisation impossible") && x.contains("HTTPS")),
        "{sent:?}"
    );
}
