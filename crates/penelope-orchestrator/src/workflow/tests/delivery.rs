//! Livraison d'un plan en dev (#192) contre de faux forgeurs GitHub et GitLab : une seule
//! PR par run, même après un redémarrage ; CI en attente, verte, rouge, indisponible ;
//! E2E vert et rouge avec ses preuves ; configuration absente demandée au propriétaire.

use super::fake_forge::{FakeForge, REPO, TOKEN};
use super::*;
use penelope_kernel::effects::{EffectKind, EffectSpec};
use penelope_kernel::session::MetadataOp;
use penelope_workflow::delivery::config::ForgeKind;
use penelope_workflow::delivery::{self, RETRY, STOP};
use std::path::{Path, PathBuf};

const WORK: &str = "penelope/stop";
const DEV: &str = "develop";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Pénélope", "-c", "user.email=p@exemple.fr"])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} : {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Un dépôt de travail sur `penelope/stop`, dont le remote `origin` est un dépôt nu local
/// où `develop` existe déjà. `ci` : fichier de CI commité (découverte de la CI).
fn repository(root: &Path, ci: Option<&str>) -> PathBuf {
    let remote = root.join("remote.git");
    let work = root.join("depot");
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&remote, &["init", "-q", "--bare"]);
    git(&work, &["init", "-q", "-b", DEV]);
    std::fs::write(work.join("README.md"), "# Service\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "départ"]);
    git(
        &work,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(&work, &["push", "-q", "origin", DEV]);
    git(&work, &["checkout", "-q", "-b", WORK]);
    std::fs::write(work.join("stop.txt"), "/stop arrête le run\n").unwrap();
    if let Some(path) = ci {
        let file = work.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "ci: true\n").unwrap();
    }
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "stop définitif"]);
    work
}

/// `.penelope/delivery.toml` pointé sur le faux forgeur : le remote est local, forgeur,
/// API et dépôt sont donc déclarés. Le fichier n'est pas suivi : il ne rend pas le dépôt
/// « sale ».
fn configure(dir: &Path, forge: &FakeForge, ci: &str, checks: &str) {
    let raw = format!(
        "[forge]\nkind = \"{}\"\napi = \"{}\"\nrepo = \"{REPO}\"\n\n[branches]\ndev = \"{DEV}\"\n\n\
         {ci}\n[e2e]\nurl = \"{}\"\n{checks}",
        forge.kind.as_str(),
        forge.base,
        forge.base,
    );
    std::fs::create_dir_all(dir.join(".penelope")).unwrap();
    std::fs::write(dir.join(".penelope/delivery.toml"), raw).unwrap();
}

struct Bench {
    e: Env,
    forge: FakeForge,
    dir: PathBuf,
}

async fn bench(kind: ForgeKind, ci_file: Option<&str>) -> Bench {
    let e = env().await;
    let forge = FakeForge::start(kind).await;
    let dir = repository(&e.d.dir.path().join("projets"), ci_file);
    Bench { e, forge, dir }
}

impl Bench {
    fn token(&self) {
        let name = self.forge.kind.default_token_secret();
        self.e.d.services.platform.secrets.set(name, TOKEN).unwrap();
    }

    /// Un run dont le plan a été accepté par sa dernière revue : il entre en livraison.
    async fn start(&self) -> String {
        let s = &self.e.d.services;
        let steps = serde_json::to_value(delivery::tail(DONE)).unwrap();
        install(
            &self.e.d,
            json!({
                "metadata": {"id": "livrer", "name": "Plan v1 · Rendre /stop définitif",
                             "description": "Plan approuvé v1", "parameters": []},
                "entryStep": delivery::ENTRY,
                "settings": {"maxIterations": 30, "budget": {"maxUsd": 1.0}},
                "steps": steps,
            }),
        )
        .await;
        let run = start_run(&self.e.d, "livrer", json!({}), &owner(), None, 0)
            .await
            .unwrap();
        s.sessions
            .metadata(
                &run.session_id,
                MetadataOp::Set,
                "project",
                json!({"dir": self.dir}),
            )
            .await
            .unwrap();
        run.id
    }

    async fn run(&self, id: &str) -> Run {
        self.e.d.services.runs.get(id).await.unwrap().unwrap()
    }

    async fn at(&self, id: &str) -> String {
        self.run(id).await.current_step.unwrap_or_default()
    }

    async fn answer(&self, id: &str, choice: &str) {
        let r = self.run(id).await;
        let visit = format!("{}.{}", r.current_step.unwrap(), r.iterations);
        answer(&self.e.d, id, &visit, choice, None).await.unwrap();
    }

    fn last_question(&self) -> String {
        self.e
            .r
            .questions()
            .last()
            .map(|q| q.1.clone())
            .unwrap_or_default()
    }
}

/// Le daemon redémarre : même base, aucun pilote ne tient plus le run.
fn restarted(e: &Env) -> Context {
    Context {
        workflows: Arc::new(State::with_ports(e.d.ports.clone())),
        ..e.d.cx.clone()
    }
}

#[tokio::test]
async fn github_pr_is_opened_once_then_ci_and_e2e_are_two_green_results() {
    let b = bench(ForgeKind::GitHub, Some(".github/workflows/ci.yml")).await;
    // Pas de `[ci]` : la CI est découverte dans le dépôt.
    configure(
        &b.dir,
        &b.forge,
        "",
        "[[e2e.checks]]\npath = \"/health\"\ncontains = \"ok\"\n\n\
         [[e2e.checks]]\nkind = \"graphql\"\nquery = \"{ version }\"\n",
    );
    b.token();
    b.forge.ci("pending");
    b.forge.with(|w| {
        w.dev
            .insert("/health".into(), (200, r#"{"status":"ok"}"#.into()));
        w.dev.insert(
            "/graphql".into(),
            (200, r#"{"data":{"version":"1.4"}}"#.into()),
        );
    });
    let run = b.start().await;

    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Running);
    assert_eq!(
        b.at(&run).await,
        "livraison-ci",
        "PR ouverte, CI en attente"
    );
    assert_eq!(b.forge.requests("POST").len(), 1);
    let pr = b.forge.with(|w| w.prs[0].clone());
    assert_eq!(pr["head"]["ref"], WORK);
    assert_eq!(pr["base"]["ref"], DEV, "vers la branche de dev déclarée");
    let remote = b.dir.parent().unwrap().join("remote.git");
    assert_eq!(
        git(&remote, &["rev-parse", WORK]),
        git(&b.dir, &["rev-parse", "HEAD"]),
        "la branche est poussée avant la PR"
    );
    let texts = b.e.r.texts().join("\n");
    assert!(texts.contains("pull request dev ouverte"), "{texts}");
    assert!(texts.contains("CI en attente"), "{texts}");

    // Redémarrage pendant l'attente : rien n'est rouvert, la CI n'est pas relue avant
    // son échéance.
    let d2 = restarted(&b.e);
    b.e.d.services.effects.recover_on_boot().await.unwrap();
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Running);
    assert_eq!(
        b.forge.requests("GET /repos/equipe/service/commits").len(),
        2
    );

    b.forge.ci("green");
    b.e.clock.advance_secs(16);
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Done);
    assert_eq!(
        b.forge.requests("POST /repos").len(),
        1,
        "une seule PR par run"
    );

    let done = b.run(&run).await;
    let ci = &done.step_outputs["livraison-ci"];
    assert_eq!(ci["result"], "passed");
    assert_eq!(ci["ci"]["verdict"], "green");
    let e2e = &done.step_outputs["livraison-e2e"];
    assert_eq!(e2e["result"], "passed", "{e2e}");
    let checks = e2e["e2e"]["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0]["check"], "GET /health");
    assert_eq!(checks[0]["status"], 200);
    assert_eq!(checks[1]["request"]["method"], "POST");
    let workdir = PathBuf::from(done.workdir.unwrap());
    let kept: Value = serde_json::from_str(
        &std::fs::read_to_string(workdir.join(e2e["evidence"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(kept["checks"], e2e["e2e"]["checks"], "preuves conservées");
    let texts = b.e.r.texts().join("\n");
    assert!(texts.contains("CI verte"), "{texts}");
    assert!(texts.contains("E2E vert"), "{texts}");
}

#[tokio::test]
async fn gitlab_mr_opened_before_a_crash_is_found_not_recreated() {
    let b = bench(ForgeKind::GitLab, None).await;
    configure(&b.dir, &b.forge, "[ci]\nprovider = \"gitlab\"\n", "");
    b.token();
    b.forge.ci("green");
    b.forge.with(|w| {
        w.dev.insert("/".into(), (200, "ok".into()));
    });
    let run = b.start().await;
    let s = &b.e.d.services;
    let session = b.run(&run).await.session_id;

    // Vie antérieure : l'intention était au ledger, le POST a abouti chez le forgeur,
    // puis le daemon est mort avant d'en noter le résultat.
    let spec = EffectSpec::new(
        EffectKind::Http,
        "delivery.pull_request",
        json!({"forge": "gitlab", "api": b.forge.base, "repo": REPO, "head": WORK, "base": DEV}),
    )
    .run(&run)
    .session(&session)
    .step(delivery::ENTRY)
    .idempotent(true);
    let Planned::Fresh(id) = s.effects.plan(spec).await.unwrap() else {
        panic!("effet neuf attendu");
    };
    s.effects.dispatching(&id).await.unwrap();
    b.forge.open(WORK, DEV);
    s.effects.recover_on_boot().await.unwrap();

    let d2 = restarted(&b.e);
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Done);
    assert!(b.forge.requests("POST").is_empty(), "aucune seconde MR");
    assert_eq!(b.forge.with(|w| w.prs.len()), 1);
    let done = b.run(&run).await;
    let pr = &done.step_outputs["livraison-pr"];
    assert_eq!(pr["pr"]["found"], true);
    assert_eq!(pr["pr"]["number"], 1);
    assert!(
        pr["content"]
            .as_str()
            .unwrap()
            .starts_with("merge request dev retrouvée"),
        "{pr}"
    );
    let effect = s.effects.get(&id).await.unwrap().unwrap();
    assert_eq!(effect.state.as_str(), "completed");
    assert_eq!(
        b.forge
            .requests("GET /projects/equipe%2Fservice/pipelines")
            .len(),
        1,
        "la CI lue sur le commit de la MR"
    );
}

#[tokio::test]
async fn a_red_ci_blocks_on_a_card_and_retry_reads_it_again() {
    let b = bench(ForgeKind::GitHub, None).await;
    configure(&b.dir, &b.forge, "[ci]\nprovider = \"github\"\n", "");
    b.token();
    b.forge.ci("red");
    b.forge.with(|w| {
        w.dev.insert("/".into(), (200, "ok".into()));
    });
    let run = b.start().await;

    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Running);
    assert_eq!(b.at(&run).await, "livraison-ci-bloquee");
    let q = b.last_question();
    assert!(q.contains("CI rouge"), "{q}");
    assert!(q.contains("tests (failure)"), "{q}");
    assert_eq!(b.e.r.questions().last().unwrap().2, [RETRY, STOP]);
    assert_eq!(
        b.run(&run).await.step_outputs["livraison-ci"]["ci"]["verdict"],
        "red"
    );
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(
        b.at(&run).await,
        "livraison-ci-bloquee",
        "bloqué jusqu'au choix"
    );

    b.forge.ci("green");
    b.answer(&run, RETRY).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Done);
    assert_eq!(
        b.forge.requests("POST").len(),
        1,
        "la PR n'est pas rouverte"
    );
}

#[tokio::test]
async fn an_unreachable_ci_is_retried_then_blocks_as_unavailable() {
    let b = bench(ForgeKind::GitLab, Some(".gitlab-ci.yml")).await;
    configure(&b.dir, &b.forge, "", "");
    b.token();
    b.forge.ci("pending");
    b.forge.with(|w| {
        w.dev.insert("/".into(), (200, "ok".into()));
    });
    let run = b.start().await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-ci");

    b.forge.with(|w| w.down = true);
    for wait in [16, 16] {
        b.e.clock.advance_secs(wait);
        assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Running);
        assert_eq!(
            b.at(&run).await,
            "livraison-ci",
            "une panne passagère attend"
        );
    }
    b.e.clock.advance_secs(31);
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-ci-bloquee");
    let q = b.last_question();
    assert!(q.contains("CI indisponible"), "{q}");
    assert!(q.contains("3 lectures d'affilée"), "{q}");

    b.forge.with(|w| w.down = false);
    b.forge.ci("green");
    b.answer(&run, RETRY).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Done);
}

#[tokio::test]
async fn a_ci_without_verdict_stops_waiting_at_its_timeout() {
    let b = bench(ForgeKind::GitHub, None).await;
    configure(
        &b.dir,
        &b.forge,
        "[ci]\nprovider = \"github\"\ntimeout_minutes = 1\n",
        "",
    );
    b.token();
    b.forge.ci("pending");
    let run = b.start().await;
    drive(&b.e.d, &run).await.unwrap();
    for wait in [16, 31] {
        b.e.clock.advance_secs(wait);
        drive(&b.e.d, &run).await.unwrap();
        assert_eq!(b.at(&run).await, "livraison-ci");
    }
    b.e.clock.advance_secs(61);
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-ci-bloquee");
    assert!(b.last_question().contains("CI sans verdict"));
}

#[tokio::test]
async fn a_red_e2e_keeps_its_evidence_and_stop_blocks_the_run() {
    let b = bench(ForgeKind::GitHub, None).await;
    configure(
        &b.dir,
        &b.forge,
        "[ci]\nprovider = \"none\"\n",
        "[[e2e.checks]]\npath = \"/health\"\n",
    );
    b.token();
    b.forge.with(|w| {
        w.dev
            .insert("/health".into(), (503, "en maintenance".into()));
    });
    let run = b.start().await;

    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-e2e-bloquee");
    assert!(
        b.forge
            .requests("GET /repos/equipe/service/commits")
            .is_empty(),
        "`none` : aucune CI lue"
    );
    let r = b.run(&run).await;
    assert_eq!(r.step_outputs["livraison-ci"]["result"], "passed");
    let e2e = &r.step_outputs["livraison-e2e"];
    assert_eq!(e2e["result"], "failed");
    assert_eq!(e2e["e2e"]["checks"][0]["status"], 503);
    assert_eq!(e2e["e2e"]["checks"][0]["excerpt"], "en maintenance");
    let q = b.last_question();
    assert!(q.contains("E2E rouge"), "{q}");
    assert!(q.contains("GET /health : statut 503 (attendu 2xx)"), "{q}");
    let workdir = PathBuf::from(r.workdir.unwrap());
    assert!(workdir.join(e2e["evidence"].as_str().unwrap()).is_file());

    b.answer(&run, STOP).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Blocked);
    let reason = b.run(&run).await.error.unwrap_or_default();
    assert!(
        reason.ends_with("livraison arrêtée par le propriétaire"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_missing_configuration_is_asked_for_precisely_and_nothing_is_assumed() {
    let b = bench(ForgeKind::GitHub, None).await;
    let run = b.start().await;

    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-pr-bloquee");
    let q = b.last_question();
    for key in ["`forge.kind`", "`forge.repo`", "`branches.dev`"] {
        assert!(q.contains(key), "{key} : {q}");
    }
    assert!(q.contains(".penelope/delivery.toml"), "{q}");
    assert!(b.forge.requests("").is_empty(), "aucun forgeur supposé");
    let remote = b.dir.parent().unwrap().join("remote.git");
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&remote)
            .args(["rev-parse", "--verify", "-q", WORK])
            .output()
            .unwrap()
            .stdout
            .is_empty(),
        "rien n'est poussé sans configuration"
    );

    // Configuration complétée, jeton toujours absent : c'est lui qui est demandé.
    configure(&b.dir, &b.forge, "[ci]\nprovider = \"none\"\n", "");
    b.answer(&run, RETRY).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-pr-bloquee");
    let q = b.last_question();
    assert!(q.contains("secret `github_token`"), "{q}");
    assert!(q.contains("penelope secret set github_token"), "{q}");

    b.token();
    b.answer(&run, RETRY).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(
        b.at(&run).await,
        "livraison-e2e-bloquee",
        "PR ouverte, pas de CI, l'E2E sans URL de dev servie"
    );
    assert_eq!(b.forge.requests("POST").len(), 1);
}

#[tokio::test]
async fn uncommitted_work_or_the_dev_branch_itself_is_not_delivered() {
    let b = bench(ForgeKind::GitHub, None).await;
    configure(&b.dir, &b.forge, "[ci]\nprovider = \"none\"\n", "");
    b.token();
    std::fs::write(b.dir.join("stop.txt"), "modifié sans commit\n").unwrap();
    let run = b.start().await;
    drive(&b.e.d, &run).await.unwrap();
    assert!(b.last_question().contains("ne sont pas commités"));

    git(&b.dir, &["checkout", "-q", "--", "stop.txt"]);
    git(&b.dir, &["checkout", "-q", DEV]);
    b.answer(&run, RETRY).await;
    drive(&b.e.d, &run).await.unwrap();
    assert!(
        b.last_question().contains("la branche de dev elle-même"),
        "{}",
        b.last_question()
    );
    assert!(b.forge.requests("POST").is_empty());
}
