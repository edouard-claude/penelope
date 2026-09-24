use super::*;
use crate::derive::{Sealed, Slot, derive};
use crate::journal::KIND_SUMMARY;
use crate::lcm::Lcm;
use crate::store::HistoryStore;
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_kernel::event::{Event, EventDraft, EventLog};
use penelope_llm::catalog::Catalog;
use penelope_llm::tokens::TokenEstimator;
use penelope_llm::types::ChatMessage;
use penelope_store::Store;
use serde_json::json;
use std::sync::Arc;

/// Un moteur dont l'historique est journalisé (double écriture, T5).
async fn journaled() -> (ContextEngine, EventLog) {
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
    let engine = ContextEngine::new(
        HistoryStore::new(store.clone(), clock.clone()).with_events(log.clone()),
        Lcm::new(store, clock.clone()),
        TokenEstimator::new(),
        Catalog::new(),
        clock,
    );
    (engine, log)
}

fn params() -> CompactionParams {
    CompactionParams {
        window: 40_000,
        threshold: 0.70,
        tail_ratio: 0.025,
        tail_min_tokens: 1_000,
        tail_max_tokens: 25_000,
        min_tail_user_messages: 2,
        max_tool_result_share: 0.25,
        large_payload_tokens: 25_000,
        background_margin: 0.10,
        max_prompt_tokens: 0,
    }
}

/// Des échanges entrecoupés d'événements d'observation : les adresses du journal
/// s'écartent des numéros V0, comme dans un vrai tour.
async fn exchanges(e: &ContextEngine, log: &EventLog, range: std::ops::Range<usize>) {
    for i in range {
        log.append(EventDraft::new("turn.started", json!({})).session("s1"))
            .await
            .unwrap();
        e.history
            .append(
                "s1",
                &ChatMessage::user(format!("demande {i} sur PROJ-{i}")),
                500,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
}

async fn job(e: &ContextEngine) -> SummaryJob {
    e.prepare_summary("s1", &params(), 128_000, "m", false)
        .await
        .unwrap()
        .expect("un travail de résumé")
}

async fn events(log: &EventLog) -> Vec<Event> {
    log.session_events("s1", 0).await.unwrap()
}

fn summaries(events: &[Event]) -> Vec<&Event> {
    events.iter().filter(|e| e.kind == KIND_SUMMARY).collect()
}

/// Le `event_id` d'un nœud LCM.
async fn node_event(e: &ContextEngine, node: &str) -> Option<i64> {
    let node = node.to_string();
    e.history
        .store()
        .read(move |c| {
            Ok(
                c.query_row("SELECT event_id FROM lcm_nodes WHERE id=?1", [node], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_summary_is_journaled_as_the_replace_of_its_range() {
    let (e, log) = journaled().await;
    exchanges(&e, &log, 0..30).await;
    let job = job(&e).await;
    let summary = json!({"objectif": "suivre", "fait": "trente demandes"});
    let node = e
        .apply_summary_as(&job, &summary, "m", "manual")
        .await
        .unwrap();

    let all = events(&log).await;
    let written = summaries(&all);
    assert_eq!(written.len(), 1);
    let p = &written[0].payload;
    let lcm = e.lcm.get(&node).await.unwrap().unwrap();
    assert_eq!(p["node_id"], node);
    assert_eq!(p["summary"], lcm.summary, "le même texte que le nœud");
    assert_eq!(p["trigger"], "manual");
    assert_eq!(p["idempotency_key"], job.idempotency_key());
    assert_eq!(node_event(&e, &node).await, Some(written[0].id));
    // Les bornes sont des adresses du journal, pas des numéros V0.
    let from = e.history.address("s1", job.from_seq).await.unwrap();
    let to = e.history.address("s1", job.to_seq).await.unwrap();
    assert_ne!((from, to), (job.from_seq, job.to_seq));
    assert_eq!(
        p["surface"],
        json!({"op": "replace", "from": from, "to": to})
    );

    // Le pliage accepte les bornes : elles désignent des nœuds présents.
    let surface = derive(&Sealed::none(), &all).expect("bornes valides");
    assert!(matches!(surface.nodes[0], Slot::Summary(_)));
    let masked = e
        .history
        .load("s1", 0)
        .await
        .unwrap()
        .iter()
        .filter(|m| m.compacted)
        .count();
    assert_eq!(
        surface.masked.len(),
        masked,
        "même couverture que le canonique"
    );
}

#[tokio::test]
async fn republishing_the_same_job_writes_no_second_event() {
    let (e, log) = journaled().await;
    exchanges(&e, &log, 0..30).await;
    let job = job(&e).await;
    let summary = json!({"objectif": "suivre"});
    let a = e.apply_summary(&job, &summary, "m").await.unwrap();
    let b = e.apply_summary(&job, &summary, "m").await.unwrap();
    assert_eq!(a, b);
    assert_eq!(summaries(&events(&log).await).len(), 1);
}

#[tokio::test]
async fn a_stop_between_the_event_and_the_node_is_repaired_from_the_event() {
    let (e, log) = journaled().await;
    exchanges(&e, &log, 0..30).await;
    let job = job(&e).await;
    let summary = json!({"objectif": "suivre"});
    let node = e.apply_summary(&job, &summary, "m").await.unwrap();
    // L'événement est commité, la seconde transaction n'a pas eu lieu.
    e.history
        .store()
        .write(|tx| {
            tx.execute("DELETE FROM lcm_nodes", [])?;
            tx.execute("UPDATE messages SET compacted = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();

    let again = e.apply_summary(&job, &summary, "m").await.unwrap();
    assert_eq!(again, node, "le nœud annoncé par l'événement");
    let all = events(&log).await;
    assert_eq!(summaries(&all).len(), 1);
    assert_eq!(node_event(&e, &node).await, Some(summaries(&all)[0].id));
    let history = e.history.load("s1", 0).await.unwrap();
    assert!(
        history
            .iter()
            .all(|m| m.compacted == (m.seq >= job.chunk_from_seq && m.seq <= job.to_seq))
    );
}

#[tokio::test]
async fn an_extension_replaces_the_previous_summary_and_what_follows() {
    let (e, log) = journaled().await;
    exchanges(&e, &log, 0..30).await;
    let first = job(&e).await;
    let one = e
        .apply_summary(&first, &json!({"objectif": "suivre"}), "m")
        .await
        .unwrap();
    exchanges(&e, &log, 30..60).await;
    let second = job(&e).await;
    assert_eq!(second.previous_node_id.as_deref(), Some(one.as_str()));
    let two = e
        .apply_summary(&second, &json!({"objectif": "suivre", "fait": "tout"}), "m")
        .await
        .unwrap();

    let all = events(&log).await;
    let written = summaries(&all);
    assert_eq!(written.len(), 2);
    assert_eq!(written[1].payload["previous_node_id"], one);
    assert_eq!(node_event(&e, &two).await, Some(written[1].id));
    let surface = derive(&Sealed::none(), &all).expect("bornes valides");
    let visible: Vec<_> = surface
        .nodes
        .iter()
        .filter(|s| matches!(s, Slot::Summary(_)))
        .collect();
    assert_eq!(
        visible.len(),
        1,
        "un seul résumé vivant, comme dans lcm_nodes"
    );
    let Slot::Summary(key) = *visible[0] else {
        unreachable!()
    };
    assert_eq!(surface.summaries[&key].node_id, two);
}

#[tokio::test]
async fn without_a_journal_nothing_is_journaled_and_the_node_has_no_event() {
    let (e, log) = journaled().await;
    let bare = ContextEngine::new(
        HistoryStore::new(e.history.store().clone(), Arc::new(TestClock::default())),
        e.lcm.clone(),
        TokenEstimator::new(),
        Catalog::new(),
        Arc::new(TestClock::default()),
    );
    exchanges(&bare, &log, 0..30).await;
    let job = job(&bare).await;
    let node = bare
        .apply_summary(&job, &json!({"objectif": "suivre"}), "m")
        .await
        .unwrap();
    assert!(summaries(&events(&log).await).is_empty());
    assert_eq!(node_event(&bare, &node).await, None);
}

#[tokio::test]
async fn an_externalised_result_is_journaled_as_the_replace_of_its_node() {
    let (e, log) = journaled().await;
    let h = &e.history;
    log.append(EventDraft::new("turn.started", json!({})).session("s1"))
        .await
        .unwrap();
    h.append("s1", &ChatMessage::user("lis tout"), 5, 0, false, None)
        .await
        .unwrap();
    let call = |id: &str| penelope_llm::types::ToolCall {
        id: id.into(),
        name: "fs_read".into(),
        arguments: json!({}),
    };
    let calls = ChatMessage::assistant("").with_tool_calls(vec![call("c1"), call("c2")]);
    h.append("s1", &calls, 10, 0, false, None).await.unwrap();
    let big = "y".repeat(500_000);
    let one = ChatMessage::tool_result("c1", "fs_read", &big);
    let seq = h.append("s1", &one, 140_000, 0, false, None).await.unwrap();
    let two = ChatMessage::tool_result("c2", "fs_read", "court");
    let small = h.append("s1", &two, 2, 0, false, None).await.unwrap();

    let steps = e
        .admit_tool_group("s1", &[(seq, big), (small, "court".into())], &params(), "m")
        .await
        .unwrap();
    assert_eq!(steps.len(), 1, "seul le gros résultat part en artefact");

    let all = events(&log).await;
    let replaced: Vec<&Event> = all
        .iter()
        .filter(|ev| ev.kind == crate::journal::KIND_TOOL_RESULT)
        .filter(|ev| ev.payload["surface"]["op"] == "replace")
        .collect();
    assert_eq!(replaced.len(), 1);
    let p = &replaced[0].payload;
    let address = h.address("s1", seq).await.unwrap();
    assert_ne!(address, seq, "une adresse du journal, pas un numéro V0");
    assert_eq!(
        p["surface"],
        json!({"op": "replace", "from": address, "to": address})
    );
    assert_eq!(p["call_id"], "c1", "le call_id du nœud remplacé");
    // L'artefact que l'événement cite existe, avec la même empreinte.
    let artifact = h
        .get_artifact(p["artifact_id"].as_str().unwrap())
        .await
        .unwrap()
        .expect("l'artefact est écrit avant l'événement");
    assert_eq!(p["artifact_sha256"], artifact.sha256);
    assert_eq!(p["original_tokens"].as_u64(), Some(steps[0].before_tokens));

    // Le pliage accepte le remplacement et redonne le corps de la ligne.
    let surface = derive(&Sealed::none(), &all).expect("bornes valides");
    let rows = h.load("s1", 0).await.unwrap();
    let row = rows.iter().find(|r| r.seq == seq).unwrap();
    let node = &surface.messages[&address];
    assert_eq!(node.message.content, row.message.content);
    assert_eq!(node.artifact_id, row.artifact_id);
    assert_eq!(surface.nodes.len(), rows.len(), "le nœud garde sa place");
}
