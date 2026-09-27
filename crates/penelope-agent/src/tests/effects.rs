use super::*;

/// #75 : un tour paie un fsync par transition d'effet non idempotent, et aucun pour
/// ses lectures.
#[tokio::test]
async fn only_non_idempotent_effects_pay_a_durable_commit() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "compile");
    let e = exec(false);
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("c1", "fs_read", json!({"path":"a.rs"})),
            call("c2", "fs_read", json!({"path":"b.rs"})),
            call("c3", "shell_exec", json!({"command":"cargo build"})),
        ],
    ));
    let id = match loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap()
    {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        s.store.durable_commits(),
        0,
        "les lectures n'en paient aucun"
    );
    loop_
        .decide_approval(&id, &Decision::approve_once("telegram"))
        .await
        .unwrap();
    p.reply("compilé");
    let out = loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(e.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        s.store.durable_commits(),
        2,
        "dispatching et completed du seul effet non idempotent"
    );
}

/// §4.2 : un effet déjà `completed` est rejoué, jamais ré-exécuté.
#[tokio::test]
async fn completed_effects_are_replayed_not_reexecuted() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;

    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("lu");
    let e = exec(false);
    AgentLoop::new(s.clone(), p.clone())
        .run_memory(request(&sid), &e)
        .await
        .unwrap();
    assert_eq!(e.calls.load(Ordering::SeqCst), 1);

    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("relu");
    AgentLoop::new(s.clone(), p.clone())
        .run_memory(request(&sid), &e)
        .await
        .unwrap();
    assert_eq!(
        e.calls.load(Ordering::SeqCst),
        1,
        "aucune seconde exécution"
    );
}

#[test]
fn effect_kinds_and_servers_are_derived_from_names() {
    assert_eq!(effect_kind("mcp__forge__create_pr"), EffectKind::Mcp);
    assert_eq!(effect_kind("shell_exec"), EffectKind::Shell);
    assert_eq!(effect_kind("git_push"), EffectKind::Git);
    assert_eq!(effect_kind("fs_write"), EffectKind::Fs);
    assert_eq!(effect_kind("send_message"), EffectKind::Telegram);
    assert_eq!(server_of("mcp__forge__create_pr").as_deref(), Some("forge"));
    assert!(server_of("fs_read").is_none());
}
