use super::*;
use std::sync::{Arc, Mutex};

async fn services() -> (tempfile::TempDir, Services) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    (dir, s)
}

fn answered(text: &str) -> TurnOutcome {
    TurnOutcome::Answered {
        text: text.into(),
        iterations: 3,
        cost_usd: 0.0,
    }
}

fn exhausted() -> TurnOutcome {
    TurnOutcome::Failed {
        error: format!("{CALLS_EXHAUSTED} en 24 itérations"),
    }
}

async fn started(s: &Services, check: Option<&str>) -> Mission {
    let (m, text) = start(
        s,
        "s1",
        Request {
            goal: "ajouter la commande rapport".into(),
            check: check.map(String::from),
            ..Default::default()
        },
        &Origin::Cli,
    )
    .await
    .unwrap();
    assert!(text.starts_with("[mission] "), "{text}");
    assert!(text.contains("ajouter la commande rapport"), "{text}");
    assert!(text.contains(DONE) && text.contains(BLOCKED), "{text}");
    m
}

#[derive(Default)]
struct Ports {
    queued: Mutex<Vec<(String, String)>>,
    compacted: Mutex<u32>,
    notified: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl MissionPorts for Ports {
    async fn enqueue(&self, _session_id: &str, text: &str, _origin: &Origin, dedup: String) {
        self.queued.lock().unwrap().push((dedup, text.to_string()));
    }
    async fn compact(&self, _session_id: &str) {
        *self.compacted.lock().unwrap() += 1;
    }
    async fn notify(&self, _origin: &Origin, text: &str) {
        self.notified.lock().unwrap().push(text.to_string());
    }
}

#[tokio::test]
async fn an_exhausted_turn_continues_the_mission_with_its_brief() {
    let (_d, s) = services().await;
    started(&s, None).await;
    assert!(is_running(&s, "s1").await);
    assert!(absorbs(&s, "s1", &exhausted()).await);
    assert!(!absorbs(&s, "autre", &exhausted()).await);

    let ports = Ports::default();
    after_turn(&s, &ports, "s1", &exhausted()).await;
    let queued = ports.queued.lock().unwrap().clone();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].0, "mission:s1:1");
    // L'objectif revient en tête de chaque relance : il survit à la compaction.
    assert!(queued[0].1.contains("ajouter la commande rapport"));
    assert!(queued[0].1.contains("plafond d'appels"));
    assert_eq!(*ports.compacted.lock().unwrap(), 1);
    assert_eq!(load(&s, "s1").await.unwrap().turns, 1);
}

#[tokio::test]
async fn done_without_check_finishes_and_tells_the_owner() {
    let (_d, s) = services().await;
    started(&s, None).await;
    let ports = Ports::default();
    after_turn(
        &s,
        &ports,
        "s1",
        &answered("Tout est fait. MISSION TERMINÉE"),
    )
    .await;
    assert!(ports.queued.lock().unwrap().is_empty());
    let m = load(&s, "s1").await.unwrap();
    assert_eq!(m.state, State::Done);
    assert!(ports.notified.lock().unwrap()[0].contains("Mission terminée"));
    // Un tour de plus après la fin n'est pas relancé.
    after_turn(&s, &ports, "s1", &exhausted()).await;
    assert!(ports.queued.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_red_check_sends_its_output_back_and_a_green_one_finishes() {
    let (_d, s) = services().await;
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("vert");
    let check = format!(
        "test -f {} || (echo 'test rapport échoue'; exit 1)",
        flag.display()
    );
    started(&s, Some(&check)).await;
    let ports = Ports::default();

    after_turn(&s, &ports, "s1", &answered("Fini. MISSION TERMINÉE")).await;
    let queued = ports.queued.lock().unwrap().clone();
    assert_eq!(queued.len(), 1, "la vérification rouge relance");
    assert!(
        queued[0].1.contains("test rapport échoue"),
        "{}",
        queued[0].1
    );
    assert_eq!(load(&s, "s1").await.unwrap().state, State::Running);

    std::fs::write(&flag, "").unwrap();
    after_turn(&s, &ports, "s1", &answered("Corrigé. MISSION TERMINÉE")).await;
    assert_eq!(load(&s, "s1").await.unwrap().state, State::Done);
    assert!(ports.notified.lock().unwrap()[0].contains("est vert"));
}

#[tokio::test]
async fn blocked_answers_and_idle_streaks_hand_back() {
    let (_d, s) = services().await;
    started(&s, None).await;
    let ports = Ports::default();
    after_turn(
        &s,
        &ports,
        "s1",
        &answered("Il me manque l'accès. MISSION BLOQUÉE"),
    )
    .await;
    assert_eq!(load(&s, "s1").await.unwrap().state, State::Blocked);

    let (_d, s) = services().await;
    started(&s, None).await;
    for _ in 0..IDLE_MAX {
        after_turn(&s, &ports, "s1", &answered("J'avance.")).await;
        assert_eq!(load(&s, "s1").await.unwrap().state, State::Running);
    }
    after_turn(&s, &ports, "s1", &answered("J'avance encore.")).await;
    assert_eq!(load(&s, "s1").await.unwrap().state, State::Waiting);
}

#[tokio::test]
async fn the_budget_and_the_owner_stop_a_mission() {
    let (_d, s) = services().await;
    let (mut m, _) = start(
        &s,
        "s1",
        Request {
            goal: "objectif".into(),
            budget_usd: Some(0.5),
            ..Default::default()
        },
        &Origin::Cli,
    )
    .await
    .unwrap();
    s.budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some("s1".into()),
            model: "m".into(),
            provider: "p".into(),
            cost_usd: 0.75,
            ..Default::default()
        })
        .await
        .unwrap();
    match decide(&s, &mut m, &exhausted()).await {
        Next::Finish(State::Budget, text) => assert!(text.contains("budget atteint"), "{text}"),
        other => panic!("{other:?}"),
    }

    let (_d, s) = services().await;
    started(&s, None).await;
    assert!(
        start(
            &s,
            "s1",
            Request {
                goal: "x".into(),
                ..Default::default()
            },
            &Origin::Cli
        )
        .await
        .is_err()
    );
    assert_eq!(stop(&s, "s1").await.unwrap().state, State::Stopped);
    assert!(!is_running(&s, "s1").await);
    let ports = Ports::default();
    after_turn(&s, &ports, "s1", &exhausted()).await;
    assert!(ports.queued.lock().unwrap().is_empty());
}

#[test]
fn a_one_line_request_is_parsed_with_its_options() {
    let r = parse_request(
        "ajouter l'export CSV --check cargo test -p atelier --cwd ~/Code/atelier --hook cargo fmt -- {file} --budget 7,5 --heures 2",
    )
    .unwrap();
    assert_eq!(r.goal, "ajouter l'export CSV");
    assert_eq!(r.check.as_deref(), Some("cargo test -p atelier"));
    assert_eq!(r.cwd.as_deref(), Some("~/Code/atelier"));
    assert_eq!(r.hook.as_deref(), Some("cargo fmt -- {file}"));
    assert_eq!(r.budget_usd, Some(7.5));
    assert_eq!(r.hours, Some(2.0));
    assert_eq!(parse_request("seulement l'objectif").unwrap().check, None);
    assert!(parse_request("--check make").is_err());
    assert!(parse_request("x --budget beaucoup").is_err());
    assert!(parse_request("x --vite oui").is_err());
}

#[test]
fn the_repository_preamble_brings_its_rules_and_commands() {
    let dir = tempfile::tempdir().unwrap();
    assert!(repo_preamble(dir.path()).is_none());
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        "# Atelier\nToujours `cargo test`.",
    )
    .unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        ".PHONY: test\ntest:\n\tcargo test\nVAR = 1\nbump:\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"scripts":{"build":"vite","test":"vitest"}}"#,
    )
    .unwrap();
    let p = repo_preamble(dir.path()).unwrap();
    assert!(
        p.contains("tirées de son CLAUDE.md")
            && p.contains("pas une consigne du propriétaire")
            && p.contains("Toujours `cargo test`"),
        "{p}"
    );
    assert!(
        p.contains("`cargo test`") && p.contains("`make test`") && p.contains("`make bump`"),
        "{p}"
    );
    assert!(
        p.contains("`npm run build`") && !p.contains("make VAR"),
        "{p}"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_rules_file_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let secret = tempfile::tempdir().unwrap();
    std::fs::write(secret.path().join("id_ed25519"), "CLÉ PRIVÉE").unwrap();
    std::os::unix::fs::symlink(
        secret.path().join("id_ed25519"),
        dir.path().join("CLAUDE.md"),
    )
    .unwrap();
    assert!(repo_preamble(dir.path()).is_none_or(|p| !p.contains("CLÉ PRIVÉE")));
}

#[tokio::test]
async fn a_mission_outside_the_workspaces_is_refused() {
    let (_d, s) = services().await;
    let outside = tempfile::tempdir().unwrap();
    let req = |cwd: String| Request {
        goal: "objectif".into(),
        cwd: Some(cwd),
        ..Default::default()
    };
    let e = start(
        &s,
        "s1",
        req(outside.path().to_string_lossy().into()),
        &Origin::Cli,
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("aucun workspace"), "{e}");
    let ws = crate::helpers::default_workspaces(&s).remove(0);
    let inside = ws.join("projet").join("..").join("projet");
    std::fs::create_dir_all(&inside).unwrap();
    assert!(
        start(&s, "s1", req(inside.to_string_lossy().into()), &Origin::Cli)
            .await
            .is_ok()
    );
}
