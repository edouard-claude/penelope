//! La grammaire de la ligne de commande : `Cli`, `Command` et ses sous-commandes, lus
//! par clap.

use super::*;
use clap::{Parser, Subcommand};

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

    /// Jobs d'outils en cours : outil, session, âge. Ce qui tourne hors d'un tour.
    Jobs {
        /// Jobs terminés aussi, pas seulement ceux qui tournent.
        #[arg(long)]
        all: bool,
    },

    /// Demandes d'approbation en attente ; `stats` : la mesure préalable au juge (#203).
    Approvals {
        #[command(subcommand)]
        cmd: Option<ApprovalsCmd>,
    },
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
        /// Regroupement : session, turn (requête), model, day, role, provider, upstream,
        /// run, miss (ratés de cache par cause).
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
    /// Audit d'un tour : ce que le modèle avait sous les yeux.
    #[command(subcommand)]
    Audit(AuditCmd),
    /// L'historique de la conversation contre le journal d'événements.
    #[command(subcommand)]
    History(HistoryCmd),
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
pub enum AuditCmd {
    /// Reconstitue une requête : prompt système, messages, empreintes (issue #205).
    /// Sans `--turn`, le dernier tour de la session.
    Show {
        #[arg(long)]
        turn: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum HistoryCmd {
    /// Dérive chaque session du journal et la compare à ses caches (messages, contextes,
    /// résumés, préfixe scellé) ; code de sortie non nul à la première divergence.
    Verify {
        #[arg(long)]
        session: Option<String>,
    },
    /// Efface les lignes de cache non scellées et les réécrit depuis le journal.
    Reindex {
        #[arg(long)]
        session: Option<String>,
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
    /// Installe des skills depuis un dépôt GitHub : `anthropics/skills:docx,pdf`,
    /// `anthropics/skills@v2`, ou le dépôt entier.
    Install {
        /// `proprietaire/depot[@revision][:skill,skill]`.
        source: String,
        /// Remplace une skill du même nom déjà installée.
        #[arg(long)]
        force: bool,
    },
}
