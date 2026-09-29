//! Gate de production (#193) contre les faux forgeurs : tout vert sans clic ne propose
//! rien ; un bouton périmé, un refus ou une nouvelle révision du plan non plus ; une
//! approbation valide, même suivie d'un redémarrage, ouvre une seule PR dev → prod et en
//! dit le lien dans le sujet.

use super::delivery::{Bench, DEV, PROD, WORK, bench, configure, git, restarted};
use super::fake_forge::REPO;
use super::*;

/// Commit de fusion de la PR dev dans `develop`.
const MERGE: &str = "feedface0000111122223333444455556666777788";
use penelope_kernel::effects::{EffectKind, EffectSpec};
use penelope_workflow::delivery::config::ForgeKind;
use penelope_workflow::delivery::gate::{self, APPROVE, GATE_STEP, PROD_STEP, RECHECK, REFUSE};
use penelope_workflow::delivery::{self, RETRY};
use penelope_workflow::plan::{Phase, Plan, PlanDraft, PlanStep, PlanStore};

/// Toute la livraison puis le gate, comme un plan compilé les enchaîne.
fn delivered() -> Vec<penelope_workflow::model::Step> {
    let mut steps = delivery::tail(gate::REPORT_STEP);
    steps.extend(gate::steps(DONE));
    steps
}

/// Un run livré en dev, CI et E2E verts, arrêté sur la carte d'approbation.
async fn at_the_gate(kind: ForgeKind) -> (Bench, String) {
    let b = bench(kind, None).await;
    configure(
        &b.dir,
        &b.forge,
        &format!("[ci]\nprovider = \"{}\"\n", kind.as_str()),
        "",
    );
    b.token();
    b.forge.ci("green");
    b.forge.with(|w| {
        w.dev.insert("/".into(), (200, "ok".into()));
    });
    let run = b.start_with(delivered()).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Running);
    assert_eq!(b.at(&run).await, GATE_STEP, "{:?}", b.last_question());
    (b, run)
}

impl Bench {
    /// Le commit de la branche de travail.
    fn sha(&self) -> String {
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    /// Le propriétaire fusionne la PR dev (sur le commit vérifié) dans `develop`, puis
    /// fait re-vérifier : la carte neuve porte le bilan du commit de fusion.
    async fn land_dev_pr(&self, run: &str) {
        self.forge.head(1, &self.sha());
        self.forge.merge(1, MERGE);
        self.answer(run, RECHECK).await;
        assert_eq!(drive(&self.e.d, run).await.unwrap(), RunState::Running);
        assert_eq!(self.at(run).await, GATE_STEP, "{}", self.last_question());
    }

    fn prod_prs(&self) -> Vec<Value> {
        self.forge.prs_to(PROD)
    }

    /// Les cartes d'approbation posées jusqu'ici.
    fn gate_cards(&self) -> usize {
        self.e
            .r
            .questions()
            .iter()
            .filter(|q| q.2.iter().any(|c| c == APPROVE))
            .count()
    }

    fn visit(&self, run: &Run) -> String {
        format!("{}.{}", run.current_step.clone().unwrap(), run.iterations)
    }
}

#[tokio::test]
async fn all_green_without_a_click_proposes_nothing_and_the_wait_survives_a_restart() {
    let (b, run) = at_the_gate(ForgeKind::GitHub).await;
    b.land_dev_pr(&run).await;
    let card = b.last_question();
    for part in [
        "Bilan vérifié avant la production",
        "/pull/1",
        &format!("`{WORK}` → `{DEV}`"),
        &format!("commit `{}`", &b.sha()[..8]),
        "fusionnée : commit de fusion `feedface`",
        "CI : verte sur `feedface`",
        "E2E : vert",
        &format!("`{DEV}` → `{PROD}`"),
        "ni fusionnée ni déployée",
    ] {
        assert!(card.contains(part), "{part} : {card}");
    }
    let bilan = &b.run(&run).await.step_outputs[gate::REPORT_STEP];
    assert_eq!(bilan["fingerprint"].as_str().unwrap().len(), 64, "{bilan}");

    // Personne ne clique : ni le temps, ni les passages du pilote, ni un redémarrage ne
    // proposent quoi que ce soit, et la carte n'est pas reposée.
    for _ in 0..3 {
        b.e.clock.advance_secs(3_600);
        assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Running);
    }
    let d2 = restarted(&b.e);
    b.e.d.services.effects.recover_on_boot().await.unwrap();
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Running);
    assert_eq!(b.at(&run).await, GATE_STEP);
    assert!(b.prod_prs().is_empty(), "zéro PR prod sans clic");
    assert_eq!(
        b.forge.requests("POST").len(),
        1,
        "la seule PR est celle de dev"
    );
    assert_eq!(
        b.gate_cards(),
        2,
        "l'attente est durable, la carte n'est pas reposée"
    );
}

async fn approval_then_restart_opens_one_prod_pr(kind: ForgeKind) {
    let (b, run) = at_the_gate(kind).await;
    b.land_dev_pr(&run).await;
    let before = b.run(&run).await;
    let old_visit = b.visit(&before);
    b.answer(&run, APPROVE).await;

    // Le daemon meurt juste après le clic : l'approbation est durable, la reprise la sert.
    let d2 = restarted(&b.e);
    b.e.d.services.effects.recover_on_boot().await.unwrap();
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Done);
    let prs = b.prod_prs();
    assert_eq!(prs.len(), 1, "{prs:?}");
    let body = prs[0]["body"]
        .as_str()
        .or(prs[0]["description"].as_str())
        .unwrap();
    assert!(
        body.contains(&format!("penelope-prod-run: {run}")),
        "{body}"
    );
    assert!(body.contains("ne déploie rien"), "{body}");

    let done = b.run(&run).await;
    let out = &done.step_outputs[PROD_STEP];
    assert_eq!(out["result"], "passed");
    assert_eq!(out["prod_pr"]["found"], false);
    let url = out["prod_pr"]["url"].as_str().unwrap().to_string();
    let texts = b.e.r.texts().join("\n");
    assert!(texts.contains(&format!("prod proposée : {url}")), "{texts}");
    assert!(texts.contains("Rien n'est fusionné ni déployé"), "{texts}");

    // Un second redémarrage, un clic rejoué sur l'ancienne carte : rien de plus.
    let d3 = restarted(&b.e);
    assert_eq!(drive(&d3, &run).await.unwrap(), RunState::Done);
    assert!(
        answer(&b.e.d, &run, &old_visit, APPROVE, None)
            .await
            .is_err()
    );
    assert_eq!(b.prod_prs().len(), 1, "une seule PR prod");
    assert_eq!(
        b.forge.requests("POST").len(),
        2,
        "dev puis prod, une fois chacune"
    );
}

#[tokio::test]
async fn github_approval_then_restart_opens_exactly_one_prod_pr() {
    approval_then_restart_opens_one_prod_pr(ForgeKind::GitHub).await;
}

#[tokio::test]
async fn gitlab_approval_then_restart_opens_exactly_one_prod_mr() {
    approval_then_restart_opens_one_prod_pr(ForgeKind::GitLab).await;
}

#[tokio::test]
async fn a_prod_pr_opened_before_a_crash_is_found_not_reopened() {
    let (b, run) = at_the_gate(ForgeKind::GitLab).await;
    b.land_dev_pr(&run).await;
    b.answer(&run, APPROVE).await;
    let s = &b.e.d.services;
    let session = b.run(&run).await.session_id;
    // Vie antérieure : l'effet était en cours, la MR ouverte chez le forgeur, puis le daemon
    // est mort avant d'en noter le résultat.
    let spec = EffectSpec::new(
        EffectKind::Http,
        "delivery.prod_pull_request",
        json!({"run": run, "pr": "prod"}),
    )
    .run(&run)
    .session(&session)
    .step(PROD_STEP)
    .idempotent(true);
    let Planned::Fresh(id) = s.effects.plan(spec).await.unwrap() else {
        panic!("effet neuf attendu");
    };
    s.effects.dispatching(&id).await.unwrap();
    b.forge.with(|w| {
        let n = w.prs.len() as u64 + 1;
        w.prs.push(json!({
            "iid": n, "state": "opened", "source_branch": DEV, "target_branch": PROD,
            "web_url": format!("{}/{REPO}/-/merge_requests/{n}", b.forge.base),
            "description": format!("…\n<!-- penelope-prod-run: {run} -->"),
        }));
    });
    // Et le bilan a vieilli entre-temps : l'approbation déjà exécutée ne se rejuge pas.
    b.e.clock.advance_secs(7_200);
    s.effects.recover_on_boot().await.unwrap();

    let d2 = restarted(&b.e);
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Done);
    assert_eq!(b.prod_prs().len(), 1);
    assert_eq!(
        b.forge.requests("POST").len(),
        1,
        "seule la MR dev a été postée"
    );
    let out = &b.run(&run).await.step_outputs[PROD_STEP];
    assert_eq!(out["prod_pr"]["found"], true);
    assert!(
        out["content"]
            .as_str()
            .unwrap()
            .starts_with("merge request prod retrouvée"),
        "{out}"
    );
    assert_eq!(
        s.effects.get(&id).await.unwrap().unwrap().state.as_str(),
        "completed"
    );
}

#[tokio::test]
async fn a_stale_report_opens_nothing_and_its_old_button_is_void() {
    let (b, run) = at_the_gate(ForgeKind::GitHub).await;
    b.land_dev_pr(&run).await;
    let old_visit = b.visit(&b.run(&run).await);

    // Le propriétaire clique deux heures plus tard : le bilan ne tient plus.
    b.e.clock.advance_secs(7_200);
    b.answer(&run, APPROVE).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Running);
    assert!(
        b.prod_prs().is_empty(),
        "aucune PR prod sur un bilan périmé"
    );
    let r = b.run(&run).await;
    let stale = r.step_outputs[PROD_STEP]["stale"].as_str().unwrap();
    assert!(stale.contains("bilan vieux de 120 min"), "{stale}");
    let texts = b.e.r.texts().join("\n");
    assert!(texts.contains("Bilan périmé"), "{texts}");
    // PR, CI et E2E refaits, une nouvelle carte posée ; l'ancienne ne vaut plus rien.
    assert_eq!(b.at(&run).await, GATE_STEP);
    assert_eq!(b.gate_cards(), 3);
    assert!(
        answer(&b.e.d, &run, &old_visit, APPROVE, None)
            .await
            .is_err()
    );
    assert_eq!(
        b.forge.requests("POST").len(),
        1,
        "la PR dev n'est pas rouverte"
    );

    // La PR dev avance d'un commit après le bilan : périmé aussi.
    b.forge.head(1, "0000000011111111");
    b.answer(&run, APPROVE).await;
    drive(&b.e.d, &run).await.unwrap();
    let stale = b.run(&run).await.step_outputs[PROD_STEP]["stale"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(stale.contains("`00000000`"), "{stale}");

    // La CI relancée tourne rouge : périmé encore, et la re-vérification s'arrête sur la
    // carte de la CI rouge.
    b.forge.head(1, &b.sha());
    b.forge.ci("red");
    b.answer(&run, APPROVE).await;
    drive(&b.e.d, &run).await.unwrap();
    assert!(b.prod_prs().is_empty());
    assert_eq!(b.at(&run).await, "livraison-ci-bloquee");

    // Tout redevient vert : la carte neuve, approuvée, propose enfin la PR.
    b.forge.ci("green");
    b.answer(&run, RETRY).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, GATE_STEP);
    b.answer(&run, APPROVE).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Done);
    assert_eq!(b.prod_prs().len(), 1);
}

#[tokio::test]
async fn a_refusal_is_final_and_durable() {
    let (b, run) = at_the_gate(ForgeKind::GitHub).await;
    b.land_dev_pr(&run).await;
    b.answer(&run, REFUSE).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Blocked);
    let reason = b.run(&run).await.error.unwrap_or_default();
    assert!(
        reason.ends_with("PR prod refusée par le propriétaire"),
        "{reason}"
    );

    let d2 = restarted(&b.e);
    b.e.d.services.effects.recover_on_boot().await.unwrap();
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Blocked);
    // Reprendre le run refusé ne rouvre pas le gate : il se termine sans rien proposer.
    control(&d2, &run, &penelope_workflow::Control::Resume)
        .await
        .unwrap();
    assert_eq!(drive(&d2, &run).await.unwrap(), RunState::Done);
    assert!(b.prod_prs().is_empty(), "un refus ne propose rien");
}

#[tokio::test]
async fn a_new_revision_of_the_plan_voids_the_approval() {
    let (b, run) = at_the_gate(ForgeKind::GitHub).await;
    b.land_dev_pr(&run).await;
    // Le run exécute la révision approuvée du plan de sa conversation.
    let s = &b.e.d.services;
    let steps = vec![
        PlanStep::new(Phase::Implementation, "Coder"),
        PlanStep::new(Phase::Review, "Relire"),
    ];
    let mut approved = PlanDraft {
        workflow_id: "build-verify".into(),
        params: json!({}),
        brief: None,
        plan: Plan::new("Rendre /stop définitif", steps.clone()).unwrap(),
    };
    approved.approve(1).unwrap();
    let plans = PlanStore::new(s.store.clone());
    plans.save("conversation", &approved).await.unwrap();
    s.kv_set(
        &format!("wf.plan.link.{run}"),
        &json!({"session": "conversation", "version": 1,
                "fingerprint": approved.fingerprint(), "workflow": "build-verify"})
        .to_string(),
    )
    .await
    .unwrap();
    // Re-vérifier pose un bilan qui nomme ce plan…
    b.answer(&run, RECHECK).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, GATE_STEP);
    assert!(b.last_question().contains("plan v1 (empreinte"));
    // … puis la conversation passe à une nouvelle révision avant le clic.
    let next = PlanDraft {
        plan: Plan::new("Rendre /stop définitif, autrement", steps).unwrap(),
        ..approved.clone()
    };
    plans
        .start_next("conversation", &approved, &next)
        .await
        .unwrap();
    b.answer(&run, APPROVE).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Blocked);
    assert!(
        b.prod_prs().is_empty(),
        "la révision approuvée n'est plus la courante"
    );
    let reason = b.run(&run).await.error.unwrap_or_default();
    assert!(reason.contains("plan révisé"), "{reason}");
}

#[tokio::test]
async fn an_unmerged_dev_pr_holds_the_approved_prod_pr_until_retry() {
    let (b, run) = at_the_gate(ForgeKind::GitHub).await;
    b.forge.head(1, &b.sha());
    b.answer(&run, APPROVE).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-prod-bloquee");
    let q = b.last_question();
    assert!(q.contains("n'est pas fusionnée dans `develop`"), "{q}");
    assert!(b.prod_prs().is_empty());

    // Fusionnée après le bilan : le bilan se ré-établit sur le commit de fusion, puis une
    // carte neuve est approuvée.
    b.forge.merge(1, MERGE);
    b.answer(&run, RETRY).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, GATE_STEP);
    assert!(b.prod_prs().is_empty());
    b.answer(&run, APPROVE).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Done);
    assert_eq!(b.prod_prs().len(), 1);
}

#[tokio::test]
async fn the_prod_branch_is_asked_for_before_any_card() {
    let b = bench(ForgeKind::GitHub, None).await;
    configure(&b.dir, &b.forge, "[ci]\nprovider = \"none\"\n", "");
    let path = b.dir.join(".penelope/delivery.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace(&format!("prod = \"{PROD}\"\n"), "")).unwrap();
    b.token();
    b.forge.with(|w| {
        w.dev.insert("/".into(), (200, "ok".into()));
    });
    let run = b.start_with(delivered()).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-bilan-bloquee");
    let q = b.last_question();
    assert!(q.contains("`branches.prod`"), "{q}");
    assert_eq!(
        b.gate_cards(),
        0,
        "pas d'approbation sans branche de production"
    );
}

/// La PR dev fusionnée après le bilan : l'environnement de dev a pu être redéployé depuis le
/// commit de fusion. Le clic ne propose rien ; CI puis E2E sont refaits sur ce commit et une
/// carte neuve est posée. Un E2E rouge après la fusion n'en pose aucune.
#[tokio::test]
async fn a_dev_pr_merged_after_the_report_is_verified_again_on_its_merge_commit() {
    let (b, run) = at_the_gate(ForgeKind::GitHub).await;
    let checks = |sha: &str| {
        b.forge
            .requests(&format!("GET /repos/{REPO}/commits/{sha}/check-runs"))
            .len()
    };
    assert_eq!(checks(MERGE), 0);
    b.forge.head(1, &b.sha());
    b.forge.merge(1, MERGE);
    b.answer(&run, APPROVE).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Running);
    assert!(b.prod_prs().is_empty(), "aucune PR prod entre les deux");
    let r = b.run(&run).await;
    let stale = r.step_outputs[PROD_STEP]["stale"].as_str().unwrap();
    assert!(stale.contains("fusionnée dans `develop`"), "{stale}");
    assert_eq!(r.step_outputs["livraison-ci"]["ci"]["sha"], MERGE);
    assert_eq!(r.step_outputs["livraison-e2e"]["e2e"]["sha"], MERGE);
    assert_eq!(checks(MERGE), 1, "la CI lue sur le commit de fusion");
    assert_eq!(b.at(&run).await, GATE_STEP);
    assert_eq!(b.gate_cards(), 2);
    assert!(b.last_question().contains("commit de fusion `feedface`"));
    assert_eq!(
        b.forge.requests("POST").len(),
        1,
        "la PR dev n'est pas rouverte"
    );

    b.answer(&run, APPROVE).await;
    assert_eq!(drive(&b.e.d, &run).await.unwrap(), RunState::Done);
    assert_eq!(b.prod_prs().len(), 1);
}

#[tokio::test]
async fn a_red_e2e_after_the_merge_proposes_nothing() {
    let (b, run) = at_the_gate(ForgeKind::GitLab).await;
    b.forge.head(1, &b.sha());
    b.forge.merge(1, MERGE);
    b.forge.with(|w| {
        w.dev.insert("/".into(), (502, "redéploiement raté".into()));
    });
    b.answer(&run, APPROVE).await;
    drive(&b.e.d, &run).await.unwrap();
    assert_eq!(b.at(&run).await, "livraison-e2e-bloquee");
    let r = b.run(&run).await;
    assert_eq!(r.step_outputs["livraison-e2e"]["e2e"]["sha"], MERGE);
    assert!(
        b.prod_prs().is_empty(),
        "E2E rouge après fusion : aucune MR prod"
    );
    assert_eq!(b.gate_cards(), 1);
}
