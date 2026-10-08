//! Budget d'un run juste et lisible (#337) : arrêts hors durée, cache à part, une ligne.

use super::*;
use crate::model::Budget;
use penelope_kernel::clock::Clock;

const HOUR: i64 = 3_600_000;

/// Un run bloqué trois heures puis repris garde toute sa durée de travail : le temps
/// d'arrêt est versé à `held_ms`, la borne de durée ne le voit pas.
#[tokio::test]
async fn a_run_blocked_three_hours_keeps_its_working_time() {
    let clock = TestClock::new(1_789_516_800_000);
    let rs = runs(clock.clone()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();
    let budget = Budget {
        max_wall_ms: 2 * HOUR as u64,
        ..Budget::default()
    };

    clock.advance_ms(HOUR);
    rs.set_state(&run.id, RunState::Blocked, Some("en attente"))
        .await
        .unwrap();
    clock.advance_hours(3);
    let blocked = rs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(blocked.held_since_ms, Some(1_789_516_800_000 + HOUR));
    assert_eq!(
        work_ms(&blocked, clock.now_ms()),
        HOUR,
        "l'arrêt en cours ne compte pas"
    );
    assert_eq!(check_limits(&blocked, &budget, clock.now_ms()), Limit::Ok);

    rs.set_state(&run.id, RunState::Running, None)
        .await
        .unwrap();
    let resumed = rs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(resumed.held_ms, 3 * HOUR);
    assert_eq!(resumed.held_since_ms, None);
    assert_eq!(check_limits(&resumed, &budget, clock.now_ms()), Limit::Ok);

    // Une pause du propriétaire s'ajoute ; le travail, lui, finit par atteindre la borne.
    rs.set_state(&run.id, RunState::Paused, None).await.unwrap();
    clock.advance_hours(1);
    rs.set_state(&run.id, RunState::Running, None)
        .await
        .unwrap();
    clock.advance_hours(1);
    let late = rs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(late.held_ms, 4 * HOUR);
    assert_eq!(
        check_limits(&late, &budget, clock.now_ms()),
        Limit::WallClock
    );
}

/// Un blocage posé par `advance` (transition vers `$blocked`) ouvre lui aussi l'arrêt.
#[tokio::test]
async fn a_block_by_transition_starts_the_hold() {
    let clock = TestClock::new(1_789_516_800_000);
    let rs = runs(clock.clone()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();
    clock.advance_ms(1_000);
    let r = rs
        .advance(
            &run.id,
            "un",
            &StepResult::Failure,
            json!({}),
            BLOCKED,
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.state, RunState::Blocked);
    assert_eq!(r.held_since_ms, Some(1_789_516_801_000));
}

/// Les tokens servis par le cache ont leur compteur et leur plafond, à part.
#[tokio::test]
async fn cached_tokens_are_counted_apart() {
    let clock = TestClock::new(1_789_516_800_000);
    let rs = runs(clock.clone()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();
    rs.set_spent(&run.id, 0.4, 31_000, 1_200_000).await.unwrap();
    let r = rs.get(&run.id).await.unwrap().unwrap();
    assert_eq!((r.spent_tokens, r.spent_cached_tokens), (31_000, 1_200_000));
    let mut budget = Budget::default();
    assert_eq!(
        check_limits(&r, &budget, clock.now_ms()),
        Limit::Ok,
        "sans plafond propre, le cache ne borne rien"
    );
    budget.max_cached_tokens = 1_000_000;
    assert_eq!(
        check_limits(&r, &budget, clock.now_ms()),
        Limit::BudgetCachedTokens
    );
}

/// Chaque plafond face à sa consommation, en une ligne lisible.
#[tokio::test]
async fn the_budget_line_shows_every_cap() {
    let clock = TestClock::new(1_789_516_800_000);
    let rs = runs(clock.clone()).await;
    let run = rs
        .create(&workflow(), "s1", json!({}), None, None, 0)
        .await
        .unwrap();
    rs.set_spent(&run.id, 0.4, 31_000, 1_200_000).await.unwrap();
    clock.advance_ms(12 * 60_000);
    let r = rs.get(&run.id).await.unwrap().unwrap();
    let line = budget_line(&r, &Budget::default(), clock.now_ms(), 0);
    assert_eq!(
        line,
        "durée 12/120 min · tokens 31 k/2 M · cache 1,2 M · coût 0.40/5.00 $ · itérations 0/40"
    );
    // L'attente du propriétaire sort de la durée ; un plafond nul se lit « sans plafond ».
    let open = Budget {
        max_wall_ms: 0,
        max_usd: 0.0,
        ..Budget::default()
    };
    let line = budget_line(&r, &open, clock.now_ms(), 2 * 60_000);
    assert!(line.starts_with("durée 10 min · "), "{line}");
    assert!(line.contains("coût 0.40 $"), "{line}");
    assert_eq!(tokens_short(950), "950");
    assert_eq!(tokens_short(2_000_000), "2 M");
}
