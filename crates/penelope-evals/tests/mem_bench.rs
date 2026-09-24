//! Suites `mem-bench` et `mem-bench-live` (issue #37) : le rêve sur des conversations
//! anonymisées, mesuré contre les souvenirs attendus et des questions dont la réponse n'est
//! que dans la mémoire.
//!
//! ```bash
//! penelope eval mem-bench
//! ```
//!
//! ```bash
//! OPENROUTER_API_KEY=… penelope eval mem-bench-live
//! ```
//!
//! `PENELOPE_BENCH_REPORT=<fichier>` écrit le rapport Markdown (joint aux releases).

use penelope_daemon::Daemon;
use penelope_evals::live;
use penelope_evals::mem_bench::{self, BENCH_START_MS, Fixture, LIVE, SIMULATED, Score};
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_llm::mock::MockProvider;
use std::sync::Arc;

async fn run_fixture(d: &Arc<Daemon>, f: &Fixture, simulated: Option<&MockProvider>) -> Score {
    mem_bench::seed(d, f).await;
    if let Some(p) = simulated {
        p.reply(&mem_bench::simulated_reply(&d.services, f).await);
    }
    penelope_daemon::dream::run(d, &d.hooks.messenger, false)
        .await
        .unwrap_or_else(|e| panic!("rêve du jeu {} : {e}", f.id));
    mem_bench::score(d, f, &mem_bench::initial_entries(f)).await
}

fn verdict(mode: &str, results: Vec<(Fixture, Score)>, thresholds: &mem_bench::Thresholds) {
    let report = mem_bench::render(mode, &results, thresholds);
    mem_bench::write_report(&report);
    println!("{report}");
    let mut total = Score::default();
    for (_, s) in &results {
        total.add(s);
    }
    let failures = thresholds.failures(&total);
    assert!(failures.is_empty(), "{report}");
}

/// Modèle de consolidation simulé, déterministe : le chemin complet garde ce qu'il doit
/// garder, met à jour, range au journal, écarte, protège les secrets et répond.
#[tokio::test]
async fn mem_bench_simulated_consolidation_keeps_what_it_should() {
    let mut results = Vec::new();
    for f in mem_bench::fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let clock: SharedClock = Arc::new(TestClock::new(BENCH_START_MS));
        let s = penelope_daemon::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        let d = Arc::new(Daemon::from_services(Arc::new(s)));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let score = run_fixture(&d, &f, Some(&p)).await;
        results.push((f, score));
    }
    verdict("simulé (déterministe)", results, &SIMULATED);
}

/// Vrai modèle de consolidation (OpenRouter) : la qualité du tri lui-même.
#[tokio::test]
#[ignore]
async fn mem_bench_live_consolidation() {
    let model = live::model(
        "PENELOPE_LIVE_MODEL",
        "openrouter:deepseek/deepseek-v4-flash",
    );
    let mut results = Vec::new();
    for f in mem_bench::fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let clock: SharedClock = Arc::new(TestClock::new(BENCH_START_MS));
        let d = live::daemon(dir.path(), clock).await;
        let score = run_fixture(&d, &f, None).await;
        results.push((f, score));
    }
    verdict(&format!("réel ({model})"), results, &LIVE);
}
