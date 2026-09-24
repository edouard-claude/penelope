use super::*;

#[test]
fn model_list_shows_the_routing_in_force() {
    let v = json!({
        "aliases": [{"alias": "main", "model": "openrouter:z-ai/glm-5.3"},
                    {"alias": "fast", "model": "openrouter:deepseek/deepseek-v4-flash"}],
        "routing": {
            "classifier": true,
            "default": {"alias": "main", "model": "openrouter:z-ai/glm-5.3"},
            "low": {"alias": "fast", "model": "openrouter:deepseek/deepseek-v4-flash"},
            "medium": {"alias": "main", "model": "openrouter:z-ai/glm-5.3"},
            "high": {"alias": "reasoning", "model": "openrouter:z-ai/glm-5.2"},
            "classifier_model": "openrouter:deepseek/deepseek-v4-flash",
            "fallback": {"main": ["fast"]}
        },
        "models": [],
        "note": ""
    });
    let out = render_model_list(&v);
    assert!(
        out.contains("simple    → fast (openrouter:deepseek/deepseek-v4-flash)"),
        "{out}"
    );
    assert!(out.contains("models.routing.classifier false"), "{out}");
    assert!(out.contains("main → fast"), "{out}");
}
use clap::CommandFactory;

fn parse(args: &[&str]) -> Cli {
    Cli::parse_from(std::iter::once("penelope").chain(args.iter().copied()))
}

#[test]
fn the_cli_definition_is_coherent() {
    Cli::command().debug_assert();
}

#[test]
fn global_flags_work_anywhere() {
    let c = parse(&["--json", "status"]);
    assert!(c.json);
    let c = parse(&["status", "--json"]);
    assert!(c.json);
    let c = parse(&["--home", "/srv/pen", "status"]);
    assert_eq!(c.home, Some(PathBuf::from("/srv/pen")));
}

#[test]
fn commands_route_to_rpc_methods() {
    for (args, expected) in [
        (vec!["status"], m::STATUS),
        (vec!["doctor"], m::DOCTOR),
        (vec!["approvals"], m::APPROVALS),
        (vec!["policies"], m::POLICIES),
        (vec!["audit-verify"], m::AUDIT_VERIFY),
        (vec!["history", "verify"], m::HISTORY_VERIFY),
        (vec!["history", "reindex"], m::HISTORY_REINDEX),
        (vec!["backup"], m::BACKUP),
        (vec!["session", "list"], m::SESSION_LIST),
        (vec!["session", "budget", "s_01", "20"], m::SESSION_BUDGET),
        (vec!["session", "model", "main"], m::SESSION_MODEL),
        (vec!["session", "compact"], m::SESSION_COMPACT),
        (vec!["session", "mode", "ask"], m::SESSION_MODE),
        (vec!["session", "project", "fidelatoo"], m::SESSION_PROJECT),
        (vec!["session", "fork"], m::SESSION_FORK),
        (vec!["session", "rewind", "2"], m::SESSION_REWIND),
        (vec!["export", "run", "r_1"], m::EXPORT),
        (vec!["store", "rebuild"], m::STORE_REBUILD),
        (vec!["skill", "rollback", "revue"], m::SKILL_ROLLBACK),
        (
            vec!["wf", "run", "build-verify", "--param", "objectif=x"],
            m::WF_RUN,
        ),
        (vec!["schedule", "run", "sch_1"], m::SCHEDULE_RUN_NOW),
        (
            vec![
                "schedule",
                "add",
                "cron",
                "--spec",
                "{\"expr\":\"0 9 * * 1\"}",
                "--target",
                "{\"type\":\"notify\",\"template\":\"revue\"}",
            ],
            m::SCHEDULE_ADD,
        ),
        (
            vec!["session", "title", "s_1", "Refonte", "du", "site"],
            m::SESSION_TITLE,
        ),
        (vec!["session", "close", "s_1"], m::SESSION_CLOSE),
        (vec!["mcp", "list"], m::MCP_LIST),
        (vec!["mcp", "show", "redmine"], m::MCP_SHOW),
        (vec!["mcp", "rm", "redmine"], m::MCP_RM),
        (vec!["mcp", "enable", "redmine"], m::MCP_ENABLE),
        (vec!["mcp", "disable", "redmine"], m::MCP_DISABLE),
        (vec!["mcp", "restart", "redmine"], m::MCP_RESTART),
        (vec!["mcp", "auth", "github"], m::MCP_AUTH),
        (vec!["mcp", "test", "redmine"], m::MCP_TEST),
        (vec!["mcp", "logs", "redmine"], m::MCP_LOGS),
        (
            vec!["mcp", "edit", "redmine", "timeout", "60s"],
            m::MCP_EDIT,
        ),
        (vec!["config", "get"], m::CONFIG_GET),
        (vec!["secret", "list"], m::SECRET_LIST),
        (vec!["model", "list"], m::MODEL_LIST),
        (vec!["model", "auth", "codex"], m::MODEL_AUTH),
        (vec!["wf", "list"], m::WF_LIST),
        (vec!["schedule", "list"], m::SCHEDULE_LIST),
        (vec!["mem", "search", "x"], m::MEM_SEARCH),
        (vec!["mem", "audit"], m::MEM_AUDIT),
        (vec!["mem", "retry-rejected"], m::MEM_RETRY_REJECTED),
        (vec!["mem", "diff", "--since", "dream"], m::MEM_DIFF),
        (vec!["mem", "dream", "--dry-run"], m::MEM_DREAM),
        (vec!["mem", "restore", "12"], m::MEM_RESTORE),
        (vec!["vault", "check"], m::VAULT_CHECK),
        (vec!["vault", "lint"], m::VAULT_LINT),
        (vec!["skill", "list"], m::SKILL_LIST),
    ] {
        let c = parse(&args);
        let (method, _) = route(&c.command).unwrap();
        assert_eq!(method, expected, "{args:?}");
    }
}

#[test]
fn every_routed_method_exists_in_the_contract() {
    for args in [
        vec!["status"],
        vec!["doctor"],
        vec!["restart"],
        vec!["upgrade", "--check"],
        vec!["import", "hermes", "--dry-run"],
        vec!["upgrade", "--rollback"],
        vec!["approvals"],
        vec!["metrics"],
        vec!["approve", "a_1"],
        vec!["approve", "a_1", "--effect", "done"],
        vec!["deny", "a_1"],
        vec!["policies"],
        vec!["usage"],
        vec!["audit-verify"],
        vec!["audit", "show", "--turn", "t_1"],
        vec!["history", "verify", "--session", "s_1"],
        vec!["history", "reindex", "--session", "s_1"],
        vec!["backup"],
        vec!["session", "list"],
        vec!["session", "new"],
        vec!["session", "export", "s_1"],
        vec!["config", "get"],
        vec!["config", "status"],
        vec!["config", "reload"],
        vec!["config", "set", "budget.daily_usd", "50"],
        vec!["secret", "list"],
        vec!["secret", "backend"],
        vec!["secret", "rm", "x"],
        vec!["model", "list"],
        vec!["model", "set", "main", "a/b"],
        vec!["wf", "list"],
        vec!["wf", "show", "demo"],
        vec!["wf", "runs"],
        vec!["wf", "trace", "r_1"],
        vec!["wf", "control", "r_1", "pause"],
        vec!["schedule", "list"],
        vec!["schedule", "pause", "s_1"],
        vec!["mem", "search", "x"],
        vec!["mem", "show", "u1"],
        vec!["skill", "list"],
        vec!["jobs"],
    ] {
        let c = parse(&args);
        let (method, _) = route(&c.command).unwrap();
        assert!(
            penelope_kernel::api::method::ALL.contains(&method),
            "{args:?} → méthode hors contrat : {method}"
        );
    }
}

#[test]
fn scalars_are_typed_from_the_command_line() {
    assert_eq!(parse_scalar("50"), json!(50));
    assert_eq!(parse_scalar("0.7"), json!(0.7));
    assert_eq!(parse_scalar("true"), json!(true));
    assert_eq!(parse_scalar("Indian/Reunion"), json!("Indian/Reunion"));
    assert_eq!(parse_scalar("[\"a\",\"b\"]"), json!(["a", "b"]));
}

/// #99 : sans daemon, `doctor` rend quand même ses contrôles locaux et nomme le
/// daemon absent en tête, en contrôle critique.
#[tokio::test]
async fn doctor_reports_local_checks_without_a_daemon() {
    let dir = tempfile::Builder::new()
        .prefix("pnl")
        .tempdir_in("/tmp")
        .unwrap();
    let home = dir.path().to_string_lossy().to_string();
    let cli = parse(&["--json", "--home", &home, "doctor"]);
    let e = doctor(&cli).await.unwrap_err();
    assert_eq!(
        e.exit_code(),
        penelope_kernel::api::exit_code::VALIDATION_FAILED,
        "{e}"
    );
}

/// #103 : `penelope logs --turn` ne garde que les lignes du tour, par le span ou par
/// le champ de l'événement.
#[test]
fn logs_are_filtered_by_turn_and_session() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("penelope-2026-09-18.jsonl");
    std::fs::write(
        &f,
        concat!(
            r#"{"fields":{"message":"modèle choisi"},"span":{"name":"turn","turn":"t_1","session":"s_1"}}"#,
            "\n",
            r#"{"fields":{"message":"autre tour"},"span":{"name":"turn","turn":"t_2","session":"s_1"}}"#,
            "\n",
            r#"{"fields":{"message":"lease perdu","turn":"t_1"}}"#,
            "\n",
            "pas du json\n",
        ),
    )
    .unwrap();
    let files = vec![f];
    let t1 = filter_log_lines(&files, Some("t_1"), None, 100);
    assert_eq!(t1.len(), 2, "{t1:?}");
    assert!(t1.iter().all(|l| l.contains("t_1")));
    assert_eq!(filter_log_lines(&files, None, Some("s_1"), 100).len(), 2);
    assert_eq!(filter_log_lines(&files, None, None, 1), vec!["pas du json"]);
}

#[test]
fn config_validate_works_without_a_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        penelope_kernel::Config::sample(42).to_toml().unwrap(),
    )
    .unwrap();
    let cli = parse(&["--json", "config", "validate"]);
    validate_config(&cli, Some(path)).unwrap();

    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "[owner]\ntelegram_user_id = 0\n").unwrap();
    let e = validate_config(&cli, Some(bad)).unwrap_err();
    assert_eq!(
        e.exit_code(),
        penelope_kernel::api::exit_code::VALIDATION_FAILED
    );

    // #76 : un fichier écrit par une version plus récente reste valide.
    let futur = dir.path().join("futur.toml");
    std::fs::write(
        &futur,
        "[owner]\ntelegram_user_id = 42\n\n[futur]\nactif = true\n",
    )
    .unwrap();
    validate_config(&cli, Some(futur)).unwrap();
}

#[test]
fn workflow_validate_works_without_a_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("build-verify.workflow.json");
    std::fs::write(&good, penelope_workflow::bundled::build_verify().to_json()).unwrap();
    let cli = parse(&["--json", "wf", "validate", "x"]);
    validate_workflow(&cli, good).unwrap();

    let bad = dir.path().join("casse.workflow.json");
    std::fs::write(&bad, "{ pas du json").unwrap();
    let e = validate_workflow(&cli, bad).unwrap_err();
    assert_eq!(
        e.exit_code(),
        penelope_kernel::api::exit_code::VALIDATION_FAILED
    );
}

/// `secret set` doit vivre hors du RPC : une installation neuve se configure
/// avant le premier démarrage du daemon.
#[test]
fn setting_a_secret_never_goes_through_the_rpc() {
    let c = parse(&["secret", "set", "openrouter_api_key"]);
    assert!(
        route(&c.command).is_err(),
        "`secret set` ne doit pas être routé vers une méthode RPC"
    );
    // Les autres sous-commandes, elles, passent bien par le daemon.
    assert!(route(&parse(&["secret", "list"]).command).is_ok());
    assert!(route(&parse(&["secret", "rm", "x"]).command).is_ok());
}

#[tokio::test]
async fn a_secret_value_on_the_command_line_is_refused_with_guidance() {
    let cli = parse(&["secret", "set", "telegram_bot_token", "123:AAH-secret"]);
    let e = run(cli).await.unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("jamais en argument"), "{msg}");
    assert!(
        msg.contains("penelope secret set telegram_bot_token"),
        "{msg}"
    );
    assert!(
        !msg.contains("123:AAH-secret"),
        "la valeur ne doit pas être réaffichée"
    );
}

#[test]
fn a_secret_name_must_be_a_slug() {
    let cli = parse(&["--home", "/srv/pen", "secret", "set", "pas un nom"]);
    let name = match &cli.command {
        Command::Secret(SecretCmd::Set { name, .. }) => name.clone(),
        other => panic!("{other:?}"),
    };
    let e = set_secret(&cli, name).unwrap_err();
    assert!(e.to_string().contains("nom de secret invalide"), "{e}");
}

/// #124 : `penelope schedule move` vise une conversation ou la conversation privée,
/// jamais rien ; `schedule list` dit où livre chaque planification.
#[test]
fn a_schedule_is_moved_and_listed_with_its_destination() {
    let c = parse(&[
        "schedule", "move", "s1", "--chat", "-100777", "--topic", "12",
    ]);
    let (method, params) = route(&c.command).unwrap();
    assert_eq!(method, m::SCHEDULE_MOVE);
    assert_eq!(params["chat_id"], -100777);
    assert_eq!(params["topic_id"], 12);
    let c = parse(&["schedule", "move", "s1", "--private"]);
    assert_eq!(route(&c.command).unwrap().1["private"], true);
    let c = parse(&["schedule", "move", "s1"]);
    assert!(route(&c.command).is_err(), "une destination est exigée");

    let out = render_schedule_list(&json!([{
        "id": "s1", "state": "active", "kind": "cron", "spec": {"expr": "33 8 * * *"},
        "target": {"type": "prompt", "label": "Veille du matin"},
        "destination": "sujet « Veille », groupe « Équipe »",
        "next_run": "2026-09-20T04:33:00Z",
    }]));
    for want in [
        "vers",
        "sujet « Veille », groupe « Équipe »",
        "Veille du matin",
        "33 8 * * *",
    ] {
        assert!(out.contains(want), "{want} :\n{out}");
    }
}

/// #122 : `penelope mcp list` dit quels serveurs joignent le trousseau.
#[test]
fn mcp_list_says_which_servers_reach_the_keychain() {
    let v = json!({"servers": [
        {"name": "mailbridge", "state": "ready", "transport": "stdio", "keychain": true},
        {"name": "notes", "state": "ready", "transport": "stdio", "keychain": false},
        {"name": "forge", "state": "ready", "transport": "http", "keychain": false},
    ]});
    let out = render_mcp_list(&v);
    assert!(out.contains("trousseau"), "{out}");
    let line = |name: &str| out.lines().find(|l| l.contains(name)).unwrap().to_string();
    assert!(line("mailbridge").contains("ouvert"), "{out}");
    assert!(line("notes").contains("fermé"), "{out}");
    assert!(!line("forge").contains("ouvert") && !line("forge").contains("fermé"));
}

#[test]
fn paths_works_without_a_daemon() {
    let cli = parse(&["--home", "/srv/pen", "--json", "paths"]);
    paths(&cli).unwrap();
}
