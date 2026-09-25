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
