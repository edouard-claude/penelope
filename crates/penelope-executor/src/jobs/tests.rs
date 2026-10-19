use super::*;
use penelope_kernel::clock::TestClock;

fn job(tool: &str, text: &str) -> ToolJob {
    ToolJob {
        id: "tj_1".into(),
        session_id: "s1".into(),
        run_id: None,
        turn_id: None,
        call_id: None,
        tool: tool.into(),
        request: json!({"prompt": "cherche"}),
        state: TaskState::Completed,
        result: Some(json!({"text": text})),
        effect_id: None,
        delivered_at: None,
        created_at: "2026-10-19T10:00:00Z".into(),
        updated_at: "2026-10-19T10:00:00Z".into(),
    }
}

/// #396 : la note d'un sous-agent arrive bornée à 400 caractères, citée comme donnée,
/// avec le total et le chemin vers la suite ; un job shell garde sa sortie.
#[test]
fn a_child_note_is_cut_at_400_characters_and_quoted() {
    let long = "x".repeat(5_000);
    let text = delivery_text(&job("sub_agent_spawn", &long), 0);
    assert!(text.contains("(400 premiers caractères sur 5000"), "{text}");
    assert!(text.contains("job_status"), "{text}");
    assert!(text.contains("DONNÉES NON FIABLES"), "{text}");
    assert!(!text.contains(&"x".repeat(401)), "tronquée");
    let short = delivery_text(&job("sub_agent_spawn", "fini"), 0);
    assert!(!short.contains("premiers caractères"), "{short}");
    let shell = delivery_text(&job("shell_exec", &long), 0);
    assert!(shell.contains(&"x".repeat(5_000)), "sortie shell gardée");
}

/// #396 : dix réveils par des résultats sans message du propriétaire, puis les résultats
/// attendent ; un message du propriétaire remet le compte à zéro ; un tour en file fait
/// attendre.
#[tokio::test]
async fn ten_wakes_without_the_owner_then_results_wait() {
    let dir = tempfile::tempdir().unwrap();
    let s = Services::for_tests(dir.path().to_path_buf(), Arc::new(TestClock::default()))
        .await
        .unwrap();
    let row = |kind: &'static str, dedup: String, at: i64| {
        let store = s.store.clone();
        async move {
            store
                .write(move |tx| {
                    Ok(tx.execute(
                        "INSERT INTO turn_queue(id, session_id, kind, payload, state, enqueued_at, dedup_key)
                         VALUES (?1, 's1', ?2, '{}', 'done', ?3, nullif(?4, ''))",
                        penelope_store::rusqlite::params![
                            format!("t{at}"), kind, format!("2026-10-19T10:00:{at:02}Z"), dedup
                        ],
                    )?)
                })
                .await
                .unwrap();
        }
    };
    row("message", String::new(), 0).await;
    for i in 1..=9 {
        row("nudge", format!("job:tj_{i}"), i).await;
    }
    assert_eq!(wake_gate(&s, "s1").await.unwrap(), WakeGate::Go);
    row("trigger", String::new(), 10).await;
    row("nudge", "job:tj_10".into(), 11).await;
    assert_eq!(consecutive_wakes(&s, "s1").await.unwrap(), 10);
    assert_eq!(wake_gate(&s, "s1").await.unwrap(), WakeGate::Capped);
    row("message", String::new(), 12).await;
    assert_eq!(wake_gate(&s, "s1").await.unwrap(), WakeGate::Go);

    s.turns
        .enqueue(
            "s1",
            penelope_kernel::turn::TurnKind::Message,
            json!({}),
            None,
            0,
        )
        .await
        .unwrap();
    assert_eq!(wake_gate(&s, "s1").await.unwrap(), WakeGate::Wait);
}
