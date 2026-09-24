use super::decisions::decide_approval;
use super::*;

#[test]
fn clone_always_rule_is_limited_to_the_source_origin() {
    use penelope_hitl::policy::ORIGIN_OP;
    assert_eq!(
        arg_pattern("git_clone", Some(&json!({"url":"Fidelatoo/imap"}))),
        Some(json!({"url":{ORIGIN_OP:"https://github.com"}}))
    );
    assert_eq!(
        arg_pattern("git_clone", Some(&json!({"url":"git@github.com:o/r.git"}))),
        Some(json!({"url":{ORIGIN_OP:"ssh://github.com"}}))
    );
    assert!(arg_pattern("git_clone", Some(&json!({"url":"file:///tmp/repo"}))).is_none());
    assert!(arg_pattern("git_clone", Some(&json!({"url":"../local"}))).is_none());
}

#[tokio::test]
async fn approving_clone_always_creates_only_a_scoped_rule() {
    let dir = tempfile::tempdir().unwrap();
    let services = AgentServices::for_tests(
        dir.path(),
        Arc::new(penelope_kernel::clock::TestClock::default()),
    )
    .unwrap();
    let approval = services
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "git_clone",
            penelope_kernel::risk::RiskClass::External,
            json!({"arguments":{"url":"Fidelatoo/imap","dest":"imap-src"}}),
            Vec::new(),
            None,
            None,
            false,
        )
        .await
        .unwrap();
    decide_approval(
        &services,
        approval.id.as_str(),
        &Decision::approve_always("cli"),
    )
    .await
    .unwrap();
    let rules = services.policies.active_rules().await.unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].arg_match,
        Some(json!({"url":{penelope_hitl::policy::ORIGIN_OP:"https://github.com"}}))
    );
}
