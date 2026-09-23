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

/// #83 : la reprise attend la décision, « C'est fait » rejoue sans relancer, et
/// aucune règle n'est créée, même demandée « toujours ».
#[tokio::test]
async fn an_uncertain_effect_marked_done_is_replayed_not_rerun() {
    let (_d, s, p, sid, conv, approval) = crashed_push().await;
    let e = exec(false);
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    assert_eq!(
        loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap(),
        TurnOutcome::AwaitingApproval {
            approval_id: approval.clone()
        },
        "la reprise attend la décision"
    );
    let d = Decision {
        choice: EFFECT_DONE.into(),
        ..Decision::approve_always("telegram")
    };
    assert!(loop_.decide_approval(&approval, &d).await.unwrap());
    assert!(
        s.policies.active_rules().await.unwrap().is_empty(),
        "aucune règle"
    );

    p.reply("poussé");
    let out = loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(e.calls.load(Ordering::SeqCst), 0, "jamais relancé");
    let result = conv.messages()[2].text();
    assert!(result.contains("fait"), "{result}");
    assert_eq!(
        s.effects
            .count_by_state(penelope_kernel::effects::EffectState::Completed)
            .await
            .unwrap(),
        1
    );
}

/// #83 : « Relancer » remet l'effet en `planned` : la reprise l'exécute, une fois.
#[tokio::test]
async fn an_uncertain_effect_retried_runs_once() {
    let (_d, s, p, sid, conv, approval) = crashed_push().await;
    let e = exec(false);
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    let d = Decision {
        choice: EFFECT_RETRY.into(),
        ..Decision::approve_once("cli")
    };
    assert!(loop_.decide_approval(&approval, &d).await.unwrap());
    p.reply("relancé");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(e.calls.load(Ordering::SeqCst), 1);
}

/// #83 : « Ignorer » laisse l'effet tel quel (`failed`) et le modèle l'apprend.
#[tokio::test]
async fn an_uncertain_effect_ignored_is_not_rerun() {
    let (_d, s, p, sid, conv, approval) = crashed_push().await;
    let e = exec(false);
    let loop_ = AgentLoop::new(s.clone(), p.clone());
    assert!(
        !loop_
            .decide_approval(&approval, &Decision::deny("telegram", None))
            .await
            .unwrap()
    );
    p.reply("compris");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(e.calls.load(Ordering::SeqCst), 0);
    let result = conv.messages()[2].text();
    assert!(result.contains("sans relance"), "{result}");
    assert_eq!(
        s.effects
            .count_by_state(penelope_kernel::effects::EffectState::Failed)
            .await
            .unwrap(),
        1
    );
    // Un « Autoriser » sans choix d'effet est refusé, sans rien trancher.
    let (_d2, s2, p2, _sid2, _conv2, approval2) = crashed_push().await;
    let e2 = decide_approval(&s2, &approval2, &Decision::approve_once("cli"))
        .await
        .unwrap_err();
    assert!(e2.to_string().contains("--effect"), "{e2}");
    assert_eq!(s2.approvals.pending(10).await.unwrap().len(), 1);
    drop(p2);
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
        .run(request(&sid), &e)
        .await
        .unwrap();
    assert_eq!(e.calls.load(Ordering::SeqCst), 1);

    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("relu");
    AgentLoop::new(s.clone(), p.clone())
        .run(request(&sid), &e)
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
