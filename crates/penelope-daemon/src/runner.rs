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
    // Lease repris par un autre runner : ce tour n'est plus le nôtre, on l'abandonne sans
    // rien livrer ni écrire (#43).
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let beat = {
        let (d, t, lost) = (daemon.clone(), turn.clone(), lost.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(heartbeat).await;
                match d.services.turns.heartbeat(&t).await {
                    Ok(()) => {}
                    Err(e) if e.is_lease_lost() => {
                        tracing::error!(turn = %t.id, holder = %t.holder, error = %e,
                            "lease perdu : le tour est abandonné");
                        lost.store(true, std::sync::atomic::Ordering::SeqCst);
                        d.bus.cancel_turn(&t.session_id, t.id.as_str());
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(turn = %t.id, error = %e, "battement de lease perdu")
                    }
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
    let lost = lost.load(std::sync::atomic::Ordering::SeqCst)
        || matches!(&stored, Err(e) if e.is_lease_lost());
    if let Err(e) = stored {
        tracing::error!(turn = %turn.id, error = %e, "fin de tour non enregistrée");
    }
    if lost {
        // Le successeur livrera. Les attentes locales sont réveillées pour ne pas rester
        // pendues, mais aucun message ne part.
        let outcome = TurnOutcome::Failed {
            error: "lease perdu : tour repris par un autre runner".into(),
        };
        daemon
            .bus
            .finish(turn.id.as_str(), &turn.session_id, &origin, outcome.clone());
        return outcome;
    }
    // Prompt planifié : l'exécution compte à la fin de son tour, un échec prévient
    // (issue #39).
    if turn.kind == penelope_kernel::turn::TurnKind::Trigger
        && let Some(schedule) = turn.payload["schedule"].as_str()
    {
        crate::scheduler::trigger_outcome(daemon, schedule, &outcome).await;
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

    /// #43 : le bail a changé de main pendant l'exécution. Le runner évincé ne livre rien
    /// et ne touche pas au tour de son successeur.
    #[tokio::test]
    async fn a_runner_that_lost_its_lease_delivers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let p = Arc::new(MockProvider::new());
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("réponse qui ne partira pas");
        d.set_provider_override(p.clone());

        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.enqueue_message(&sid, "bonjour", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = s.turns.claim("runner-0").await.unwrap().expect("réclamé");

        // Un autre runner s'est emparé du bail (processus figé, horloge qui a sauté).
        let voleur = format!("turn:{}", turn.id);
        s.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE leases SET holder='runner-voleur' WHERE resource=?1",
                    [&voleur],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let out = process(&d, turn.clone(), Duration::from_millis(20)).await;
        match out {
            TurnOutcome::Failed { error } => assert!(error.contains("lease perdu"), "{error}"),
            other => panic!("le tour devait être abandonné : {other:?}"),
        }

        // Le tour reste au voleur : ni état, ni bail réécrits par l'évincé.
        let id = turn.id.0.clone();
        let (state, holder): (String, String) = s
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT q.state, l.holder FROM turn_queue q
                     JOIN leases l ON l.resource = 'turn:' || q.id WHERE q.id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(
            (state.as_str(), holder.as_str()),
            ("leased", "runner-voleur")
        );
    }
}
