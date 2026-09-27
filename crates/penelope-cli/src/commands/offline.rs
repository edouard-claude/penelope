//! Les commandes hors daemon : elles marchent sans socket, pour l'édition en SSH et la
//! CI (`paths`, `doctor`, `config validate`, `wf validate`, `eval`, `secret set`) et pour
//! le service lui-même (`install`, `start`, `stop`, `uninstall`).

use super::*;

/// Suite d'évaluation depuis les sources : `cargo test` avec le filtre de la suite.
pub(super) async fn eval_local(suite: &str) -> CliResult<()> {
    let Some((sub, args)) = penelope_evals::suites::cargo_filter(suite) else {
        let known: Vec<String> = penelope_evals::suites::all_suites()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect();
        return Err(CliError::Usage(format!(
            "suite inconnue : `{suite}` (suites : {})",
            known.join(", ")
        )));
    };
    let needed = penelope_evals::suites::required_env(suite);
    if !needed.is_empty() {
        let missing: Vec<&str> = needed
            .iter()
            .copied()
            .filter(|v| {
                std::env::var(v)
                    .map(|x| x.trim().is_empty())
                    .unwrap_or(true)
            })
            .collect();
        if !missing.is_empty() {
            return Err(CliError::Usage(format!(
                "suite réseau `{suite}` : variable(s) à exporter d'abord : {}",
                missing.join(", ")
            )));
        }
        eprintln!("⚠️ suite réseau `{suite}` : services réels, appels facturés.");
    }
    let root = std::env::var("PENELOPE_SOURCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    if !manifest.contains("[workspace]") || !root.join("crates/penelope-evals").exists() {
        return Err(CliError::Usage(
            "à lancer depuis le dépôt de Pénélope (ou `PENELOPE_SOURCE_DIR`)".into(),
        ));
    }
    let status = tokio::process::Command::new("cargo")
        .arg(sub)
        .args(&args)
        .current_dir(&root)
        .status()
        .await
        .map_err(|e| CliError::Io(format!("cargo : {e}")))?;
    if status.success() {
        println!("✅ suite `{suite}` verte");
        Ok(())
    } else {
        Err(CliError::Validation(format!("suite `{suite}` en échec")))
    }
}

pub(super) fn paths(cli: &Cli) -> CliResult<()> {
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
pub(super) fn set_secret(cli: &Cli, name: String) -> CliResult<()> {
    penelope_platform::validate_secret_name(&name).map_err(|e| CliError::Usage(e.to_string()))?;

    let raw = penelope_platform::terminal::read_secret(&format!(
        "Colle la valeur de `{name}` puis Entrée (rien ne s'affiche) : "
    ))
    .map_err(|e| CliError::Io(format!("lecture de la valeur : {e}")))?;
    // Un copier-coller traîne presque toujours un retour à la ligne ou une espace.
    let value = raw.trim();
    if value.is_empty() {
        return Err(CliError::Usage(format!(
            "valeur vide : relancer `penelope secret set {name}` et coller la valeur à \
             l'invite"
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
/// `doctor` en deux temps (issue #99) : ce qui se vérifie sans le daemon d'abord, puis
/// ses propres contrôles. Un daemon muet ou absent est un contrôle en échec, en tête du
/// rapport, pas une commande qui pend.
pub(super) async fn doctor(cli: &Cli) -> CliResult<()> {
    use penelope_kernel::api::DoctorCheck;
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let socket = dirs.socket_path();
    let mut checks: Vec<DoctorCheck> = Vec::new();

    let remote = call(&socket, m::DOCTOR, json!({})).await;
    checks.push(match &remote {
        Ok(_) => DoctorCheck::ok("daemon", "Daemon", "répond"),
        Err(e @ CliError::DaemonUnresponsive(_)) => DoctorCheck::fail(
            "daemon",
            "Daemon",
            e.to_string(),
            Some("penelope restart".into()),
        )
        .critical(),
        Err(e) => DoctorCheck::fail(
            "daemon",
            "Daemon",
            e.to_string(),
            Some("penelope start".into()),
        )
        .critical(),
    });
    checks.push(DoctorCheck::ok(
        "binary",
        "Binaire",
        format!("penelope {}", env!("CARGO_PKG_VERSION")),
    ));
    let config = dirs.config_file();
    checks.push(match std::fs::read_to_string(&config) {
        Ok(raw) => match penelope_kernel::Config::parse(&raw) {
            Ok((cfg, _)) => match cfg.validate() {
                Ok(()) => DoctorCheck::ok("config.file", "Fichier de configuration", "valide"),
                Err(e) => DoctorCheck::fail(
                    "config.file",
                    "Fichier de configuration",
                    e.to_string(),
                    Some("penelope config validate".into()),
                ),
            },
            Err(e) => DoctorCheck::fail(
                "config.file",
                "Fichier de configuration",
                e.to_string(),
                Some("penelope config validate".into()),
            )
            .critical(),
        },
        Err(e) => DoctorCheck::fail(
            "config.file",
            "Fichier de configuration",
            format!("{} : {e}", config.display()),
            None,
        ),
    });
    if let Ok(v) = remote {
        let from_daemon: Vec<DoctorCheck> = serde_json::from_value(v).unwrap_or_default();
        checks.extend(from_daemon);
    }
    if cli.json {
        output::print(&json!(checks), true);
    } else {
        print!("{}", penelope_ops::doctor::render(&checks));
    }
    if checks.iter().any(|c| !c.ok && c.severity == "error") {
        return Err(CliError::Validation(
            "des contrôles critiques sont en échec".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_config(cli: &Cli, file: Option<PathBuf>) -> CliResult<()> {
    let path = match file {
        Some(p) => p,
        None => penelope_platform::resolve_directories(cli.home.clone())
            .map_err(|e| CliError::Io(e.to_string()))?
            .config_file(),
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| CliError::Io(format!("{} : {e}", path.display())))?;
    // Tolérant comme le daemon (#76) : une clé inconnue est nommée, pas fatale.
    let (cfg, unknown) =
        penelope_kernel::Config::parse(&raw).map_err(|e| CliError::Validation(e.to_string()))?;
    cfg.validate()
        .map_err(|e| CliError::Validation(e.to_string()))?;
    for k in &unknown {
        println!(
            "⚠️ clé ignorée par cette version : {k} (écrite par une version plus récente, ou \
             faute de frappe)"
        );
    }
    let found = penelope_kernel::coherence::contradictions(&cfg);
    let refusals: Vec<String> = found
        .iter()
        .filter(|c| c.gravity == penelope_kernel::coherence::Gravity::Refus)
        .map(|c| format!("{} ({})", c.message, c.keys.join(", ")))
        .collect();
    if !refusals.is_empty() {
        return Err(CliError::Validation(format!(
            "réglages qui s'annulent :\n- {}",
            refusals.join("\n- ")
        )));
    }
    for c in &found {
        println!("⚠️ {} ({})", c.message, c.keys.join(", "));
    }
    output::ok(&format!("{} est valide", path.display()), cli.json);
    Ok(())
}

pub(super) fn validate_workflow(cli: &Cli, file: PathBuf) -> CliResult<()> {
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

pub(super) fn service(cli: &Cli) -> CliResult<()> {
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
