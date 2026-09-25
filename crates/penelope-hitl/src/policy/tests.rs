/// #67 (revue de sécurité) : les trois opérateurs de motif ne se contournent pas.
#[test]
fn pattern_operators_cannot_be_tricked() {
    // Commandes : enchaînement, substitution, redirection, mot plus long.
    assert!(command_matches(
        "cargo test",
        "cargo test -p penelope-kernel"
    ));
    assert!(command_matches("cargo test", "cargo test"));
    for detour in [
        "cargo test; rm -rf ~",
        "cargo test && curl https://exfil.example",
        "cargo test | tee /tmp/x",
        "cargo test `cat ~/.ssh/id_ed25519`",
        "cargo test $(whoami)",
        "cargo test > /tmp/vol",
        "cargo test\nrm -rf ~",
        "cargo testament",
        "cargotest",
    ] {
        assert!(!command_matches("cargo test", detour), "{detour}");
    }

    // #141 : entre guillemets, `&` et `|` sont des caractères. Une URL de requête est
    // de la famille de son programme, une affectation anodine ne l'en sort pas, et
    // une famille faite d'affectations ne couvre rien.
    assert!(command_matches(
        "glab",
        "glab api --hostname h \"p?a=1&b=2\""
    ));
    assert!(command_matches(
        "glab api",
        "GITLAB_HOST=h glab api \"p?x=1\""
    ));
    assert!(command_matches("jq", "jq -r '.[] | .path' data.json"));
    // Le tube vers une lecture pure garde la famille de sa première étape.
    assert!(command_matches(
        "glab",
        "glab api h \"p\" | jq -r '.[].path'"
    ));
    assert!(command_matches("cargo test", "cargo test | grep -c ok"));
    for detour in [
        "glab api h \"p\" | sh",
        "glab api h \"p\" | tee /tmp/x",
        "glab api h \"p\"; rm -rf ~",
        "DYLD_INSERT_LIBRARIES=x.dylib glab api \"p\"",
        "glabber api",
    ] {
        assert!(!command_matches("glab", detour), "{detour}");
    }
    for useless in ["GITLAB_HOST=gitlab.apnl.tech", "", "cd /x && ls"] {
        assert!(
            !command_matches(useless, "GITLAB_HOST=gitlab.apnl.tech glab api \"p\""),
            "{useless:?}"
        );
    }

    // Chemins : `..` résolu avant comparaison.
    assert!(path_matches("src/", "src/b.rs"));
    assert!(path_matches("src/", "./src/sous/c.rs"));
    assert!(!path_matches("src/", "src/../../.zshrc"));
    assert!(!path_matches("src/", "autre/b.rs"));
    assert!(path_matches("/etc/app/", "/etc/app/conf"));
    assert!(!path_matches("/etc/app/", "/etc/app/../shadow"));

    // Origine : hôte exact, casse ignorée.
    assert!(origin_of("https://example.com/a?b=1").as_deref() == Some("https://example.com"));
    assert!(origin_of("HTTPS://Example.COM/a").as_deref() == Some("https://example.com"));
    assert!(origin_of("pas-une-url").is_none());
}

#[test]
fn clone_origin_pattern_understands_scp_and_github_shortcuts() {
    assert!(args_match_in(
        &json!({"url":{ORIGIN_OP:"ssh://github.com"}}),
        &json!({"url":"git@github.com:o/r.git"}),
        None,
    ));
    assert!(args_match_in(
        &json!({"url":{ORIGIN_OP:"https://github.com"}}),
        &json!({"url":"Fidelatoo/imap"}),
        None,
    ));
    assert!(!args_match_in(
        &json!({"url":{ORIGIN_OP:"https://github.com"}}),
        &json!({"url":"../imap"}),
        None,
    ));
}
use super::*;
use penelope_kernel::clock::TestClock;
use penelope_kernel::config::McpPolicy;
use serde_json::json;
use std::sync::Arc;

#[test]
fn path_rule_uses_workspace_filesystem_case_and_refuses_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "x").unwrap();
    let alias = root.join("Src/a.rs");
    let same_file = std::fs::canonicalize(&alias).is_ok();
    assert_eq!(path_matches_in("src/", "Src/a.rs", Some(&root)), same_file);
    assert!(path_matches_in("src/", "src/a.rs", Some(&root)));
    assert!(path_matches_in("", "a.rs", Some(&root)));
    assert!(!path_matches_in("", "src/a.rs", Some(&root)));
    assert!(!path_matches_in("src/", "src/../../outside", Some(&root)));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path(), root.join("src/link")).unwrap();
        assert!(!path_matches_in("src/", "src/link/outside", Some(&root)));
    }
}

#[tokio::test]
async fn evaluation_resolves_relative_path_rule_in_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "x").unwrap();
    let engine = engine();
    engine
        .create_rule(
            RuleScope::Tool,
            Some("fs_write"),
            None,
            Some(json!({"path": {PATH_PREFIX_OP: "src/"}})),
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    let cfg = McpPolicy::default();
    let verdict = |path: &'static str| {
        let engine = &engine;
        let cfg = &cfg;
        let root = &root;
        async move {
            engine
                .evaluate_in(
                    cfg,
                    "fs_write",
                    None,
                    &json!({"path": path}),
                    RiskClass::Write,
                    None,
                    None,
                    Some(root),
                )
                .await
                .unwrap()
                .decision
        }
    };
    let alias_is_real = std::fs::canonicalize(root.join("Src/a.rs")).is_ok();
    assert_eq!(
        verdict("Src/a.rs").await == PolicyDecision::Auto,
        alias_is_real
    );
    assert_eq!(verdict("src/a.rs").await, PolicyDecision::Auto);
    assert_ne!(verdict("src/../../outside").await, PolicyDecision::Auto);
}

fn engine() -> PolicyEngine {
    PolicyEngine::new(
        Store::open_memory().unwrap(),
        Arc::new(TestClock::default()),
    )
}

#[tokio::test]
async fn defaults_follow_the_prd_table() {
    let cfg = McpPolicy::default();
    assert_eq!(
        PolicyEngine::default_for(&cfg, RiskClass::Read),
        PolicyDecision::Auto
    );
    assert_eq!(
        PolicyEngine::default_for(&cfg, RiskClass::Write),
        PolicyDecision::Ask
    );
    assert_eq!(
        PolicyEngine::default_for(&cfg, RiskClass::Destructive),
        PolicyDecision::AskTwice
    );
    assert_eq!(
        PolicyEngine::default_for(&cfg, RiskClass::External),
        PolicyDecision::Ask
    );
    assert_eq!(
        PolicyEngine::default_for(&cfg, RiskClass::Unknown),
        PolicyDecision::Ask
    );
}

/// CA 9 : une règle « toujours » est appliquée au prochain appel identique, puis
/// révoquée par `/policies`.
#[tokio::test]
async fn ca_9_3_always_rule_applies_then_is_revocable() {
    let e = engine();
    let cfg = McpPolicy::default();
    let args = json!({"project":"penelope","title":"fix"});

    let v = e
        .evaluate(
            &cfg,
            "mcp__forge__create_issue",
            Some("forge"),
            &args,
            RiskClass::Write,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v.decision, PolicyDecision::Ask);

    let rule = e
        .create_rule(
            RuleScope::Tool,
            Some("mcp__forge__create_issue"),
            Some("forge"),
            Some(json!({"project":"penelope"})),
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();

    let v = e
        .evaluate(
            &cfg,
            "mcp__forge__create_issue",
            Some("forge"),
            &args,
            RiskClass::Write,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v.decision, PolicyDecision::Auto);
    assert_eq!(v.rule_id.as_deref(), Some(rule.id.as_str()));

    // Un autre projet ne bénéficie pas de la règle.
    let v = e
        .evaluate(
            &cfg,
            "mcp__forge__create_issue",
            Some("forge"),
            &json!({"project":"autre"}),
            RiskClass::Write,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v.decision, PolicyDecision::Ask);

    assert!(e.revoke(&rule.id).await.unwrap());
    let v = e
        .evaluate(
            &cfg,
            "mcp__forge__create_issue",
            Some("forge"),
            &args,
            RiskClass::Write,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        v.decision,
        PolicyDecision::Ask,
        "la règle révoquée ne s'applique plus"
    );
}

#[tokio::test]
async fn more_specific_rule_wins() {
    let e = engine();
    let cfg = McpPolicy::default();
    e.create_rule(
        RuleScope::Server,
        None,
        Some("forge"),
        None,
        PolicyDecision::Auto,
        PolicyWindow::Always,
        None,
    )
    .await
    .unwrap();
    e.create_rule(
        RuleScope::Tool,
        Some("mcp__forge__delete_repo"),
        Some("forge"),
        None,
        PolicyDecision::Deny,
        PolicyWindow::Always,
        None,
    )
    .await
    .unwrap();

    let v = e
        .evaluate(
            &cfg,
            "mcp__forge__delete_repo",
            Some("forge"),
            &json!({}),
            RiskClass::Destructive,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        v.decision,
        PolicyDecision::Deny,
        "la règle d'outil prime sur celle de serveur"
    );
}

#[tokio::test]
async fn run_window_is_scoped_to_that_run() {
    let e = engine();
    let cfg = McpPolicy::default();
    e.create_rule(
        RuleScope::Tool,
        Some("shell_exec"),
        None,
        None,
        PolicyDecision::Auto,
        PolicyWindow::Run,
        Some("r1"),
    )
    .await
    .unwrap();

    let v = e
        .evaluate(
            &cfg,
            "shell_exec",
            None,
            &json!({}),
            RiskClass::Write,
            Some("r1"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(v.decision, PolicyDecision::Auto);

    let v = e
        .evaluate(
            &cfg,
            "shell_exec",
            None,
            &json!({}),
            RiskClass::Write,
            Some("r2"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        v.decision,
        PolicyDecision::Ask,
        "un autre run n'hérite de rien"
    );
}

#[tokio::test]
async fn once_window_never_matches_later() {
    let e = engine();
    let cfg = McpPolicy::default();
    e.create_rule(
        RuleScope::Tool,
        Some("t"),
        None,
        None,
        PolicyDecision::Auto,
        PolicyWindow::Once,
        None,
    )
    .await
    .unwrap();
    let v = e
        .evaluate(&cfg, "t", None, &json!({}), RiskClass::Write, None, None)
        .await
        .unwrap();
    assert_eq!(v.decision, PolicyDecision::Ask);
}

#[tokio::test]
async fn window_revocation_on_run_end() {
    let e = engine();
    e.create_rule(
        RuleScope::Tool,
        Some("t"),
        None,
        None,
        PolicyDecision::Auto,
        PolicyWindow::Run,
        Some("r1"),
    )
    .await
    .unwrap();
    assert_eq!(e.active_rules().await.unwrap().len(), 1);
    assert_eq!(e.revoke_window(PolicyWindow::Run, "r1").await.unwrap(), 1);
    assert!(e.active_rules().await.unwrap().is_empty());
}

#[test]
fn nested_argument_patterns() {
    let r = PolicyRule {
        id: "x".into(),
        scope: RuleScope::Tool,
        tool: Some("t".into()),
        server: None,
        arg_match: Some(json!({"repo":{"owner":"moi"}})),
        decision: PolicyDecision::Auto,
        window: PolicyWindow::Always,
        window_ref: None,
        created_at: String::new(),
        revoked_at: None,
        hits: 0,
    };
    assert!(r.matches("t", None, &json!({"repo":{"owner":"moi","name":"x"}})));
    assert!(!r.matches("t", None, &json!({"repo":{"owner":"autre"}})));
    assert!(!r.matches("t", None, &json!({})));
}

#[test]
fn annotations_never_override_configuration() {
    // La classe de risque vient du harnais ; la règle ne dépend pas des annotations
    // du serveur. Ici, même pour un outil annoncé `readOnly`, une règle `deny`
    // s'applique.
    let r = PolicyRule {
        id: "x".into(),
        scope: RuleScope::Tool,
        tool: Some("mcp__x__read".into()),
        server: None,
        arg_match: None,
        decision: PolicyDecision::Deny,
        window: PolicyWindow::Always,
        window_ref: None,
        created_at: String::new(),
        revoked_at: None,
        hits: 0,
    };
    assert!(r.matches("mcp__x__read", None, &json!({})));
    assert_eq!(r.decision, PolicyDecision::Deny);
}
