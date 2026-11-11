use super::*;
use penelope_hitl::RuleScope;
use serde_json::json;
use std::sync::Arc;

async fn services() -> (tempfile::TempDir, Services) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    (dir, s)
}

async fn always(s: &Services, tool: &str, decision: PolicyDecision) -> String {
    s.policies
        .create_rule(
            RuleScope::Tool,
            Some(tool),
            None,
            None,
            decision,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap()
        .id
}

async fn forge(s: &Services) {
    let tools = [
        json!({"name": "delete_repo", "description": "Supprime un dépôt",
               "inputSchema": {"type": "object"}, "annotations": {"destructiveHint": true}}),
        json!({"name": "list_repos", "description": "Liste les dépôts",
               "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}),
    ]
    .iter()
    .map(|d| {
        let d = penelope_mcp::protocol::ToolDescriptor::parse(d).unwrap();
        penelope_mcp::registry::RegisteredTool::from_descriptor("forge", &d)
    })
    .collect();
    s.mcp_tools
        .replace_server_tools("forge", tools, "2026-11-11T10:00:00Z")
        .await
        .unwrap();
}

/// `doctor` nomme les règles « Toujours » qu'une double confirmation rend mortes (#445) :
/// celle d'un outil natif destructif, celle d'un outil MCP destructif, et elles seules.
#[tokio::test]
async fn doctor_names_the_rules_a_double_confirmation_ignores() {
    let (_d, s) = services().await;
    forge(&s).await;
    assert!(doctor_check(&s).await.ok);

    let forget = always(&s, "mem_forget", PolicyDecision::Auto).await;
    let delete = always(&s, "mcp__forge__delete_repo", PolicyDecision::Auto).await;
    // Ni une lecture, ni une écriture, ni un refus ne sont concernés.
    always(&s, "mcp__forge__list_repos", PolicyDecision::Auto).await;
    always(&s, "fs_write", PolicyDecision::Auto).await;
    always(&s, "mem_forget", PolicyDecision::Deny).await;
    // Une règle de session meurt avec sa session : elle n'est pas à retirer.
    s.policies
        .create_rule(
            RuleScope::Tool,
            Some("mem_forget"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Session,
            Some("s_1"),
        )
        .await
        .unwrap();

    let mut dead: Vec<String> = dead_rules(&s)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    dead.sort();
    let mut expected = vec![forget.clone(), delete.clone()];
    expected.sort();
    assert_eq!(dead, expected);

    let c = doctor_check(&s).await;
    assert!(!c.ok, "{c:?}");
    assert_eq!(c.severity, "warn");
    assert!(c.detail.contains("2 règle(s)"), "{}", c.detail);
    assert!(c.detail.contains(&forget), "{}", c.detail);
    assert!(
        c.detail.contains("`mcp__forge__delete_repo`"),
        "{}",
        c.detail
    );
    assert!(c.fix.unwrap().contains("/policies"));
}

/// Quand le propriétaire ramène la classe destructive à une seule confirmation, ses règles
/// valent à nouveau : plus rien à signaler.
#[tokio::test]
async fn a_single_confirmation_leaves_the_rules_alive() {
    let (_d, s) = services().await;
    always(&s, "mem_forget", PolicyDecision::Auto).await;
    assert!(!doctor_check(&s).await.ok);
    s.config
        .mutate("test", |c| {
            c.mcp.policy.destructive = "ask".into();
            Ok(vec![])
        })
        .unwrap();
    assert!(doctor_check(&s).await.ok);
}
