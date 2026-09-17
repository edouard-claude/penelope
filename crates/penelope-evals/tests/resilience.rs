//! Suite `resilience` (§17, CA 17) : crash, reprise, effets incertains.
//!
//! Chaque test simule un **vrai** redémarrage : les services sont détruits, puis
//! reconstruits sur le même répertoire. Rien n'est gardé en mémoire d'une vie à l'autre.

use penelope_daemon::{Daemon, Services};
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_kernel::effects::{EffectKind, EffectSpec, Planned, UnknownDecision};
use penelope_kernel::session::SessionKind;
use penelope_kernel::turn::TurnKind;
use serde_json::json;
use std::sync::Arc;

/// Reconstruit les services sur une racine existante : c'est notre « redémarrage ».
async fn boot(root: &std::path::Path) -> Arc<Services> {
    let clock: SharedClock = Arc::new(TestClock::default());
    Arc::new(
        Services::for_tests(root.to_path_buf(), clock)
            .await
            .expect("bootstrap"),
    )
}

async fn a_session(s: &Services) -> String {
    s.sessions.create(SessionKind::Chat, None).await.unwrap();
    s.sessions.list(None, 10).await.unwrap()[0].id.to_string()
}

/// CA 17 : tuer le daemon pendant un tour ; au redémarrage, le tour reprend, rien
/// n'est perdu, rien n'est exécuté deux fois.
#[tokio::test]
async fn ca_17_1_a_turn_interrupted_mid_flight_is_requeued_once() {
    let dir = tempfile::tempdir().unwrap();

    // --- Vie n°1 : le tour est réclamé, puis le processus meurt.
    let sid = {
        let s = boot(dir.path()).await;
        let sid = a_session(&s).await;
        s.turns
            .enqueue(
                &sid,
                TurnKind::Message,
                json!({"text": "calcule la TVA"}),
                None,
                0,
            )
            .await
            .unwrap()
            .expect("tour créé");
        let claimed = s.turns.claim("runner-1").await.unwrap().expect("réclamé");
        assert_eq!(claimed.session_id, sid);
        // Verrou de session : un second runner ne prend rien.
        assert!(s.turns.claim("runner-2").await.unwrap().is_none());
        sid
    };

    // --- Vie n°2 : reprise.
    let s = boot(dir.path()).await;
    let d = Daemon::from_services(s.clone());
    let r = d.recover().await.unwrap();
    assert_eq!(r.turns_requeued, 1);
    assert_eq!(s.turns.pending_count().await.unwrap(), 1);

    // Le tour est réexécutable, et une seule fois.
    let again = s.turns.claim("runner-3").await.unwrap().expect("reprise");
    assert_eq!(again.session_id, sid);
    assert_eq!(again.payload["text"], "calcule la TVA");
    s.turns.complete(&again).await.unwrap();
    assert_eq!(s.turns.pending_count().await.unwrap(), 0);
    assert!(s.turns.claim("runner-4").await.unwrap().is_none());
}

/// #43 : l'écrivain est figé (sauvegarde, réindexation, veille du Mac) plus longtemps que
/// le bail. Le tour en cours n'est pas repris : ni double exécution, ni double livraison.
#[tokio::test]
async fn ca_17_7_a_frozen_writer_does_not_duplicate_a_turn_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock.clone() as SharedClock)
            .await
            .expect("bootstrap"),
    );
    let sid = a_session(&s).await;
    for t in ["premier", "second"] {
        s.turns
            .enqueue(&sid, TurnKind::Message, json!({"text": t}), None, 0)
            .await
            .unwrap()
            .expect("tour créé");
    }
    let en_cours = s.turns.claim("runner-1").await.unwrap().expect("réclamé");

    // L'écrivain part pour un long travail : la file d'écriture est bloquée, aucun
    // battement ne passe, et le bail (60 s) expire.
    let store = s.store.clone();
    let c = clock.clone();
    let fige = tokio::spawn(async move {
        store
            .write(move |_tx| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                c.advance_ms(91_000);
                Ok(())
            })
            .await
            .unwrap();
    });
    fige.await.unwrap();

    // Le runner est vivant : personne ne lui vole son tour.
    assert!(
        s.turns.claim("runner-2").await.unwrap().is_none(),
        "un tour en cours chez un runner vivant n'est pas repris"
    );
    // L'écrivain repart : le battement retrouve son bail, et lui seul clôt le tour.
    s.turns.heartbeat(&en_cours).await.unwrap();
    s.turns.complete(&en_cours).await.unwrap();

    let suivant = s.turns.claim("runner-2").await.unwrap().expect("le second");
    assert_ne!(suivant.id.to_string(), en_cours.id.to_string());
    assert_eq!(suivant.attempts, 1, "le second tour n'a pas été retenté");
    assert_eq!(suivant.payload["text"], "second");
}

/// Un `update_id` Telegram rejoué après le crash ne crée pas un second tour.
#[tokio::test]
async fn replayed_updates_do_not_duplicate_turns() {
    let dir = tempfile::tempdir().unwrap();
    let dedup = Some("tg:update:5501".to_string());

    let first = {
        let s = boot(dir.path()).await;
        let sid = a_session(&s).await;
        s.turns
            .enqueue(
                &sid,
                TurnKind::Message,
                json!({"text": "salut"}),
                dedup.clone(),
                0,
            )
            .await
            .unwrap()
    };
    assert!(first.is_some());

    let s = boot(dir.path()).await;
    let sid = s.sessions.list(None, 10).await.unwrap()[0].id.to_string();
    let second = s
        .turns
        .enqueue(&sid, TurnKind::Message, json!({"text": "salut"}), dedup, 0)
        .await
        .unwrap();
    assert!(second.is_none(), "le doublon doit être ignoré");
    assert_eq!(s.turns.pending_count().await.unwrap(), 1);
}

/// CA 17 : un effet terminé avant le crash est **rejoué**, pas ré-exécuté.
#[tokio::test]
async fn ca_17_2_a_completed_effect_is_replayed_not_reexecuted() {
    let dir = tempfile::tempdir().unwrap();
    let spec = || {
        EffectSpec::new(
            EffectKind::Mcp,
            "mcp__forge__create_pr",
            json!({"title": "corrige la TVA", "branch": "penelope/4312"}),
        )
        .run("r_1")
        .step("ouvrir_pr")
    };

    {
        let s = boot(dir.path()).await;
        let id = match s.effects.plan(spec()).await.unwrap() {
            Planned::Fresh(id) => id,
            other => panic!("{other:?}"),
        };
        s.effects.dispatching(&id).await.unwrap();
        s.effects
            .complete(&id, json!({"number": 91, "url": "https://forge/pr/91"}))
            .await
            .unwrap();
    }

    let s = boot(dir.path()).await;
    let d = Daemon::from_services(s.clone());
    assert!(d.recover().await.unwrap().is_clean());

    match s.effects.plan(spec()).await.unwrap() {
        Planned::Replayed(v) => assert_eq!(v["number"], 91),
        other => panic!("la PR aurait été créée deux fois : {other:?}"),
    }

    // Une tentative logique différente est un effet neuf, volontairement.
    match s.effects.plan(spec().attempt(1)).await.unwrap() {
        Planned::Fresh(_) => {}
        other => panic!("{other:?}"),
    }
}

/// CA 17 : un effet `dispatching` au moment du crash devient `unknown`, lève une
/// demande humaine et n'est **jamais** relancé tout seul.
#[tokio::test]
async fn ca_17_3_an_uncertain_effect_asks_instead_of_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let spec = || {
        EffectSpec::new(
            EffectKind::Tool,
            "send_email",
            json!({"to": "client@exemple.fr"}),
        )
        .session("s_1")
    };

    {
        let s = boot(dir.path()).await;
        let id = match s.effects.plan(spec()).await.unwrap() {
            Planned::Fresh(id) => id,
            other => panic!("{other:?}"),
        };
        s.effects.dispatching(&id).await.unwrap();
    }

    let s = boot(dir.path()).await;
    let d = Daemon::from_services(s.clone());
    let r = d.recover().await.unwrap();
    assert_eq!(r.effects_unknown, 1);
    assert!(!r.is_clean());

    let pending = s.approvals.pending(10).await.unwrap();
    assert_eq!(pending.len(), 1, "une décision humaine est requise");
    assert_eq!(pending[0].kind, penelope_hitl::ApprovalKind::EffectUnknown);

    // Replanifier la même chose ne relance rien : la décision reste due.
    let id = match s.effects.plan(spec()).await.unwrap() {
        Planned::NeedsDecision(id) => id,
        other => panic!("relance silencieuse : {other:?}"),
    };

    // L'humain tranche : « c'est passé ».
    s.effects
        .resolve_unknown(&id, UnknownDecision::MarkCompleted(json!({"sent": true})))
        .await
        .unwrap();
    match s.effects.plan(spec()).await.unwrap() {
        Planned::Replayed(v) => assert_eq!(v["sent"], true),
        other => panic!("{other:?}"),
    }
}

/// Un outil déclaré idempotent peut, lui, repartir sans humain.
#[tokio::test]
async fn an_idempotent_tool_may_be_retried_without_a_human() {
    let dir = tempfile::tempdir().unwrap();
    let spec = || {
        EffectSpec::new(
            EffectKind::Mcp,
            "mcp__forge__get_issue",
            json!({"id": 4312}),
        )
        .idempotent(true)
    };

    {
        let s = boot(dir.path()).await;
        let id = match s.effects.plan(spec()).await.unwrap() {
            Planned::Fresh(id) => id,
            other => panic!("{other:?}"),
        };
        s.effects.dispatching(&id).await.unwrap();
    }

    let s = boot(dir.path()).await;
    let d = Daemon::from_services(s.clone());
    let r = d.recover().await.unwrap();
    assert_eq!(r.effects_unknown, 0, "un outil idempotent ne bloque pas");
    assert!(r.is_clean());
    assert!(s.approvals.pending(10).await.unwrap().is_empty());
}

/// §4.3 : coupure **avant** les en-têtes ⇒ `send_unknown`, coût possiblement facturé.
#[tokio::test]
async fn ca_17_4_llm_calls_are_classified_by_where_the_crash_happened() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = boot(dir.path()).await;
        // Partie, pas d'en-têtes.
        s.llm_state
            .plan(
                "q_avant",
                None,
                None,
                "anthropic/claude-sonnet-4.5",
                "openrouter",
                &json!({"m": 1}),
            )
            .await
            .unwrap();
        assert!(s.llm_state.dispatching("q_avant").await.unwrap());

        // Partie, en-têtes reçus : sûrement facturée.
        s.llm_state
            .plan(
                "q_apres",
                None,
                None,
                "anthropic/claude-sonnet-4.5",
                "openrouter",
                &json!({"m": 2}),
            )
            .await
            .unwrap();
        s.llm_state.dispatching("q_apres").await.unwrap();
        s.llm_state.response_started("q_apres").await.unwrap();

        // Jamais partie.
        s.llm_state
            .plan(
                "q_jamais",
                None,
                None,
                "anthropic/claude-sonnet-4.5",
                "openrouter",
                &json!({"m": 3}),
            )
            .await
            .unwrap();
    }

    let s = boot(dir.path()).await;
    let d = Daemon::from_services(s.clone());
    let r = d.recover().await.unwrap();
    assert_eq!(r.llm_unknown, 1);

    let avant = s.llm_state.get("q_avant").await.unwrap().unwrap();
    assert_eq!(avant.state, penelope_llm::state::LlmState::SendUnknown);
    assert!(avant.maybe_billed);

    let apres = s.llm_state.get("q_apres").await.unwrap().unwrap();
    assert_eq!(apres.state, penelope_llm::state::LlmState::Failed);
    assert!(apres.maybe_billed, "les en-têtes reçus ⇒ facturé");

    let jamais = s.llm_state.get("q_jamais").await.unwrap().unwrap();
    assert_eq!(jamais.state, penelope_llm::state::LlmState::Planned);

    // La politique décide du renvoi automatique, pas le hasard.
    assert!(s.llm_state.may_retry("openrouter"));
    assert!(!s.llm_state.may_retry("un-provider-inconnu"));
    let strict = penelope_llm::state::LlmStateMachine::new(s.store.clone(), s.clock.clone())
        .with_policy(penelope_llm::state::UnknownSendPolicy::AskHuman);
    assert!(!strict.may_retry("openrouter"));
}

/// CA 17 : un run de workflow reprend à son étape courante, pas au début.
#[tokio::test]
async fn ca_17_5_a_run_resumes_at_its_current_step() {
    use penelope_workflow::model::StepResult;

    let dir = tempfile::tempdir().unwrap();
    let (run_id, second_step) = {
        let s = boot(dir.path()).await;
        let wf = s
            .workflows
            .get("ticket-to-deploy")
            .expect("workflow fourni");
        let sid = a_session(&s).await;
        let run = s
            .runs
            .create(&wf, &sid, json!({"ticket": 4312}), None, None, 0)
            .await
            .unwrap();
        let first = run.current_step.clone().unwrap();
        // Une transition sortante quelconque, pour avancer d'une étape réelle.
        let next = wf
            .step(&first)
            .unwrap()
            .transitions
            .first()
            .map(|t| t.goto.clone())
            .expect("au moins une transition");
        s.runs
            .advance(
                &run.id,
                &first,
                &StepResult::Success,
                json!({"ok": true}),
                &next,
                None,
            )
            .await
            .unwrap();
        (run.id, next)
    };

    let s = boot(dir.path()).await;
    let d = Daemon::from_services(s.clone());
    assert_eq!(d.recover().await.unwrap().runs_resumed, 1);

    let run = s.runs.get(&run_id).await.unwrap().unwrap();
    assert_eq!(run.state, penelope_workflow::RunState::Running);
    assert_eq!(run.current_step.as_deref(), Some(second_step.as_str()));
    assert_eq!(run.iterations, 1, "l'étape franchie n'est pas refaite");
    assert_eq!(run.step_outputs["__last"]["ok"], true);
    assert_eq!(s.runs.trace(&run_id).await.unwrap().len(), 1);
}

/// Le journal d'événements reste vérifiable après le crash, et une altération se voit.
#[tokio::test]
async fn ca_17_6_the_event_chain_survives_and_detects_tampering() {
    use penelope_kernel::event::EventDraft;

    let dir = tempfile::tempdir().unwrap();
    {
        let s = boot(dir.path()).await;
        for i in 0..20 {
            s.events
                .append(EventDraft::new("turn.started", json!({"n": i})))
                .await
                .unwrap();
        }
    }

    let s = boot(dir.path()).await;
    let report = s.events.verify().await.unwrap();
    assert!(report.ok, "{report:?}");
    assert!(report.checked >= 20);
    assert_eq!(s.store.integrity().unwrap(), "ok");

    // Sauvegarde à chaud : la copie est lisible et vérifiable elle aussi.
    let backup = dir.path().join("copie.db");
    s.store.backup_to(&backup).unwrap();
    assert!(backup.is_file());

    // Altération directe d'une charge utile : la chaîne casse au bon endroit.
    s.store
        .write_blocking(|tx| {
            tx.execute(
                "UPDATE events SET payload = '{\"n\":999}' WHERE id = (SELECT MIN(id)+3 FROM events)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let broken = s.events.verify().await.unwrap();
    assert!(!broken.ok, "l'altération doit être détectée");
    assert!(broken.first_broken.is_some());
}

/// Un effet en vol dans le **même** processus n'est pas planifié deux fois.
#[tokio::test]
async fn double_planning_inside_one_process_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let s = boot(dir.path()).await;
    let spec = || EffectSpec::new(EffectKind::Tool, "shell_exec", json!({"cmd": "cargo test"}));

    let id = match s.effects.plan(spec()).await.unwrap() {
        Planned::Fresh(id) => id,
        other => panic!("{other:?}"),
    };
    // Tant qu'il n'est que `planned`, replanifier reprend le même effet : même
    // identifiant, donc une seule ligne dans le ledger.
    match s.effects.plan(spec()).await.unwrap() {
        Planned::Fresh(again) => assert_eq!(again.as_str(), id.as_str()),
        other => panic!("{other:?}"),
    }

    // Une fois parti, la seconde planification voit qu'il est en vol.
    s.effects.dispatching(&id).await.unwrap();
    match s.effects.plan(spec()).await.unwrap() {
        Planned::InFlight(other) => assert_eq!(other.as_str(), id.as_str()),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        s.effects
            .count_by_state(penelope_kernel::effects::EffectState::Dispatching)
            .await
            .unwrap(),
        1
    );
}

/// La configuration publiée avant le crash est celle qui revient au démarrage :
/// `mutate` écrit le fichier avant de publier la génération N+1 (§4.4).
#[tokio::test]
async fn the_configuration_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (path, generation) = {
        let s = boot(dir.path()).await;
        let d = Daemon::from_services(s.clone());
        let g = d
            .publish_config("test", |c| {
                c.context.compaction_threshold = 0.71;
                Ok(vec!["context.compaction_threshold".into()])
            })
            .unwrap();
        (s.config.path().to_path_buf(), g)
    };
    assert_eq!(generation, 2);

    let s = boot(dir.path()).await;
    let reloaded = penelope_kernel::config::ConfigStore::load_or_create(
        &path,
        Some(s.store.clone()),
        s.clock.clone(),
        42,
    )
    .unwrap();
    assert_eq!(
        reloaded.config().context.compaction_threshold,
        0.71,
        "la génération publiée doit être relue du disque"
    );
    assert_eq!(
        reloaded.generation(),
        1,
        "un démarrage repart à la génération 1"
    );
}
