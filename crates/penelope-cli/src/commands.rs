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

    /// Attente maximale d'une réponse du daemon, en secondes (15 par défaut, sauf pour
    /// les commandes longues ; 0 : sans limite).
    #[arg(long, global = true)]
    pub timeout: Option<u64>,

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
    /// Journaux du daemon (JSON du jour et de la veille), filtrés par tour ou par session.
    Logs {
        /// Lignes du tour dont c'est l'identifiant.
        #[arg(long)]
        turn: Option<String>,
        /// Lignes de la session dont c'est l'identifiant.
        #[arg(long)]
        session: Option<String>,
        /// Dernières lignes gardées.
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
    /// Converse avec Pénélope : un message, ou une session interactive sans argument.
    Chat {
        /// Session à utiliser (par défaut : la session courante de la CLI).
        #[arg(long)]
        session: Option<String>,
        /// Message à envoyer. Sans message : mode interactif.
        message: Vec<String>,
    },
    /// Entretien d'accueil : rôle, projets, outils, style, limites. Une partie seule :
    /// `penelope onboard limites`.
    Onboard { part: Option<String> },
    /// Démarre le service.
    Start,
    /// Arrête le service.
    Stop,
    /// Redémarre le daemon.
    Restart,
    /// Met à jour le binaire depuis les releases GitHub (somme SHA-256 vérifiée, retour
    /// automatique à l'ancienne version si la nouvelle ne démarre pas).
    Upgrade {
        /// Indique seulement la dernière version publiée.
        #[arg(long, conflicts_with_all = ["rollback", "tag", "force"])]
        check: bool,
        /// Remet en place le binaire précédent.
        #[arg(long, conflicts_with_all = ["tag", "force"])]
        rollback: bool,
        /// Version précise (`v0.3.1`), antérieure comprise.
        #[arg(long)]
        tag: Option<String>,
        /// Réinstalle même si cette version tourne déjà.
        #[arg(long)]
        force: bool,
        /// Installation depuis les sources : bascule vers les releases (binaire re-signé
        /// dans `upgrade.install_dir`, service réécrit, retour automatique sans santé).
        #[arg(long, conflicts_with_all = ["check", "rollback"])]
        switch: bool,
    },
    /// État du daemon.
    Status,
    /// Métriques du daemon, texte Prometheus (tours, outils, approbations, mémoire).
    Metrics,
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
        /// Effet incertain après un arrêt brutal : `done` (vérifié, il a eu lieu) ou
        /// `retry` (le relancer). `penelope deny` le laisse tel quel.
        #[arg(long, value_parser = ["done", "retry"])]
        effect: Option<String>,
    },
    /// Refuse une demande.
    Deny {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Règles d'autorisation.
    Policies,

    /// Consommation et coûts, du plus cher au moins cher : tokens d'entrée, en cache, de
    /// sortie, part de cache.
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
    /// Sauvegarde cohérente. `--push` : archive chiffrée complète, poussée dans le dépôt
    /// privé de `backup.git_remote`.
    Backup {
        #[arg(long)]
        push: bool,
        /// Archive complète (base, vault, skills, workflows, `mcp.d`, configuration).
        #[arg(long)]
        full: bool,
        /// Inclure artefacts et médias reçus.
        #[arg(long)]
        media: bool,
    },
    /// Restaure **tout** depuis une sauvegarde chiffrée (archive locale ou dépôt privé),
    /// daemon arrêté, sur une machine neuve.
    #[command(name = "restore-all")]
    RestoreAll {
        /// Archive `.tar.gz.enc`, ou dépôt git à cloner ; vide : `backup.git_remote`.
        source: Option<String>,
        /// Dire ce qui serait restauré, sans rien écrire.
        #[arg(long)]
        dry_run: bool,
    },
    /// Restaure une sauvegarde, daemon arrêté (la base actuelle est d'abord mise de côté).
    Restore { file: PathBuf },
    /// Import depuis un autre agent.
    #[command(subcommand)]
    Import(ImportCmd),
    /// Exporte en JSONL : `session [id]`, `run <id>` ou `all`.
    Export { what: String, id: Option<String> },
    /// Stockage : reconstruction des index dérivés.
    #[command(subcommand)]
    Store(StoreCmd),
    /// Lance une suite d'évaluation depuis les sources (`cargo test`), sans daemon.
    Eval { suite: String },
}

#[derive(Subcommand, Debug)]
pub enum ImportCmd {
    /// Skills, SOUL.md, AGENTS.md, mémoire et serveurs MCP d'une instance Hermes.
    Hermes {
        /// Racine de l'instance (par défaut `$HERMES_HOME`, sinon `~/.hermes`).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Montre ce qui serait importé, sans rien écrire.
        #[arg(long)]
        dry_run: bool,
        /// N'essaie pas les serveurs MCP importés.
        #[arg(long)]
        no_test: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum StoreCmd {
    /// Reconstruit l'index plein texte et l'index mémoire, vérifie l'audit.
    Rebuild,
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
    /// Ferme une session : tour en cours arrêté, file vidée (identifiant, préfixe ou titre).
    Close {
        session: String,
    },
    /// Efface le contenu d'une session (RGPD) : messages, résumés, artefacts, requêtes.
    /// La chaîne d'audit garde ses lignes, sans leur contenu.
    Purge {
        session: String,
        /// Sans confirmation interactive.
        #[arg(long)]
        yes: bool,
        /// Raison notée dans `audit.purge`.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Renomme une session.
    Title {
        session: String,
        #[arg(required = true, num_args = 1..)]
        title: Vec<String>,
    },
    /// Duplique une session (transcript, métadonnées, résumés).
    Fork {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        title: Option<String>,
    },
    /// Défait les derniers échanges (mis de côté dans une session d'archive).
    Rewind {
        #[arg(default_value_t = 1)]
        turns: usize,
        #[arg(long)]
        session: Option<String>,
    },
    /// Plafond de dépense propre à une session : sans montant l'état, `off` pour revenir au
    /// plafond de la configuration.
    Budget {
        session: String,
        usd: Option<String>,
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
    /// Mode d'approbation de la session : sans argument l'état, sinon `ask` (demander
    /// tout), `reads` (lectures sans demande), `auto` (tout sauf le destructif) ou
    /// `default`.
    Mode {
        #[arg(value_parser = ["ask", "reads", "auto", "default"])]
        mode: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Sujet de travail de la session : sans argument l'état et les projets connus, sinon
    /// un projet, ou `aucun`. Filtre la mémoire injectée d'office.
    Project {
        project: Option<String>,
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
    /// Connecte un fournisseur à compte (`codex` : abonnement ChatGPT).
    Auth {
        /// Fournisseur à connecter.
        #[arg(default_value = "codex")]
        provider: String,
        /// Déconnecte au lieu de connecter : le jeton est révoqué puis oublié.
        #[arg(long)]
        logout: bool,
        /// Affiche l'état de la connexion, sans rien changer.
        #[arg(long)]
        status: bool,
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
    /// Modifie un champ : `penelope mcp edit redmine timeout 60s` ; pour une liste
    /// (`args`, `scopes`, `roots`), une valeur seule (`roots /chemin`) ou
    /// `'["/a", "/b"]'`.
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
    /// Autorisation OAuth d'un serveur HTTP : sans option, l'URL à ouvrir ; avec
    /// `--callback`, l'adresse affichée par le navigateur après l'autorisation.
    Auth {
        name: String,
        #[arg(long)]
        callback: Option<String>,
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
    /// `pause`, `resume`, `cancel`, `retry-step`, `skip-step`, `goto:<étape>`,
    /// `answer --choice <choix> [--input <texte>]` pour une étape qui pose une question, ou
    /// `budget --usd <montant> --tokens <nombre>` pour relever les plafonds du run.
    Control {
        run: String,
        op: String,
        #[arg(long)]
        choice: Option<String>,
        #[arg(long)]
        input: Option<String>,
        #[arg(long)]
        usd: Option<f64>,
        #[arg(long)]
        tokens: Option<u64>,
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
    /// Change où livre une planification, sans la recréer : `--private`, ou `--chat`
    /// (et `--topic`) d'une conversation autorisée.
    Move {
        id: String,
        /// Un groupe a un identifiant négatif (`-100…`).
        #[arg(long, conflicts_with = "private", allow_negative_numbers = true)]
        chat: Option<i64>,
        #[arg(long, requires = "chat")]
        topic: Option<i64>,
        #[arg(long)]
        private: bool,
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
    Reindex {
        /// Recalcule aussi tous les vecteurs (mémoire, intentions, outils MCP).
        #[arg(long)]
        embeddings: bool,
    },
    Forget {
        uid: String,
    },
    /// Candidats en attente de consolidation, et questions sans réponse.
    Candidates,
    /// Propose le découpage d'une entrée trop longue : une carte, jamais une écriture.
    Split {
        uid: String,
    },
    /// Audit de la mémoire noté sur 100, avec la prochaine action par axe.
    Audit,
    /// Remet à consolider les règles rejetées pour leur seule origine : elles seront
    /// demandées au propriétaire.
    #[command(name = "retry-rejected")]
    RetryRejected,
    /// Changements du vault non commités, ou depuis le dernier rêve (`--since dream`).
    Diff {
        #[arg(long, value_parser = ["dream"])]
        since: Option<String>,
    },
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
    /// Signaux d'usage d'une entrée (rappels, rappels utiles, succès) et le facteur de
    /// classement qu'ils lui donnent.
    Signals {
        uid: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum VaultCmd {
    /// Commit du vault (et push si un remote est configuré).
    Sync,
    /// Vérifie frontmatter, pratiques et contenu interdit.
    Check,
    /// Lint du wiki : liens non résolus, orphelines, impasses, alias et noms en double,
    /// identifiants de bloc, propriétés ; entrées expirées et contradictions à trancher.
    Lint,
}

#[derive(Subcommand, Debug)]
pub enum SkillCmd {
    List,
    Show {
        name: String,
    },
    /// Restaure la version précédente d'une skill.
    Rollback {
        name: String,
    },
    /// Relit les dossiers de skills tout de suite (après un dépôt par `scp`).
    Reload,
}

/// Exécute la commande.
pub async fn run(cli: Cli) -> CliResult<()> {
    crate::client::set_timeout(cli.timeout);
    // Les commandes hors daemon d'abord : elles doivent marcher sans socket.
    match &cli.command {
        Command::Paths => return paths(&cli),
        Command::Doctor => return doctor(&cli).await,
        Command::Config(ConfigCmd::Validate { file }) => {
            return validate_config(&cli, file.clone());
        }
        Command::Wf(WfCmd::Validate { file }) => return validate_workflow(&cli, file.clone()),
        Command::Eval { suite } => return eval_local(suite).await,
        Command::Restore { file } => return restore_offline(&cli, file).await,
        Command::RestoreAll { source, dry_run } => {
            return restore_all(&cli, source.clone(), *dry_run).await;
        }
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
        Command::Logs {
            turn,
            session,
            lines,
        } => return logs(&cli, turn.as_deref(), session.as_deref(), *lines),
        Command::Upgrade { .. } => return upgrade(&cli).await,
        Command::Chat { session, message } => {
            return chat(&cli, session.clone(), message.clone()).await;
        }
        Command::Onboard { part } => return onboard(&cli, part.clone()).await,
        // Connexion d'un compte : le code s'affiche, puis Pénélope attend la validation.
        Command::Model(ModelCmd::Auth {
            provider,
            logout,
            status,
        }) if !cli.json && !*logout && !*status => {
            return model_auth(&cli, provider.clone()).await;
        }
        _ => {}
    }

    // Purge : effacement sans retour, confirmé à l'invite sauf `--yes` (issue #46).
    if let Command::Session(SessionCmd::Purge { session, yes, .. }) = &cli.command
        && !yes
    {
        eprint!(
            "Effacer définitivement le contenu de la session « {session} » (messages, \
             résumés, artefacts) ? La chaîne d'audit garde ses lignes, sans leur contenu. \
             [o]ui / [n]on : "
        );
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !matches!(
            answer.trim().to_lowercase().as_str(),
            "o" | "oui" | "y" | "yes"
        ) {
            eprintln!("Rien n'a été effacé.");
            return Ok(());
        }
    }

    let socket = socket_path(cli.home.clone())?;
    let (method, params) = route(&cli.command)?;
    let value = call(&socket, method, params).await?;

    match &cli.command {
        Command::Metrics if !cli.json => {
            print!("{}", value["text"].as_str().unwrap_or_default());
        }
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
        Command::Schedule(ScheduleCmd::List) if !cli.json => {
            println!("{}", render_schedule_list(&value));
        }
        Command::Session(SessionCmd::List) if !cli.json => {
            println!("{}", render_session_list(&value));
        }
        Command::Session(SessionCmd::Compact { .. })
        | Command::Mem(MemCmd::Dream { .. })
        | Command::Mem(MemCmd::Diff { .. })
        | Command::Vault(VaultCmd::Lint)
        | Command::Import(_)
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

/// `penelope session list` : titre et date d'abord, l'identifiant pour `switch`.
fn render_session_list(v: &Value) -> String {
    let sessions = v.as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        return "Aucune session.".into();
    }
    let rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let when = s["last_activity"]
                .as_str()
                .or_else(|| s["created_at"].as_str())
                .unwrap_or("")
                .replace('T', " ");
            json!({
                "titre": s["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("(sans titre)"),
                "activité": when.chars().take(16).collect::<String>(),
                "état": s["state"],
                "session": s["id"],
            })
        })
        .collect();
    output::table(&rows)
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
                    "trousseau": keychain_cell(s),
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

/// `penelope schedule list` : une planification par ligne, avec où elle livre (#124).
fn render_schedule_list(v: &Value) -> String {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "Aucune planification.".into();
    }
    let rows: Vec<Value> = list
        .iter()
        .map(|s| {
            let spec = &s["spec"];
            let quand = match s["kind"].as_str().unwrap_or("?") {
                "cron" => spec["expr"].as_str().unwrap_or("?").to_string(),
                "interval" | "mcp_poll" => format!(
                    "toutes les {} min",
                    spec["every_ms"].as_u64().unwrap_or(0) / 60_000
                ),
                "watch_file" => spec["path"].as_str().unwrap_or("?").to_string(),
                _ => spec["event"].as_str().unwrap_or("?").to_string(),
            };
            let t = &s["target"];
            let quoi = t["label"]
                .as_str()
                .or(t["prompt"].as_str())
                .or(t["template"].as_str())
                .or(t["workflowId"].as_str())
                .unwrap_or("")
                .chars()
                .take(40)
                .collect::<String>();
            json!({
                "id": s["id"],
                "état": s["state"],
                "quand": quand,
                "quoi": quoi,
                "vers": s["destination"].as_str().unwrap_or(""),
                "prochain": s["next_run"].as_str().unwrap_or(""),
            })
        })
        .collect();
    output::table(&rows)
}

/// Colonne « trousseau » de `penelope mcp list` : un serveur distant n'a pas de processus
/// local, donc rien à dire (issue #122).
fn keychain_cell(s: &Value) -> &'static str {
    match (s["transport"].as_str(), s["keychain"].as_bool()) {
        (Some("stdio"), Some(true)) => "ouvert",
        (Some("stdio"), _) => "fermé",
        _ => "",
    }
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
        Command::Metrics => (m::METRICS, json!({})),
        Command::Doctor => (m::DOCTOR, json!({})),
        Command::Restart => (m::RESTART, json!({})),
        Command::Import(ImportCmd::Hermes {
            path,
            dry_run,
            no_test,
        }) => (
            m::IMPORT_HERMES,
            json!({
                "path": path.as_ref().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())),
                "apply": !dry_run,
                "test": !no_test,
            }),
        ),
        Command::Upgrade {
            check,
            rollback,
            tag,
            force,
            switch,
        } => (
            m::UPGRADE,
            json!({"check": check, "rollback": rollback, "tag": tag, "force": force, "switch": switch}),
        ),

        Command::Session(SessionCmd::List) => (m::SESSION_LIST, json!({})),
        Command::Session(SessionCmd::New { title }) => (m::SESSION_NEW, json!({"title": title})),
        Command::Session(SessionCmd::Close { session }) => {
            (m::SESSION_CLOSE, json!({"session": session}))
        }
        Command::Session(SessionCmd::Purge {
            session, reason, ..
        }) => (
            m::SESSION_PURGE,
            json!({"session": session, "reason": reason}),
        ),
        Command::Session(SessionCmd::Title { session, title }) => (
            m::SESSION_TITLE,
            json!({"session": session, "title": title.join(" ")}),
        ),
        Command::Session(SessionCmd::Budget { session, usd }) => (
            m::SESSION_BUDGET,
            json!({
                "session": session,
                "usd": match usd.as_deref() {
                    None => Value::Null,
                    Some("off") => json!(0),
                    Some(x) => json!(x),
                },
            }),
        ),
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
        Command::Session(SessionCmd::Mode { mode, session }) => {
            (m::SESSION_MODE, json!({"mode": mode, "session": session}))
        }
        Command::Session(SessionCmd::Project { project, session }) => (
            m::SESSION_PROJECT,
            json!({"project": project, "session": session}),
        ),

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
        Command::Mcp(McpCmd::Auth { name, callback }) => {
            (m::MCP_AUTH, json!({"name": name, "callback": callback}))
        }
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
        // `model auth` sans option passe par `model_auth` (code affiché, puis attente) ;
        // cette route sert la parité CLI↔RPC et le mode `--json`.
        Command::Model(ModelCmd::Auth {
            provider,
            logout,
            status,
        }) => (
            m::MODEL_AUTH,
            json!({
                "provider": provider,
                "action": if *logout { "logout" } else if *status { "status" } else { "start" },
            }),
        ),

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
            usd,
            tokens,
        }) => (
            m::WF_CONTROL,
            json!({"run": run, "op": op, "choice": choice, "input": input,
                   "usd": usd, "tokens": tokens}),
        ),

        Command::Schedule(ScheduleCmd::List) => (m::SCHEDULE_LIST, json!({})),
        Command::Schedule(ScheduleCmd::Pause { id }) => (m::SCHEDULE_PAUSE, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Resume { id }) => (m::SCHEDULE_RESUME, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Rm { id }) => (m::SCHEDULE_RM, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Run { id }) => (m::SCHEDULE_RUN_NOW, json!({"id": id})),
        Command::Schedule(ScheduleCmd::Move {
            id,
            chat,
            topic,
            private,
        }) => {
            if !private && chat.is_none() {
                return Err(CliError::Usage(
                    "où l'envoyer : `--private`, ou `--chat <id>` (et `--topic <id>`)".into(),
                ));
            }
            (
                m::SCHEDULE_MOVE,
                json!({"id": id, "private": private, "chat_id": chat, "topic_id": topic}),
            )
        }
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
        Command::Mem(MemCmd::Reindex { embeddings }) => {
            (m::MEM_REINDEX, json!({"embeddings": embeddings}))
        }
        Command::Mem(MemCmd::Forget { uid }) => (m::MEM_FORGET, json!({"uid": uid})),
        Command::Mem(MemCmd::Candidates) => (m::MEM_CANDIDATES, json!({})),
        Command::Mem(MemCmd::Split { uid }) => (m::MEM_SPLIT, json!({"uid": uid})),
        Command::Mem(MemCmd::Audit) => (m::MEM_AUDIT, json!({})),
        Command::Mem(MemCmd::RetryRejected) => (m::MEM_RETRY_REJECTED, json!({})),
        Command::Mem(MemCmd::Diff { since }) => (m::MEM_DIFF, json!({"since": since})),
        Command::Mem(MemCmd::Dream { dry_run }) => (m::MEM_DREAM, json!({"dry_run": dry_run})),
        Command::Mem(MemCmd::Learned { days }) => (m::MEM_LEARNED, json!({"days": days})),
        Command::Mem(MemCmd::Signals { uid }) => (m::MEM_SIGNALS, json!({"uid": uid})),
        Command::Vault(VaultCmd::Sync) => (m::VAULT_SYNC, json!({})),
        Command::Vault(VaultCmd::Check) => (m::VAULT_CHECK, json!({})),
        Command::Vault(VaultCmd::Lint) => (m::VAULT_LINT, json!({})),

        Command::Skill(SkillCmd::List) => (m::SKILL_LIST, json!({})),
        Command::Skill(SkillCmd::Show { name }) => (m::SKILL_SHOW, json!({"name": name})),
        Command::Skill(SkillCmd::Rollback { name }) => (m::SKILL_ROLLBACK, json!({"name": name})),
        Command::Skill(SkillCmd::Reload) => (m::SKILL_RELOAD, json!({})),

        Command::Approvals => (m::APPROVALS, json!({})),
        Command::Approve { id, always, effect } => (
            m::APPROVE,
            json!({"id": id, "always": always, "effect": effect}),
        ),
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
        Command::Backup { push, full, media } => (
            m::BACKUP,
            json!({"push": push, "full": *full || *push, "media": media}),
        ),
        Command::Export { what, id } => (m::EXPORT, json!({"what": what, "id": id})),
        Command::Store(StoreCmd::Rebuild) => (m::STORE_REBUILD, json!({})),
        Command::Session(SessionCmd::Fork { session, title }) => {
            (m::SESSION_FORK, json!({"session": session, "title": title}))
        }
        Command::Session(SessionCmd::Rewind { turns, session }) => (
            m::SESSION_REWIND,
            json!({"session": session, "turns": turns}),
        ),

        other => {
            return Err(CliError::Usage(format!("commande non routée : {other:?}")));
        }
    })
}

/// `penelope upgrade` : par le daemon s'il tourne (il redémarre ensuite), sinon ici.
async fn upgrade(cli: &Cli) -> CliResult<()> {
    let (method, params) = route(&cli.command)?;
    let socket = socket_path(cli.home.clone())?;
    let (value, offline) = match call(&socket, method, params.clone()).await {
        Ok(v) => (v, false),
        Err(CliError::DaemonUnreachable(_)) => (upgrade_offline(cli, &params).await?, true),
        Err(e) => return Err(e),
    };
    if cli.json {
        output::print(&value, true);
        return Ok(());
    }
    println!("{}", penelope_daemon::upgrade::render(&value));
    let changed = value["installed"].is_string() || value["rolled_back"].as_bool() == Some(true);
    if changed && offline {
        println!("Daemon arrêté : `penelope start` pour démarrer la nouvelle version.");
    } else if changed {
        println!("Le daemon redémarre : `penelope status` dans quelques secondes.");
    }
    Ok(())
}

async fn upgrade_offline(cli: &Cli, p: &Value) -> CliResult<Value> {
    use penelope_daemon::upgrade as up;
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    // Sans daemon, la configuration est lue sur disque (clé minisign, adresse des releases).
    let cfg = std::fs::read_to_string(dirs.config_file())
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(cfg, _)| cfg)
        .unwrap_or_default();
    let source = up::Source::from_config(&cfg);
    if p["check"].as_bool().unwrap_or(false) {
        return up::check(&source).await.map_err(CliError::Io);
    }
    let state = dirs.state();
    if p["switch"].as_bool() == Some(true) {
        let install_dir = dirs.expand(&cfg.upgrade.install_dir);
        let current = up::running_binary().map_err(CliError::Io)?;
        return up::switch_to_releases(up::Switch {
            source: &source,
            tag: p["tag"].as_str(),
            current: &current,
            install_dir: &install_dir,
            state_dir: &state,
            now: chrono::Utc::now().to_rfc3339(),
            codesign: up::codesign_of(&cfg),
            host: &up::SystemHost,
        })
        .await
        .map_err(CliError::Io);
    }
    let binary = up::installed_binary().map_err(CliError::Usage)?;
    if p["rollback"].as_bool().unwrap_or(false) {
        return up::manual_rollback(&binary, &state).map_err(CliError::Io);
    }
    up::install(up::Install {
        source: &source,
        tag: p["tag"].as_str(),
        force: p["force"].as_bool().unwrap_or(false),
        binary: &binary,
        state_dir: &state,
        now: chrono::Utc::now().to_rfc3339(),
        codesign: up::codesign_of(&cfg),
    })
    .await
    .map_err(CliError::Io)
}

/// Suite d'évaluation depuis les sources : `cargo test` avec le filtre de la suite.
async fn eval_local(suite: &str) -> CliResult<()> {
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

/// Restauration hors ligne : refusée daemon en marche, base actuelle mise de côté.
async fn restore_offline(cli: &Cli, file: &std::path::Path) -> CliResult<()> {
    let socket = socket_path(cli.home.clone())?;
    if call(&socket, m::STATUS, json!({})).await.is_ok() {
        return Err(CliError::Usage(
            "le daemon tourne : `penelope stop` d'abord, puis relancer la restauration".into(),
        ));
    }
    let raw = std::fs::read(file).map_err(|e| CliError::Io(format!("{} : {e}", file.display())))?;
    if !raw.starts_with(b"SQLite format 3\0") {
        return Err(CliError::Validation(format!(
            "{} n'est pas une base SQLite (sauvegarde `penelope backup` attendue)",
            file.display()
        )));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let db = dirs.db_path();
    if db.exists() {
        let aside = dirs.data().join("backups").join(format!(
            "avant-restauration-{}.db",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        ));
        std::fs::create_dir_all(aside.parent().unwrap_or(std::path::Path::new(".")))
            .map_err(|e| CliError::Io(e.to_string()))?;
        std::fs::copy(&db, &aside).map_err(|e| CliError::Io(e.to_string()))?;
        println!("Base actuelle mise de côté : {}", aside.display());
    }
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", db.display()));
        let _ = std::fs::remove_file(side);
    }
    std::fs::write(&db, &raw).map_err(|e| CliError::Io(e.to_string()))?;
    println!(
        "✅ Restauré depuis {} : `penelope start` pour relancer.",
        file.display()
    );
    Ok(())
}

/// `penelope restore-all` : remonte une instance entière depuis une sauvegarde chiffrée
/// (issue #42). Se fait daemon arrêté, sur une machine où il n'y a encore rien.
async fn restore_all(cli: &Cli, source: Option<String>, dry_run: bool) -> CliResult<()> {
    let socket = socket_path(cli.home.clone())?;
    if !dry_run && call(&socket, m::STATUS, json!({})).await.is_ok() {
        return Err(CliError::Usage(
            "le daemon tourne : `penelope stop` d'abord, puis relancer la restauration".into(),
        ));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let work = dirs.data().join("backups").join("restore");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| CliError::Io(e.to_string()))?;

    // Source : une archive locale, ou un dépôt à cloner.
    let source = source.ok_or_else(|| {
        CliError::Usage(
            "donner l'archive `.tar.gz.enc` ou le dépôt privé des sauvegardes : \
             `penelope restore-all git@github.com:moi/penelope-backups.git`"
                .into(),
        )
    })?;
    let archive = if source.ends_with(".enc") {
        PathBuf::from(&source)
    } else {
        let repo = work.join("depot");
        println!("Clonage de {source}…");
        penelope_platform::process::git_sync_repo(&repo, &source)
            .map_err(|e| CliError::Io(e.to_string()))?;
        let mut found: Vec<PathBuf> = std::fs::read_dir(&repo)
            .map_err(|e| CliError::Io(e.to_string()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".tar.gz.enc"))
            .collect();
        found.sort();
        found.pop().ok_or_else(|| {
            CliError::Validation(format!("aucune sauvegarde chiffrée dans {source}"))
        })?
    };
    if !archive.is_file() {
        return Err(CliError::Validation(format!(
            "{} introuvable",
            archive.display()
        )));
    }

    // Phrase de passe : demandée à l'invite, jamais en argument.
    eprint!("Phrase de passe de la sauvegarde : ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut pass = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut pass)
        .map_err(|e| CliError::Io(e.to_string()))?;
    let pass = pass.trim().to_string();

    let tar = work.join("sauvegarde.tar.gz");
    penelope_platform::archive::open(&archive, &tar, &pass)
        .map_err(|e| CliError::Validation(e.to_string()))?;
    penelope_platform::process::extract_tar_gz(&tar, &work)
        .map_err(|e| CliError::Io(e.to_string()))?;
    let root = work.join("penelope");
    let manifest: Value = std::fs::read_to_string(root.join("MANIFEST.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);

    // Ce qui serait écrit, dans l'ordre.
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
    if root.join("penelope.db").is_file() {
        plan.push((root.join("penelope.db"), dirs.db_path()));
    }
    for (name, dst) in [
        ("vault", dirs.data().join("vault")),
        ("skills", dirs.data().join("skills")),
        ("workflows", dirs.data().join("workflows")),
        ("templates", dirs.data().join("templates")),
        ("mcp.d", dirs.data().join("mcp.d")),
        ("artifacts", dirs.data().join("artifacts")),
        ("media", dirs.data().join("media")),
        ("config.toml", dirs.config_file()),
    ] {
        let src = root.join(name);
        if src.exists() {
            plan.push((src, dst));
        }
    }

    println!(
        "Sauvegarde du {} (version {}) :",
        manifest["created_at"].as_str().unwrap_or("?"),
        manifest["version"].as_str().unwrap_or("?")
    );
    for (src, dst) in &plan {
        println!(
            "  {} → {}",
            src.file_name().unwrap_or_default().to_string_lossy(),
            dst.display()
        );
    }
    if dry_run {
        println!("\n(--dry-run : rien n'a été écrit)");
        return Ok(());
    }

    for (src, dst) in &plan {
        if dst.exists() {
            let aside = dst.with_extension(format!(
                "avant-restauration-{}",
                chrono::Utc::now().format("%Y%m%dT%H%M%S")
            ));
            let _ = std::fs::rename(dst, &aside);
            println!("Existant mis de côté : {}", aside.display());
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| CliError::Io(e.to_string()))?;
        }
        copy_tree(src, dst).map_err(|e| CliError::Io(e.to_string()))?;
    }
    // Journal WAL d'une base copiée : retiré, la base restaurée est cohérente.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!(
            "{}{suffix}",
            dirs.db_path().display()
        )));
    }

    let secrets: Vec<String> = manifest["secrets_expected"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    println!("\n✅ Fichiers restaurés. Il reste à faire, dans cet ordre :");
    println!("  1. `penelope install` puis `penelope start` (service).");
    if !secrets.is_empty() {
        println!("  2. Ressaisir les secrets, qui ne sont jamais sauvegardés :");
        for name in &secrets {
            println!("       penelope secret set {name}");
        }
    }
    println!("  3. `penelope doctor` : serveurs MCP à réautoriser, modèle de transcription à");
    println!("     télécharger, phrase de passe de sauvegarde à reposer.");
    Ok(())
}

/// Copie récursive, fichier ou répertoire.
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if src.is_file() {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        copy_tree(&e.path(), &dst.join(e.file_name()))?;
    }
    Ok(())
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

/// `penelope logs` : lit les journaux JSON du jour et de la veille, sans daemon, et garde
/// les lignes d'un tour ou d'une session (champ du span ou de l'événement, issue #103).
fn logs(cli: &Cli, turn: Option<&str>, session: Option<&str>, keep: usize) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dirs.logs())
        .map_err(|e| CliError::Io(format!("{} : {e}", dirs.logs().display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.starts_with("penelope-") && n.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    let recent = files.split_off(files.len().saturating_sub(2));
    let out = filter_log_lines(&recent, turn, session, keep);
    for l in &out {
        println!("{l}");
    }
    if out.is_empty() {
        eprintln!("aucune ligne ne correspond dans {}", dirs.logs().display());
    }
    Ok(())
}

/// Lignes JSON dont le span ou les champs portent ce tour ou cette session.
fn filter_log_lines(
    files: &[PathBuf],
    turn: Option<&str>,
    session: Option<&str>,
    keep: usize,
) -> Vec<String> {
    let matches = |v: &Value, key: &str, want: &str| {
        v["span"][key].as_str() == Some(want) || v["fields"][key].as_str() == Some(want)
    };
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        for line in text.lines() {
            let keep_it = match (turn, session) {
                (None, None) => true,
                _ => match serde_json::from_str::<Value>(line) {
                    Ok(v) => {
                        turn.is_some_and(|t| matches(&v, "turn", t))
                            || session.is_some_and(|s| matches(&v, "session", s))
                    }
                    Err(_) => false,
                },
            };
            if keep_it {
                out.push(line.to_string());
            }
        }
    }
    let skip = out.len().saturating_sub(keep.max(1));
    out.split_off(skip)
}

/// `doctor` en deux temps (issue #99) : ce qui se vérifie sans le daemon d'abord, puis
/// ses propres contrôles. Un daemon muet ou absent est un contrôle en échec, en tête du
/// rapport, pas une commande qui pend.
async fn doctor(cli: &Cli) -> CliResult<()> {
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
        print!("{}", penelope_daemon::doctor::render(&checks));
    }
    if checks.iter().any(|c| !c.ok && c.severity == "error") {
        return Err(CliError::Validation(
            "des contrôles critiques sont en échec".into(),
        ));
    }
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
    use penelope_daemon::upgrade::{self, Boot};
    // Nouveau binaire à l'essai : ce démarrage est compté avant d'ouvrir quoi que ce soit,
    // pour qu'un plantage plus loin mène aussi au retour arrière.
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    match upgrade::on_boot_now(&dirs.state(), penelope_daemon::VERSION) {
        Boot::RolledBack { from, to } => {
            // L'ancien binaire est au même chemin : `KeepAlive` le relance (issue #36).
            return Err(CliError::Io(format!(
                "la version {from} n'a pas confirmé son démarrage : binaire {to} remis en \
                 place, le service repart avec lui"
            )));
        }
        Boot::Trial { attempt } => {
            eprintln!(
                "mise à jour {} à l'essai (démarrage {attempt})",
                penelope_daemon::VERSION
            );
            upgrade::arm_watchdog(upgrade::WATCHDOG);
        }
        Boot::Normal => {}
    }
    let d = penelope_daemon::Daemon::new(cli.home.clone())
        .await
        .map_err(|e| CliError::Io(e.to_string()))?;
    let cfg = d.services.config.config();
    // Sous launchd, stderr est `daemon.err.log`, jamais tourné : le JSON à rétention suffit,
    // `penelope logs` le relit. Une panique y reste visible, elle passe par le crochet de
    // panique et non par `tracing` (issue #103).
    let service = std::env::var_os("PENELOPE_SERVICE").is_some_and(|v| v == "1");
    penelope_observe::init(
        &d.services.platform.dirs.logs(),
        &cfg.observability.log_level,
        cfg.observability.log_retention_days,
        !service,
    );
    tracing::info!(version = penelope_daemon::VERSION, "Pénélope démarre");
    std::sync::Arc::new(d)
        .run()
        .await
        .map_err(|e| CliError::Io(e.to_string()))
}

/// `penelope chat` : un message, ou une conversation interactive.
/// `penelope onboard` : une question à la fois, réponse vide pour passer, `q` pour
/// reprendre plus tard ; le récapitulatif est validé avant écriture.
async fn onboard(cli: &Cli, part: Option<String>) -> CliResult<()> {
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let socket = socket_path(cli.home.clone())?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut read = async || -> CliResult<Option<String>> {
        let _ = std::io::stdout().flush();
        lines
            .next_line()
            .await
            .map_err(|e| CliError::Io(e.to_string()))
    };
    loop {
        let v = call(&socket, m::ONBOARD_NEXT, json!({"part": part})).await?;
        if v["done"].as_bool() == Some(true) {
            println!("\n{}", v["text"].as_str().unwrap_or_default());
            print!("Écrire dans le profil et la mémoire ? [o/N] ");
            let ok = read()
                .await?
                .is_some_and(|l| matches!(l.trim(), "o" | "O" | "oui" | "y"));
            if ok {
                let w = call(&socket, m::ONBOARD_WRITE, json!({"rel": v["rel"]})).await?;
                println!(
                    "Enregistré : {} ajout(s), {} remplacement(s).",
                    w["added"], w["replaced"]
                );
            } else {
                println!("Rien n'est écrit.");
            }
            return Ok(());
        }
        let q = &v["question"];
        println!(
            "\n[{}/{}] {}",
            q["position"],
            q["total"],
            q["text"].as_str().unwrap_or_default()
        );
        if let Some(h) = q["hint"].as_str().filter(|h| !h.is_empty()) {
            println!("  {h}");
        }
        if let Some(choices) = q["choices"].as_array().filter(|c| !c.is_empty()) {
            let list: Vec<&str> = choices.iter().filter_map(|c| c.as_str()).collect();
            println!("  Choix : {}", list.join(", "));
        }
        let multi = q["list"].as_bool() == Some(true);
        if multi {
            println!("  Une réponse par ligne, ligne vide pour finir.");
        }
        print!("› ");
        let mut answer = String::new();
        loop {
            let Some(line) = read().await? else {
                return Ok(());
            };
            if line.trim() == "q" && answer.is_empty() {
                println!("Accueil en pause : `penelope onboard` reprend ici.");
                return Ok(());
            }
            if line.trim().is_empty() {
                break;
            }
            answer.push_str(line.trim());
            answer.push('\n');
            if !multi {
                break;
            }
            print!("› ");
        }
        let answer = answer.trim();
        let params = json!({
            "rel": q["rel"],
            "n": q["n"],
            "answer": (!answer.is_empty()).then_some(answer),
        });
        if let Err(e) = call(&socket, m::ONBOARD_ANSWER, params).await {
            println!("⚠️ {e}");
        }
    }
}

/// Connexion d'un fournisseur à compte (issue #142) : Pénélope demande un code
/// d'appareil, l'affiche avec l'adresse à ouvrir, puis attend que le propriétaire l'ait
/// saisi. Le code ne vaut que quinze minutes.
async fn model_auth(cli: &Cli, provider: String) -> CliResult<()> {
    let socket = socket_path(cli.home.clone())?;
    let start = call(
        &socket,
        m::MODEL_AUTH,
        json!({"provider": provider, "action": "start"}),
    )
    .await?;
    println!(
        "🔐 Ouvrir {}
   et saisir le code : {}
",
        start["url"].as_str().unwrap_or_default(),
        start["user_code"].as_str().unwrap_or_default()
    );
    println!("J'attends la validation (quinze minutes)…");
    // L'attente dure autant que le propriétaire : pas de délai côté client.
    crate::client::set_timeout(Some(0));
    let done = call(
        &socket,
        m::MODEL_AUTH,
        json!({"provider": provider, "action": "wait"}),
    )
    .await?;
    println!(
        "✅ Connecté : plan {}, compte {}",
        done["plan"].as_str().unwrap_or("?"),
        done["account"].as_str().unwrap_or("?")
    );
    println!(
        "Le fournisseur `{provider}` est actif. Pour lui donner un alias :\n  \
         penelope model set code codex:gpt-6-astra"
    );
    Ok(())
}

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
    let stream = crate::client::call_stream(
        socket,
        m::CHAT_STREAM,
        json!({"text": text, "session": session}),
        &mut on_event,
    );
    // Ctrl-C arrête le tour côté daemon, pas seulement l'affichage (issue #100) ; un
    // second Ctrl-C quitte sans attendre la confirmation.
    let result = tokio::select! {
        r = stream => r?,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\n⏹ arrêt demandé");
            tokio::select! {
                _ = call(socket, m::CHAT_STOP, json!({"session": session})) => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return Err(CliError::Interrupted);
        }
    };
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
            // « Toujours » sur une commande composée n'écrit aucune règle : le dire
            // avant le clic, comme la carte Telegram (issue #141).
            let no_rule = penelope_daemon::agent::always_creates_no_rule(
                detail["subject"].as_str().unwrap_or_default(),
                detail["payload"].get("arguments"),
            );
            if no_rule {
                println!(
                    "ℹ️  commande composée : « toujours » l'autorise cette fois, sans créer \
                     de règle."
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
    let mut body = serde_json::to_string(&crate::client::request(socket, m::TAIL, json!({})))
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
}
