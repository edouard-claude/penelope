use super::*;
use crate::conditions::{EvalContext, choose};
use crate::model::StepResult;
use crate::plan::{Plan, plan_run_id};
use crate::validate::{Known, validate};

fn draft(steps: Vec<PlanStep>) -> PlanDraft {
    let mut plan = Plan::new("Corriger la commande /stop", steps).unwrap();
    plan.approve().unwrap();
    PlanDraft {
        workflow_id: "build-verify".into(),
        params: json!({"objectif": "stop"}),
        brief: Some("Le propriétaire veut que /stop arrête aussi le run.".into()),
        plan,
    }
}

fn full() -> Vec<PlanStep> {
    vec![
        PlanStep::new(Phase::Specification, "Définir le contrat"),
        PlanStep::new(Phase::Tests, "Écrire les tests"),
        PlanStep::new(Phase::Implementation, "Coder"),
        PlanStep::new(Phase::Review, "Relire"),
        PlanStep::new(Phase::Verification, "Vérifier"),
    ]
}

fn known() -> Known {
    Known {
        model_aliases: ["main", "reasoning", "code"]
            .into_iter()
            .map(String::from)
            .collect(),
        templates: ["question".to_string()].into_iter().collect(),
        max_depth: 3,
        ..Known::default()
    }
}

/// Suit le graphe comme le pilote : chaque étape rend le résultat suivant de la liste.
fn walk(w: &Workflow, results: &[&str]) -> Vec<String> {
    let mut at = w.entry_step.clone();
    let mut path = vec![at.clone()];
    for r in results {
        let step = w
            .step(&at)
            .unwrap_or_else(|| panic!("étape `{at}` absente"));
        at = choose(
            &step.transitions,
            &EvalContext {
                step_result: &StepResult::parse(r),
                step_output: &json!({}),
                metadata: &json!({}),
            },
        );
        path.push(at.clone());
        if at == DONE || at == BLOCKED {
            break;
        }
    }
    path
}

#[test]
fn nothing_compiles_before_the_go_gate() {
    let mut d = draft(full());
    d.plan = Plan::new("Corriger", full()).unwrap();
    assert_eq!(compile(&d, &Limits::default()), Err(PlanError::NotApproved));
}

#[test]
fn a_phased_plan_is_a_valid_workflow_of_fresh_agents_with_their_models() {
    let w = compile(&draft(full()), &Limits::default()).unwrap();
    let report = validate(&w, None, &known());
    assert!(report.is_valid(), "{:#?}", report.issues);
    assert_eq!(w.entry_step, "e1-spec");
    assert!(w.metadata.id.starts_with("plan-"));
    for step in w.steps.iter().filter(|s| s.kind == "agent") {
        assert_eq!(step.context, "fresh", "{}", step.id);
        assert!(step.prompt.contains("Corriger la commande /stop"));
        assert!(step.prompt.contains("{{brief}}"), "{}", step.id);
    }
    let model = |id: &str| w.step(id).unwrap().model.clone();
    assert_eq!(model("e1-spec"), "reasoning");
    assert_eq!(model("e2-tests"), "code");
    assert_eq!(model("e3-code"), "code");
    assert_eq!(model("e4-revue"), "reasoning");
    assert_eq!(model("e5-verif"), "main");
    assert!(
        w.step("e4-revue")
            .unwrap()
            .prompt
            .contains("tu n'es pas l'auteur")
    );
    assert!(w.settings.max_iterations as usize > w.steps.len());
}

#[test]
fn every_ordinary_phase_waits_for_the_owner_ok() {
    let w = compile(&draft(full()), &Limits::default()).unwrap();
    let path = walk(
        &w,
        &[
            "completed",
            CONTINUE,
            "completed",
            CONTINUE,
            "completed",
            CONTINUE,
            PASSED,
            PASSED,
        ],
    );
    assert_eq!(
        path,
        [
            "e1-spec",
            "ok-e1",
            "e2-tests",
            "ok-e2",
            "e3-code",
            "ok-e3",
            "e4-revue",
            "e5-verif",
            delivery::ENTRY
        ]
    );
    let ok = w.step("ok-e1").unwrap();
    assert_eq!(ok.kind, "user");
    assert_eq!(ok.choices, [CONTINUE, LET_RUN, STOP]);
}

#[test]
fn letting_it_run_skips_ordinary_checkpoints_but_not_the_review_stop() {
    let w = compile(&draft(full()), &Limits::default()).unwrap();
    let path = walk(
        &w,
        &[
            "completed",
            LET_RUN,
            "completed",
            "completed",
            PASSED,
            PASSED,
        ],
    );
    assert_eq!(
        path,
        [
            "e1-spec",
            "ok-e1",
            "e2-tests-libre",
            "e3-code-libre",
            "e4-revue-libre",
            "e5-verif-libre",
            delivery::ENTRY
        ]
    );
    // En roue libre, la limite de reprises arrête toujours le run.
    let path = walk(
        &w,
        &[
            "completed",
            LET_RUN,
            "completed",
            "completed",
            FAILED,
            "completed",
            FAILED,
            "completed",
            FAILED,
        ],
    );
    assert_eq!(path.last().map(String::as_str), Some(BLOCKED), "{path:?}");
    assert!(!path.iter().skip(2).any(|id| id.starts_with("ok-")));
}

#[test]
fn a_refusal_or_an_error_stops_the_run() {
    let w = compile(&draft(full()), &Limits::default()).unwrap();
    assert_eq!(walk(&w, &["completed", STOP]).last().unwrap(), BLOCKED);
    assert_eq!(
        w.step("ok-e1").unwrap().transitions[2].tag,
        "arrêt demandé par le propriétaire"
    );
    assert_eq!(walk(&w, &["error"]).last().unwrap(), BLOCKED);
}

#[test]
fn a_failed_review_goes_back_to_code_within_its_bound() {
    let w = compile(&draft(full()), &Limits::default()).unwrap();
    let path = walk(
        &w,
        &[
            "completed",
            CONTINUE,
            "completed",
            CONTINUE, // spec, tests
            "completed",
            CONTINUE,
            FAILED, // code, revue refuse
            "completed",
            CONTINUE,
            PASSED,
            PASSED, // reprise acceptée
        ],
    );
    assert_eq!(
        &path[6..],
        [
            "e4-revue",
            "e3-code-r1",
            "ok-e3-r1",
            "e4-revue-r1",
            "e5-verif-r1",
            delivery::ENTRY
        ]
    );
    let refusals = [
        "completed",
        CONTINUE,
        "completed",
        CONTINUE,
        "completed",
        CONTINUE,
        FAILED,
        "completed",
        CONTINUE,
        FAILED,
        "completed",
        CONTINUE,
        FAILED,
    ];
    let path = walk(&w, &refusals);
    assert_eq!(path.last().unwrap(), BLOCKED, "{path:?}");
    assert_eq!(path[path.len() - 2], "e4-revue-r2");
    let last = w.step("e4-revue-r2").unwrap();
    assert_eq!(last.transitions[1].tag, "limite de 2 reprises atteinte");
}

#[test]
fn a_failed_verification_goes_back_to_the_code_block() {
    let steps = vec![
        PlanStep::new(Phase::Implementation, "Coder la base"),
        PlanStep::new(Phase::Implementation, "Coder la commande"),
        PlanStep::new(Phase::Verification, "Vérifier"),
    ];
    let w = compile(&draft(steps), &Limits::default()).unwrap();
    assert_eq!(
        walk(&w, &["completed", "completed", CONTINUE, FAILED]).last(),
        Some(&"e1-code-r1".to_string())
    );
    // Un juge sans travail avant lui ne peut rien renvoyer : il arrête.
    let steps = vec![
        PlanStep::new(Phase::Review, "Relire l'existant"),
        PlanStep::new(Phase::Implementation, "Coder"),
    ];
    let w = compile(&draft(steps), &Limits::default()).unwrap();
    assert_eq!(walk(&w, &[FAILED]).last().unwrap(), BLOCKED);
}

#[test]
fn a_small_request_stays_with_a_single_agent() {
    let steps = vec![
        PlanStep::new(Phase::Implementation, "Corriger la coquille"),
        PlanStep::new(Phase::Tests, "Ajouter le test"),
    ];
    assert_eq!(Mode::of(&steps), Mode::Single);
    let w = compile(&draft(steps), &Limits::default()).unwrap();
    assert_eq!(w.steps.len(), 1);
    let only = &w.steps[0];
    assert_eq!(only.context, "fresh");
    assert!(only.prompt.contains("réalise tout le plan"));
    assert!(validate(&w, None, &known()).is_valid());
    // Une revue demandée suffit à passer en phases, même sur deux pas.
    let judged = [
        PlanStep::new(Phase::Implementation, "Coder"),
        PlanStep::new(Phase::Review, "Relire"),
    ];
    assert_eq!(Mode::of(&judged), Mode::Phased);
}

#[test]
fn the_fingerprint_names_the_exact_revision() {
    let a = draft(full());
    let mut b = a.clone();
    assert_eq!(a.fingerprint(), b.fingerprint());
    b.params = json!({"objectif": "autre"});
    assert_ne!(
        a.fingerprint(),
        b.fingerprint(),
        "même version, autre contenu"
    );
    let mut c = a.clone();
    c.plan = Plan::new("Corriger la commande /stop", full()).unwrap();
    c.plan.revise("Corriger la commande /stop", full()).unwrap();
    assert_ne!(
        a.fingerprint(),
        c.fingerprint(),
        "même contenu, autre version"
    );
    let id = plan_run_id("s1", &a.fingerprint());
    assert_eq!(id, plan_run_id("s1", &a.fingerprint()));
    assert_ne!(id, plan_run_id("s2", &a.fingerprint()));
    assert!(id.starts_with("r_plan_"));
    assert_eq!(
        compile(&a, &Limits::default()).unwrap().metadata.id,
        workflow_id(&a.fingerprint())
    );
}

#[test]
fn a_judged_plan_that_writes_code_is_delivered_after_its_last_verdict() {
    let w = compile(&draft(full()), &Limits::default()).unwrap();
    let mut results = vec![
        "completed",
        LET_RUN,
        "completed",
        "completed",
        PASSED,
        PASSED,
    ];
    results.extend([delivery::PASSED; 3]);
    let path = walk(&w, &results);
    assert_eq!(
        &path[6..],
        ["livraison-pr", "livraison-ci", "livraison-e2e", DONE],
        "« laisse filer » ne saute aucune étape de livraison"
    );
    // Une livraison bloquée pose sa carte ; « Réessayer » rejoue l'étape, « Arrêter »
    // bloque le run.
    let mut blocked = results[..6].to_vec();
    blocked.extend([
        delivery::PASSED,
        "failed",
        delivery::RETRY,
        "blocked",
        delivery::STOP,
    ]);
    assert_eq!(
        &walk(&w, &blocked)[7..],
        [
            "livraison-ci",
            "livraison-ci-bloquee",
            "livraison-ci",
            "livraison-ci-bloquee",
            BLOCKED
        ]
    );
    assert!(
        !w.steps
            .iter()
            .any(|s| s.id.starts_with("livraison") && s.id.contains("libre")),
        "une seule livraison, quel que soit le chemin"
    );
}

#[test]
fn only_a_plan_that_writes_code_and_ends_with_a_judge_opens_a_pr() {
    assert!(delivers(&full()));
    let without_code = vec![
        PlanStep::new(Phase::Specification, "Rédiger"),
        PlanStep::new(Phase::Review, "Relire"),
    ];
    let unjudged = vec![
        PlanStep::new(Phase::Implementation, "Coder"),
        PlanStep::new(Phase::Review, "Relire"),
        PlanStep::new(Phase::Implementation, "Finir"),
    ];
    let single = vec![PlanStep::new(Phase::Implementation, "Coder")];
    for steps in [without_code, unjudged, single] {
        assert!(!delivers(&steps), "{steps:?}");
        let w = compile(&draft(steps.clone()), &Limits::default()).unwrap();
        assert!(
            !w.steps.iter().any(|s| s.kind == "delivery"),
            "{steps:?} n'est pas livré"
        );
        let report = validate(&w, None, &known());
        assert!(report.is_valid(), "{:#?}", report.issues);
    }
}
