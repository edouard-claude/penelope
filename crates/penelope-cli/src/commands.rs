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
    /// Converse avec Pénélope : un message, ou une session interactive sans argument.
    Chat {
        /// Session à utiliser (par défaut : la session courante de la CLI).
        #[arg(long)]
        session: Option<String>,
        /// Message à envoyer. Sans message : mode interactif.
        message: Vec<String>,
    },
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
    /// Serveurs MCP déclarés dans `mcp.d`.
    #[command(subcommand)]
    Mcp(McpCmd),
    /// Workflows.
    #[command(subcommand)]
    Wf(WfCmd),
    /// Déclencheurs planifiés.
    #[command(subcommand)]
    Schedule(ScheduleCmd),
    /// Mémoire.
    #[command(subcommand)]
    Mem(MemCmd),
    /// Vault : synchronisation git et vérification.
    #[command(subcommand)]
    Vault(VaultCmd),
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

    /// Consommation et coûts, du plus cher au moins cher.
    Usage {
        /// Regroupement : session, turn (requête), model, day, role, provider, upstream, run.
        #[arg(long, default_value = "session")]
        by: String,
        /// Limite à une session.
        #[arg(long)]
        session: Option<String>,
        /// Depuis une date (AAAA-MM-JJ).
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: i64,
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
    New {
        title: Option<String>,
    },
    Export {
        session: String,
    },
    /// Modèle de la session : sans argument l'état, sinon un alias à épingler ou `auto`.
    Model {
        alias: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Résume les anciens échanges de la session (compaction niveau 3, lève le cooldown).
    Compact {
        #[arg(long)]
        session: Option<String>,
    },
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
    /// Enregistre un secret. La valeur est demandée sans écho, ou lue sur l'entrée
    /// standard si elle est redirigée.
    ///
    /// Elle n'est jamais un argument de la ligne de commande : elle resterait dans
    /// l'historique du shell et serait visible dans `ps`. Fonctionne **sans daemon**,
    /// pour qu'une installation neuve puisse être configurée avant le premier démarrage.
    ///
    /// En SSH, coller la valeur à l'invite : `pbpaste` lirait le presse-papiers distant.
    Set {
        name: String,
        /// Refusé : une valeur en argument reste dans l'historique du shell.
        #[arg(hide = true)]
        value: Option<String>,
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
pub enum McpCmd {
    /// État de chaque serveur, et déclarations invalides.
    List,
    /// Détail d'un serveur : état, déclaration, outils, journal.
    Show {
        name: String,
    },
    /// Ajoute un serveur depuis un fichier TOML (copié dans `mcp.d`).
    Add {
        file: PathBuf,
        /// Nom du serveur, si le fichier ne le donne pas.
        #[arg(long)]
        name: Option<String>,
    },
    /// Modifie un champ : `penelope mcp edit redmine timeout 60s`.
    Edit {
        name: String,
        field: String,
        value: String,
    },
    /// Retire un serveur : processus arrêté, outils retirés.
    Rm {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Redémarre un serveur et relit ses outils.
    Restart {
        name: String,
    },
    /// Essai à blanc : connexion, négociation, liste des outils.
    Test {
        /// Serveur déclaré à essayer.
        name: Option<String>,
        /// Ou un fichier TOML pas encore ajouté.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Dernières lignes d'erreur du serveur.
    Logs {
        name: String,
        #[arg(long, default_value_t = 50)]
        lines: u64,
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
    /// Démarre un run : `penelope wf run ticket-to-deploy --param ticket_url=https://…`.
    Run {
        id: String,
        #[arg(long = "param")]
        params: Vec<String>,
    },
    Trace {
        run: String,
    },
    /// `pause`, `resume`, `cancel`, `retry-step`, `skip-step`, `goto:<étape>`, ou
    /// `answer --choice <choix> [--input <texte>]` pour une étape qui pose une question.
    Control {
        run: String,
        op: String,
        #[arg(long)]
        choice: Option<String>,
        #[arg(long)]
        input: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScheduleCmd {
    List,
    /// Crée un déclencheur : `penelope schedule add cron --spec '{"expr":"0 9 * * 1"}'
    /// --target '{"type":"notify","template":"⏰ Revue hebdo"}'`.
    Add {
        kind: String,
        #[arg(long)]
        spec: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        dedup: Option<String>,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Rm {
        id: String,
    },
    /// Déclenche tout de suite, hors calendrier.
    Run {
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemCmd {
    Search {
        query: String,
    },
    Show {
        uid: String,
    },
    /// Pré-images d'une entrée ou d'un fichier du vault.
    History {
        #[arg(long)]
        uid: Option<String>,
        #[arg(long)]
        file: Option<String>,
    },
    /// Remet un fichier dans l'état d'une pré-image (`mem history` donne l'identifiant).
    Restore {
        id: i64,
    },
    /// Reconstruit l'index depuis le vault.
    Reindex,
    Forget {
        uid: String,
    },
    /// Candidats en attente de consolidation.
    Candidates,
    /// Lance la consolidation (`--dry-run` : rien n'est écrit).
    Dream {
        #[arg(long)]
        dry_run: bool,
    },
    /// Apprentissages des derniers jours.
    Learned {
        #[arg(default_value_t = 7)]
        days: i64,
    },
}

#[derive(Subcommand, Debug)]
pub enum VaultCmd {
    /// Commit du vault (et push si un remote est configuré).
    Sync,
    /// Vérifie frontmatter, pratiques et contenu interdit.
    Check,
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
        Command::Secret(SecretCmd::Set { name, value }) => {
            if value.is_some() {
                return Err(CliError::Usage(format!(
                    "la valeur d'un secret ne se passe jamais en argument : elle reste dans \
                     l'historique du shell et apparaît dans `ps`. Rien n'a été enregistré.\n\
                     → relancer sans valeur : `penelope secret set {name}`, puis coller la \
                     valeur à l'invite\n\
                     → si la vraie valeur a été tapée, la considérer comme exposée : en \
                     générer une nouvelle (pour un bot : /revoke chez @BotFather)"
                )));
            }
            return set_secret(&cli, name.clone());
        }
        Command::Install | Command::Uninstall | Command::Start | Command::Stop => {
            return service(&cli);
        }
        Command::Daemon => return daemon(&cli).await,
        Command::Chat { session, message } => {
            return chat(&cli, session.clone(), message.clone()).await;
        }
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
        Command::Model(ModelCmd::List { .. }) if !cli.json => {
            println!("{}", render_model_list(&value));
        }
        Command::Mcp(McpCmd::List) if !cli.json => {
            println!("{}", render_mcp_list(&value));
        }
        Command::Session(SessionCmd::Compact { .. }) | Command::Mem(MemCmd::Dream { .. })
            if !cli.json =>
        {
            println!("{}", value["text"].as_str().unwrap_or_default());
        }
        Command::Mcp(McpCmd::Logs { .. }) if !cli.json => {
            for l in value["lines"].as_array().cloned().unwrap_or_default() {
                println!("{}", l.as_str().unwrap_or_default());
            }
        }
        Command::Mcp(_) => output::print(&value, true),
        _ => output::print(&value, cli.json),
    }
    Ok(())
}

/// `penelope mcp list` : un serveur par ligne, puis les déclarations invalides.
fn render_mcp_list(v: &Value) -> String {
    let servers = v["servers"].as_array().cloned().unwrap_or_default();
    let mut out = if servers.is_empty() {
        format!(
            "Aucun serveur MCP déclaré dans {}",
            v["dir"].as_str().unwrap_or("mcp.d")
        )
    } else {
        let rows: Vec<Value> = servers
            .iter()
            .map(|s| {
                json!({
                    "serveur": s["name"],
                    "état": s["state"],
                    "outils": s["tools"],
                    "actif": if s["running"].as_bool().unwrap_or(false) { "oui" } else { "non" },
                    "appels": s["calls"],
                    "erreur": s["last_error"].as_str().unwrap_or(""),
                })
            })
            .collect();
        output::table(&rows)
    };
    for bad in v["invalid"].as_array().cloned().unwrap_or_default() {
        out.push_str(&format!(
            "\n⚠️ {} : {}",
            bad["file"].as_str().unwrap_or("?"),
            bad["error"].as_str().unwrap_or("?")
        ));
    }
    out
}

/// `penelope model list` : alias, routage en vigueur, puis recherche au catalogue.
fn render_model_list(v: &Value) -> String {
    let mut out = String::from("Alias\n");
    if let Some(a) = v["aliases"].as_array() {
        out.push_str(&output::table(a));
    }
    let r = &v["routing"];
    if r.is_object() {
        let step = |k: &str| {
            format!(
                "{} ({})",
                r[k]["alias"].as_str().unwrap_or("?"),
                r[k]["model"].as_str().unwrap_or("?")
            )
        };
        out.push_str("\n\nRoutage\n");
        if r["classifier"].as_bool().unwrap_or(false) {
            out.push_str(&format!(
                "adaptatif, classifieur {}\n  simple    → {}\n  ordinaire → {}\n  difficile → {}\n",
                r["classifier_model"].as_str().unwrap_or("?"),
                step("low"),
                step("medium"),
                step("high")
            ));
            out.push_str("tout sur main : penelope config set models.routing.classifier false");
        } else {
            out.push_str(&format!(
                "fixe : tout passe par {}\nadaptatif : penelope config set models.routing.classifier true",
                step("default")
            ));
        }
        if let Some(fb) = r["fallback"].as_object().filter(|f| !f.is_empty()) {
            out.push_str("\nreplis sur panne :");
            for (from, to) in fb {
                let to: Vec<&str> = to
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                    .unwrap_or_default();
                out.push_str(&format!(" {from} → {} ;", to.join(", ")));
            }
            out.pop();
        }
    }
    if let Some(m) = v["models"].as_array().filter(|m| !m.is_empty()) {
        out.push_str("\n\nCatalogue\n");
        out.push_str(&output::table(m));
    }
    if let Some(note) = v["note"].as_str().filter(|n| !n.is_empty()) {
        out.push_str(&format!("\n\n{note}"));
    }
    out
}

/// Associe une commande à sa méthode RPC (CA 15 : parité Telegram ↔ CLI).
pub fn route(cmd: &Command) -> CliResult<(&'static str, Value)> {
    Ok(match cmd {
        Command::Status => (m::STATUS, json!({})),
        Command::Doctor => (m::DOCTOR, json!({})),
        Command::Restart => (m::RESTART, json!({})),

        Command::Session(SessionCmd::List) => (m::SESSION_LIST, json!({})),
        Command::Session(SessionCmd::New { title }) => (m::SESSION_NEW, json!({"title": title})),
        Command::Session(SessionCmd::Model { alias, session }) => (
            m::SESSION_MODEL,
            json!({"alias": alias, "session": session}),
        ),
        Command::Session(SessionCmd::Export { session }) => {
            (m::SESSION_EXPORT, json!({"session": session}))
        }
        Command::Session(SessionCmd::Compact { session }) => {
            (m::SESSION_COMPACT, json!({"session": session}))
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
        Command::Mcp(McpCmd::List) => (m::MCP_LIST, json!({})),
        Command::Mcp(McpCmd::Show { name }) => (m::MCP_SHOW, json!({"name": name})),
        Command::Mcp(McpCmd::Add { file, name }) => {
            (m::MCP_ADD, json!({"toml": read_toml(file)?, "name": name}))
        }
        Command::Mcp(McpCmd::Edit { name, field, value }) => (
            m::MCP_EDIT,
            json!({"name": name, "patch": {field.clone(): parse_scalar(value)}}),
        ),
        Command::Mcp(McpCmd::Rm { name }) => (m::MCP_RM, json!({"name": name})),
        Command::Mcp(McpCmd::Enable { name }) => (m::MCP_ENABLE, json!({"name": name})),
        Command::Mcp(McpCmd::Disable { name }) => (m::MCP_DISABLE, json!({"name": name})),
        Command::Mcp(McpCmd::Restart { name }) => (m::MCP_RESTART, json!({"name": name})),
        Command::Mcp(McpCmd::Test { name, file }) => match file {
            Some(f) => (m::MCP_TEST, json!({"toml": read_toml(f)?, "name": name})),
            None => (m::MCP_TEST, json!({"name": name})),
        },
        Command::Mcp(McpCmd::Logs { name, lines }) => {
            (m::MCP_LOGS, json!({"name": name, "lines": lines}))
        }
        Command::Model(ModelCmd::Set { alias, model }) => {
            (m::MODEL_SET, json!({"alias": alias, "model": model}))
        }

        Command::Wf(WfCmd::List) => (m::WF_LIST, json!({})),
        Command::Wf(WfCmd::Show { id }) => (m::WF_SHOW, json!({"id": id})),
        Command::Wf(WfCmd::Runs) => (m::WF_RUNS, json!({})),
        Command::Wf(WfCmd::Trace { run }) => (m::WF_TRACE, json!({"run": run})),
        Command::Wf(WfCmd::Run { id, params }) => (
            m::WF_RUN,
            json!({"id": id, "params": penelope_daemon::telegram::parse_params(&params.join(" "))}),
        ),
        Command::Wf(WfCmd::Control {
            run,
            op,
            choice,
            input,
        }) => (
            m::WF_CONTROL,
            json!({"run": run, "op": op, "choice": choice, "input": input}),
        ),

        Command::Schedule(ScheduleCmd::List) => (m::SCHEDULE_LIST, json!({})),
        Command::Schedule(ScheduleCmd::Pause { id }) => (m::SCHEDULE_PAUSE, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Resume { id }) => (m::SCHEDULE_RESUME, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Rm { id }) => (m::SCHEDULE_RM, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Run { id }) => (m::SCHEDULE_RUN_NOW, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Add {
            kind,
            spec,
            target,
            dedup,
        }) => {
            let json_arg = |name: &str, raw: &str| {
                serde_json::from_str::<Value>(raw)
                    .map_err(|e| CliError::Usage(format!("--{name} n'est pas du JSON : {e}")))
            };
            (
                m::SCHEDULE_ADD,
                json!({
                    "kind": kind,
                    "spec": json_arg("spec", spec)?,
                    "target": json_arg("target", target)?,
                    "dedup": match dedup {
                        Some(d) => json_arg("dedup", d)?,
                        None => json!({}),
                    },
                }),
            )
        }

        Command::Mem(MemCmd::Search { query }) => (m::MEM_SEARCH, json!({"query": query})),
        Command::Mem(MemCmd::Show { uid }) => (m::MEM_SHOW, json!({"uid": uid})),
        Command::Mem(MemCmd::History { uid, file }) => {
            (m::MEM_HISTORY, json!({"uid": uid, "file": file}))
        }
        Command::Mem(MemCmd::Restore { id }) => (m::MEM_RESTORE, json!({"id": id})),
        Command::Mem(MemCmd::Reindex) => (m::MEM_REINDEX, json!({})),
        Command::Mem(MemCmd::Forget { uid }) => (m::MEM_FORGET, json!({"uid": uid})),
        Command::Mem(MemCmd::Candidates) => (m::MEM_CANDIDATES, json!({})),
        Command::Mem(MemCmd::Dream { dry_run }) => (m::MEM_DREAM, json!({"dry_run": dry_run})),
        Command::Mem(MemCmd::Learned { days }) => (m::MEM_LEARNED, json!({"days": days})),
        Command::Vault(VaultCmd::Sync) => (m::VAULT_SYNC, json!({})),
        Command::Vault(VaultCmd::Check) => (m::VAULT_CHECK, json!({})),

        Command::Skill(SkillCmd::List) => (m::SKILL_LIST, json!({})),
        Command::Skill(SkillCmd::Show { name }) => (m::SKILL_SHOW, json!({"name": name})),

        Command::Approvals => (m::APPROVALS, json!({})),
        Command::Approve { id, always } => (m::APPROVE, json!({"id": id, "always": always})),
        Command::Deny { id, reason } => (m::DENY, json!({"id": id, "reason": reason})),
        Command::Policies => (m::POLICIES, json!({})),

        Command::Usage {
            by,
            session,
            since,
            limit,
        } => (
            m::USAGE,
            json!({"by": by, "session": session, "since": since, "limit": limit}),
        ),
        Command::AuditVerify => (m::AUDIT_VERIFY, json!({})),
        Command::Backup => (m::BACKUP, json!({})),
        Command::Eval { suite } => (m::EVAL_RUN, json!({"suite": suite})),

        other => {
            return Err(CliError::Usage(format!("commande non routée : {other:?}")));
        }
    })
}

/// Contenu d'un fichier de déclaration MCP.
fn read_toml(path: &std::path::Path) -> CliResult<String> {
    std::fs::read_to_string(path).map_err(|e| CliError::Io(format!("{} : {e}", path.display())))
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
    if ((raw.starts_with('{') && raw.ends_with('}'))
        || (raw.starts_with('[') && raw.ends_with(']')))
        && let Ok(v) = serde_json::from_str::<Value>(raw)
    {
        return v;
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
    let cfg = d.services.config.config();
    penelope_observe::init(
        &d.services.platform.dirs.logs(),
        "info",
        cfg.observability.log_retention_days,
        true,
    );
    tracing::info!(version = penelope_daemon::VERSION, "Pénélope démarre");
    std::sync::Arc::new(d)
        .run()
        .await
        .map_err(|e| CliError::Io(e.to_string()))
}

/// `penelope chat` : un message, ou une conversation interactive.
async fn chat(cli: &Cli, session: Option<String>, message: Vec<String>) -> CliResult<()> {
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let socket = socket_path(cli.home.clone())?;
    if !message.is_empty() {
        let text = message.join(" ");
        return chat_turn(&socket, &text, session.as_deref(), false).await;
    }

    println!(
        "Pénélope : conversation (Ctrl-D pour quitter, /new pour une nouvelle session, /stop pour arrêter)"
    );
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut session = session;
    loop {
        print!("\n› ");
        let _ = std::io::stdout().flush();
        let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| CliError::Io(e.to_string()))?
        else {
            println!();
            break;
        };
        let line = line.trim();
        match line {
            "" => continue,
            "/quit" | "/exit" => break,
            "/new" => {
                let v = call(&socket, m::SESSION_NEW, json!({"title": "CLI"})).await?;
                let id = v["id"].as_str().unwrap_or_default().to_string();
                call(&socket, m::SESSION_SWITCH, json!({"session": id})).await?;
                println!("nouvelle session {id}");
                session = Some(id);
                continue;
            }
            "/stop" => {
                call(&socket, m::CHAT_STOP, json!({"session": session})).await?;
                continue;
            }
            _ => {}
        }
        if let Err(e) = chat_turn(&socket, line, session.as_deref(), true).await {
            eprintln!("erreur : {e}");
            if let Some(h) = e.hint() {
                eprintln!("→ {h}");
            }
        }
    }
    Ok(())
}

/// Un tour, affiché au fil de l'eau. En mode interactif, une approbation est demandée
/// sur place, puis la suite du tour est suivie jusqu'à sa fin.
async fn chat_turn(
    socket: &std::path::Path,
    text: &str,
    session: Option<&str>,
    interactive: bool,
) -> CliResult<()> {
    use std::io::Write;

    let mut streamed = false;
    let mut on_event = |ev: &Value| match ev["type"].as_str() {
        Some("delta") => {
            print!("{}", ev["text"].as_str().unwrap_or(""));
            let _ = std::io::stdout().flush();
            streamed = true;
        }
        Some("tool_call") => {
            eprintln!("\n⚙️  {}", ev["name"].as_str().unwrap_or("?"));
        }
        Some("tool_result") if ev["ok"] == false => {
            eprintln!("   ✗ {}", ev["preview"].as_str().unwrap_or(""));
        }
        _ => {}
    };
    let result = crate::client::call_stream(
        socket,
        m::CHAT_STREAM,
        json!({"text": text, "session": session}),
        &mut on_event,
    )
    .await?;
    let session_id = result["session"].as_str().unwrap_or_default().to_string();
    finish_turn(socket, &result, streamed, &session_id, interactive).await
}

async fn finish_turn(
    socket: &std::path::Path,
    result: &Value,
    streamed: bool,
    session_id: &str,
    interactive: bool,
) -> CliResult<()> {
    match result["outcome"].as_str().unwrap_or("") {
        "answered" => {
            if streamed {
                println!();
            } else {
                println!("{}", result["text"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        "awaiting_approval" => {
            let id = result["approval_id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let pending = call(socket, m::APPROVALS, json!({})).await?;
            let detail = pending
                .as_array()
                .and_then(|a| a.iter().find(|x| x["id"] == id.as_str()))
                .cloned()
                .unwrap_or(Value::Null);
            println!(
                "\n⚠️  approbation requise : {} (risque {})",
                detail["subject"].as_str().unwrap_or("?"),
                detail["risk"].as_str().unwrap_or("?")
            );
            if !detail["payload"]["arguments"].is_null() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&detail["payload"]["arguments"])
                        .unwrap_or_default()
                );
            }
            if !interactive {
                println!("→ penelope approve {id}   ou   penelope deny {id}");
                return Ok(());
            }
            print!("Autoriser ? [o]ui / [n]on / [t]oujours : ");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            let (method, params) = match answer.trim().to_lowercase().as_str() {
                "o" | "oui" | "y" | "yes" => (m::APPROVE, json!({"id": id})),
                "t" | "toujours" | "a" | "always" => {
                    (m::APPROVE, json!({"id": id, "always": true}))
                }
                _ => (m::DENY, json!({"id": id})),
            };
            // Suivre la suite du tour avant de trancher, pour ne rien manquer.
            follow_session(socket, session_id, method, params).await
        }
        "failed" => {
            eprintln!("\n❌ {}", result["error"].as_str().unwrap_or("échec"));
            Ok(())
        }
        "cancelled" => {
            eprintln!("\n⏹ arrêté");
            Ok(())
        }
        "loop_aborted" => {
            eprintln!("\n⛔ boucle détectée, tour arrêté");
            Ok(())
        }
        "budget_exceeded" => {
            eprintln!(
                "\n💸 budget `{}` atteint",
                result["scope"].as_str().unwrap_or("?")
            );
            Ok(())
        }
        other => {
            eprintln!("\nissue inattendue : {other}");
            Ok(())
        }
    }
}

/// Tranche une approbation puis affiche la suite du tour de la session.
async fn follow_session(
    socket: &std::path::Path,
    session_id: &str,
    method: &str,
    params: Value,
) -> CliResult<()> {
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = penelope_platform::ipc::connect(socket)
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;
    let (read, mut write) = stream.into_split();
    let mut body = serde_json::to_string(&penelope_kernel::api::RpcRequest::new(
        1,
        m::TAIL,
        json!({}),
    ))
    .unwrap_or_default();
    body.push('\n');
    write
        .write_all(body.as_bytes())
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;

    call(socket, method, params).await?;
    if method == m::DENY {
        println!("refusé ; le modèle en est informé.");
    }

    let mut lines = BufReader::new(read).lines();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1800);
    let mut streamed = false;
    loop {
        let next = tokio::time::timeout_at(deadline, lines.next_line()).await;
        let Ok(Ok(Some(line))) = next else { break };
        let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let ev = &v["params"];
        let mine = ev["session_id"].as_str() == Some(session_id);
        match ev["type"].as_str() {
            Some("delta") if mine => {
                print!("{}", ev["text"].as_str().unwrap_or(""));
                let _ = std::io::stdout().flush();
                streamed = true;
            }
            Some("tool_call") if mine => eprintln!("\n⚙️  {}", ev["name"].as_str().unwrap_or("?")),
            Some("done") if mine => {
                if !streamed {
                    println!("{}", ev["text"].as_str().unwrap_or(""));
                } else {
                    println!();
                }
                break;
            }
            Some("error") if mine => {
                eprintln!("\n❌ {}", ev["message"].as_str().unwrap_or(""));
                break;
            }
            Some("approval") => {
                println!(
                    "\n⚠️  nouvelle approbation requise : {} → penelope approve {}",
                    ev["subject"].as_str().unwrap_or("?"),
                    ev["id"].as_str().unwrap_or("?")
                );
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
            (vec!["backup"], m::BACKUP),
            (vec!["session", "list"], m::SESSION_LIST),
            (vec!["session", "model", "main"], m::SESSION_MODEL),
            (vec!["session", "compact"], m::SESSION_COMPACT),
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
            (vec!["mcp", "list"], m::MCP_LIST),
            (vec!["mcp", "show", "redmine"], m::MCP_SHOW),
            (vec!["mcp", "rm", "redmine"], m::MCP_RM),
            (vec!["mcp", "enable", "redmine"], m::MCP_ENABLE),
            (vec!["mcp", "disable", "redmine"], m::MCP_DISABLE),
            (vec!["mcp", "restart", "redmine"], m::MCP_RESTART),
            (vec!["mcp", "test", "redmine"], m::MCP_TEST),
            (vec!["mcp", "logs", "redmine"], m::MCP_LOGS),
            (
                vec!["mcp", "edit", "redmine", "timeout", "60s"],
                m::MCP_EDIT,
            ),
            (vec!["config", "get"], m::CONFIG_GET),
            (vec!["secret", "list"], m::SECRET_LIST),
            (vec!["model", "list"], m::MODEL_LIST),
            (vec!["wf", "list"], m::WF_LIST),
            (vec!["schedule", "list"], m::SCHEDULE_LIST),
            (vec!["mem", "search", "x"], m::MEM_SEARCH),
            (vec!["mem", "dream", "--dry-run"], m::MEM_DREAM),
            (vec!["mem", "restore", "12"], m::MEM_RESTORE),
            (vec!["vault", "check"], m::VAULT_CHECK),
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

    #[test]
    fn paths_works_without_a_daemon() {
        let cli = parse(&["--home", "/srv/pen", "--json", "paths"]);
        paths(&cli).unwrap();
    }
}
