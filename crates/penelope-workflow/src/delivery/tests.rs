use super::config::*;
use super::verdicts::*;
use super::*;
use crate::conditions::{EvalContext, choose};
use crate::model::{DONE, StepResult};
use serde_json::json;

fn cfg(raw: &str) -> FileConfig {
    FileConfig::parse(raw).unwrap()
}

fn found(remote: &str) -> Discovered {
    Discovered {
        remote_url: Some(remote.into()),
        ..Discovered::default()
    }
}

fn keys(missing: &[Missing]) -> Vec<&str> {
    missing.iter().map(|m| m.key.as_str()).collect()
}

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

#[test]
fn the_tail_chains_pr_ci_and_e2e_each_with_its_blocking_card() {
    let steps = tail(DONE);
    let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "livraison-pr",
            "livraison-pr-bloquee",
            "livraison-ci",
            "livraison-ci-bloquee",
            "livraison-e2e",
            "livraison-e2e-bloquee"
        ]
    );
    assert_eq!(next(&steps[0], "passed"), "livraison-ci");
    assert_eq!(next(&steps[2], "passed"), "livraison-e2e");
    assert_eq!(next(&steps[4], "passed"), DONE);
    for (work, card) in [(0, 1), (2, 3), (4, 5)] {
        for result in ["blocked", "failed", "error"] {
            assert_eq!(next(&steps[work], result), steps[card].id, "{result}");
        }
        assert_eq!(next(&steps[card], RETRY), steps[work].id);
        assert_eq!(next(&steps[card], STOP), crate::model::BLOCKED);
        assert_eq!(steps[card].choices, [RETRY, STOP]);
    }
    assert!(
        steps
            .iter()
            .filter(|s| s.kind == "delivery")
            .map(|s| s.delivery.as_str())
            .eq(ACTIONS[..3].iter().copied())
    );
}

#[test]
fn the_forge_comes_from_the_remote_of_a_public_host_and_the_dev_branch_is_never_guessed() {
    let gh = resolve_forge(
        &cfg("[branches]\ndev = \"develop\"\n"),
        &found("https://github.com/Equipe/Service.git"),
    )
    .unwrap();
    assert_eq!(gh.kind, ForgeKind::GitHub);
    assert_eq!(gh.api, "https://api.github.com");
    assert_eq!(gh.repo, "Equipe/Service");
    assert_eq!(gh.remote, "origin");
    assert_eq!(gh.token_secret, "github_token");
    assert_eq!(gh.dev_branch, "develop");

    let gl = resolve_forge(
        &cfg("[branches]\ndev = \"dev\"\n"),
        &found("git@gitlab.com:groupe/sous/depot.git"),
    )
    .unwrap();
    assert_eq!(gl.kind, ForgeKind::GitLab);
    assert_eq!(gl.api, "https://gitlab.com/api/v4");
    assert_eq!(gl.repo, "groupe/sous/depot");
    assert_eq!(gl.token_secret, "gitlab_token");

    // Ni `main` ni `develop` par défaut : la branche de dev manque, et c'est dit.
    let missing =
        resolve_forge(&FileConfig::default(), &found("https://github.com/a/b")).unwrap_err();
    assert_eq!(keys(&missing), ["branches.dev"]);
}

#[test]
fn a_private_host_declares_its_forge_and_gets_its_api_under_its_own_name() {
    let missing = resolve_forge(
        &cfg("[branches]\ndev = \"develop\"\n"),
        &found("ssh://git@git.exemple.fr:2222/equipe/depot.git"),
    )
    .unwrap_err();
    assert_eq!(keys(&missing), ["forge.kind"], "aucun forgeur supposé");

    let gl = resolve_forge(
        &cfg("[forge]\nkind = \"gitlab\"\n[branches]\ndev = \"develop\"\n"),
        &found("ssh://git@git.exemple.fr:2222/equipe/depot.git"),
    )
    .unwrap();
    assert_eq!(gl.api, "https://git.exemple.fr/api/v4");
    assert_eq!(gl.repo, "equipe/depot");

    let ghe = resolve_forge(
        &cfg("[forge]\nkind = \"github\"\ntoken_secret = \"ghe\"\n[branches]\ndev = \"d\"\n"),
        &found("https://ghe.exemple.fr/equipe/depot"),
    )
    .unwrap();
    assert_eq!(ghe.api, "https://ghe.exemple.fr/api/v3");
    assert_eq!(ghe.token_secret, "ghe");

    let unknown = resolve_forge(
        &cfg("[forge]\nkind = \"bitbucket\"\n[branches]\ndev = \"d\"\n"),
        &found("https://github.com/a/b"),
    )
    .unwrap_err();
    assert_eq!(keys(&unknown), ["forge.kind"]);
}

#[test]
fn a_local_remote_needs_the_forge_api_and_repo_declared() {
    let missing = resolve_forge(
        &cfg("[forge]\nkind = \"github\"\n[branches]\ndev = \"develop\"\n"),
        &found("/tmp/depot.git"),
    )
    .unwrap_err();
    assert_eq!(keys(&missing), ["forge.api", "forge.repo"]);

    let ok = resolve_forge(
        &cfg(
            "[forge]\nkind = \"github\"\napi = \"http://127.0.0.1:9/\"\nrepo = \"a/b\"\n\
             [branches]\ndev = \"develop\"\n",
        ),
        &found("file:///tmp/depot.git"),
    )
    .unwrap();
    assert_eq!(ok.api, "http://127.0.0.1:9", "barre finale retirée");

    let no_remote =
        resolve_forge(&cfg("[branches]\ndev = \"d\"\n"), &Discovered::default()).unwrap_err();
    assert_eq!(keys(&no_remote), ["forge.remote"]);
}

#[test]
fn remotes_are_read_in_their_three_forms() {
    for (url, host, path) in [
        ("https://github.com/a/b.git", "github.com", "a/b"),
        ("https://jeton@GitLab.com/g/s/d/", "gitlab.com", "g/s/d"),
        ("git@github.com:a/b.git", "github.com", "a/b"),
        ("ssh://git@h.fr:22/a/b", "h.fr", "a/b"),
    ] {
        assert_eq!(
            remote_host_path(url),
            Some((host.to_string(), path.to_string())),
            "{url}"
        );
    }
    for url in [
        "/srv/depot.git",
        "file:///srv/depot.git",
        "depot",
        "https://h.fr/",
    ] {
        assert_eq!(remote_host_path(url), None, "{url}");
    }
}

#[test]
fn the_ci_is_found_in_the_repository_or_declared_never_assumed() {
    let github = Discovered {
        has_github_workflows: true,
        ..Discovered::default()
    };
    let plan = resolve_ci(&FileConfig::default(), &github, ForgeKind::GitHub).unwrap();
    assert_eq!(plan.provider, Some(ForgeKind::GitHub));
    assert_eq!(plan.timeout_ms, 60 * 60_000);

    let missing = resolve_ci(
        &FileConfig::default(),
        &Discovered::default(),
        ForgeKind::GitLab,
    )
    .unwrap_err();
    assert_eq!(keys(&missing), ["ci.provider"]);
    assert!(missing[0].why.contains("none"), "{missing:?}");

    // Des workflows GitHub dans un dépôt GitLab ne sont pas sa CI.
    assert!(resolve_ci(&FileConfig::default(), &github, ForgeKind::GitLab).is_err());

    let none = resolve_ci(
        &cfg("[ci]\nprovider = \"none\"\n"),
        &Discovered::default(),
        ForgeKind::GitLab,
    )
    .unwrap();
    assert_eq!(none.provider, None);

    let declared = resolve_ci(
        &cfg("[ci]\nprovider = \"gitlab\"\ntimeout_minutes = 5\n"),
        &Discovered::default(),
        ForgeKind::GitLab,
    )
    .unwrap();
    assert_eq!(declared.provider, Some(ForgeKind::GitLab));
    assert_eq!(declared.timeout_ms, 5 * 60_000);

    let foreign = resolve_ci(
        &cfg("[ci]\nprovider = \"github\"\n"),
        &Discovered::default(),
        ForgeKind::GitLab,
    )
    .unwrap_err();
    assert_eq!(keys(&foreign), ["ci.provider"]);
}

#[test]
fn the_dev_url_is_required_and_checks_default_to_a_get_that_must_answer_2xx() {
    let missing = resolve_e2e(&FileConfig::default()).unwrap_err();
    assert_eq!(keys(&missing), ["e2e.url"]);
    let not_http = resolve_e2e(&cfg("[e2e]\nurl = \"dev.exemple.fr\"\n")).unwrap_err();
    assert_eq!(keys(&not_http), ["e2e.url"]);

    let plan = resolve_e2e(&cfg("[e2e]\nurl = \"https://dev.exemple.fr/\"\n")).unwrap();
    assert_eq!(plan.url, "https://dev.exemple.fr");
    assert_eq!(plan.checks, [Check::default()]);
    assert_eq!(plan.checks[0].label(), "GET /");

    let incomplete = resolve_e2e(&cfg(
        "[e2e]\nurl = \"https://d.fr\"\n[[e2e.checks]]\nkind = \"graphql\"\n\
         [[e2e.checks]]\nkind = \"command\"\n",
    ))
    .unwrap_err();
    assert_eq!(
        keys(&incomplete),
        ["e2e.checks[0].query", "e2e.checks[1].command"]
    );
}

#[test]
fn an_unknown_key_is_refused_rather_than_ignored() {
    let e = FileConfig::parse("[branches]\ndevelop = \"x\"\n").unwrap_err();
    assert!(e.contains(".penelope/delivery.toml"), "{e}");
}

#[test]
fn the_owner_is_asked_for_exactly_what_is_missing_and_where() {
    let text = ask(
        "ouvrir la PR dev",
        "/srv/depot",
        &[Missing::new("branches.dev", "la branche de développement")],
    );
    assert!(
        text.starts_with("Pour ouvrir la PR dev, il me manque :"),
        "{text}"
    );
    assert!(text.contains("- `branches.dev` : la branche de développement"));
    assert!(text.contains("/srv/depot/.penelope/delivery.toml"));
    assert!(text.contains("Réessayer"));
}

#[test]
fn github_ci_waits_then_turns_green_or_red() {
    let none = github_ci(&json!({"check_runs": []}), &json!({"statuses": []}));
    assert!(matches!(none, CiVerdict::Pending(_)), "{none:?}");

    let running = github_ci(
        &json!({"check_runs": [
            {"name": "tests", "status": "in_progress", "conclusion": null},
            {"name": "lint", "status": "completed", "conclusion": "success"}
        ]}),
        &json!({"state": "pending", "statuses": []}),
    );
    assert_eq!(
        running,
        CiVerdict::Pending("1 contrôle(s) sur 2 en cours".into())
    );

    let green = github_ci(
        &json!({"check_runs": [{"name": "tests", "status": "completed", "conclusion": "success"},
                               {"name": "doc", "status": "completed", "conclusion": "skipped"}]}),
        &json!({"statuses": [{"context": "ci/legacy", "state": "success"}]}),
    );
    assert_eq!(green, CiVerdict::Green("3 contrôle(s) au vert".into()));

    let red = github_ci(
        &json!({"check_runs": [{"name": "tests", "status": "completed", "conclusion": "failure"},
                               {"name": "lint", "status": "in_progress"}]}),
        &json!({"statuses": [{"context": "ci/legacy", "state": "error"}]}),
    );
    assert_eq!(
        red,
        CiVerdict::Red("en échec : tests (failure), ci/legacy (error)".into()),
        "un contrôle rouge suffit, même si d'autres tournent"
    );
}

#[test]
fn gitlab_ci_reads_the_latest_pipeline_of_the_commit() {
    assert!(matches!(gitlab_ci(&json!([])), CiVerdict::Pending(_)));
    assert_eq!(
        gitlab_ci(&json!([{"id": 7, "status": "failed"}, {"id": 9, "status": "running"}])),
        CiVerdict::Pending("pipeline 9 : running".into())
    );
    assert_eq!(
        gitlab_ci(&json!([{"id": 9, "status": "success"}])),
        CiVerdict::Green("pipeline 9 au vert".into())
    );
    for s in ["failed", "canceled", "skipped"] {
        assert!(
            matches!(
                gitlab_ci(&json!([{"id": 3, "status": s}])),
                CiVerdict::Red(_)
            ),
            "{s}"
        );
    }
    assert!(matches!(
        gitlab_ci(&json!([{"id": 3, "status": "manual"}])),
        CiVerdict::Pending(_)
    ));
}

#[test]
fn e2e_responses_are_judged_on_status_content_and_graphql_errors() {
    let seen = |status, body: &str| Observed {
        status,
        body: body.into(),
    };
    let get = Check::default();
    assert!(judge(&get, &seen(204, "")).is_ok());
    assert_eq!(
        judge(&get, &seen(502, "Bad Gateway")).unwrap_err(),
        "statut 502 (attendu 2xx)"
    );
    let health = Check {
        path: "/health".into(),
        status: Some(200),
        contains: Some("\"ok\"".into()),
        ..Check::default()
    };
    assert!(judge(&health, &seen(200, "{\"status\":\"ok\"}")).is_ok());
    assert!(judge(&health, &seen(200, "{\"status\":\"ko\"}")).is_err());
    assert!(judge(&health, &seen(201, "\"ok\"")).is_err());

    let gql = Check {
        kind: CheckKind::Graphql,
        query: Some("{ version }".into()),
        ..Check::default()
    };
    assert!(judge(&gql, &seen(200, r#"{"data":{"version":"1.2"}}"#)).is_ok());
    assert_eq!(
        judge(
            &gql,
            &seen(200, r#"{"data":null,"errors":[{"message":"boum"}]}"#)
        )
        .unwrap_err(),
        "1 erreur(s) GraphQL : boum"
    );
    assert!(judge(&gql, &seen(200, "<html>")).is_err());
    assert!(judge(&gql, &seen(200, r#"{"data":null}"#)).is_err());
    assert_eq!(gql.label(), "GraphQL /graphql");
}

#[test]
fn ci_polls_back_off_up_to_five_minutes() {
    assert_eq!(ci_backoff_ms(0), 15_000);
    assert_eq!(ci_backoff_ms(1), 30_000);
    assert_eq!(ci_backoff_ms(4), 240_000);
    assert_eq!(ci_backoff_ms(5), 300_000);
    assert_eq!(ci_backoff_ms(40), 300_000);
}
