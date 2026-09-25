//! Jobs d'outils de bout en bout (épopée #208, lot K, tâches T18 à T20 de
//! `design/v1/boucle-et-outils.md`), sur l'API publique du daemon seulement.
//!
//! Les tests unitaires de `tool_jobs.rs` couvrent le magasin et la livraison ; ceux-ci
//! tiennent les critères de fin que le relevé d'écart du lot K a trouvés sans preuve :
//! un sous-agent long en job que `/stop` interrompt, un redémarrage avec un job vivant,
//! le groupe de processus tué, les plafonds par défaut.
//!
//! Les assertions portent sur le monde : lignes de `tool_jobs` et d'`effects`, demandes
//! en base, tours en file, appels au modèle, processus vivants.

use penelope_agent::TurnOutcome;
use penelope_app::bus::Origin;
use penelope_app::services::Services;
use penelope_daemon::Daemon;
use penelope_executor::jobs::{ToolJob, store};
use penelope_kernel::clock::{SharedClock, SystemClock};
use penelope_kernel::turn::TurnKind;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::{ChatRequest, Role, ToolCall};
use penelope_mcp::tasks::TaskState;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

async fn daemon_at(root: &Path) -> (Arc<Daemon>, Arc<MockProvider>) {
    let clock: SharedClock = Arc::new(SystemClock);
    let s = Arc::new(
        Services::for_tests(root.to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    // Sans classifieur : chaque réponse va à la boucle d'agent.
    d.publish_config("test", |c| {
        c.models.routing.classifier = false;
        Ok(vec!["models.routing.classifier".into()])
    })
    .unwrap();
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    d.hooks
        .set_orchestrator(Arc::new(penelope_daemon::workflow::orchestrator_of(&d)));
    (d, p)
}

/// Une session de chat qui n'exige pas d'approbation : la carte n'est pas le sujet.
async fn session(d: &Arc<Daemon>) -> String {
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    penelope_daemon::approval_mode::set(
        &d.services,
        &sid,
        penelope_agent::ApprovalMode::parse("auto"),
    )
    .await
    .unwrap();
    sid
}

/// Envoie un message et exécute son tour, borné : un tour qui attendrait son outil
/// échoue ici au lieu de pendre.
async fn say(d: &Arc<Daemon>, sid: &str, text: &str) -> TurnOutcome {
    d.enqueue_message(sid, text, &Origin::Cli, None)
        .await
        .unwrap()
        .expect("tour créé");
    next_turn(d).await
}

async fn next_turn(d: &Arc<Daemon>) -> TurnOutcome {
    let turn = d.services.turns.claim("test").await.unwrap().unwrap();
    let out = tokio::time::timeout(Duration::from_secs(20), d.run_turn(&turn))
        .await
        .expect("le tour ne doit pas attendre le job");
    d.services.turns.complete(&turn).await.unwrap();
    out
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> Scripted {
    Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }],
    )
}

/// Un tour qui lance `command` en arrière-plan puis répond.
async fn background_shell(d: &Arc<Daemon>, p: &MockProvider, sid: &str, command: &str) {
    p.push(call(
        "c1",
        "shell_exec",
        json!({"command": command, "background": true}),
    ));
    p.reply("C'est parti.");
    let mut out = say(d, sid, "lance-le").await;
    // Une commande composée demande l'accord même en mode `auto` : le propriétaire le
    // donne, le tour reprend. La carte n'est pas le sujet ici.
    if let TurnOutcome::AwaitingApproval { approval_id } = &out {
        penelope_daemon::agent::decide_approval(
            &d.services,
            approval_id,
            &penelope_hitl::Decision::approve_once("cli"),
        )
        .await
        .unwrap();
        d.enqueue_resume(sid, approval_id, &Origin::Cli)
            .await
            .unwrap()
            .expect("tour de reprise créé");
        out = next_turn(d).await;
    }
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
}

async fn only_job(d: &Daemon, sid: &str) -> ToolJob {
    let jobs = store(&d.services).of_session(sid, false).await.unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    jobs[0].clone()
}

/// Attend qu'un job soit terminal, ou échoue au bout de quinze secondes.
async fn settled(d: &Daemon, id: &str) -> ToolJob {
    for _ in 0..300 {
        let job = store(&d.services).get(id).await.unwrap().unwrap();
        if job.state.is_terminal() {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {id} toujours en cours");
}

async fn effect_state(d: &Daemon, id: &str) -> String {
    let id = id.to_string();
    d.services
        .store
        .read(move |c| {
            Ok(
                c.query_row("SELECT state FROM effects WHERE id = ?1", [&id], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap()
}

fn mentions(req: &ChatRequest, needle: &str) -> bool {
    req.messages.iter().any(|m| m.text().contains(needle))
}

const SUB_PROMPT: &str = "relis tout le dépôt, fichier par fichier";

/// T20 : un sous-agent long, lancé en job, ne bloque pas le tour ; `/stop` sur la
/// session l'interrompt au point de contrôle suivant — plus aucun appel au modèle —, le
/// job est `cancelled` et son effet clos (non-régression #57).
///
/// Le sous-agent ne conclut jamais de lui-même : à chaque appel il consulte la
/// documentation, une seconde par appel au modèle. Seul l'arrêt le fait sortir.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_sub_agent_job_frees_the_turn_and_stop_interrupts_it() {
    let dir = tempfile::tempdir().unwrap();
    let (d, p) = daemon_at(dir.path()).await;
    let sid = session(&d).await;
    let seen = Arc::new(AtomicUsize::new(0));
    let n = seen.clone();
    p.slow(Duration::from_millis(300));
    p.set_responder(Some(Arc::new(move |req: &ChatRequest| {
        if mentions(req, SUB_PROMPT) && !mentions(req, "confie-lui") {
            let i = n.fetch_add(1, Ordering::SeqCst);
            return call(
                &format!("s{i}"),
                "self_docs",
                json!({"action": "search", "query": format!("sujet {i}")}),
            );
        }
        let answered = req.messages.last().is_some_and(|m| m.role == Role::Tool);
        if answered {
            Scripted::Text("Je lui ai confié ça, je te dis.".into())
        } else {
            call(
                "c1",
                "sub_agent_spawn",
                json!({
                    "kind": "general",
                    "prompt": SUB_PROMPT,
                    "tools": ["self_docs"],
                    "background": true
                }),
            )
        }
    })));

    let out = say(&d, &sid, "confie-lui la relecture").await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let job = only_job(&d, &sid).await;
    assert_eq!(job.tool, "sub_agent_spawn");

    // Le sous-agent travaille après la fin du tour.
    for _ in 0..100 {
        if seen.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        seen.load(Ordering::SeqCst) >= 2,
        "le sous-agent ne tourne pas"
    );
    let job = store(&d.services).get(&job.id).await.unwrap().unwrap();
    assert_eq!(job.state, TaskState::Working);
    let effect = job.effect_id.clone().unwrap();
    assert_eq!(effect_state(&d, &effect).await, "dispatching");

    // `/stop` : ce que la commande Telegram et `session.stop` appellent pour les jobs.
    assert_eq!(d.services.jobs.cancel_session(&sid), 1);
    let done = settled(&d, &job.id).await;
    assert_eq!(done.state, TaskState::Cancelled, "{done:?}");
    assert_eq!(effect_state(&d, &effect).await, "failed");

    let calls = p.call_count();
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(
        p.call_count(),
        calls,
        "le sous-agent appelle encore le modèle"
    );

    // L'annulation revient dans la conversation, comme toute fin de job.
    assert_eq!(
        penelope_daemon::tool_jobs::deliver_due(&d).await.unwrap(),
        1
    );
    let nudge = d.services.turns.claim("test").await.unwrap().unwrap();
    assert_eq!(nudge.kind, TurnKind::Nudge);
    assert!(
        nudge.payload["text"]
            .as_str()
            .unwrap()
            .contains("cancelled"),
        "{}",
        nudge.payload
    );
}

async fn cards_for(d: &Daemon, effect: &str) -> usize {
    d.services
        .approvals
        .pending(50)
        .await
        .unwrap()
        .iter()
        .filter(|a| a.payload["effect_id"] == effect)
        .count()
}

async fn effects_of(d: &Daemon, tool: &str) -> usize {
    let tool = tool.to_string();
    d.services
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM effects WHERE tool = ?1",
                [&tool],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap() as usize
}

/// T19 : un redémarrage pendant un job. Le job devient `failed` avec sa raison, son
/// effet aussi — un job est observable, sa complétion n'est pas inconnaissable —, aucune
/// carte `effect_unknown` ne part, même après un second démarrage, rien n'est relancé, et
/// un tour `Nudge` le dit dans la conversation (décision 0012, révisée au lot K).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_during_a_job_fails_it_and_says_so_without_a_card() {
    let dir = tempfile::tempdir().unwrap();
    let (d, p) = daemon_at(dir.path()).await;
    let sid = session(&d).await;
    background_shell(&d, &p, &sid, "sleep 300").await;
    let job = only_job(&d, &sid).await;
    let effect = job.effect_id.clone().unwrap();
    assert_eq!(effect_state(&d, &effect).await, "dispatching");

    // Redémarrage : un autre daemon reprend la même base.
    let (after, _p) = daemon_at(dir.path()).await;
    let report = after.recover().await.unwrap();
    assert_eq!(report.tool_jobs_lost, 1, "{report:?}");
    assert_eq!(report.effects_unknown, 0, "{report:?}");

    let lost = store(&after.services).get(&job.id).await.unwrap().unwrap();
    assert_eq!(lost.state, TaskState::Failed);
    assert!(
        lost.result.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("redémarré")
    );
    assert_eq!(effect_state(&after, &effect).await, "failed");
    assert_eq!(cards_for(&after, &effect).await, 0, "aucune carte");

    // Le modèle l'apprend dans la conversation.
    assert_eq!(
        penelope_daemon::tool_jobs::deliver_due(&after)
            .await
            .unwrap(),
        1
    );
    let nudge = after.services.turns.claim("test").await.unwrap().unwrap();
    assert_eq!(nudge.kind, TurnKind::Nudge);
    assert_eq!(nudge.session_id, sid);
    let text = nudge.payload["text"].as_str().unwrap();
    assert!(
        text.contains(&job.id) && text.contains("redémarré") && text.contains("sleep 300"),
        "{text}"
    );

    // Un second démarrage ne relance rien, ne redemande rien, ne relivre rien.
    after.recover().await.unwrap();
    assert_eq!(cards_for(&after, &effect).await, 0);
    assert_eq!(
        penelope_daemon::tool_jobs::deliver_due(&after)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store(&after.services)
            .of_session(&sid, false)
            .await
            .unwrap()
            .len(),
        1,
        "aucun job relancé"
    );
    assert_eq!(
        effects_of(&after, "shell_exec").await,
        1,
        "aucun effet rejoué"
    );
    d.services.jobs.cancel_all();
}

/// Processus vivants dont la ligne de commande contient `pattern`.
fn alive(pattern: &str) -> usize {
    std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
        .unwrap_or(0)
}

/// T19 : `/stop` tue le **groupe** de processus du job, pas seulement le shell. Une
/// commande qui lance ses propres enfants (`a | b`) ne laisse aucun survivant, et le
/// ramasse-miettes des orphelins n'a plus rien à faire (#57, #65).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_kills_the_whole_process_group_of_a_job() {
    let dir = tempfile::tempdir().unwrap();
    let (d, p) = daemon_at(dir.path()).await;
    let sid = session(&d).await;
    // Durées singulières : `pgrep` ne doit voir que les enfants de ce test.
    let marker = format!("sleep 30{}.", std::process::id() % 1000);
    let command = format!("{marker}1 | {marker}2");
    background_shell(&d, &p, &sid, &command).await;
    let job = only_job(&d, &sid).await;

    for _ in 0..100 {
        if alive(&marker) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(alive(&marker) >= 2, "les deux enfants doivent tourner");

    assert_eq!(d.services.jobs.cancel_session(&sid), 1);
    assert_eq!(settled(&d, &job.id).await.state, TaskState::Cancelled);
    for _ in 0..100 {
        if alive(&marker) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(alive(&marker), 0, "un enfant du job a survécu à `/stop`");

    use penelope_platform::ProcessHost;
    let pid_dir = d.services.platform.dirs.pid_dir();
    let orphans = d
        .services
        .platform
        .processes
        .reap_orphans(&pid_dir)
        .unwrap_or_default();
    assert!(orphans.is_empty(), "orphelins : {orphans:?}");
}

/// Résultats d'outils que le modèle a lus dans une session.
async fn tool_results(d: &Daemon, sid: &str) -> Vec<String> {
    d.services
        .context
        .history
        .load(sid, 0)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.message.role == Role::Tool)
        .map(|e| e.message.text())
        .collect()
}

/// Le dernier refus de job lu par le modèle dans cette session.
async fn refusal(d: &Daemon, sid: &str) -> String {
    let results = tool_results(d, sid).await;
    results
        .iter()
        .rev()
        .find(|r| r.contains("Job refusé"))
        .cloned()
        .unwrap_or_else(|| panic!("aucun refus : {results:?}"))
}

/// T18 : les plafonds par défaut, 3 jobs par conversation et 10 pour tout le daemon.
/// Le job de trop est refusé avec un texte qui nomme le plafond et dit quoi faire, ne
/// crée aucune ligne, et son effet est clos au lieu de rester en attente.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_caps_refuse_the_fourth_job_of_a_session_and_the_eleventh_overall() {
    let dir = tempfile::tempdir().unwrap();
    let (d, p) = daemon_at(dir.path()).await;
    let cfg = d.services.config.config();
    assert_eq!((cfg.tools.jobs_per_session, cfg.tools.jobs_total), (3, 10));

    let sid = session(&d).await;
    // Trois commandes distinctes : un appel identique à un effet en cours est écarté
    // plus tôt (« Appel déjà en cours »), ce n'est pas le plafond.
    for i in 0..3 {
        background_shell(&d, &p, &sid, &format!("sleep 300.{i}")).await;
    }
    background_shell(&d, &p, &sid, "sleep 301").await;
    let text = refusal(&d, &sid).await;
    assert!(
        text.contains("jobs_per_session") && text.contains("= 3") && text.contains("job_wait"),
        "{text}"
    );
    let jobs = store(&d.services).of_session(&sid, false).await.unwrap();
    assert_eq!(jobs.len(), 3, "le refus ne crée pas de ligne");
    assert_eq!(effects_of(&d, "shell_exec").await, 4);
    let refused: i64 = d
        .services
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM effects WHERE tool = 'shell_exec' AND state = 'failed'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(refused, 1, "l'effet du job refusé est clos");

    // Sept jobs vivants ailleurs : le daemon en porte dix.
    for i in 0..7 {
        store(&d.services)
            .create(penelope_executor::jobs::NewJob {
                session_id: format!("ailleurs-{i}"),
                run_id: None,
                turn_id: None,
                call_id: None,
                tool: "shell_exec".into(),
                request: json!({"command": "sleep 300"}),
                effect_id: None,
            })
            .await
            .unwrap();
    }
    let other = d
        .services
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    d.pin_model(&other, Some("main")).await.unwrap();
    penelope_daemon::approval_mode::set(
        &d.services,
        &other,
        penelope_agent::ApprovalMode::parse("auto"),
    )
    .await
    .unwrap();
    background_shell(&d, &p, &other, "sleep 302").await;
    let text = refusal(&d, &other).await;
    assert!(
        text.contains("jobs_total") && text.contains("= 10"),
        "{text}"
    );
    assert!(
        store(&d.services)
            .of_session(&other, false)
            .await
            .unwrap()
            .is_empty()
    );
    d.services.jobs.cancel_all();
}
