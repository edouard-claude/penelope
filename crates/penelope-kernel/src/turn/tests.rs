use super::*;
use crate::clock::TestClock;
use serde_json::json;
use std::sync::Arc;

fn queue(store: Store, clock: TestClock) -> TurnQueue {
    TurnQueue::new(store, Arc::new(clock), 60_000)
}

#[tokio::test]
async fn enqueue_claim_complete() {
    let q = queue(Store::open_memory().unwrap(), TestClock::default());
    q.enqueue("s1", TurnKind::Message, json!({"t":"salut"}), None, 0)
        .await
        .unwrap();
    let t = q.claim("runner-1").await.unwrap().unwrap();
    assert_eq!(t.session_id, "s1");
    assert_eq!(t.attempts, 1);
    assert!(
        q.claim("runner-2").await.unwrap().is_none(),
        "session verrouillée"
    );
    q.complete(&t).await.unwrap();
    assert_eq!(q.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn dedup_key_prevents_double_enqueue() {
    let q = queue(Store::open_memory().unwrap(), TestClock::default());
    let a = q
        .enqueue("s1", TurnKind::Message, json!({}), Some("u42".into()), 0)
        .await
        .unwrap();
    let b = q
        .enqueue("s1", TurnKind::Message, json!({}), Some("u42".into()), 0)
        .await
        .unwrap();
    assert!(a.is_some());
    assert!(b.is_none(), "un update rejoué ne crée pas un second tour");
}

/// #161 : les messages Telegram distincts d'un même sujet forment un tour, sans
/// perdre leur identité, leur ordre ni leur clé de déduplication.
#[tokio::test]
async fn claim_merges_pending_messages_of_one_origin() {
    let clock = TestClock::default();
    let store = Store::open_memory().unwrap();
    let q = queue(store.clone(), clock.clone());
    let origin = |message_id| {
        json!({
            "channel": "telegram", "chat_id": 10, "topic_id": 7, "message_id": message_id
        })
    };
    let first = q
        .enqueue(
            "s1",
            TurnKind::Message,
            json!({"text":"un", "origin":origin(1)}),
            Some("tg:1".into()),
            0,
        )
        .await
        .unwrap()
        .unwrap();
    clock.advance_ms(1);
    let second = q
        .enqueue(
            "s1",
            TurnKind::Message,
            json!({"text":"deux", "origin":origin(2)}),
            Some("tg:2".into()),
            0,
        )
        .await
        .unwrap()
        .unwrap();
    clock.advance_ms(1);
    let third = q
        .enqueue(
            "s1",
            TurnKind::Message,
            json!({"text":"trois", "origin":origin(3)}),
            Some("tg:3".into()),
            0,
        )
        .await
        .unwrap()
        .unwrap();

    let turn = q.claim("r1").await.unwrap().unwrap();
    assert_eq!(turn.id, first);
    assert_eq!(
        turn.merged_messages
            .iter()
            .map(|m| m.payload["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["deux", "trois"]
    );
    for (id, key) in [(second, "tg:2"), (third, "tg:3")] {
        let id = id.to_string();
        let row: (String, Option<String>, String) = store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT state, merged_into, dedup_key FROM turn_queue WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(row, ("merged".into(), Some(first.to_string()), key.into()));
    }
    assert_eq!(
        q.last_merged_payload(first.as_str())
            .await
            .unwrap()
            .unwrap()["origin"]["message_id"],
        3
    );
    assert!(
        q.enqueue("s1", TurnKind::Message, json!({}), Some("tg:2".into()), 0)
            .await
            .unwrap()
            .is_none()
    );
    q.complete(&turn).await.unwrap();
    assert!(q.claim("r2").await.unwrap().is_none());
}

#[tokio::test]
async fn claim_keeps_sessions_origins_and_resume_priority_separate() {
    let q = queue(Store::open_memory().unwrap(), TestClock::default());
    let telegram = |id| {
        json!({"text":format!("m{id}"), "origin":{
            "channel":"telegram", "chat_id":10, "message_id":id
        }})
    };
    q.enqueue("s1", TurnKind::Message, telegram(1), None, 0)
        .await
        .unwrap();
    q.enqueue("s1", TurnKind::Message, telegram(2), None, 0)
        .await
        .unwrap();
    q.enqueue(
        "s1",
        TurnKind::Message,
        json!({"text":"cli", "origin":{"channel":"cli"}}),
        None,
        0,
    )
    .await
    .unwrap();
    q.enqueue("s2", TurnKind::Message, telegram(3), None, 0)
        .await
        .unwrap();
    q.enqueue("s2", TurnKind::Message, telegram(4), None, 0)
        .await
        .unwrap();
    q.enqueue(
        "s1",
        TurnKind::Resume,
        json!({"origin":{"channel":"telegram","chat_id":10}}),
        None,
        10,
    )
    .await
    .unwrap();

    let resume = q.claim("r1").await.unwrap().unwrap();
    assert_eq!(resume.kind, TurnKind::Resume);
    assert!(resume.merged_messages.is_empty());
    q.complete(&resume).await.unwrap();
    let a = q.claim("r1").await.unwrap().unwrap();
    assert_eq!(a.session_id, "s1");
    assert_eq!(a.merged_messages.len(), 1);
    q.complete(&a).await.unwrap();
    let b = q.claim("r1").await.unwrap().unwrap();
    assert_eq!(b.session_id, "s1");
    assert_eq!(b.payload["text"], "cli");
    assert!(b.merged_messages.is_empty());
    q.complete(&b).await.unwrap();
    let c = q.claim("r1").await.unwrap().unwrap();
    assert_eq!(c.session_id, "s2");
    assert_eq!(c.merged_messages.len(), 1);
}

#[tokio::test]
async fn a_live_turn_absorbs_pending_messages_and_recovery_keeps_them() {
    let store = Store::open_memory().unwrap();
    let clock = TestClock::default();
    let q = queue(store.clone(), clock.clone());
    let payload = |text| json!({"text":text, "origin":{"channel":"cli"}});
    q.enqueue("s1", TurnKind::Message, payload("premier"), None, 0)
        .await
        .unwrap();
    let turn = q.claim("r1").await.unwrap().unwrap();
    q.enqueue(
        "s1",
        TurnKind::Message,
        payload("ajout"),
        Some("u2".into()),
        0,
    )
    .await
    .unwrap();
    let absorbed = q.absorb_pending(&turn).await.unwrap();
    assert_eq!(absorbed.len(), 1);
    assert_eq!(absorbed[0].payload["text"], "ajout");
    assert!(q.absorb_pending(&turn).await.unwrap().is_empty());

    let restarted = queue(store, clock);
    restarted.recover_on_boot().await.unwrap();
    let replayed = restarted.claim("r2").await.unwrap().unwrap();
    assert_eq!(replayed.id, turn.id);
    assert_eq!(replayed.merged_messages.len(), 1);
    assert_eq!(replayed.merged_messages[0].id, absorbed[0].id);
}

#[tokio::test]
async fn a_message_after_completion_remains_a_separate_turn() {
    let q = queue(Store::open_memory().unwrap(), TestClock::default());
    let payload = |text| json!({"text":text, "origin":{"channel":"cli"}});
    q.enqueue("s1", TurnKind::Message, payload("premier"), None, 0)
        .await
        .unwrap();
    let first = q.claim("r1").await.unwrap().unwrap();
    q.complete(&first).await.unwrap();
    q.enqueue("s1", TurnKind::Message, payload("après"), None, 0)
        .await
        .unwrap();
    let second = q.claim("r1").await.unwrap().unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(second.payload["text"], "après");
    assert!(second.merged_messages.is_empty());
}

#[tokio::test]
async fn a_photo_message_is_not_absorbed_as_text() {
    let q = queue(Store::open_memory().unwrap(), TestClock::default());
    let origin = json!({"channel":"telegram", "chat_id":10});
    q.enqueue(
        "s1",
        TurnKind::Message,
        json!({"text":"question", "origin":origin}),
        None,
        0,
    )
    .await
    .unwrap();
    q.enqueue(
        "s1",
        TurnKind::Message,
        json!({"text":"regarde", "images":["photo.jpg"], "origin":origin}),
        None,
        0,
    )
    .await
    .unwrap();
    let first = q.claim("r1").await.unwrap().unwrap();
    assert!(first.merged_messages.is_empty());
    q.complete(&first).await.unwrap();
    let second = q.claim("r1").await.unwrap().unwrap();
    assert_eq!(second.payload["images"][0], "photo.jpg");
}

#[tokio::test]
async fn cancelling_a_merged_turn_cancels_every_original_message() {
    let store = Store::open_memory().unwrap();
    let q = queue(store.clone(), TestClock::default());
    let payload = |text| json!({"text":text, "origin":{"channel":"cli"}});
    q.enqueue("s1", TurnKind::Message, payload("un"), None, 0)
        .await
        .unwrap();
    q.enqueue("s1", TurnKind::Message, payload("deux"), None, 0)
        .await
        .unwrap();
    let turn = q.claim("r1").await.unwrap().unwrap();
    assert_eq!(turn.merged_messages.len(), 1);
    q.cancel_leased(&turn).await.unwrap();
    let count: i64 = store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM turn_queue WHERE state='cancelled'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 2);
    assert!(q.claim("r2").await.unwrap().is_none());
}

/// CA 3 : un tour interrompu par un crash est réclamé à nouveau après expiration
/// du lease. Le « kill -9 » est un autre processus, donc une autre file (le runner
/// d'origine n'est plus vivant nulle part, #43).
#[tokio::test]
async fn ca_3_3_expired_lease_is_reclaimed() {
    let clock = TestClock::default();
    let store = Store::open_memory().unwrap();
    let mort = queue(store.clone(), clock.clone());
    mort.enqueue("s1", TurnKind::Message, json!({}), None, 0)
        .await
        .unwrap();

    let t1 = mort.claim("runner-1").await.unwrap().unwrap();
    assert!(mort.claim("runner-2").await.unwrap().is_none());

    // « kill -9 » du processus : personne n'envoie plus de heartbeat.
    let vivant = queue(store, clock.clone());
    clock.advance_ms(61_000);
    let t2 = vivant.claim("runner-2").await.unwrap().unwrap();
    assert_eq!(t2.id, t1.id);
    assert_eq!(t2.attempts, 2, "le compteur de tentatives doit progresser");
}

/// #43 : l'écrivain a gelé 90 s (sauvegarde, réindexation, veille du Mac), aucun
/// battement n'est passé, mais r1 est vivant : son tour n'est pas repris et lui seul
/// le clôt.
#[tokio::test]
async fn a_live_runner_keeps_its_turn_when_the_writer_was_frozen() {
    let clock = TestClock::default();
    let q = queue(Store::open_memory().unwrap(), clock.clone());
    q.enqueue("s1", TurnKind::Message, json!({"t":"premier"}), None, 0)
        .await
        .unwrap();
    q.enqueue("s1", TurnKind::Message, json!({"t":"second"}), None, 0)
        .await
        .unwrap();

    let a = q.claim("r1").await.unwrap().unwrap();
    clock.advance_ms(91_000);
    assert!(
        q.claim("r2").await.unwrap().is_none(),
        "un runner vivant garde son tour même sans battement"
    );
    // L'écrivain repart : le battement retrouve son lease.
    q.heartbeat(&a).await.unwrap();
    q.complete(&a).await.unwrap();

    let b = q.claim("r2").await.unwrap().unwrap();
    assert_ne!(b.id, a.id, "le tour suivant de la session vient après");
}

/// #43 : un runner évincé (processus figé puis réveillé, tour repris ailleurs) ne
/// clôt pas le tour de son successeur et n'efface pas son lease.
#[tokio::test]
async fn an_evicted_runner_loses_its_lease_and_writes_nothing() {
    let clock = TestClock::default();
    let store = Store::open_memory().unwrap();
    let fige = queue(store.clone(), clock.clone());
    let repris = queue(store.clone(), clock.clone());
    fige.enqueue("s1", TurnKind::Message, json!({"t":"premier"}), None, 0)
        .await
        .unwrap();
    fige.enqueue("s1", TurnKind::Message, json!({"t":"second"}), None, 0)
        .await
        .unwrap();

    let a_r1 = fige.claim("r1").await.unwrap().unwrap();
    clock.advance_ms(61_000);
    let a_r2 = repris.claim("r2").await.unwrap().unwrap();
    assert_eq!(a_r1.id, a_r2.id, "r2 reprend le tour laissé sans battement");

    let e = fige.complete(&a_r1).await.unwrap_err();
    assert!(e.is_lease_lost(), "clôture refusée à l'évincé : {e}");
    let e = fige.heartbeat(&a_r1).await.unwrap_err();
    assert!(e.is_lease_lost(), "battement refusé à l'évincé : {e}");

    // Le verrou de session tient : le second tour attend la fin du premier.
    assert!(
        repris.claim("r3").await.unwrap().is_none(),
        "aucun autre tour de la session pendant que r2 exécute"
    );
    let id = a_r1.id.0.clone();
    let state: String = store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT state FROM turn_queue WHERE id=?1",
                [id.as_str()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(state, "leased", "le tour reste à r2");

    // r2 le termine : la session se libère pour le second tour.
    repris.complete(&a_r2).await.unwrap();
    assert!(repris.claim("r3").await.unwrap().is_some());
}

#[tokio::test]
async fn heartbeat_prevents_reclaim() {
    let clock = TestClock::default();
    let q = queue(Store::open_memory().unwrap(), clock.clone());
    q.enqueue("s1", TurnKind::Message, json!({}), None, 0)
        .await
        .unwrap();
    let t = q.claim("r1").await.unwrap().unwrap();
    for _ in 0..5 {
        clock.advance_ms(30_000);
        q.heartbeat(&t).await.unwrap();
    }
    assert!(
        q.claim("r2").await.unwrap().is_none(),
        "un runner vivant garde son tour"
    );
}

/// CA 3 : 4 sessions concurrentes s'exécutent sans interblocage.
#[tokio::test]
async fn ca_3_2_four_sessions_run_concurrently() {
    let q = queue(Store::open_memory().unwrap(), TestClock::default());
    for i in 0..4 {
        q.enqueue(&format!("s{i}"), TurnKind::Message, json!({}), None, 0)
            .await
            .unwrap();
    }
    let mut claimed = Vec::new();
    for i in 0..4 {
        let t = q.claim(&format!("runner-{i}")).await.unwrap();
        assert!(t.is_some(), "le runner {i} doit obtenir un tour");
        claimed.push(t.unwrap());
    }
    let mut sids: Vec<_> = claimed.iter().map(|t| t.session_id.clone()).collect();
    sids.sort();
    assert_eq!(sids, vec!["s0", "s1", "s2", "s3"]);
    assert!(q.claim("runner-5").await.unwrap().is_none());
}

#[tokio::test]
async fn priority_is_respected() {
    let q = queue(Store::open_memory().unwrap(), TestClock::default());
    q.enqueue("a", TurnKind::Message, json!({"p":0}), None, 0)
        .await
        .unwrap();
    q.enqueue("b", TurnKind::Resume, json!({"p":9}), None, 9)
        .await
        .unwrap();
    let t = q.claim("r").await.unwrap().unwrap();
    assert_eq!(t.session_id, "b");
}

#[tokio::test]
async fn recover_on_boot_releases_everything() {
    let store = Store::open_memory().unwrap();
    let q = queue(store.clone(), TestClock::default());
    q.enqueue("s1", TurnKind::Message, json!({}), None, 0)
        .await
        .unwrap();
    q.claim("r1").await.unwrap().unwrap();

    let q2 = queue(store, TestClock::default());
    assert_eq!(q2.recover_on_boot().await.unwrap(), 1);
    assert!(q2.claim("r2").await.unwrap().is_some());
}
