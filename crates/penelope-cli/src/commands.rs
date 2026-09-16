//! Surface de commandes (§15).
//!
//! Deux familles : celles qui parlent au daemon par RPC, et celles qui fonctionnent
//! **hors daemon** (`wf validate`, `config validate`, `paths`) pour l'édition en SSH et
//! la CI.

use crate::client::{CliError, CliResult, call, socket_path};
use crate::output;
use clap::{Parser, Subcommand};
use penelope_kernel::api::method as m;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "penelope",
    version,
    about = "Pénélope, agent personnel autonome",
    disable_help_subcommand = false
)]
pub struct Cli {
    /// Racine unique des répertoires (équivaut à `PENELOPE_HOME`).
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,

    /// Sortie JSON.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Installe le service système.
    Install,
    /// Désinstalle le service système.
    Uninstall,
    /// Lance le daemon au premier plan.
    Daemon,
    /// Démarre le service.
    Start,
    /// Arrête le service.
    Stop,
    /// Redémarre le daemon.
    Restart,
    /// État du daemon.
    Status,
    /// Diagnostic complet.
    Doctor,
    /// Répertoires effectifs.
    Paths,

    /// Sessions.
    #[command(subcommand)]
    Session(SessionCmd),
    /// Configuration.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Secrets.
    #[command(subcommand)]
    Secret(SecretCmd),
    /// Modèles.
    #[command(subcommand)]
    Model(ModelCmd),
    /// Workflows.
    #[command(subcommand)]
    Wf(WfCmd),
    /// Déclencheurs planifiés.
    #[command(subcommand)]
    Schedule(ScheduleCmd),
    /// Mémoire.
    #[command(subcommand)]
    Mem(MemCmd),
    /// Skills.
    #[command(subcommand)]
    Skill(SkillCmd),

    /// Demandes d'approbation en attente.
    Approvals,
    /// Autorise une demande.
    Approve {
        id: String,
        /// Crée une règle « toujours ».
        #[arg(long)]
        always: bool,
    },
    /// Refuse une demande.
    Deny {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Règles d'autorisation.
    Policies,

    /// Consommation et coûts.
    Usage {
        #[arg(long, default_value = "model")]
        by: String,
    },
    /// Vérifie la chaîne d'audit.
    #[command(name = "audit-verify")]
    AuditVerify,
    /// Sauvegarde cohérente.
    Backup,
    /// Lance une suite d'évaluation.
    Eval { suite: String },
}

#[derive(Subcommand, Debug)]
pub enum SessionCmd {
    List,
    New { title: Option<String> },
    Export { session: String },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    Get,
    Set {
        path: String,
        value: String,
    },
    Status,
    Reload,
    /// Valide un fichier de configuration **sans daemon**.
    Validate {
        file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum SecretCmd {
    List,
    Backend,
    /// Enregistre un secret, la valeur étant lue sur l'entrée standard.
    ///
    /// Elle n'est jamais un argument de la ligne de commande : elle resterait dans
    /// l'historique du shell et serait visible dans `ps`. Fonctionne **sans daemon**,
    /// pour qu'une installation neuve puisse être configurée avant le premier démarrage.
    ///
    /// Sur macOS, le plus propre est de passer par le presse-papiers :
    /// `pbpaste | penelope secret set openrouter_api_key`
    Set {
        name: String,
    },
    Rm {
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ModelCmd {
    List {
        #[arg(long)]
        filter: Option<String>,
    },
    Set {
        alias: String,
        model: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum WfCmd {
    List,
    Show {
        id: String,
    },
    /// Valide un fichier de workflow **sans daemon**.
    Validate {
        file: PathBuf,
    },
    Runs,
    Trace {
        run: String,
    },
    Control {
        run: String,
        op: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScheduleCmd {
    List,
    Pause { id: String },
    Resume { id: String },
    Rm { id: String },
}

#[derive(Subcommand, Debug)]
pub enum MemCmd {
    Search { query: String },
    Show { uid: String },
}

#[derive(Subcommand, Debug)]
pub enum SkillCmd {
    List,
    Show { name: String },
}

/// Exécute la commande.
pub async fn run(cli: Cli) -> CliResult<()> {
    // Les commandes hors daemon d'abord : elles doivent marcher sans socket.
    match &cli.command {
        Command::Paths => return paths(&cli),
        Command::Config(ConfigCmd::Validate { file }) => {
            return validate_config(&cli, file.clone());
        }
        Command::Wf(WfCmd::Validate { file }) => return validate_workflow(&cli, file.clone()),
        Command::Secret(SecretCmd::Set { name }) => return set_secret(&cli, name.clone()),
        Command::Install | Command::Uninstall | Command::Start | Command::Stop => {
            return service(&cli);
        }
        Command::Daemon => return daemon(&cli).await,
        _ => {}
    }

    let socket = socket_path(cli.home.clone())?;
    let (method, params) = route(&cli.command)?;
    let value = call(&socket, method, params).await?;

    match &cli.command {
        Command::Doctor => {
            let checks: Vec<penelope_kernel::api::DoctorCheck> =
                serde_json::from_value(value.clone()).unwrap_or_default();
            if cli.json {
                output::print(&value, true);
            } else {
                print!("{}", penelope_daemon::doctor::render(&checks));
            }
            if checks.iter().any(|c| !c.ok && c.severity == "error") {
                return Err(CliError::Validation(
                    "des contrôles critiques sont en échec".into(),
                ));
            }
        }
        _ => output::print(&value, cli.json),
    }
    Ok(())
}

/// Associe une commande à sa méthode RPC (CA 15 : parité Telegram ↔ CLI).
pub fn route(cmd: &Command) -> CliResult<(&'static str, Value)> {
    Ok(match cmd {
        Command::Status => (m::STATUS, json!({})),
        Command::Doctor => (m::DOCTOR, json!({})),
        Command::Restart => (m::RESTART, json!({})),

        Command::Session(SessionCmd::List) => (m::SESSION_LIST, json!({})),
        Command::Session(SessionCmd::New { title }) => (m::SESSION_NEW, json!({"title": title})),
        Command::Session(SessionCmd::Export { session }) => {
            (m::SESSION_EXPORT, json!({"session": session}))
        }

        Command::Config(ConfigCmd::Get) => (m::CONFIG_GET, json!({})),
        Command::Config(ConfigCmd::Status) => (m::CONFIG_STATUS, json!({})),
        Command::Config(ConfigCmd::Reload) => (m::CONFIG_RELOAD, json!({})),
        Command::Config(ConfigCmd::Set { path, value }) => (
            m::CONFIG_SET,
            json!({"path": path, "value": parse_scalar(value)}),
        ),

        Command::Secret(SecretCmd::List) => (m::SECRET_LIST, json!({})),
        Command::Secret(SecretCmd::Backend) => (m::SECRET_BACKEND, json!({})),
        Command::Secret(SecretCmd::Rm { name }) => (m::SECRET_RM, json!({"name": name})),

        Command::Model(ModelCmd::List { filter }) => (m::MODEL_LIST, json!({"filter": filter})),
        Command::Model(ModelCmd::Set { alias, model }) => {
            (m::MODEL_SET, json!({"alias": alias, "model": model}))
        }

        Command::Wf(WfCmd::List) => (m::WF_LIST, json!({})),
        Command::Wf(WfCmd::Show { id }) => (m::WF_SHOW, json!({"id": id})),
        Command::Wf(WfCmd::Runs) => (m::WF_RUNS, json!({})),
        Command::Wf(WfCmd::Trace { run }) => (m::WF_TRACE, json!({"run": run})),
        Command::Wf(WfCmd::Control { run, op }) => (m::WF_CONTROL, json!({"run": run, "op": op})),

        Command::Schedule(ScheduleCmd::List) => (m::SCHEDULE_LIST, json!({})),
        Command::Schedule(ScheduleCmd::Pause { id }) => (m::SCHEDULE_PAUSE, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Resume { id }) => (m::SCHEDULE_RESUME, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Rm { id }) => (m::SCHEDULE_RM, json!({"id": id})),

        Command::Mem(MemCmd::Search { query }) => (m::MEM_SEARCH, json!({"query": query})),
        Command::Mem(MemCmd::Show { uid }) => (m::MEM_SHOW, json!({"uid": uid})),

        Command::Skill(SkillCmd::List) => (m::SKILL_LIST, json!({})),
        Command::Skill(SkillCmd::Show { name }) => (m::SKILL_SHOW, json!({"name": name})),

        Command::Approvals => (m::APPROVALS, json!({})),
        Command::Approve { id, always } => (m::APPROVE, json!({"id": id, "always": always})),
        Command::Deny { id, reason } => (m::DENY, json!({"id": id, "reason": reason})),
        Command::Policies => (m::POLICIES, json!({})),

        Command::Usage { by } => (m::USAGE, json!({"by": by})),
        Command::AuditVerify => (m::AUDIT_VERIFY, json!({})),
        Command::Backup => (m::BACKUP, json!({})),
        Command::Eval { suite } => (m::EVAL_RUN, json!({"suite": suite})),

        other => {
            return Err(CliError::Usage(format!("commande non routée : {other:?}")));
        }
    })
}

/// `"12"` devient un nombre, `"true"` un booléen, le reste une chaîne.
fn parse_scalar(raw: &str) -> Value {
    if let Ok(b) = raw.parse::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(i) = raw.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return json!(f);
    }
    if (raw.starts_with('{') && raw.ends_with('}')) || (raw.starts_with('[') && raw.ends_with(']'))
    {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            return v;
        }
    }
    Value::String(raw.to_string())
}

fn paths(cli: &Cli) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let v = json!({
        "config": dirs.config(),
        "data": dirs.data(),
        "state": dirs.state(),
        "logs": dirs.logs(),
        "cache": dirs.cache(),
        "db": dirs.db_path(),
        "socket": dirs.socket_path(),
        "vault": dirs.vault(),
        "skills": dirs.skills(),
        "workflows": dirs.workflows(),
        "templates": dirs.templates(),
        "mcp.d": dirs.mcp_d(),
    });
    output::print(&v, cli.json);
    Ok(())
}

/// `penelope secret set <nom>` : la valeur vient de l'entrée standard, jamais d'un
/// argument, et n'est **jamais** réaffichée.
fn set_secret(cli: &Cli, name: String) -> CliResult<()> {
    use std::io::Read;

    penelope_platform::validate_secret_name(&name).map_err(|e| CliError::Usage(e.to_string()))?;

    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .map_err(|e| CliError::Io(format!("lecture de l'entrée standard : {e}")))?;
    // Un copier-coller traîne presque toujours un retour à la ligne ou une espace.
    let value = raw.trim();
    if value.is_empty() {
        return Err(CliError::Usage(format!(
            "valeur vide : passer le secret sur l'entrée standard, par exemple \
             `pbpaste | penelope secret set {name}`"
        )));
    }

    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    dirs.ensure_all().map_err(|e| CliError::Io(e.to_string()))?;
    let store = penelope_platform::backend::secret_store(dirs.as_ref())
        .map_err(|e| CliError::Io(e.to_string()))?;
    store
        .set(&name, value)
        .map_err(|e| CliError::Io(e.to_string()))?;

    output::print(
        &json!({
            "name": name,
            "backend": store.backend(),
            "bytes": value.len(),
            "stored": true,
        }),
        cli.json,
    );
    Ok(())
}

fn validate_config(cli: &Cli, file: Option<PathBuf>) -> CliResult<()> {
    let path = match file {
        Some(p) => p,
        None => penelope_platform::resolve_directories(cli.home.clone())
            .map_err(|e| CliError::Io(e.to_string()))?
            .config_file(),
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| CliError::Io(format!("{} : {e}", path.display())))?;
    let cfg = penelope_kernel::Config::from_toml(&raw)
        .map_err(|e| CliError::Validation(e.to_string()))?;
    cfg.validate()
        .map_err(|e| CliError::Validation(e.to_string()))?;
    output::ok(&format!("{} est valide", path.display()), cli.json);
    Ok(())
}

fn validate_workflow(cli: &Cli, file: PathBuf) -> CliResult<()> {
    let raw = std::fs::read_to_string(&file)
        .map_err(|e| CliError::Io(format!("{} : {e}", file.display())))?;
    let w = penelope_workflow::Workflow::from_json(&raw)
        .map_err(|e| CliError::Validation(format!("JSON invalide : {e}")))?;
    let stem = file
        .file_name()
        .map(|n| n.to_string_lossy().replace(".workflow.json", ""));

    // Hors daemon : on valide sur ce qui est connu statiquement.
    let known = penelope_workflow::Known {
        workflow_ids: penelope_workflow::bundled::all()
            .into_iter()
            .map(|x| x.metadata.id)
            .collect(),
        native_tools: penelope_tools::all_tools()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect(),
        templates: penelope_telegram::templates::CATALOG
            .iter()
            .map(|s| s.to_string())
            .collect(),
        max_depth: 3,
        ..Default::default()
    };
    let report = penelope_workflow::validate(&w, stem.as_deref(), &known);

    if cli.json {
        output::print(
            &json!({
                "valid": report.is_valid(),
                "issues": report.issues.iter().map(|i| json!({
                    "path": i.path, "message": i.message,
                    "severity": format!("{:?}", i.severity).to_lowercase(),
                })).collect::<Vec<_>>(),
            }),
            true,
        );
    } else {
        println!("{}", report.render());
    }
    if report.is_valid() {
        Ok(())
    } else {
        Err(CliError::Validation(format!(
            "{} : {} erreur(s)",
            file.display(),
            report.errors().len()
        )))
    }
}

fn service(cli: &Cli) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let mgr = penelope_platform::backend::service_manager(dirs.as_ref())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let exe = std::env::current_exe().map_err(|e| CliError::Io(e.to_string()))?;

    let out = match cli.command {
        Command::Install => {
            let p = mgr
                .install(&exe, cli.home.as_deref())
                .map_err(|e| CliError::Io(e.to_string()))?;
            json!({"installed": true, "unit": p, "mechanism": mgr.mechanism()})
        }
        Command::Uninstall => {
            mgr.uninstall().map_err(|e| CliError::Io(e.to_string()))?;
            json!({"uninstalled": true})
        }
        Command::Start => {
            mgr.start().map_err(|e| CliError::Io(e.to_string()))?;
            json!({"started": true})
        }
        Command::Stop => {
            mgr.stop().map_err(|e| CliError::Io(e.to_string()))?;
            json!({"stopped": true})
        }
        _ => unreachable!("routage service"),
    };
    output::print(&out, cli.json);
    Ok(())
}

async fn daemon(cli: &Cli) -> CliResult<()> {
    let d = penelope_daemon::Daemon::new(cli.home.clone())
        .await
        .map_err(|e| CliError::Io(e.to_string()))?;
    let report = d.recover().await.map_err(|e| CliError::Io(e.to_string()))?;
    tracing::info!(?report, "reprise terminée");

    let daemon = std::sync::Arc::new(d);
    let serve = penelope_daemon::rpc::serve(daemon.clone());

    tokio::select! {
        r = serve => r.map_err(|e| CliError::Io(e.to_string()))?,
        _ = tokio::signal::ctrl_c() => {
            daemon.handle.shutdown();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            (vec!["backup"], m::BACKUP),
            (vec!["session", "list"], m::SESSION_LIST),
            (vec!["config", "get"], m::CONFIG_GET),
            (vec!["secret", "list"], m::SECRET_LIST),
            (vec!["model", "list"], m::MODEL_LIST),
            (vec!["wf", "list"], m::WF_LIST),
            (vec!["schedule", "list"], m::SCHEDULE_LIST),
            (vec!["mem", "search", "x"], m::MEM_SEARCH),
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
            vec!["approvals"],
            vec!["approve", "a_1"],
            vec!["deny", "a_1"],
            vec!["policies"],
            vec!["usage"],
            vec!["audit-verify"],
            vec!["backup"],
            vec!["eval", "unit"],
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

    #[test]
    fn a_secret_name_must_be_a_slug() {
        let cli = parse(&["--home", "/srv/pen", "secret", "set", "pas un nom"]);
        let name = match &cli.command {
            Command::Secret(SecretCmd::Set { name }) => name.clone(),
            other => panic!("{other:?}"),
        };
        let e = set_secret(&cli, name).unwrap_err();
        assert!(e.to_string().contains("nom de secret invalide"), "{e}");
    }

    #[test]
    fn paths_works_without_a_daemon() {
        let cli = parse(&["--home", "/srv/pen", "--json", "paths"]);
        paths(&cli).unwrap();
    }
}
