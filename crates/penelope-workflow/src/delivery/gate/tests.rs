use super::*;
use crate::conditions::{EvalContext, choose};
use crate::delivery::{RETRY, STOP};
use crate::model::{DONE, StepResult};
use serde_json::json;

fn next(step: &Step, result: &str) -> String {
    choose(
        &step.transitions,
        &EvalContext {
            step_result: &StepResult::parse(result),
            step_output: &json!({}),
            metadata: &json!({}),
        },
    )
}

fn plan(version: u64, fingerprint: &str) -> PlanRef {
    PlanRef {
        version,
        fingerprint: fingerprint.into(),
    }
}

fn bilan() -> Bilan {
    Bilan {
        run: "r_plan_1".into(),
        plan: Some(plan(3, "abcdef0123456789")),
        forge: "gitlab".into(),
        repo: "equipe/service".into(),
        head: "penelope/stop".into(),
        sha: "0123456789abcdef".into(),
        dev_branch: "develop".into(),
        prod_branch: "main".into(),
        dev_pr_number: 7,
        dev_pr_url: "https://forge/equipe/service/-/merge_requests/7".into(),
        ci: "green".into(),
        ci_detail: "pipeline 41 au vert".into(),
        e2e_url: "https://dev.exemple.fr".into(),
        e2e_checks: 2,
        evidence: "livraison/e2e-9.json".into(),
        verified_at_ms: 1_000_000,
    }
}

fn seen() -> Seen {
    Seen {
        now_ms: 1_000_000 + 5 * 60_000,
        active_plan: Some(plan(3, "abcdef0123456789")),
        dev_pr_head: Some("0123456789abcdef".into()),
        ci: Some(CiVerdict::Green("pipeline 41 au vert".into())),
    }
}

const HOUR: u64 = 60 * 60_000;

#[test]
fn the_gate_presents_a_report_then_waits_for_an_explicit_approval() {
    let steps = steps(DONE);
    let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            REPORT_STEP,
            "livraison-bilan-bloquee",
            GATE_STEP,
            PROD_STEP,
            "livraison-prod-bloquee"
        ]
    );
    let [report, report_card, gate, prod, prod_card] = &steps[..] else {
        panic!("cinq étapes attendues");
    };
    assert_eq!(next(report, PASSED), GATE_STEP, "le bilan mène à la carte");
    assert_eq!(next(report, SUPERSEDED), BLOCKED);
    assert_eq!(next(report, "blocked"), report_card.id);
    assert_eq!(next(report_card, RETRY), REPORT_STEP);

    assert_eq!(gate.kind, "user");
    assert_eq!(gate.choices, [APPROVE, RECHECK, REFUSE]);
    assert_eq!(next(gate, APPROVE), PROD_STEP);
    assert_eq!(next(gate, RECHECK), ENTRY, "re-vérifier refait PR, CI, E2E");
    assert_eq!(next(gate, REFUSE), BLOCKED);
    assert_eq!(
        next(gate, "autre"),
        BLOCKED,
        "aucun autre choix ne mène à la PR"
    );

    assert_eq!(next(prod, PASSED), DONE);
    assert_eq!(next(prod, STALE), ENTRY, "un bilan périmé se re-vérifie");
    assert_eq!(next(prod, SUPERSEDED), BLOCKED);
    for result in ["blocked", "failed", "error"] {
        assert_eq!(next(prod, result), prod_card.id, "{result}");
    }
    assert_eq!(next(prod_card, RETRY), PROD_STEP);
    assert_eq!(next(prod_card, STOP), BLOCKED);
    // Seule l'approbation mène à l'étape qui ouvre la PR de production.
    let into_prod: Vec<&str> = steps
        .iter()
        .filter(|s| s.transitions.iter().any(|t| t.goto == PROD_STEP))
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(into_prod, [GATE_STEP, "livraison-prod-bloquee"]);
    assert!(!steps.iter().any(|s| s.id.contains("libre")));
}

#[test]
fn the_prod_branch_is_never_guessed_nor_the_dev_branch_itself() {
    let cfg = |raw: &str| FileConfig::parse(raw).unwrap();
    let keys = |m: Vec<Missing>| m.into_iter().map(|m| m.key).collect::<Vec<_>>();
    assert_eq!(
        keys(resolve_prod(&cfg("[branches]\ndev = \"develop\"\n")).unwrap_err()),
        ["branches.prod"]
    );
    assert_eq!(
        keys(resolve_prod(&FileConfig::default()).unwrap_err()),
        ["branches.dev", "branches.prod"]
    );
    let same = resolve_prod(&cfg("[branches]\ndev = \"main\"\nprod = \"main\"\n")).unwrap_err();
    assert!(same[0].why.contains("aussi la branche de dev"), "{same:?}");

    let target = resolve_prod(&cfg("[branches]\ndev = \"develop\"\nprod = \"main\"\n")).unwrap();
    assert_eq!(target.prod_branch, "main");
    assert_eq!(target.max_age_ms, MAX_AGE_MINUTES * 60_000);
    let custom = resolve_prod(&cfg(
        "[branches]\ndev = \"develop\"\nprod = \"main\"\n[prod]\nmax_age_minutes = 15\n",
    ))
    .unwrap();
    assert_eq!(custom.max_age_ms, 15 * 60_000);
}

#[test]
fn a_fresh_report_is_approvable() {
    assert_eq!(freshness(&bilan(), &seen(), HOUR), Freshness::Fresh);
}

#[test]
fn a_revised_plan_supersedes_the_run() {
    let mut s = seen();
    s.active_plan = Some(plan(4, "ffff000011112222"));
    let Freshness::Superseded(why) = freshness(&bilan(), &s, HOUR) else {
        panic!("plan révisé");
    };
    assert!(why.contains("plan v4") && why.contains("plan v3"), "{why}");
    // Même numéro, autre contenu : l'empreinte tranche.
    s.active_plan = Some(plan(3, "ffff000011112222"));
    assert!(matches!(
        freshness(&bilan(), &s, HOUR),
        Freshness::Superseded(_)
    ));
    s.active_plan = None;
    assert!(matches!(
        freshness(&bilan(), &s, HOUR),
        Freshness::Superseded(_)
    ));
    // Un run hors plan n'a que lui-même à tenir.
    let mut b = bilan();
    b.plan = None;
    assert_eq!(freshness(&b, &s, HOUR), Freshness::Fresh);
}

#[test]
fn an_old_report_a_moved_pr_or_a_ci_no_longer_green_is_stale() {
    let mut s = seen();
    s.now_ms = bilan().verified_at_ms + HOUR + 60_000;
    let Freshness::Stale(why) = freshness(&bilan(), &s, HOUR) else {
        panic!("trop vieux");
    };
    assert!(why.contains("61 min"), "{why}");

    let mut s = seen();
    s.dev_pr_head = Some("99999999aaaa".into());
    let Freshness::Stale(why) = freshness(&bilan(), &s, HOUR) else {
        panic!("autre commit");
    };
    assert!(
        why.contains("`99999999`") && why.contains("`01234567`"),
        "{why}"
    );
    s.dev_pr_head = None;
    assert!(matches!(freshness(&bilan(), &s, HOUR), Freshness::Stale(_)));

    for (ci, word) in [
        (
            Some(CiVerdict::Red("pipeline 42 : failed".into())),
            "plus verte",
        ),
        (
            Some(CiVerdict::Pending("pipeline 42 : running".into())),
            "relancée",
        ),
        (None, "relue"),
    ] {
        let mut s = seen();
        s.ci = ci;
        let Freshness::Stale(why) = freshness(&bilan(), &s, HOUR) else {
            panic!("{word}");
        };
        assert!(why.contains(word), "{why}");
    }
    // Sans CI déclarée, il n'y a rien à relire.
    let mut b = bilan();
    b.ci = "none".into();
    let mut s = seen();
    s.ci = None;
    assert_eq!(freshness(&b, &s, HOUR), Freshness::Fresh);
}

#[test]
fn the_report_names_what_is_approved_and_its_fingerprint_follows_it() {
    let b = bilan();
    let text = b.render(HOUR);
    for part in [
        "plan v3 (empreinte abcdef012345)",
        "`r_plan_1`",
        "merge_requests/7",
        "commit `01234567`",
        "pipeline 41 au vert",
        "2 contrôle(s), preuves `livraison/e2e-9.json`",
        "Valable 60 min",
        "`develop` → `main`",
        "ni fusionnée ni déployée",
        APPROVE,
        REFUSE,
    ] {
        assert!(text.contains(part), "{part} : {text}");
    }
    let mut other = b.clone();
    other.sha = "fedcba9876543210".into();
    assert_ne!(other.fingerprint(), b.fingerprint());
    let mut other = b.clone();
    other.plan = Some(plan(4, "abcdef0123456789"));
    assert_ne!(other.fingerprint(), b.fingerprint());
    let raw = serde_json::to_string(&b).unwrap();
    assert_eq!(serde_json::from_str::<Bilan>(&raw).unwrap(), b);
}
