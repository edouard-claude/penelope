//! `append_in` et `append_with` (épopée #208, T2) : la chaîne et l'ordre de diffusion
//! restent ceux de `append`.

use super::*;
use crate::clock::TestClock;
use penelope_store::StoreError;
use std::sync::Arc;

fn log() -> EventLog {
    EventLog::new(
        Store::open_memory().unwrap(),
        Arc::new(TestClock::default()),
    )
}

async fn scratch(l: &EventLog) {
    l.store()
        .write(|tx| {
            tx.execute_batch("CREATE TABLE cache(event_id INTEGER, kind TEXT);")?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn cache_rows(l: &EventLog) -> Vec<(i64, String)> {
    l.store()
        .read(|c| {
            let mut st = c.prepare("SELECT event_id, kind FROM cache ORDER BY event_id")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn after_sees_the_committed_event_and_the_chain_holds() {
    let l = log();
    scratch(&l).await;
    let mut live = l.subscribe();
    l.append(EventDraft::new("turn.started", json!({})).session("s1"))
        .await
        .unwrap();
    let ev = l
        .append_with(
            EventDraft::new("conv.user", json!({"v": 1})).session("s1"),
            |tx, ev| {
                tx.execute(
                    "INSERT INTO cache(event_id, kind) VALUES(?1, ?2)",
                    params![ev.id, ev.kind],
                )?;
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(ev.seq, 2);
    assert_eq!(cache_rows(&l).await, vec![(ev.id, "conv.user".to_string())]);
    assert_eq!(live.recv().await.unwrap().seq, 1);
    assert_eq!(live.recv().await.unwrap(), ev);
    let r = l.verify().await.unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.checked, 2);
}

/// Un `after` en erreur laisse l'événement commité (la vérité), annule ses seules
/// écritures et renvoie l'erreur.
#[tokio::test]
async fn a_failing_after_keeps_the_event_and_returns_the_error() {
    let l = log();
    scratch(&l).await;
    let mut live = l.subscribe();
    let e = l
        .append_with(
            EventDraft::new("conv.user", json!({"v": 1})).session("s1"),
            |tx, ev| {
                tx.execute(
                    "INSERT INTO cache(event_id, kind) VALUES(?1, ?2)",
                    params![ev.id, ev.kind],
                )?;
                Err(StoreError::other("projection impossible"))
            },
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("projection impossible"), "{e}");
    assert!(
        cache_rows(&l).await.is_empty(),
        "l'écriture de after est annulée"
    );
    let kept = l.session_events("s1", 0).await.unwrap();
    assert_eq!(kept.len(), 1, "l'événement reste dans le journal");
    assert_eq!(live.recv().await.unwrap(), kept[0], "et il est diffusé");
    assert!(l.verify().await.unwrap().ok);
}

/// `append_in` suit la transaction de l'appelant : annulée, rien n'est écrit ;
/// validée, l'événement est chaîné comme les autres.
#[tokio::test]
async fn append_in_follows_the_callers_transaction() {
    let l = log();
    l.append(EventDraft::new("turn.started", json!({})).session("s1"))
        .await
        .unwrap();
    let inner = l.clone();
    let refused = l
        .store()
        .write(move |tx| {
            inner.append_in(tx, EventDraft::new("conv.user", json!({})).session("s1"))?;
            Err::<(), _>(StoreError::other("annulé"))
        })
        .await;
    assert!(refused.is_err());
    assert_eq!(l.count().await.unwrap(), 1, "rien n'a été écrit");

    let inner = l.clone();
    let (a, b) = l
        .store()
        .write(move |tx| {
            let a = inner.append_in(tx, EventDraft::new("conv.user", json!({})).session("s1"))?;
            let b = inner.append_in(tx, EventDraft::new("x", json!({})).session("s2"))?;
            Ok((a, b))
        })
        .await
        .unwrap();
    assert_eq!((a.seq, b.seq), (2, 1));
    assert_eq!(b.prev_hash, a.hash);
    let c = l
        .append(EventDraft::new("turn.finished", json!({})).session("s1"))
        .await
        .unwrap();
    assert_eq!((c.seq, c.prev_hash.as_str()), (3, b.hash.as_str()));
    let r = l.verify().await.unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.checked, 4);
}

/// #47 : `append_with` et `append_in` passent par la même lecture du dernier maillon ;
/// une erreur de lecture fait échouer l'écriture, `after` ne tourne pas.
#[tokio::test]
async fn a_read_error_fails_append_with_before_after_runs() {
    let l = log();
    l.append(EventDraft::new("turn.started", json!({})).session("s1"))
        .await
        .unwrap();
    l.store()
        .write(|tx| {
            tx.execute_batch("ALTER TABLE events RENAME TO events_absent;")?;
            Ok(())
        })
        .await
        .unwrap();
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = ran.clone();
    let e = l
        .append_with(
            EventDraft::new("conv.user", json!({})).session("s1"),
            move |_, _| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(e, KernelError::Store(_)), "{e}");
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    l.store()
        .write(|tx| {
            tx.execute_batch("ALTER TABLE events_absent RENAME TO events;")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(l.count().await.unwrap(), 1);
    assert!(l.verify().await.unwrap().ok);
}

/// #162 : des `append_with` concurrents, mêlés à des `append`, gardent une seule
/// chaîne et une diffusion dans l'ordre des identifiants.
#[tokio::test]
async fn concurrent_append_with_keeps_order_and_chain() {
    let l = log();
    let mut live = l.subscribe();
    let mut hs = Vec::new();
    for i in 0..30 {
        let l2 = l.clone();
        hs.push(tokio::spawn(async move {
            let d = EventDraft::new("x", json!({ "i": i })).session("s");
            if i % 2 == 0 {
                l2.append_with(d, |_, _| Ok(())).await.unwrap()
            } else {
                l2.append(d).await.unwrap()
            }
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
    assert!(l.verify().await.unwrap().ok);
    for expected_id in 1..=30 {
        assert_eq!(live.recv().await.unwrap().id, expected_id);
    }
}
