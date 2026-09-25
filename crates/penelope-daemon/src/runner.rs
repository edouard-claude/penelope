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
        // Un runner qui panique est relancé sous le même nom : le pool garde toujours
        // `runners.count` runners (#84).
        let holder = format!("runner-{i}");
        let d = daemon.clone();
        tasks.push(crate::tasks::spawn_supervised(
            &daemon.supervision(),
            holder.clone(),
            move || runner_loop(d.clone(), holder.clone(), heartbeat),
        ));
    }
    for t in tasks {
        let _ = t.await;
    }
}

/// Arrête le battement du bail quoi qu'il arrive au tour, panique comprise.
struct StopBeat(tokio::task::JoinHandle<()>);

impl Drop for StopBeat {
    fn drop(&mut self) {
        self.0.abort();
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
///
/// Tout ce que le tour journalise, outils et fournisseur compris, porte le span `turn`
/// (identifiant du tour, session) : `penelope logs --turn <id>` le relit d'un bloc
/// (issue #103).
pub async fn process(daemon: &Arc<Daemon>, turn: Turn, heartbeat: Duration) -> TurnOutcome {
    use tracing::Instrument;
    let span = tracing::info_span!(
        "turn",
        turn = %turn.id,
        session = %turn.session_id,
        kind = ?turn.kind,
    );
    process_turn(daemon, turn, heartbeat).instrument(span).await
}

async fn process_turn(daemon: &Arc<Daemon>, turn: Turn, heartbeat: Duration) -> TurnOutcome {
    let started = std::time::Instant::now();
    crate::history::catch_up(&daemon.services, &turn.session_id).await;
    let outcome = if let Some(parts) = oversized_telegram_merge(daemon, &turn) {
        let _ = daemon
            .services
            .events
            .append(
                penelope_kernel::event::EventDraft::new(
                    "turn.merged",
                    serde_json::json!({"turn": turn.id.as_str(), "count": turn.merged_messages.len(), "phase": "queued"}),
                )
                .session(&turn.session_id),
            )
            .await;
        let origin = turn
            .merged_messages
            .last()
            .map(|message| Origin::from_payload(&message.payload))
            .unwrap_or_else(|| Origin::from_payload(&turn.payload));
        let offered = match daemon.hooks.telegram() {
            Some(channel) => channel.offer_burst(&turn.session_id, &origin, parts).await,
            None => Err("canal Telegram indisponible".into()),
        };
        let outcome = match offered {
            Ok(()) => TurnOutcome::Cancelled,
            Err(error) => TurnOutcome::Failed { error },
        };
        let stored = match &outcome {
            TurnOutcome::Failed { error } => daemon.services.turns.fail(&turn, error).await,
            _ => daemon.services.turns.cancel_leased(&turn).await,
        };
        if let Err(error) = stored {
            tracing::error!(turn = %turn.id, %error, "rafale non clôturée");
        }
        if matches!(outcome, TurnOutcome::Failed { .. }) {
            daemon.deliver(&turn, &origin, &outcome).await;
        }
        daemon
            .bus
            .finish(turn.id.as_str(), &turn.session_id, &origin, outcome.clone());
        outcome
    } else {
        run_and_deliver(daemon, turn, heartbeat).await
    };
    let label = match &outcome {
        TurnOutcome::Answered { .. } => "answered",
        TurnOutcome::AwaitingApproval { .. } => "awaiting_approval",
        TurnOutcome::Failed { .. } => "failed",
        TurnOutcome::Cancelled => "cancelled",
        _ => "other",
    };
    penelope_observe::metrics::counter_inc("penelope_turns_total", &[("outcome", label)], 1.0);
    penelope_observe::metrics::histogram_observe(
        "penelope_turn_duration_ms",
        &[],
        started.elapsed().as_millis() as f64,
    );
    outcome
}

fn oversized_telegram_merge(daemon: &Daemon, turn: &Turn) -> Option<Vec<String>> {
    if !matches!(Origin::from_payload(&turn.payload), Origin::Telegram { .. }) {
        return None;
    }
    let cfg = daemon.services.config.config();
    let parts: Vec<String> = std::iter::once(&turn.payload)
        .chain(turn.merged_messages.iter().map(|message| &message.payload))
        .filter_map(|payload| payload.get("text").and_then(|text| text.as_str()))
        .map(ToString::to_string)
        .collect();
    let chars: usize = parts.iter().map(|part| part.chars().count()).sum();
    let too_many = cfg.telegram.burst_messages > 0 && parts.len() >= cfg.telegram.burst_messages;
    let too_long = cfg.telegram.burst_chars > 0 && chars >= cfg.telegram.burst_chars;
    (too_many || too_long).then_some(parts)
}

async fn run_and_deliver(daemon: &Arc<Daemon>, turn: Turn, heartbeat: Duration) -> TurnOutcome {
    use tracing::Instrument;
    // Lease repris par un autre runner : ce tour n'est plus le nôtre, on l'abandonne sans
    // rien livrer ni écrire (#43).
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let beat = {
        let (d, t, lost) = (daemon.clone(), turn.clone(), lost.clone());
        let span = tracing::Span::current();
        tokio::spawn(
            async move {
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
            }
            .instrument(span),
        )
    };

    let beat = StopBeat(beat);
    // Une panique pendant le tour ne tue ni le runner ni le verrou de session : le tour
    // échoue, le propriétaire le sait, le bail est rendu (#84).
    crate::tasks::install_panic_hook();
    let outcome = {
        use futures::FutureExt;
        match std::panic::AssertUnwindSafe(daemon.run_turn(&turn))
            .catch_unwind()
            .await
        {
            Ok(o) => o,
            Err(payload) => {
                let msg = crate::tasks::panic_text(payload.as_ref());
                crate::tasks::report_panic(&daemon.supervision(), "tour", &msg).await;
                TurnOutcome::Failed {
                    error: format!(
                        "erreur interne pendant le tour ({msg}) : il est arrêté, rien d'autre \
                         n'est touché. Réessaie ; `penelope doctor` la signale."
                    ),
                }
            }
        }
    };
    drop(beat);

    let origin = daemon
        .services
        .turns
        .last_merged_payload(turn.id.as_str())
        .await
        .ok()
        .flatten()
        .map(|payload| Origin::from_payload(&payload))
        .unwrap_or_else(|| Origin::from_payload(&turn.payload));
    let stored = match &outcome {
        TurnOutcome::Failed { error } => daemon.services.turns.fail(&turn, error).await,
        TurnOutcome::Cancelled => daemon.services.turns.cancel_leased(&turn).await,
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
    let scheduled = turn.kind == penelope_kernel::turn::TurnKind::Trigger;
    if scheduled && let Some(schedule) = turn.payload["schedule"].as_str() {
        let ports = daemon.hooks.scheduler();
        crate::scheduler::trigger_outcome_of(daemon, &ports, schedule, &outcome, &turn).await;
    }

    // Tour planifié : la réponse finale est le livrable, livrée une fois ; si l'agent a
    // déjà envoyé le même contenu pendant le tour, elle ne repart pas (issue #133). Le
    // livrable a été évalué avant, sur ce qui est réellement parti.
    let repeated = scheduled
        && matches!(&outcome, TurnOutcome::Answered { text, .. }
            if crate::scheduler::final_already_sent(daemon, &turn.session_id, text).await);
    let burst_card_sent = matches!(outcome, TurnOutcome::Cancelled)
        && daemon
            .services
            .kv_get(&format!("turn.burst_card.{}", turn.id))
            .await
            .ok()
            .flatten()
            .is_some();
    if repeated {
        let _ = daemon
            .services
            .events
            .append(
                penelope_kernel::event::EventDraft::new(
                    "schedule.final_not_repeated",
                    serde_json::json!({"turn": turn.id.as_str()}),
                )
                .session(&turn.session_id),
            )
            .await;
    }
    if !repeated && !burst_card_sent {
        daemon.deliver(&turn, &origin, &outcome).await;
    }
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
    use crate::agent::Conversation;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::MockProvider;

    struct BurstChannel(std::sync::Mutex<Vec<String>>);

    struct DeliveryChannel(std::sync::Mutex<Option<Origin>>);

    #[async_trait::async_trait]
    impl crate::bus::ChannelDelivery for DeliveryChannel {
        async fn deliver(
            &self,
            _turn_id: &str,
            _session_id: &str,
            origin: &Origin,
            _outcome: &TurnOutcome,
        ) {
            *self.0.lock().unwrap() = Some(origin.clone());
        }
    }

    #[async_trait::async_trait]
    impl crate::bus::ChannelDelivery for BurstChannel {
        async fn deliver(
            &self,
            _turn_id: &str,
            _session_id: &str,
            _origin: &Origin,
            _outcome: &TurnOutcome,
        ) {
        }

        async fn offer_burst(
            &self,
            _session_id: &str,
            _origin: &Origin,
            parts: Vec<String>,
        ) -> Result<(), String> {
            *self.0.lock().unwrap() = parts;
            Ok(())
        }
    }

    #[tokio::test]
    async fn six_pending_telegram_messages_show_a_card_without_calling_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let services = Arc::new(
            crate::runtime::Services::for_tests(
                dir.path().to_path_buf(),
                Arc::new(TestClock::default()),
            )
            .await
            .unwrap(),
        );
        let daemon = Arc::new(Daemon::from_services(services.clone()));
        let provider = Arc::new(MockProvider::new());
        daemon.set_provider_override(provider.clone());
        let channel = Arc::new(BurstChannel(std::sync::Mutex::new(Vec::new())));
        *daemon.hooks.delivery.write().unwrap() = Some(channel.clone());
        let origin = Origin::Telegram {
            chat_id: 10,
            topic_id: None,
            message_id: Some(1),
        };
        let sid = daemon.chat_session_for(&origin).await.unwrap();
        for i in 0..6 {
            let origin = Origin::Telegram {
                chat_id: 10,
                topic_id: None,
                message_id: Some(i + 1),
            };
            daemon
                .enqueue_message(
                    &sid,
                    &format!("message {i}"),
                    &origin,
                    Some(format!("tg:{i}")),
                )
                .await
                .unwrap();
        }
        let turn = services.turns.claim("test").await.unwrap().unwrap();
        assert_eq!(turn.merged_messages.len(), 5);
        let outcome = process(&daemon, turn, Duration::from_secs(60)).await;
        assert_eq!(outcome, TurnOutcome::Cancelled);
        assert_eq!(channel.0.lock().unwrap().len(), 6);
        assert!(provider.requests().is_empty());
        let cancelled: i64 = services
            .store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM turn_queue WHERE state='cancelled'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(cancelled, 6);
    }

    #[tokio::test]
    async fn messages_arriving_during_tools_hit_the_burst_limit_before_another_model_call() {
        let dir = tempfile::tempdir().unwrap();
        let services = Arc::new(
            crate::runtime::Services::for_tests(
                dir.path().to_path_buf(),
                Arc::new(TestClock::default()),
            )
            .await
            .unwrap(),
        );
        let daemon = Arc::new(Daemon::from_services(services.clone()));
        let channel = Arc::new(BurstChannel(std::sync::Mutex::new(Vec::new())));
        let origin = Origin::Telegram {
            chat_id: 10,
            topic_id: None,
            message_id: Some(1),
        };
        let sid = daemon.chat_session_for(&origin).await.unwrap();
        daemon
            .enqueue_message(&sid, "début", &origin, None)
            .await
            .unwrap();
        let turn = services.turns.claim("test").await.unwrap().unwrap();
        let tiers = crate::conversation::build_tiers(&services, "début", &[], None).await;
        let cancel = penelope_llm::CancelToken::new();
        let conv = crate::conversation::SessionConversation::new(
            services.clone(),
            &sid,
            "openrouter:mock/model",
            tiers,
            0,
        );
        let inbox = crate::conversation::TurnInbox::for_turn(
            &services,
            &turn,
            Some(channel.clone()),
            &cancel,
        )
        .unwrap();
        conv.record(&penelope_llm::types::ChatMessage::user("début"), false)
            .await
            .unwrap();
        conv.record(
            &penelope_llm::types::ChatMessage::tool_result("call-1", "test", "outil fini"),
            false,
        )
        .await
        .unwrap();
        for i in 2..=5 {
            let origin = Origin::Telegram {
                chat_id: 10,
                topic_id: None,
                message_id: Some(i),
            };
            daemon
                .enqueue_message(&sid, &format!("suite {i}"), &origin, None)
                .await
                .unwrap();
        }
        inbox
            .claim(crate::agent::Checkpoint::BeforeModelCall)
            .await
            .unwrap();
        assert!(cancel.is_cancelled());
        assert_eq!(channel.0.lock().unwrap().len(), 5);
        assert!(
            services
                .kv_get(&format!("turn.burst_card.{}", turn.id))
                .await
                .unwrap()
                .is_some()
        );
        services.turns.cancel_leased(&turn).await.unwrap();
        assert_eq!(services.turns.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_merged_reply_targets_the_last_telegram_message() {
        let dir = tempfile::tempdir().unwrap();
        let services = Arc::new(
            crate::runtime::Services::for_tests(
                dir.path().to_path_buf(),
                Arc::new(TestClock::default()),
            )
            .await
            .unwrap(),
        );
        let daemon = Arc::new(Daemon::from_services(services.clone()));
        let provider = Arc::new(MockProvider::new());
        provider.reply(r#"{"complexity":"low"}"#);
        provider.reply("réponse unique");
        daemon.set_provider_override(provider);
        let channel = Arc::new(DeliveryChannel(std::sync::Mutex::new(None)));
        *daemon.hooks.delivery.write().unwrap() = Some(channel.clone());
        let first = Origin::Telegram {
            chat_id: 10,
            topic_id: None,
            message_id: Some(1),
        };
        let sid = daemon.chat_session_for(&first).await.unwrap();
        for i in 1..=3 {
            daemon
                .enqueue_message(
                    &sid,
                    &format!("message {i}"),
                    &Origin::Telegram {
                        chat_id: 10,
                        topic_id: None,
                        message_id: Some(i),
                    },
                    None,
                )
                .await
                .unwrap();
        }
        let turn = services.turns.claim("test").await.unwrap().unwrap();
        assert_eq!(turn.merged_messages.len(), 2);
        assert!(matches!(
            process(&daemon, turn, Duration::from_secs(60)).await,
            TurnOutcome::Answered { .. }
        ));
        assert_eq!(
            *channel.0.lock().unwrap(),
            Some(Origin::Telegram {
                chat_id: 10,
                topic_id: None,
                message_id: Some(3)
            })
        );
    }

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
            .enqueue_message(&sid, "bonjour, fais le point", &Origin::Cli, None)
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

    /// #84 : un tour qui panique échoue proprement ; son runner survit, le verrou de
    /// session est rendu, le tour suivant de la même session est servi, et le pool garde
    /// `runners.count` runners vivants.
    #[tokio::test]
    async fn a_panicking_turn_neither_kills_its_runner_nor_locks_its_session() {
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
        p.push(penelope_llm::mock::Scripted::Panic(
            "boum dans le fournisseur".into(),
        ));
        d.set_provider_override(p.clone());
        let pool = tokio::spawn(run_pool(d.clone()));
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();

        let id = d
            .enqueue_message(&sid, "fais le point sur la facturation", &Origin::Cli, None)
            .await
            .unwrap()
            .unwrap();
        let out = tokio::time::timeout(Duration::from_secs(10), d.bus.wait_for(id.as_str()))
            .await
            .expect("le tour se termine")
            .unwrap();
        match out {
            // #130 : l'emplacement de la panique accompagne son message.
            TurnOutcome::Failed { error } => {
                assert!(error.contains("boum"), "{error}");
                assert!(error.contains(".rs:"), "emplacement : {error}");
            }
            other => panic!("{other:?}"),
        }

        p.reply(r#"{"complexity":"low"}"#);
        p.reply("réponse après la panique");
        let id = d
            .enqueue_message(&sid, "et maintenant, où en est-on ?", &Origin::Cli, None)
            .await
            .unwrap()
            .unwrap();
        let out = tokio::time::timeout(Duration::from_secs(10), d.bus.wait_for(id.as_str()))
            .await
            .expect("la session n'est pas verrouillée")
            .unwrap();
        assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
        let expected = s.config.config().runners.count.max(1);
        assert_eq!(d.tasks.alive("runner-"), expected);
        assert_eq!(d.tasks.snapshot()["tour"].panics, 1);
        assert_eq!(d.status().await.unwrap().runners_alive, expected as u64);

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
        d.enqueue_message(&sid, "bonjour, fais le point", &Origin::Cli, None)
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
