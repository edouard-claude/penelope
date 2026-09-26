use super::*;
use crate::runtime::Daemon;
use penelope_app::engine::{SessionModels, TurnIntake};
use penelope_kernel::clock::TestClock;
use penelope_kernel::turn::Turn;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ToolCall;
use penelope_ops::session_ops;
use penelope_store::Store;
use serde_json::Value;
use std::sync::Arc;

fn jobs() -> (Store, JobStore, TestClock) {
    let store = Store::open_memory().unwrap();
    let clock = TestClock::default();
    let j = JobStore::new(store.clone(), Arc::new(clock.clone()));
    (store, j, clock)
}

fn spec(session: &str, tool: &str) -> NewJob {
    NewJob {
        session_id: session.into(),
        run_id: None,
        turn_id: Some("t1".into()),
        call_id: Some("call_1".into()),
        tool: tool.into(),
        request: json!({"command": "sleep 30"}),
        effect_id: Some("ef_1".into()),
    }
}

/// #204 : un job naît `working`, se retrouve par son identifiant, et son résultat
/// arrive à la fin — le modèle durable des tâches MCP, appliqué aux outils natifs.
#[tokio::test]
async fn a_job_lives_until_its_result_lands() {
    let (_s, j, _clock) = jobs();
    let job = j.create(spec("s1", "shell_exec")).await.unwrap();
    assert_eq!(job.state, TaskState::Working);
    assert_eq!(j.of_session("s1", true).await.unwrap().len(), 1);
    assert_eq!(j.counts("s1").await.unwrap(), (1, 1));

    assert!(
        j.finish(&job.id, TaskState::Completed, Some(json!({"exit_code": 0})))
            .await
            .unwrap()
    );
    let got = j.get(&job.id).await.unwrap().unwrap();
    assert_eq!(got.state, TaskState::Completed);
    assert_eq!(got.result.unwrap()["exit_code"], 0);
    assert!(j.of_session("s1", true).await.unwrap().is_empty());
    assert_eq!(j.of_session("s1", false).await.unwrap().len(), 1);
    assert_eq!(j.counts("s1").await.unwrap(), (0, 0));
}

/// #204 : le premier état terminal gagne. `/stop` annule, la commande tuée rend son
/// échec juste après : le job reste `cancelled`.
#[tokio::test]
async fn the_first_terminal_state_wins() {
    let (_s, j, _clock) = jobs();
    let job = j.create(spec("s1", "shell_exec")).await.unwrap();
    assert!(j.finish(&job.id, TaskState::Cancelled, None).await.unwrap());
    assert!(
        !j.finish(&job.id, TaskState::Failed, Some(json!({"error": "tué"})))
            .await
            .unwrap()
    );
    assert_eq!(
        j.get(&job.id).await.unwrap().unwrap().state,
        TaskState::Cancelled
    );
}

/// #204 : un job terminé attend sa livraison, et ne part qu'une fois — même si deux
/// boucles le voient ensemble.
#[tokio::test]
async fn a_finished_job_is_delivered_exactly_once() {
    let (_s, j, _clock) = jobs();
    let job = j.create(spec("s1", "shell_exec")).await.unwrap();
    assert!(j.undelivered(10).await.unwrap().is_empty(), "pas fini");
    j.finish(&job.id, TaskState::Completed, None).await.unwrap();

    let due = j.undelivered(10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, job.id);
    assert!(j.mark_delivered(&job.id).await.unwrap());
    assert!(!j.mark_delivered(&job.id).await.unwrap(), "une seule fois");
    assert!(j.undelivered(10).await.unwrap().is_empty());
}

/// #204 : au redémarrage, un job en cours est perdu avec son processus. Il devient
/// `failed` sans être relancé (décision 0012), et reste à livrer pour que le modèle
/// cesse de l'attendre.
#[tokio::test]
async fn a_running_job_is_lost_on_restart_and_never_replayed() {
    let (store, j, clock) = jobs();
    let job = j.create(spec("s1", "shell_exec")).await.unwrap();
    let done = j.create(spec("s1", "shell_exec")).await.unwrap();
    j.finish(&done.id, TaskState::Completed, None)
        .await
        .unwrap();
    j.mark_delivered(&done.id).await.unwrap();

    let after = JobStore::new(store, Arc::new(clock));
    let lost = after.recover_on_boot().await.unwrap();
    assert_eq!(lost.len(), 1, "seul le job vivant est perdu");
    assert_eq!(lost[0].id, job.id);
    let got = after.get(&job.id).await.unwrap().unwrap();
    assert_eq!(got.state, TaskState::Failed);
    assert!(
        got.result.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("redémarré")
    );
    assert_eq!(
        after.undelivered(10).await.unwrap().len(),
        1,
        "le job perdu se dit, il ne disparaît pas"
    );
}

/// #204 : les plafonds se comptent par session **et** pour tout le daemon.
#[tokio::test]
async fn caps_count_per_session_and_globally() {
    let (_s, j, _clock) = jobs();
    for _ in 0..2 {
        j.create(spec("s1", "shell_exec")).await.unwrap();
    }
    j.create(spec("s2", "sub_agent_spawn")).await.unwrap();
    assert_eq!(j.counts("s1").await.unwrap(), (2, 3));
    assert_eq!(j.counts("s2").await.unwrap(), (1, 3));
    assert_eq!(j.live().await.unwrap().len(), 3);
}

#[test]
fn a_job_reports_its_age() {
    let job = ToolJob {
        id: "tj_1".into(),
        session_id: "s1".into(),
        run_id: None,
        turn_id: None,
        call_id: None,
        tool: "shell_exec".into(),
        request: json!({}),
        state: TaskState::Working,
        result: None,
        effect_id: None,
        delivered_at: None,
        created_at: "2026-09-23T10:00:00Z".into(),
        updated_at: "2026-09-23T10:00:00Z".into(),
    };
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-23T10:02:30Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(job.age_s(now), 150);
    assert_eq!(job.view(now)["state"], "working");
}

// ------------------------------------------------------------ de bout en bout

async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Arc::new(
        penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    (dir, d, p)
}

/// Une session de chat qui n'exige pas d'approbation : la carte n'est pas le sujet.
async fn session(d: &Arc<Daemon>) -> String {
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    crate::approval_mode::set(
        &d.services,
        &sid,
        penelope_agent::ApprovalMode::parse("auto"),
    )
    .await
    .unwrap();
    sid
}

async fn claim(d: &Daemon) -> Turn {
    d.services.turns.claim("test").await.unwrap().unwrap()
}

/// Un tour qui lance `command` en arrière-plan puis répond.
async fn background_turn(d: &Arc<Daemon>, p: &Arc<MockProvider>, sid: &str, command: &str) {
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": command, "background": true}),
        }],
    ));
    p.reply("C'est parti, je te dis quand c'est fini.");
    d.enqueue_message(sid, "lance les tests", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(d).await;
    let out = d.run_turn(&turn).await;
    assert!(
        matches!(out, penelope_agent::TurnOutcome::Answered { .. }),
        "le tour doit répondre sans attendre l'outil : {out:?}"
    );
    d.services.turns.complete(&turn).await.unwrap();
}

/// Attend qu'un job soit terminal, ou échoue au bout de dix secondes.
async fn settled(d: &Arc<Daemon>, id: &str) -> ToolJob {
    for _ in 0..200 {
        let job = store(&d.services).get(id).await.unwrap().unwrap();
        if job.state.is_terminal() {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job {id} toujours en cours");
}

/// #204 : `shell_exec background=true` sur un `sleep` — le tour répond tout de suite,
/// et le résultat arrive plus tard dans la session par un tour `Nudge`.
#[tokio::test]
async fn a_background_shell_frees_the_turn_and_comes_back_as_a_nudge() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    background_turn(&d, &p, &sid, "sleep 0.3").await;

    let jobs = store(&d.services).of_session(&sid, false).await.unwrap();
    assert_eq!(jobs.len(), 1, "un job créé");
    let job = &jobs[0];
    assert_eq!(job.tool, "shell_exec");
    assert!(job.effect_id.is_some(), "le job porte son effet du ledger");

    // Ce que le modèle a lu : l'appel a rendu la main, pas la sortie de la commande.
    let results = tool_results(&d, &sid).await;
    assert!(
        results.iter().any(|r| r.contains("lancé en arrière-plan")),
        "{results:?}"
    );

    // Pendant ce temps, l'effet reste `dispatching` : rien ne contourne le ledger.
    let done = settled(&d, &job.id).await;
    assert_eq!(done.state, TaskState::Completed, "{done:?}");

    assert_eq!(deliver_due(&d).await.unwrap(), 1);
    assert_eq!(deliver_due(&d).await.unwrap(), 0, "livré une seule fois");
    let next = claim(&d).await;
    assert_eq!(next.kind, TurnKind::Nudge);
    let text = next.payload["text"].as_str().unwrap();
    assert!(
        text.contains(&job.id) && text.contains("sleep 0.3"),
        "{text}"
    );
}

/// #204 : la livraison ne dépend de personne. La boucle de fond rend le résultat
/// dans la conversation sans qu'un tour ni une commande aient à la réclamer.
#[tokio::test]
async fn the_delivery_loop_brings_the_result_back_on_its_own() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    let loop_handle = tokio::spawn(deliver_loop(d.core.clone()));
    background_turn(&d, &p, &sid, "true").await;

    let mut delivered = None;
    for _ in 0..200 {
        if let Some(turn) = d.services.turns.claim("livraison").await.unwrap() {
            delivered = Some(turn);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    d.handle.shutdown();
    loop_handle.abort();
    let turn = delivered.expect("aucune relance livrée par la boucle");
    assert_eq!(turn.kind, TurnKind::Nudge);
    assert!(
        turn.payload["text"]
            .as_str()
            .unwrap()
            .contains("Résultat du job")
    );
}

/// #204 : un job n'existe que pour une conversation. Un sous-agent ou une étape de
/// workflow qui demande l'arrière-plan exécute comme avant : sa session meurt avec son
/// travail, la relance n'y trouverait personne, et le workflow a déjà son attente
/// d'étape. Le flag est ignoré, l'appel rend son vrai résultat.
#[tokio::test]
async fn a_sub_agent_session_runs_the_call_instead_of_making_a_job() {
    let (_dir, d, _p) = daemon().await;
    let s = d.services.clone();
    let sub = s
        .sessions
        .create(penelope_kernel::session::SessionKind::SubAgent, None)
        .await
        .unwrap()
        .id
        .to_string();
    let effect = match s
        .effects
        .plan(
            penelope_kernel::effects::EffectSpec::new(
                penelope_kernel::effects::EffectKind::Tool,
                "shell_exec",
                json!({"command": "true"}),
            )
            .session(&sub),
        )
        .await
        .unwrap()
    {
        penelope_kernel::effects::Planned::Fresh(id) => id,
        o => panic!("{o:?}"),
    };
    let call = ToolCall {
        id: "c1".into(),
        name: "shell_exec".into(),
        arguments: json!({"command": "true", "background": true}),
    };
    let out = maybe_spawn(
        &s,
        &Paniquant,
        JobRequest {
            session_id: &sub,
            run_id: None,
            turn_id: None,
            call: &call,
            tool: "shell_exec",
            effect: &effect,
        },
    )
    .await
    .unwrap();
    assert!(out.is_none(), "l'appel doit suivre le chemin ordinaire");
    assert!(store(&s).of_session(&sub, false).await.unwrap().is_empty());
}

/// #204 : un message du propriétaire pendant un job est traité sans attendre sa fin.
#[tokio::test]
async fn an_owner_message_is_answered_while_a_job_runs() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    background_turn(&d, &p, &sid, "sleep 30").await;
    let job = store(&d.services).of_session(&sid, true).await.unwrap()[0].clone();

    p.reply("Oui, j'écoute.");
    d.enqueue_message(&sid, "laisse tomber en fait", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = tokio::time::timeout(std::time::Duration::from_secs(20), d.run_turn(&turn))
        .await
        .expect("le message ne doit pas attendre le job");
    d.services.turns.complete(&turn).await.unwrap();
    assert!(
        matches!(out, penelope_agent::TurnOutcome::Answered { .. }),
        "{out:?}"
    );
    assert_eq!(
        store(&d.services)
            .get(&job.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        TaskState::Working,
        "le job tourne toujours"
    );
    d.services.jobs.cancel_all();
}

/// #204 : `/stop` annule les jobs de la session, le processus est tué, et le
/// ramassage des orphelins ne trouve rien (issues #57 et #65).
#[tokio::test]
async fn stop_cancels_the_jobs_of_a_session_and_leaves_no_orphan() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    background_turn(&d, &p, &sid, "sleep 300").await;
    let job = store(&d.services).of_session(&sid, true).await.unwrap()[0].clone();

    assert_eq!(d.services.jobs.cancel_session(&sid), 1);
    let done = settled(&d, &job.id).await;
    assert_eq!(done.state, TaskState::Cancelled);

    // Le processus du job était bien déclaré au ramasse-miettes (#65) : c'est ce qui
    // fait que `reap_orphans` les connaît. Et il est mort : rien à tuer.
    use penelope_platform::ProcessHost;
    let pid_dir = d.services.platform.dirs.pid_dir();
    let pids = |dir: &std::path::Path| {
        std::fs::read_dir(dir)
            .map(|e| {
                e.flatten()
                    .filter(|f| f.path().extension().is_some_and(|x| x == "pid"))
                    .count()
            })
            .unwrap_or(0)
    };
    assert!(pids(&pid_dir) >= 1, "le job n'a pas déclaré son processus");
    let orphans = d
        .services
        .platform
        .processes
        .reap_orphans(&pid_dir)
        .unwrap_or_default();
    assert!(
        orphans.is_empty(),
        "processus orphelins vivants : {orphans:?}"
    );
    assert_eq!(pids(&pid_dir), 0, "étiquettes PID non ramassées");
}

/// #204 : au plafond, le job suivant est refusé avec ce qu'il faut pour s'en sortir,
/// et son effet est clos — il ne reste pas en attente d'un appel qui n'aura pas lieu.
#[tokio::test]
async fn the_job_over_the_cap_is_refused_with_what_to_do() {
    let (_dir, d, p) = daemon().await;
    d.publish_config("test", |c| {
        c.tools.jobs_per_session = 1;
        Ok(vec!["tools.jobs_per_session".into()])
    })
    .unwrap();
    let sid = session(&d).await;
    background_turn(&d, &p, &sid, "sleep 30").await;
    background_turn(&d, &p, &sid, "sleep 31").await;

    let results = tool_results(&d, &sid).await;
    let refus = results
        .iter()
        .find(|r| r.contains("Job refusé"))
        .unwrap_or_else(|| panic!("aucun refus : {results:?}"));
    assert!(refus.contains("jobs_per_session"), "{refus}");
    assert!(refus.contains("job_wait"), "il dit quoi faire : {refus}");
    assert_eq!(
        store(&d.services)
            .of_session(&sid, false)
            .await
            .unwrap()
            .len(),
        1,
        "le refus ne crée pas de ligne"
    );
    d.services.jobs.cancel_all();
}

/// #204 : un job terminé alors que la session est fermée n'est pas perdu ; il part à
/// la réouverture.
#[tokio::test]
async fn a_job_finished_in_a_closed_session_is_delivered_when_it_reopens() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    background_turn(&d, &p, &sid, "true").await;
    let job = store(&d.services).of_session(&sid, false).await.unwrap()[0].clone();
    settled(&d, &job.id).await;
    let (s, pr, bus) = (&d.services, d.providers.clone(), &d.bus);
    session_ops::close(s, pr, bus, &sid).await.unwrap();
    assert_eq!(
        deliver_due(&d).await.unwrap(),
        0,
        "rien dans une session fermée"
    );
    assert_eq!(store(&d.services).undelivered(10).await.unwrap().len(), 1);

    d.services.sessions.set_state(&sid, "active").await.unwrap();
    assert_eq!(deliver_due(&d).await.unwrap(), 1);
}

/// Exécuteur de test qui panique : #84 a montré qu'une tâche qui panique s'arrête en
/// silence. Un job qui disparaîtrait ainsi laisserait son effet `dispatching` pour
/// toujours, et une question au propriétaire au prochain démarrage.
struct Paniquant;

#[async_trait::async_trait]
impl ToolExecutor for Paniquant {
    async fn execute(
        &self,
        _name: &str,
        _args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        panic!("outil en panique");
    }
    fn detached(&self) -> Option<Arc<dyn ToolExecutor + Send + Sync>> {
        Some(Arc::new(Paniquant))
    }
}

/// #204 et #84 : un job dont la tâche panique est conclu `failed`, et son effet est
/// clos — pas laissé `dispatching`.
#[tokio::test]
async fn a_panicking_job_is_closed_not_left_dispatching() {
    let (_dir, d, _p) = daemon().await;
    let s = d.services.clone();
    let sid = session(&d).await;
    let effect = match s
        .effects
        .plan(
            penelope_kernel::effects::EffectSpec::new(
                penelope_kernel::effects::EffectKind::Tool,
                "shell_exec",
                json!({"command": "boum"}),
            )
            .session(&sid),
        )
        .await
        .unwrap()
    {
        penelope_kernel::effects::Planned::Fresh(id) => id,
        o => panic!("{o:?}"),
    };
    let call = ToolCall {
        id: "c1".into(),
        name: "shell_exec".into(),
        arguments: json!({"command": "boum", "background": true}),
    };
    let out = maybe_spawn(
        &s,
        &Paniquant,
        JobRequest {
            session_id: &sid,
            run_id: None,
            turn_id: None,
            call: &call,
            tool: "shell_exec",
            effect: &effect,
        },
    )
    .await
    .unwrap()
    .expect("un job");
    let id = out.value["job"].as_str().unwrap().to_string();

    for _ in 0..100 {
        if store(&s)
            .get(&id)
            .await
            .unwrap()
            .unwrap()
            .state
            .is_terminal()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let job = store(&s).get(&id).await.unwrap().unwrap();
    assert_eq!(job.state, TaskState::Failed, "{job:?}");
    assert!(s.jobs.is_empty(), "le registre est purgé");
    let state: String = s
        .store
        .read({
            let e = effect.as_str().to_string();
            move |c| {
                Ok(
                    c.query_row("SELECT state FROM effects WHERE id = ?1", [&e], |r| {
                        r.get(0)
                    })?,
                )
            }
        })
        .await
        .unwrap();
    assert_eq!(state, "failed", "l'effet ne reste pas `dispatching`");
}

/// #204 et #104 : lancer un job expose les outils de suivi à la session. Sans ça, le
/// résultat dit « `job_status` donne son état » en désignant un outil que le modèle
/// n'a pas sous la main.
#[tokio::test]
async fn starting_a_job_puts_the_follow_up_tools_in_reach() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    assert!(
        tools_on_demand::exposed_for_turn(&d.services, &sid)
            .await
            .is_empty()
    );
    background_turn(&d, &p, &sid, "sleep 30").await;
    let exposed = tools_on_demand::exposed_for_turn(&d.services, &sid).await;
    for t in ["job_status", "job_wait", "job_cancel", "job_list"] {
        assert!(exposed.contains(&t.to_string()), "{t} absent : {exposed:?}");
    }
    d.services.jobs.cancel_all();
}

/// #204 et §6.6 : le résultat d'un job revient dans l'épisode en cours, il n'en ouvre
/// pas un nouveau. Une relance n'est pas un message du propriétaire : elle ne doit ni
/// paraître un changement de sujet, ni clore l'épisode que le propriétaire poursuit.
#[tokio::test]
async fn a_delivered_result_joins_the_episode_it_does_not_open_one() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    background_turn(&d, &p, &sid, "true").await;
    let job = store(&d.services).of_session(&sid, false).await.unwrap()[0].clone();
    settled(&d, &job.id).await;
    let before = d.services.sessions.require(&sid).await.unwrap().episode_seq;

    assert_eq!(deliver_due(&d).await.unwrap(), 1);
    p.reply("Les tests passent.");
    let nudge = claim(&d).await;
    assert_eq!(nudge.kind, TurnKind::Nudge);
    d.run_turn(&nudge).await;
    d.services.turns.complete(&nudge).await.unwrap();

    assert_eq!(
        d.services.sessions.require(&sid).await.unwrap().episode_seq,
        before,
        "une relance ne déplace pas la frontière d'épisode"
    );
}

/// #204 : `job_status`, `job_wait` et `job_cancel` rendent ce que le modèle attend, et
/// `job_wait` est borné — il rend l'état courant plutôt que de bloquer le tour.
#[tokio::test]
async fn the_model_can_follow_and_cancel_a_job() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    background_turn(&d, &p, &sid, "sleep 300").await;
    let job = store(&d.services).of_session(&sid, true).await.unwrap()[0].clone();
    let s = &d.services;

    let listed = tool(s, &sid, "job_list", &json!({})).await.unwrap();
    assert_eq!(listed["running"], 1);

    let status = tool(s, &sid, "job_status", &json!({"job": job.id}))
        .await
        .unwrap();
    assert_eq!(status["state"], "working");

    // Borné : l'attente rend la main sans que le job soit fini.
    let waited = tool(
        s,
        &sid,
        "job_wait",
        &json!({"job": job.id, "timeout_ms": 1000}),
    )
    .await
    .unwrap();
    assert_eq!(waited["state"], "working");
    assert_eq!(waited["waited"], true);

    let cancelled = tool(s, &sid, "job_cancel", &json!({"job": job.id}))
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(settled(&d, &job.id).await.state, TaskState::Cancelled);

    let unknown = tool(s, &sid, "job_status", &json!({"job": "tj_inconnu"})).await;
    assert!(unknown.is_err());
}

/// #204 et #19 : `sub_agent_spawn` est précisément l'outil que le harnais recommande
/// pour les travaux longs (le nudge de #19) — et il était attendu comme les autres.
/// En job, le tour répond tout de suite et la conclusion du sous-agent revient seule.
#[tokio::test]
async fn a_sub_agent_can_run_as_a_job_and_report_back() {
    let (_dir, d, p) = daemon().await;
    d.hooks
        .set_orchestrator(Arc::new(crate::workflow::orchestrator_of(&d.core)));
    let sid = session(&d).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "sub_agent_spawn".into(),
            arguments: json!({
                "kind": "general",
                "prompt": "relis le dépôt et conclus",
                "background": true
            }),
        }],
    ));
    p.reply("Je lui ai confié ça.");
    // Ce que le sous-agent répondra, plus tard, hors du tour.
    p.reply("Conclusion du sous-agent : rien à signaler.");
    d.enqueue_message(&sid, "confie ça à un sous-agent", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    assert!(
        matches!(out, penelope_agent::TurnOutcome::Answered { .. }),
        "{out:?}"
    );
    d.services.turns.complete(&turn).await.unwrap();

    let job = store(&d.services).of_session(&sid, false).await.unwrap()[0].clone();
    assert_eq!(job.tool, "sub_agent_spawn");
    let done = settled(&d, &job.id).await;
    assert_eq!(done.state, TaskState::Completed, "{done:?}");

    assert_eq!(deliver_due(&d).await.unwrap(), 1);
    let nudge = claim(&d).await;
    assert_eq!(nudge.kind, TurnKind::Nudge);
    let text = nudge.payload["text"].as_str().unwrap();
    assert!(text.contains("sub_agent_spawn"), "{text}");
    assert!(
        text.contains("relis le dépôt"),
        "de quoi il est le résultat : {text}"
    );
}

/// #204 et #104 : `tool_call` enveloppe l'appel dans `{name, args}`. La demande
/// d'arrière-plan est dans l'enveloppe : sans la lire là, un `background: true` passé
/// par ce chemin partait en silence dans le tour bloquant.
#[tokio::test]
async fn the_background_flag_is_read_through_tool_call() {
    let (_dir, d, p) = daemon().await;
    let sid = session(&d).await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "tool_call".into(),
            arguments: json!({
                "name": "shell_exec",
                "args": {"command": "sleep 30", "background": true}
            }),
        }],
    ));
    p.reply("C'est lancé.");
    d.enqueue_message(&sid, "lance", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    assert!(
        matches!(out, penelope_agent::TurnOutcome::Answered { .. }),
        "{out:?}"
    );
    let jobs = store(&d.services).of_session(&sid, true).await.unwrap();
    assert_eq!(jobs.len(), 1, "le drapeau doit traverser `tool_call`");
    assert_eq!(jobs[0].tool, "shell_exec");
    d.services.jobs.cancel_all();
}

/// #204 : un `timeout_ms` au-delà de `tools.background_after` propose l'arrière-plan
/// au lieu de le prendre : c'est le modèle qui décide.
#[test]
fn a_long_timeout_is_offered_the_background_not_forced_into_it() {
    let cfg = penelope_kernel::config::Config::sample(1);
    let hint = background_hint(
        &cfg,
        "shell_exec",
        &json!({"command": "x", "timeout_ms": 900_000}),
    )
    .expect("au-delà du seuil");
    assert!(
        hint.contains("background: true") && hint.contains("900"),
        "{hint}"
    );
    assert!(
        background_hint(
            &cfg,
            "shell_exec",
            &json!({"command": "x", "timeout_ms": 5_000})
        )
        .is_none()
    );
    assert!(
        background_hint(
            &cfg,
            "shell_exec",
            &json!({"command": "x", "timeout_ms": 900_000, "background": true})
        )
        .is_none(),
        "déjà en arrière-plan"
    );
    assert!(background_hint(&cfg, "fs_read", &json!({"timeout_ms": 900_000})).is_none());
}

async fn tool_results(d: &Daemon, sid: &str) -> Vec<String> {
    d.services
        .context
        .history
        .load(sid, 0)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.message.role == penelope_llm::types::Role::Tool)
        .map(|e| e.message.text())
        .collect()
}

/// Un job déclaré perdu par la reprise d'un démarrage, dont la tâche conclut ensuite
/// quand même (son processus orphelin vient d'être tué) : la fin tardive ne réécrit ni
/// l'effet ni la ligne. Sans cette garde, l'effet `failed` redevenait `completed`
/// derrière un job `failed`, et `tool_jobs_e2e` échouait une fois sur deux sous charge.
#[tokio::test]
async fn a_late_conclusion_does_not_overwrite_a_job_lost_on_boot() {
    let (_dir, d, _p) = daemon().await;
    let s = d.services.clone();
    let sid = session(&d).await;
    let effect = match s
        .effects
        .plan(
            penelope_kernel::effects::EffectSpec::new(
                penelope_kernel::effects::EffectKind::Tool,
                "shell_exec",
                json!({"command": "sleep 300"}),
            )
            .session(&sid),
        )
        .await
        .unwrap()
    {
        penelope_kernel::effects::Planned::Fresh(id) => id,
        o => panic!("{o:?}"),
    };
    s.effects.dispatching(&effect).await.unwrap();
    let job = store(&s)
        .create(NewJob {
            effect_id: Some(effect.as_str().to_string()),
            ..spec(&sid, "shell_exec")
        })
        .await
        .unwrap();
    assert_eq!(store(&s).recover_on_boot().await.unwrap().len(), 1);

    let late = ToolOutcome {
        value: json!({"exit_code": -1}),
        is_error: false,
        text: "tué".into(),
        eager: false,
    };
    conclude(&s, &job.id, &sid, &effect, Ok(late), false)
        .await
        .unwrap();

    let got = store(&s).get(&job.id).await.unwrap().unwrap();
    assert_eq!(got.state, TaskState::Failed);
    assert!(
        got.result.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("redémarré")
    );
    let state: String = s
        .store
        .read({
            let e = effect.as_str().to_string();
            move |c| {
                Ok(
                    c.query_row("SELECT state FROM effects WHERE id = ?1", [&e], |r| {
                        r.get(0)
                    })?,
                )
            }
        })
        .await
        .unwrap();
    assert_eq!(state, "failed", "la fin tardive ne rouvre pas l'effet");
}
