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
            id: id.into(),
            pid,
            session_id: "s_essai".into(),
            job_id: Some(job.into()),
            effect_id: None,
            extra: json!({"model": "local:essai", "kind": "explore"}),
            started_ms: e.d.services.clock.now_ms(),
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
        [job.clone()],
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
