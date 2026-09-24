use super::*;
use crate::derive::{Sealed, derive};
use crate::journal::{KIND_REWIND, UserSource};
use penelope_kernel::clock::TestClock;

async fn journaled() -> (HistoryStore, EventLog) {
    let store = Store::open_memory().unwrap();
    store
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                 VALUES('s1','chat','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let clock: SharedClock = Arc::new(TestClock::default());
    let log = EventLog::new(store.clone(), clock.clone());
    (
        HistoryStore::new(store, clock).with_events(log.clone()),
        log,
    )
}

/// `(seq, event_id)` de chaque ligne de la session.
async fn rows(h: &HistoryStore) -> Vec<(i64, Option<i64>)> {
    h.store()
        .read(|c| {
            let mut st =
                c.prepare("SELECT seq, event_id FROM messages WHERE session_id='s1' ORDER BY seq")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .await
        .unwrap()
}

/// Chaque ligne cite son événement, et l'événement redonne le même message.
async fn assert_each_row_has_its_event(h: &HistoryStore, log: &EventLog) {
    let events = log.session_events("s1", 0).await.unwrap();
    let loaded = h.load("s1", 0).await.unwrap();
    let rows = rows(h).await;
    assert_eq!(rows.len(), loaded.len());
    for ((seq, event_id), entry) in rows.iter().zip(&loaded) {
        let ev = events
            .iter()
            .find(|e| Some(e.id) == *event_id)
            .unwrap_or_else(|| panic!("ligne {seq} sans événement : {event_id:?}"));
        let surface = derive(&Sealed::none(), std::slice::from_ref(ev)).unwrap();
        let derived = surface.entries();
        assert_eq!(derived.len(), 1, "{}", ev.kind);
        assert_eq!(derived[0].message, entry.message, "ligne {seq}");
        assert_eq!(derived[0].tokens, entry.tokens);
        assert_eq!(derived[0].episode, entry.episode);
        assert_eq!(derived[0].eager, entry.eager);
    }
}

#[tokio::test]
async fn every_message_is_written_with_its_event() {
    let (h, log) = journaled().await;
    h.append_user_turn_at("s1", "q1", "lis le fichier", "2026-09-24T08:00:00Z", 4, 1)
        .await
        .unwrap();
    let call = ToolCall {
        id: "c1".into(),
        name: "fs_read".into(),
        arguments: json!({"path": "a.rs"}),
    };
    let answer = ChatMessage {
        reasoning: Some("je lis".into()),
        ..ChatMessage::assistant("")
    }
    .with_tool_calls(vec![call]);
    let prov = Provenance {
        turn: Some("q1".into()),
        step: 1,
        ..Default::default()
    };
    h.append_as("s1", &answer, 7, 1, false, None, &prov)
        .await
        .unwrap();
    let result = ChatMessage::tool_result("c1", "fs_read", "fn main() {}");
    h.append("s1", &result, 5, 1, true, None).await.unwrap();
    h.append(
        "s1",
        &ChatMessage::assistant("c'est un main vide"),
        6,
        1,
        false,
        None,
    )
    .await
    .unwrap();

    assert_each_row_has_its_event(&h, &log).await;
    let events = log.session_events("s1", 0).await.unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "conv.user",
            "conv.assistant",
            "conv.tool_result",
            "conv.assistant"
        ]
    );
    assert_eq!(events[0].payload["arrived_at"], "2026-09-24T08:00:00Z");
    assert_eq!(events[0].payload["turn_message_id"], "q1");
    assert_eq!(events[1].payload["turn"], "q1");
    assert_eq!(events[1].payload["step"], 1);
    assert_eq!(events[2].payload["eager"], true);
    // Le journal plié redonne tout l'historique, dans l'ordre.
    let surface = derive(&Sealed::none(), &events).unwrap();
    let loaded = h.load("s1", 0).await.unwrap();
    let derived: Vec<_> = surface.entries().into_iter().map(|e| e.message).collect();
    let tables: Vec<_> = loaded.into_iter().map(|e| e.message).collect();
    assert_eq!(derived, tables);
}

/// #161, et §2.7 : un message de la file rejoué ne s'écrit ni ne se journalise deux
/// fois ; un crash entre l'événement et la ligne ne réécrit que la ligne.
#[tokio::test]
async fn a_queued_message_is_written_once_even_across_a_crash() {
    let (h, log) = journaled().await;
    let first = h
        .append_user_turn_at("s1", "q1", "bonjour", "2026-09-24T08:00:00Z", 2, 0)
        .await
        .unwrap();
    let again = h
        .append_user_turn_at("s1", "q1", "bonjour", "2026-09-24T08:00:00Z", 2, 0)
        .await
        .unwrap();
    assert_eq!(first, again);

    // Crash simulé : l'événement de q2 est commité, sa ligne jamais écrite.
    let prov = Provenance::queued(UserSource::Merged, "q2", "2026-09-24T08:01:00Z");
    let lost = message_event(&ChatMessage::user("encore"), 1, 0, false, &prov).unwrap();
    let ev = log
        .append(EventDraft::new(lost.kind(), lost.payload()).session("s1"))
        .await
        .unwrap();
    h.append_queued("s1", &ChatMessage::user("encore"), 1, 0, &prov)
        .await
        .unwrap();

    let events = log.session_events("s1", 0).await.unwrap();
    assert_eq!(events.len(), 2, "aucun événement en double");
    let rows = rows(&h).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[1].1,
        Some(ev.id),
        "la ligne réparée cite l'événement existant"
    );
    assert_each_row_has_its_event(&h, &log).await;
}

/// Sans journal attaché, rien ne change : la ligne s'écrit seule.
#[tokio::test]
async fn without_a_journal_the_row_is_written_alone() {
    let store = Store::open_memory().unwrap();
    let h = HistoryStore::new(store, Arc::new(TestClock::default()));
    h.store()
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                 VALUES('s1','chat','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    h.append("s1", &ChatMessage::user("seul"), 1, 0, false, None)
        .await
        .unwrap();
    assert_eq!(rows(&h).await, [(1, None)]);
}

fn tiers(context: &str) -> Tiers {
    Tiers {
        identity: "Tu es Pénélope.".into(),
        index: "outils".into(),
        context: context.into(),
        volatile: String::new(),
    }
}

/// T6 : le préfixe entre au journal en entier, une fois par changement, avec sa raison.
#[tokio::test]
async fn the_prefix_is_journaled_once_per_change_with_its_reason() {
    let (h, log) = journaled().await;
    let reason = |r: Option<SystemReason>| r.map(|r| format!("{r:?}"));
    let first = h.journal_system("s1", &tiers("a")).await.unwrap();
    assert_eq!(reason(first).as_deref(), Some("First"));
    assert_eq!(h.journal_system("s1", &tiers("a")).await.unwrap(), None);
    let cold = h.journal_system("s1", &tiers("b")).await.unwrap();
    assert_eq!(reason(cold).as_deref(), Some("Cold"));
    log.append(EventDraft::new("context.compacted", json!({})).session("s1"))
        .await
        .unwrap();
    let after = h.journal_system("s1", &tiers("c")).await.unwrap();
    assert_eq!(reason(after).as_deref(), Some("Compaction"));

    let events = log.session_events("s1", 0).await.unwrap();
    let systems: Vec<_> = events.iter().filter(|e| e.kind == KIND_SYSTEM).collect();
    assert_eq!(systems.len(), 3);
    assert_eq!(systems[0].payload["surface"], json!({"op": "append"}));
    assert_eq!(systems[0].payload["reason"], "first");
    assert_eq!(systems[0].payload["rendered"], tiers("a").prefix());
    assert_eq!(
        systems[1].payload["surface"],
        json!({"op": "replace", "from": systems[0].seq, "to": systems[0].seq})
    );
    assert_eq!(systems[2].payload["reason"], "compaction");
    // Le pliage retient le dernier.
    let surface = derive(&Sealed::none(), &events).unwrap();
    assert_eq!(surface.system.unwrap().rendered, tiers("c").prefix());
}

/// T6 : le contexte figé est journalisé une fois, sur l'adresse du `conv.user` visé.
#[tokio::test]
async fn the_frozen_context_is_journaled_once_on_its_message() {
    let (h, log) = journaled().await;
    h.append("s1", &ChatMessage::assistant("avant"), 1, 0, false, None)
        .await
        .unwrap();
    let seq = h
        .append("s1", &ChatMessage::user("bonjour"), 1, 0, false, None)
        .await
        .unwrap();
    assert!(
        h.freeze_context("s1", seq, "<contexte>\nlundi\n</contexte>\n\n")
            .await
            .unwrap()
    );
    assert!(!h.freeze_context("s1", seq, "autre").await.unwrap());

    let events = log.session_events("s1", 0).await.unwrap();
    let contexts: Vec<_> = events.iter().filter(|e| e.kind == "conv.context").collect();
    assert_eq!(contexts.len(), 1);
    let user = events.iter().find(|e| e.kind == KIND_USER).unwrap();
    assert_eq!(contexts[0].payload["target"], user.seq);
    let surface = derive(&Sealed::none(), &events).unwrap();
    assert_eq!(
        surface.contexts.get(&user.seq).map(String::as_str),
        Some("<contexte>\nlundi\n</contexte>\n\n")
    );
    assert_eq!(
        h.contexts("s1").await.unwrap().get(&seq).unwrap(),
        "<contexte>\nlundi\n</contexte>\n\n"
    );
}

/// T10 : une fille hérite par référence du préfixe de sa mère ; ses propres événements
/// (préfixe système, contexte figé, coupe) visent des adresses que le pliage retrouve.
#[tokio::test]
async fn a_fork_addresses_its_own_events_after_what_it_inherits() {
    let (h, log) = journaled().await;
    h.store()
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, created_at, updated_at)
                 VALUES('s2','chat','t','t')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    h.journal_system("s1", &tiers("a")).await.unwrap();
    h.append("s1", &ChatMessage::user("un"), 1, 0, false, None)
        .await
        .unwrap();
    h.append("s1", &ChatMessage::assistant("1"), 1, 0, false, None)
        .await
        .unwrap();

    h.journal_fork("s2", "s1").await.unwrap();
    h.copy_messages("s1", "s2", 0, None).await.unwrap();
    let first = h.journal_system("s2", &tiers("a")).await.unwrap();
    assert_eq!(first, Some(SystemReason::First), "le premier de la fille");
    let seq = h
        .append("s2", &ChatMessage::user("deux"), 1, 0, false, None)
        .await
        .unwrap();
    h.freeze_context("s2", seq, "<contexte/>").await.unwrap();
    h.append("s2", &ChatMessage::assistant("2"), 1, 0, false, None)
        .await
        .unwrap();

    let mother = log.session_events("s1", 0).await.unwrap();
    let child = log.session_events("s2", 0).await.unwrap();
    let fork = child.iter().find(|e| e.kind == KIND_FORK).unwrap();
    let up_to = fork.payload["up_to"].as_i64().unwrap();
    assert_eq!(up_to, mother.last().unwrap().seq);
    let system = child.iter().find(|e| e.kind == KIND_SYSTEM).unwrap();
    let inherited = mother.iter().find(|e| e.kind == KIND_SYSTEM).unwrap();
    assert_eq!(
        system.payload["surface"],
        json!({"op": "replace", "from": inherited.seq, "to": inherited.seq})
    );
    let address = h.address("s2", seq).await.unwrap();
    assert_eq!(
        address,
        up_to + child.iter().find(|e| e.kind == KIND_USER).unwrap().seq
    );
    // Une ligne copiée garde l'adresse qu'elle a chez la mère.
    assert_eq!(
        h.address("s2", 1).await.unwrap(),
        h.address("s1", 1).await.unwrap()
    );

    let prefix = Sealed::fork("s1", &Sealed::none(), &mother, up_to).unwrap();
    let surface = derive(&prefix, &child).expect("bornes valides");
    assert_eq!(surface.nodes.len(), 4);
    assert_eq!(
        surface.contexts.get(&address).map(String::as_str),
        Some("<contexte/>")
    );
    // Le contexte désigne le message écrit après le fork, pas un nœud du préfixe.
    assert_eq!(surface.messages[&address].message.text(), "deux");
    assert!(address > up_to);

    // La coupe vise le nœud qui précède le message retiré.
    let removed = h.rewind_from("s2", seq, 1, None).await.unwrap();
    assert_eq!(removed, 2);
    let child = log.session_events("s2", 0).await.unwrap();
    let cut = child.iter().find(|e| e.kind == KIND_REWIND).unwrap();
    let before = h.address("s2", seq - 1).await.unwrap();
    assert_eq!(
        cut.payload["surface"],
        json!({"op": "cut", "after": before})
    );
    let surface = derive(&prefix, &child).expect("bornes valides");
    assert_eq!(surface.nodes.len(), 2);
    assert_eq!(h.load("s2", 0).await.unwrap().len(), 2);
}

/// T14 : l'instantané du prompt (#205) est un cache du `conv.system`, tenu par le
/// projecteur : écrit avec l'événement, refait par le rattrapage et par la refonte.
#[tokio::test]
async fn the_prompt_snapshot_follows_the_system_event() {
    let (h, _log) = journaled().await;
    h.journal_system("s1", &tiers("a")).await.unwrap();
    let hash = penelope_kernel::canonical::sha256_hex(tiers("a").prefix().as_bytes());
    let snapshot = |h: &HistoryStore| {
        let hash = hash.clone();
        let store = h.store().clone();
        async move {
            store
                .read(move |c| {
                    Ok(c.query_row(
                        "SELECT rendered, uses FROM prompt_snapshots WHERE hash = ?1",
                        [hash],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                    )
                    .optional()?)
                })
                .await
                .unwrap()
        }
    };
    assert_eq!(snapshot(&h).await, Some((tiers("a").prefix(), 0)));
    let erase = |h: &HistoryStore| {
        let store = h.store().clone();
        async move {
            store
                .write(|tx| {
                    tx.execute("DELETE FROM prompt_snapshots", [])?;
                    tx.execute("DELETE FROM projections_session", [])?;
                    Ok(())
                })
                .await
                .unwrap()
        }
    };
    erase(&h).await;
    h.catch_up("s1").await.unwrap();
    assert_eq!(snapshot(&h).await, Some((tiers("a").prefix(), 0)));
    erase(&h).await;
    h.reindex(Some("s1")).await.unwrap();
    assert_eq!(snapshot(&h).await, Some((tiers("a").prefix(), 0)));
}
