use super::*;
use crate::clock::TestClock;
use std::sync::Arc;

#[test]
fn status_thresholds() {
    let s = BudgetStatus::compute(BudgetScope::Daily, 16.0, 20.0, 0.8);
    assert!(s.alerting && !s.exceeded);
    let s = BudgetStatus::compute(BudgetScope::Daily, 20.0, 20.0, 0.8);
    assert!(s.exceeded && !s.alerting);
    let s = BudgetStatus::compute(BudgetScope::Daily, 1.0, 20.0, 0.8);
    assert!(!s.alerting && !s.exceeded);
}

#[test]
fn no_limit_never_alerts() {
    let s = BudgetStatus::compute(BudgetScope::Run, 999.0, 0.0, 0.8);
    assert!(!s.alerting && !s.exceeded);
}

#[tokio::test]
async fn records_and_sums() {
    let store = Store::open_memory().unwrap();
    let l = BudgetLedger::new(store, Arc::new(TestClock::default()));
    for i in 0..3 {
        l.record(UsageRecord {
            session_id: Some("s1".into()),
            model: "m".into(),
            provider: "openrouter".into(),
            prompt: 100,
            completion: 10,
            cost_usd: 0.5 * (i as f64 + 1.0),
            ..Default::default()
        })
        .await
        .unwrap();
    }
    assert!((l.spent_today().await.unwrap() - 3.0).abs() < 1e-9);
    assert!((l.spent_session("s1").await.unwrap() - 3.0).abs() < 1e-9);

    let b = l.breakdown("model", 10).await.unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].0, "m");
    assert_eq!(b[0].2, 330);
}

#[tokio::test]
async fn every_recorded_model_usage_has_a_runtime_event() {
    let store = Store::open_memory().unwrap();
    let clock = Arc::new(TestClock::default());
    let events = crate::event::EventLog::new(store.clone(), clock.clone());
    let ledger = BudgetLedger::new(store, clock).with_events(events.clone());
    ledger
        .record(UsageRecord {
            session_id: Some("s1".into()),
            model: "model-a".into(),
            provider: "openrouter".into(),
            prompt: 12,
            completion: 3,
            cost_usd: 0.02,
            estimated: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let logged = events.range(0, 10).await.unwrap();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].kind, "runtime.llm");
    assert_eq!(logged[0].payload["prompt_tokens"], 12);
    assert_eq!(logged[0].payload["cost_usd"], 0.02);
}

fn conso(cost: f64) -> UsageRecord {
    UsageRecord {
        model: "m".into(),
        provider: "openrouter".into(),
        cost_usd: cost,
        ..Default::default()
    }
}

/// #79 : à La Réunion (UTC+4), 00:30 le 2 janvier est encore le 1er en UTC. La
/// consommation compte pour le 2, le relèvement du jour vaut jusqu'à 23:59 locale.
#[tokio::test]
async fn the_budget_day_is_the_owners_day() {
    let store = Store::open_memory().unwrap();
    // 2026-01-01T20:30:00Z = 2026-01-02T00:30+04:00
    let clock = Arc::new(TestClock::new(1_767_299_400_000));
    let tz = Arc::new(std::sync::RwLock::new("Indian/Reunion".to_string()));
    let l = BudgetLedger::new(store.clone(), clock.clone()).with_timezone({
        let tz = tz.clone();
        move || tz.read().unwrap().clone()
    });
    assert_eq!(l.today(), "2026-01-02");
    l.record(conso(1.5)).await.unwrap();
    assert!((l.spent_today().await.unwrap() - 1.5).abs() < 1e-9);
    let day: String = store
        .read(|c| Ok(c.query_row("SELECT day FROM usage", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(day, "2026-01-02");

    let cfg = crate::config::Budget::default();
    l.raise_daily(99.0).await.unwrap();
    let key: i64 = store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM kv WHERE k = 'budget.daily.2026-01-02'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(key, 1);
    // 23:59 locale : toujours relevé.
    clock.set_ms(1_767_383_940_000); // 2026-01-02T19:59:00Z
    assert_eq!(l.limits(&cfg, None, None).await.unwrap().0, 99.0);
    // Minuit local : un nouveau jour, plafond de la configuration, compteur à zéro.
    clock.set_ms(1_767_384_000_000); // 2026-01-02T20:00:00Z
    assert_eq!(l.today(), "2026-01-03");
    assert_eq!(l.limits(&cfg, None, None).await.unwrap().0, cfg.daily_usd);
    assert_eq!(l.spent_today().await.unwrap(), 0.0);
    l.record(conso(0.5)).await.unwrap();

    let days = l.report("day", None, None, 10).await.unwrap();
    let mut keys: Vec<String> = days.iter().map(|r| r.key.clone()).collect();
    keys.sort();
    assert_eq!(keys, vec!["2026-01-02", "2026-01-03"]);
    assert_eq!(l.mixed_days().await.unwrap(), 0);

    // Changement de fuseau à chaud : les lignes suivantes le suivent, et les lignes
    // récentes comptées dans l'ancien sont signalées.
    *tz.write().unwrap() = "UTC".into();
    assert_eq!(l.today(), "2026-01-02");
    assert_eq!(l.mixed_days().await.unwrap(), 2);
}

#[tokio::test]
async fn costs_are_attributed_to_sessions_and_requests() {
    let store = Store::open_memory().unwrap();
    let clock = Arc::new(TestClock::default());
    let l = BudgetLedger::new(store.clone(), clock.clone());
    store
        .write(|tx| {
            tx.execute(
                "INSERT INTO sessions(id, kind, title, created_at, updated_at)
                 VALUES('s1', 'chat', NULL, 'x', 'x'), ('s2', 'chat', 'Refonte', 'x', 'x')",
                [],
            )?;
            tx.execute(
                "INSERT INTO messages(session_id, seq, role, content, ts)
                 VALUES('s1', 1, 'user', '{\"blocks\":[{\"type\":\"text\",\"text\":\"Liste mes repo  qui contiennent mcp\"}]}', 'x')",
                [],
            )?;
            tx.execute(
                "INSERT INTO turn_queue(id, session_id, kind, payload, state, enqueued_at)
                 VALUES('t1', 's1', 'message', '{\"text\":\"Liste mes repo qui contiennent mcp\"}', 'done', 'x'),
                       ('t2', 's1', 'resume', '{\"approval_id\":\"a1\"}', 'done', 'x')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let call = |session: &str, turn: &str, role: &str, cost: f64, estimated: bool| UsageRecord {
        session_id: Some(session.into()),
        turn_id: Some(turn.into()),
        role: Some(role.into()),
        model: "z-ai/glm-5.3".into(),
        provider: "openrouter".into(),
        generation_id: Some("gen-1".into()),
        upstream: Some("Z.AI".into()),
        prompt: 1000,
        completion: 100,
        cost_usd: cost,
        estimated,
        ..Default::default()
    };
    l.record(call("s1", "t1", "classifier", 0.0001, false))
        .await
        .unwrap();
    l.record(call("s1", "t1", "chat", 0.03, false))
        .await
        .unwrap();
    l.record(call("s1", "t1", "chat", 0.02, true))
        .await
        .unwrap();
    l.record(call("s2", "t3", "chat", 0.01, false))
        .await
        .unwrap();

    let by_session = l.report("session", None, None, 10).await.unwrap();
    assert_eq!(by_session[0].key, "s1");
    assert_eq!(by_session[0].calls, 3);
    assert_eq!(by_session[0].estimated, 1);
    assert_eq!(
        by_session[0].label.as_deref(),
        Some("Liste mes repo qui contiennent mcp")
    );
    assert_eq!(by_session[1].label.as_deref(), Some("Refonte"));

    let by_turn = l.report("turn", Some("s1"), None, 10).await.unwrap();
    assert_eq!(
        by_turn.len(),
        1,
        "la reprise est rattachée au tour d'origine"
    );
    assert!((by_turn[0].cost_usd - 0.0501).abs() < 1e-9);
    assert_eq!(
        by_turn[0].label.as_deref(),
        Some("Liste mes repo qui contiennent mcp")
    );

    let by_role = l
        .report("role", None, Some("1970-01-01"), 10)
        .await
        .unwrap();
    assert_eq!(by_role[0].key, "chat");
    assert!(
        l.report("day", None, Some("2999-01-01"), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// CA 10 : le budget journalier dépassé déclenche la demande HITL.
#[tokio::test]
async fn ca_10_4_daily_budget_exceeded_is_detected() {
    let store = Store::open_memory().unwrap();
    let l = BudgetLedger::new(store, Arc::new(TestClock::default()));
    l.record(UsageRecord {
        model: "m".into(),
        provider: "p".into(),
        cost_usd: 25.0,
        ..Default::default()
    })
    .await
    .unwrap();
    let cfg = crate::config::Budget::default();
    let st = l.status(&cfg, None, None).await.unwrap();
    assert!(st[0].exceeded, "{st:?}");
}

/// Le dernier appel de conversation d'une session (issue #17, épopée #208, T27) : les
/// autres rôles et les autres sessions ne comptent pas, l'heure revient en millisecondes.
#[tokio::test]
async fn the_previous_call_is_the_last_chat_call_of_the_session() {
    let store = Store::open_memory().unwrap();
    let clock = Arc::new(TestClock::new(1_767_299_400_000));
    let l = BudgetLedger::new(store, clock.clone());
    assert_eq!(l.previous_call("s1").await.unwrap(), None);
    let call = |session: &str, role: &str, hash: &str| UsageRecord {
        session_id: Some(session.into()),
        role: Some(role.into()),
        model: "z-ai/glm-5.3".into(),
        provider: "openrouter".into(),
        upstream: Some("Together".into()),
        msg_count: Some(4),
        request_hash: Some(hash.into()),
        system_hash: Some("sys".into()),
        tools_hash: Some("tools".into()),
        ..Default::default()
    };
    l.record(call("s1", "chat", "h1")).await.unwrap();
    clock.advance_secs(30);
    l.record(call("s1", "chat", "h2")).await.unwrap();
    l.record(call("s1", "classifier", "h3")).await.unwrap();
    l.record(call("s2", "chat", "h4")).await.unwrap();

    let p = l.previous_call("s1").await.unwrap().unwrap();
    assert_eq!(p.request_hash.as_deref(), Some("h2"));
    assert_eq!(p.ts_ms, 1_767_299_430_000);
    assert_eq!(p.upstream.as_deref(), Some("Together"));
    assert_eq!(p.msg_count, Some(4));
    assert_eq!(p.system_hash.as_deref(), Some("sys"));
    assert_eq!(p.tools_hash.as_deref(), Some("tools"));
}
