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

/// §4.2 : l'effet d'un appel déjà `completed` (fait dans une vie antérieure, le processus
/// mort avant d'écrire le résultat au transcript) est rejoué, jamais ré-exécuté.
#[tokio::test]
async fn completed_effects_are_replayed_not_reexecuted() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;

    // Vie n°1 : l'effet de l'appel `c1` est allé au bout, son résultat n'est pas écrit.
    let done = EffectSpec::new(effect_kind("fs_read"), "fs_read", json!({"path":"a.rs"}))
        .session(&sid)
        .step("c1")
        .idempotent(true);
    let id = match s.effects.plan(done).await.unwrap() {
        Planned::Fresh(id) => id,
        other => panic!("{other:?}"),
    };
    s.effects.dispatching(&id).await.unwrap();
    s.effects
        .complete(&id, json!({"lu": "a.rs (vie n°1)"}))
        .await
        .unwrap();

    // Vie n°2 : le tour reprend sur le même appel.
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("lu");
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis a.rs");
    let e = exec(false);
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(
        e.calls.load(Ordering::SeqCst),
        0,
        "le résultat vient du ledger, rien ne s'exécute"
    );
    assert!(conv.messages()[2].text().contains("vie n°1"));
}

/// #266 : deux appels identiques d'une session qui portent le **même identifiant** (un
/// fournisseur qui numérote ses appels de façon constante, comme `call_0` pour un appel
/// rendu en texte) sont deux appels : le second s'exécute, il n'est pas rejoué depuis le
/// premier résultat. L'écart est journalisé, et le transcript garde l'identifiant émis.
#[tokio::test]
async fn a_call_id_seen_again_in_the_session_is_executed_not_replayed() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis a.rs");
    let e = exec(false);
    let loop_ = AgentLoop::new(s.clone(), p.clone());

    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("call_0", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("lu");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(e.calls.load(Ordering::SeqCst), 1);

    conv.record(&ChatMessage::user("relis-le"), false)
        .await
        .unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("call_0", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("relu");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(
        e.calls.load(Ordering::SeqCst),
        2,
        "le second appel s'exécute au lieu d'être rejoué"
    );

    let msgs = conv.messages();
    assert_eq!(msgs[5].tool_calls[0].id, "call_0");
    assert_eq!(msgs[6].tool_call_id.as_deref(), Some("call_0"));

    let events = s.events.session_events(&sid, 0).await.unwrap();
    let reused: Vec<_> = events
        .iter()
        .filter(|ev| ev.kind == "tool.call_id_reused")
        .collect();
    assert_eq!(reused.len(), 1, "{events:?}");
    assert_eq!(reused[0].payload["call_id"], "call_0");
    assert_eq!(reused[0].payload["tool"], "fs_read");
    assert_eq!(reused[0].payload["identity"], "call_0@5");
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
