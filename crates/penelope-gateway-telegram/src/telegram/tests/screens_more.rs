//! Écrans d'aide, d'état, de journal, de prompts MCP et de skills.

use super::*;
use penelope_mcp_host::testing::{FakeConnector, declare, server, tool};

/// Faux serveur qui propose deux prompts : l'un sans argument, l'autre avec.
fn with_prompts() -> penelope_mcp_host::testing::Handler {
    let base = server(Arc::new(std::sync::Mutex::new(vec![tool(
        "lire",
        json!({"readOnlyHint": true}),
    )])));
    Arc::new(move |m, p| match m {
        "prompts/list" => Ok(json!({"prompts": [
            {"name": "resume", "description": "Résumé du jour"},
            {"name": "ticket", "description": "Ouvre un ticket",
             "arguments": [{"name": "titre", "required": true}, {"name": "note"}]}
        ]})),
        "prompts/get" => Ok(json!({"messages": [
            {"role": "user", "content": {"type": "text", "text": format!("Prompt {}", p["name"])}}
        ]})),
        _ => base(m, p),
    })
}

/// L'aide ouvre une famille, et un bouton de commande l'exécute dans la conversation.
#[tokio::test]
async fn help_lists_families_then_runs_a_command() {
    let (_d, g, t, _p) = gateway().await;
    let (text, buttons) = screen_of(&g, "help", json!({})).await;
    assert!(text.starts_with("**Commandes**"), "{text}");
    let cat = penelope_telegram::commands::find("status")
        .unwrap()
        .category;
    let (text, buttons2) = screen_of(&g, "help", json!({"cat": cat})).await;
    assert!(text.contains("\n- /status : "), "{text}");
    token_of(&buttons, &format!("📂 {cat}"));
    token_of(&buttons2, "« Familles");
    press(&g, &token_of(&buttons2, "/status")).await;
    assert!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.contains("Pénélope") && x.contains("version")),
        "la commande a répondu"
    );
}

/// L'état donne un bouton par chose qui attend : demandes, serveurs MCP pas prêts, runs.
#[tokio::test]
async fn status_offers_what_is_waiting() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    s.approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "fs_write",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "fs_write"}),
            vec!["Autoriser".into(), "Refuser".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap();
    let fake = Arc::new(FakeConnector::default());
    fake.fail_open
        .lock()
        .unwrap()
        .insert("casse".into(), "introuvable".into());
    let sup = penelope_mcp_host::testing::supervisor(s.clone(), fake);
    declare(&sup, "casse", "lazy_start = false\n");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());
    penelope_orchestrator::workflow::start_run(
        &penelope_daemon::workflow::context_of(&g.daemon),
        "build-verify",
        json!({"objectif": "x"}),
        &Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        },
        None,
        0,
    )
    .await
    .unwrap();
    let (text, buttons) = screen_of(&g, "status", json!({})).await;
    assert!(text.contains("- Demandes en attente : 1"), "{text}");
    token_of(&buttons, "📋 Demandes (1)");
    token_of(&buttons, "🔌 MCP (0/1)");
    token_of(&buttons, "🏃 Runs");
}

/// Le journal se filtre par composant et s'allonge.
#[tokio::test]
async fn logs_are_filtered_by_component() {
    let (_d, g, _t, _p) = gateway().await;
    let dir = g.daemon.services.platform.dirs.logs();
    std::fs::create_dir_all(&dir).unwrap();
    let line = |target: &str, message: &str| {
        json!({"timestamp": "2026-09-26T10:11:12Z", "level": "INFO", "target": target,
               "fields": {"message": message}})
        .to_string()
    };
    std::fs::write(
        dir.join("penelope-2026-09-26.jsonl"),
        format!(
            "{}\n{}\npas du JSON\n",
            line("penelope_mcp_host::lifecycle", "serveur prêt"),
            line("penelope_llm::router", "repli sur main")
        ),
    )
    .unwrap();
    let (text, buttons) = screen_of(&g, "logs", json!({})).await;
    assert!(text.contains("· 2 ligne(s)"), "{text}");
    assert!(
        text.contains("10:11:12 INFO lifecycle : serveur prêt"),
        "{text}"
    );
    token_of(&buttons, "Plus");
    let (text, buttons) = screen_of(&g, "logs", json!({"component": "mcp", "n": 5})).await;
    assert!(text.contains("**Journal** `mcp` · 1 ligne(s)"), "{text}");
    token_of(&buttons, "Tout");
    let (text, _) = screen_of(&g, "logs", json!({"component": "workflow"})).await;
    assert_eq!(text, "📜 Aucune ligne de journal pour `workflow`.");
}

/// Un prompt sans argument part au modèle ; un prompt à arguments ouvre un formulaire.
#[tokio::test]
async fn mcp_prompts_are_listed_and_run() {
    let (_d, g, _t, _p) = gateway().await;
    let (text, _) = screen_of(&g, "prompts", json!({})).await;
    assert_eq!(text, "Aucun serveur MCP chargé.");
    let s = g.daemon.services.clone();
    let fake = Arc::new(FakeConnector::default());
    fake.serve("docs", with_prompts());
    let sup = penelope_mcp_host::testing::supervisor(s.clone(), fake);
    declare(&sup, "docs", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());

    let (text, buttons) = screen_of(&g, "prompts", json!({})).await;
    assert!(text.starts_with("💬 **Prompts MCP**"), "{text}");
    token_of(&buttons, "🟢 docs");
    let (text, buttons) = screen_of(&g, "prompts", json!({"server": "docs"})).await;
    assert!(text.contains("- `ticket` : Ouvre un ticket"), "{text}");
    press(&g, &token_of(&buttons, "▶️ resume")).await;
    assert_eq!(
        s.turns.pending_count().await.unwrap(),
        1,
        "prompt remis au modèle"
    );
    press(&g, &token_of(&buttons, "▶️ ticket")).await;
    let form = s.kv_get(&form_key(OWNER, None)).await.unwrap().unwrap();
    assert!(form.contains("\"docs · ticket\""), "{form}");
}

/// Une skill du propriétaire se lit, et son retour en arrière se confirme ; sans
/// version précédente, le refus est dit.
#[tokio::test]
async fn a_user_skill_is_read_and_its_rollback_confirmed() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let dir = s.platform.dirs.skills().join("compte-rendu");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: compte-rendu\ndescription: Rédiger un compte rendu de réunion.\nversion: 1.0.0\ndeclencheurs:\n  - réunion\n---\n# Compte rendu\n\nTrois parties.\n",
    )
    .unwrap();
    penelope_app::services::reload_skills(&s).await.unwrap();
    let (text, buttons) = screen_of(&g, "skills", json!({})).await;
    assert!(
        text.contains("- **compte-rendu** · user · v1.0.0"),
        "{text}"
    );
    token_of(&buttons, "📖 compte-rendu");
    let (text, buttons) = screen_of(&g, "skill", json!({"name": "compte-rendu"})).await;
    assert!(text.contains("Déclencheurs : réunion"), "{text}");
    assert!(text.contains("Trois parties."), "{text}");
    confirm(&g, &token_of(&buttons, "⏪ Version précédente")).await;
    assert!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.starts_with("❌")),
        "sans version précédente, le refus est dit"
    );
    assert!(
        g.build_screen(OWNER, None, "skill", &json!({"name": "absente"}))
            .await
            .is_err()
    );
}
