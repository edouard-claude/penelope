//! #395, reprise : au démarrage, un enfant noté qui a fini pendant le redémarrage voit son
//! job conclu avec son résultat ; un enfant mort sans résultat laisse un échec lisible.

use super::*;
use penelope_app::child::{FromChild, line, registry};
use penelope_executor::jobs::{NewJob, store};

async fn open_job(e: &Env) -> String {
    store(&e.d.services)
        .create(NewJob {
            session_id: "s_essai".into(),
            run_id: None,
            turn_id: None,
            call_id: Some("c1".into()),
            tool: "sub_agent_spawn".into(),
            request: json!({"kind": "explore", "background": true}),
            effect_id: None,
        })
        .await
        .unwrap()
        .id
}

fn note(e: &Env, id: &str, job: &str, pid: u32) -> std::path::PathBuf {
    let state = e.d.services.platform.dirs.state();
    registry::write(
        &state,
        &registry::Entry {
            job_id: Some(job.into()),
            extra: json!({"model": "local:essai", "kind": "explore"}),
            ..registry::Entry::of(id, pid, "s_essai", e.d.services.clock.now_ms())
        },
    )
    .unwrap();
    registry::socket(&state, id).unwrap()
}

async fn settled(e: &Env, job: &str) -> penelope_executor::jobs::ToolJob {
    for _ in 0..100 {
        let j = store(&e.d.services).get(job).await.unwrap().unwrap();
        if j.state.is_terminal() {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job {job} toujours ouvert");
}

#[tokio::test]
async fn a_child_that_finished_during_the_restart_concludes_its_job() {
    let e = env().await;
    let job = open_job(&e).await;
    // Un pid qui n'existe plus : l'enfant a fini et laissé son résultat.
    let sock = note(&e, "c_fini", &job, 999_999);
    let done = FromChild::Done {
        result: json!({"text": "Rapport rendu après le redémarrage."}),
    };
    std::fs::write(penelope_app::child::result_path(&sock), line(&done)).unwrap();
    assert_eq!(
        penelope_app::child::registry::live_jobs(&e.d.services.platform.dirs.state()),
        std::slice::from_ref(&job),
        "la reprise des jobs le laisse ouvert"
    );
    crate::workflow::child::reattach_all(&e.d).await;
    let j = settled(&e, &job).await;
    assert_eq!(j.state.as_str(), "completed");
    let r = j.result.unwrap();
    assert_eq!(r["text"], "Rapport rendu après le redémarrage.");
    assert_eq!(r["value"]["kind"], "explore");
    assert!(
        registry::list(&e.d.services.platform.dirs.state()).is_empty(),
        "fiche effacée"
    );
}

#[tokio::test]
async fn a_child_that_died_during_the_restart_fails_its_job_readably() {
    let e = env().await;
    let job = open_job(&e).await;
    note(&e, "c_mort", &job, 999_998);
    crate::workflow::child::reattach_all(&e.d).await;
    let j = settled(&e, &job).await;
    assert_eq!(j.state.as_str(), "failed");
    let text = j.result.unwrap()["text"].as_str().unwrap().to_string();
    assert!(text.contains("redémarrage du daemon"), "{text}");
}

/// #454 : une fiche sans job fait arrêter son enfant au démarrage. Si le pid a été redonné
/// à un autre processus depuis, c'est lui qui recevait le signal.
#[tokio::test]
async fn a_pid_given_to_another_process_is_not_stopped() {
    let e = env().await;
    let state = e.d.services.platform.dirs.state();
    // Chef de son groupe, comme un enfant lancé par le daemon : c'est le groupe qu'on tue.
    let sleeper = || {
        use std::os::unix::process::CommandExt;
        std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap()
    };
    let (mut other, mut ours) = (sleeper(), sleeper());
    let reused = registry::Entry {
        pid_started: Some("Thu Jan 1 00:00:00 1970".into()),
        ..registry::Entry::of("c_repris", other.id(), "s_essai", 1)
    };
    registry::write(&state, &reused).unwrap();
    registry::write(
        &state,
        &registry::Entry::of("c_enfant", ours.id(), "s_essai", 1),
    )
    .unwrap();

    crate::workflow::child::reattach_all(&e.d).await;
    assert!(registry::list(&state).is_empty(), "fiches effacées");
    assert!(ours.wait().unwrap().code().is_none(), "l'enfant est arrêté");
    assert!(
        other.try_wait().unwrap().is_none(),
        "le processus d'un autre a été arrêté"
    );
    other.kill().unwrap();
    other.wait().unwrap();
}
