//! Pool de runners (§3.3) : réclamer un tour, le battre, l'exécuter, livrer l'issue.
//!
//! Le verrou de session de `TurnQueue` garantit une seule exécution par session ; le
//! pool apporte la concurrence entre sessions.

use crate::agent::TurnOutcome;
use crate::bus::Origin;
use crate::runtime::Daemon;
use penelope_kernel::turn::Turn;
use std::sync::Arc;
use std::time::Duration;

/// Lance `runners.count` runners et attend leur fin (arrêt du daemon).
pub async fn run_pool(daemon: Arc<Daemon>) {
    let cfg = daemon.services.config.config();
    let n = cfg.runners.count.max(1);
    let heartbeat = penelope_kernel::config::parse_duration(&cfg.runners.heartbeat)
        .unwrap_or(Duration::from_secs(15));
    let mut tasks = Vec::with_capacity(n);
    for i in 0..n {
        tasks.push(tokio::spawn(runner_loop(
            daemon.clone(),
            format!("runner-{i}"),
            heartbeat,
        )));
    }
    for t in tasks {
        let _ = t.await;
    }
}

async fn runner_loop(daemon: Arc<Daemon>, holder: String, heartbeat: Duration) {
    while !daemon.handle.is_shutting_down() {
        match daemon.services.turns.claim(&holder).await {
            Ok(Some(turn)) => {
                process(&daemon, turn, heartbeat).await;
            }
            Ok(None) => daemon.bus.wait_enqueued(Duration::from_millis(500)).await,
            Err(e) => {
                tracing::warn!(runner = %holder, error = %e, "réclamation impossible");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Exécute un tour réclamé jusqu'au bout et livre son issue.
pub async fn process(daemon: &Arc<Daemon>, turn: Turn, heartbeat: Duration) -> TurnOutcome {
    let beat = {
        let d = daemon.clone();
        let t = turn.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(heartbeat).await;
                if let Err(e) = d.services.turns.heartbeat(&t).await {
                    tracing::warn!(turn = %t.id, error = %e, "battement de lease perdu");
                }
            }
        })
    };

    let outcome = daemon.run_turn(&turn).await;
    beat.abort();

    let origin = Origin::from_payload(&turn.payload);
    let stored = match &outcome {
        TurnOutcome::Failed { error } => daemon.services.turns.fail(&turn, error).await,
        _ => daemon.services.turns.complete(&turn).await,
    };
    if let Err(e) = stored {
        tracing::error!(turn = %turn.id, error = %e, "fin de tour non enregistrée");
    }

    daemon.deliver(&turn, &origin, &outcome).await;
    daemon
        .bus
        .finish(turn.id.as_str(), &turn.session_id, &origin, outcome.clone());
    outcome
}

impl Daemon {
    /// Livre l'issue d'un tour à son canal, par le chemin durable.
    pub async fn deliver(&self, turn: &Turn, origin: &Origin, outcome: &TurnOutcome) {
        if let Origin::Telegram { .. } = origin
            && let Some(tg) = self.hooks.telegram()
        {
            tg.deliver(turn.id.as_str(), &turn.session_id, origin, outcome)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::MockProvider;

    #[tokio::test]
    async fn the_pool_answers_queued_turns_and_waiters_get_the_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let _ = TestClock::default();
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        let p = Arc::new(MockProvider::new());
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("réponse du pool");
        d.set_provider_override(p.clone());

        let pool = tokio::spawn(run_pool(d.clone()));
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        let id = d
            .enqueue_message(&sid, "bonjour", &Origin::Cli, None)
            .await
            .unwrap()
            .unwrap();
        let out = tokio::time::timeout(Duration::from_secs(10), d.bus.wait_for(id.as_str()))
            .await
            .expect("le pool doit répondre")
            .unwrap();
        assert_eq!(
            out,
            TurnOutcome::Answered {
                text: "réponse du pool".into(),
                iterations: 1,
                cost_usd: 0.0
            }
        );
        assert_eq!(d.services.turns.pending_count().await.unwrap(), 0);

        d.handle.shutdown();
        d.bus.notify_enqueued();
        tokio::time::timeout(Duration::from_secs(5), pool)
            .await
            .expect("le pool s'arrête")
            .unwrap();
    }
}
