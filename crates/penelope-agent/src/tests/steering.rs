//! Steering explicite (§3.4, épopée #208, lot K).

use super::*;

/// Boîte en mémoire : les messages déposés sont rendus à la réclamation suivante, et
/// chaque réclamation qui rend quelque chose note son point de contrôle.
#[derive(Default)]
struct MemoryInbox {
    queued: Mutex<Vec<Steer>>,
    claimed_at: Mutex<Vec<Checkpoint>>,
}

impl MemoryInbox {
    fn push(&self, text: &str) {
        let mut queued = self.queued.lock().unwrap();
        let id = format!("m{}", queued.len() + 1);
        queued.push(Steer {
            id,
            text: Some(text.into()),
            arrived_at: "2026-09-25T10:00:00Z".into(),
        });
    }
}

#[async_trait::async_trait]
impl Inbox for MemoryInbox {
    async fn claim(&self, at: Checkpoint) -> anyhow::Result<Vec<Steer>> {
        let steers: Vec<Steer> = self.queued.lock().unwrap().drain(..).collect();
        if !steers.is_empty() {
            self.claimed_at.lock().unwrap().push(at);
        }
        Ok(steers)
    }
}

/// T13 : trois lectures et une écriture, un message pendant la première. La première
/// finit, les trois autres reçoivent « Non exécuté », le message est écrit après leurs
/// résultats, le modèle est rappelé une fois ; aucun appel ne reste sans résultat.
#[tokio::test]
async fn a_message_during_a_batch_skips_the_calls_not_started() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    s.policies
        .create_rule(
            penelope_hitl::RuleScope::Tool,
            Some("shell_exec"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("c1", "fs_read", json!({"path":"a.rs"})),
            call("c2", "shell_exec", json!({"command":"cargo fmt"})),
            call("c3", "fs_read", json!({"path":"b.rs"})),
            call("c4", "fs_read", json!({"path":"c.rs"})),
        ],
    ));
    p.reply("d'accord, je lis d.rs");
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis et formate");
    let e = TimedExecutor::new(300);
    let inbox = Arc::new(MemoryInbox::default());
    let arriving = inbox.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        arriving.push("laisse tomber, lis plutôt d.rs");
    });
    let out = AgentLoop::new(s.clone(), p.clone())
        .with_inbox(Some(inbox.clone()))
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");

    let log = e.log.lock().unwrap();
    assert_eq!(log.len(), 1, "seule la première lecture est partie");
    assert_eq!(log[0].0, "a.rs");
    assert_eq!(
        *inbox.claimed_at.lock().unwrap(),
        vec![Checkpoint::BetweenCalls]
    );

    let messages = conv.messages();
    assert!(pending_calls(&messages).is_empty());
    let shape: Vec<(Role, Option<String>, String)> = messages
        .iter()
        .map(|m| (m.role, m.tool_call_id.clone(), m.text()))
        .collect();
    assert_eq!(shape[1].0, Role::Assistant);
    assert_eq!(shape[2].1.as_deref(), Some("c1"));
    assert!(shape[2].2.contains("a.rs"), "{shape:?}");
    for (i, id) in [(3, "c2"), (4, "c3"), (5, "c4")] {
        assert_eq!(shape[i].1.as_deref(), Some(id));
        assert_eq!(shape[i].2, NOT_RUN_NEW_MESSAGE);
    }
    assert_eq!(shape[6].0, Role::User);
    assert_eq!(shape[6].2, "laisse tomber, lis plutôt d.rs");
    assert_eq!(shape[7].0, Role::Assistant);
    assert_eq!(messages.len(), 8);

    // Le modèle est rappelé une fois, avec le message après les résultats et la note.
    let requests = p.requests();
    assert_eq!(requests.len(), 2);
    let last = &requests[1].messages;
    assert_eq!(
        last.last().unwrap().text(),
        "laisse tomber, lis plutôt d.rs"
    );
    assert!(last.iter().any(|m| m.text() == MERGE_NOTE));
}

/// T12 : sans message arrivé, la boîte ne change rien au lot ni aux requêtes.
#[tokio::test]
async fn an_empty_inbox_changes_nothing() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "fs_read", json!({"path":"a.rs"}))],
    ));
    p.reply("lu");
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis");
    let inbox = Arc::new(MemoryInbox::default());
    AgentLoop::new(s.clone(), p.clone())
        .with_inbox(Some(inbox.clone()))
        .run_conversation(&spec(&sid), &conv, &TimedExecutor::new(10), &NullSink)
        .await
        .unwrap();
    assert!(inbox.claimed_at.lock().unwrap().is_empty());
    assert!(
        p.requests()
            .iter()
            .all(|r| r.messages.iter().all(|m| m.text() != MERGE_NOTE))
    );
}

/// T14 : `/stop` pendant un lot. L'appel en cours est interrompu, ceux qui ne sont pas
/// partis reçoivent « Non exécuté : arrêté » ; le tour suivant porte la note
/// d'interruption après le message du propriétaire, et le préfixe ne bouge pas.
#[tokio::test]
async fn a_stop_during_a_batch_closes_the_calls_and_notes_it_next_turn() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    s.policies
        .create_rule(
            penelope_hitl::RuleScope::Tool,
            Some("shell_exec"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            call("c1", "fs_read", json!({"path":"a.rs"})),
            call("c2", "shell_exec", json!({"command":"cargo fmt"})),
            call("c3", "fs_read", json!({"path":"b.rs"})),
        ],
    ));
    let conv = MemoryConversation::new("Tu es Pénélope.", "lis et formate");
    let e = TimedExecutor::new(10_000);
    let first = spec(&sid);
    let cancel = first.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&first, &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(out, TurnOutcome::Cancelled);
    assert_eq!(
        e.log.lock().unwrap().len(),
        1,
        "seule la première est partie"
    );
    let messages = conv.messages();
    assert!(pending_calls(&messages).is_empty());
    let closed: Vec<String> = messages[3..].iter().map(ChatMessage::text).collect();
    assert_eq!(closed, vec![NOT_RUN_STOPPED, NOT_RUN_STOPPED]);

    // Le tour suivant.
    conv.record(&ChatMessage::user("reprends"), false)
        .await
        .unwrap();
    p.reply("je reprends");
    let out = AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let requests = p.requests();
    let (before, after) = (&requests[0].messages, &requests[1].messages);
    let n = after.len();
    assert_eq!(after[n - 2].text(), "reprends");
    assert_eq!(after[n - 1].role, Role::System);
    assert_eq!(after[n - 1].text(), INTERRUPTED_NOTE);
    assert_eq!(
        Fingerprint::of(before, &[]).system_hash,
        Fingerprint::of(after, &[]).system_hash
    );
    // Rien d'écrit : la note se déduit du transcript à chaque requête.
    assert!(conv.messages().iter().all(|m| m.text() != INTERRUPTED_NOTE));
}

/// La note ne vient que d'un arrêt pendant des outils, juste avant le propriétaire.
#[test]
fn the_interruption_note_follows_only_a_stopped_batch() {
    let calls = ChatMessage {
        tool_calls: vec![call("c1", "fs_read", json!({}))],
        ..ChatMessage::assistant("")
    };
    let stopped = vec![
        ChatMessage::system("préfixe"),
        ChatMessage::user("lis"),
        calls.clone(),
        ChatMessage::tool_result("c1", "fs_read", NOT_RUN_STOPPED),
        ChatMessage::user("reprends"),
    ];
    let mut messages = stopped.clone();
    interruption_note(&messages).unwrap().apply(&mut messages);
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[5].text(), INTERRUPTED_NOTE);
    let answered = vec![
        ChatMessage::user("lis"),
        calls,
        ChatMessage::tool_result("c1", "fs_read", "contenu"),
        ChatMessage::user("merci"),
    ];
    assert!(interruption_note(&answered).is_none());
    let later = [
        stopped,
        vec![ChatMessage::assistant("fait"), ChatMessage::user("et ?")],
    ]
    .concat();
    assert!(interruption_note(&later).is_none());
}
