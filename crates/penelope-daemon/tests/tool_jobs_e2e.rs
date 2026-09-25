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

use penelope_daemon::agent::TurnOutcome;
use penelope_daemon::bus::Origin;
use penelope_daemon::runtime::{Daemon, Services};
use penelope_daemon::tool_jobs::{ToolJob, store};
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
        penelope_daemon::approval_mode::ApprovalMode::parse("auto"),
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
