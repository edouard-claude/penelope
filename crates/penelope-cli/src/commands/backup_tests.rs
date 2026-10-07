//! `backup setup`, `backup kit` et `restore` sur une invite scriptée (#328, #329) : un
//! home de test, aucun daemon, un magasin en mémoire, jamais le vrai trousseau.

use super::console::Console;
use super::*;
use penelope_ops::backup::PASSPHRASE_SECRET;
use penelope_platform::{MemorySecretStore, SecretStore};
use std::sync::Arc;

/// Le magasin partagé entre le test et la commande.
#[derive(Clone, Default)]
struct Shared(Arc<MemorySecretStore>);

impl SecretStore for Shared {
    fn backend(&self) -> String {
        self.0.backend()
    }
    fn get(&self, name: &str) -> penelope_platform::Result<Option<String>> {
        self.0.get(name)
    }
    fn set(&self, name: &str, value: &str) -> penelope_platform::Result<()> {
        self.0.set(name, value)
    }
    fn delete(&self, name: &str) -> penelope_platform::Result<()> {
        self.0.delete(name)
    }
    fn list(&self) -> penelope_platform::Result<Vec<String>> {
        self.0.list()
    }
}

/// Réponses par début de question ; les mots de confirmation sont lus dans la phrase de
/// passe rangée ; les valeurs masquées, dans l'ordre.
struct Scripted {
    answers: Vec<(&'static str, String)>,
    secrets: Vec<String>,
    store: Shared,
    asked: Vec<String>,
}

impl Scripted {
    fn new(store: &Shared, answers: &[(&'static str, &str)], secrets: &[&str]) -> Self {
        Scripted {
            answers: answers.iter().map(|(q, a)| (*q, a.to_string())).collect(),
            secrets: secrets.iter().rev().map(|s| s.to_string()).collect(),
            store: store.clone(),
            asked: Vec::new(),
        }
    }
}

impl Console for Scripted {
    fn ask(&mut self, question: &str, default: &str) -> CliResult<String> {
        self.asked.push(question.to_string());
        if let Some(n) = question
            .strip_prefix("Mot n° ")
            .and_then(|r| r.split(' ').next())
            .and_then(|n| n.parse::<usize>().ok())
        {
            let pass = self.store.get(PASSPHRASE_SECRET).unwrap().unwrap();
            return Ok(penelope_ops::backup::kit::words(&pass)[n - 1].to_string());
        }
        Ok(self
            .answers
            .iter()
            .find(|(q, _)| question.starts_with(q))
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| default.to_string()))
    }
    fn secret(&mut self, _prompt: &str) -> CliResult<String> {
        self.secrets
            .pop()
            .ok_or_else(|| CliError::Usage("plus de valeur scriptée".into()))
    }
    fn store(&self, _dirs: &dyn penelope_platform::Directories) -> CliResult<Box<dyn SecretStore>> {
        Ok(Box::new(self.store.clone()))
    }
}

fn cli(home: &std::path::Path, args: &[&str]) -> Cli {
    let mut v = vec!["penelope", "--home", home.to_str().unwrap()];
    v.extend_from_slice(args);
    Cli::try_parse_from(v).unwrap()
}

fn config(home: &std::path::Path) -> penelope_kernel::config::Config {
    let dirs = penelope_platform::RootedDirs::new(home);
    let raw = std::fs::read_to_string(penelope_platform::Directories::config_file(&dirs)).unwrap();
    penelope_kernel::config::Config::parse(&raw).unwrap().0
}

/// #328 : la mise en place essaie le dossier, génère la phrase de passe, écrit la
/// configuration d'un bloc et fait confirmer le kit par quatre mots ; une seconde mise
/// en place garde ou remplace la phrase ; `backup kit` ne s'affiche que sur demande.
#[tokio::test]
async fn setup_writes_the_provider_and_a_confirmed_passphrase() {
    let home = tempfile::tempdir().unwrap();
    let nas = home.path().join("nas");
    let store = Shared::default();
    let c = cli(home.path(), &["backup", "setup", "--provider", "dir"]);
    let Command::Backup { cmd: Some(cmd), .. } = &c.command else {
        panic!()
    };
    let mut io = Scripted::new(&store, &[("Dossier", nas.to_str().unwrap())], &[]);
    let e = backup_setup::run_with(&c, cmd, &mut io).await.unwrap_err();
    assert!(e.to_string().contains("penelope onboard"), "{e}");
    let dirs = penelope_platform::RootedDirs::new(home.path());
    penelope_platform::Directories::ensure_all(&dirs).unwrap();
    std::fs::write(
        penelope_platform::Directories::config_file(&dirs),
        "[owner]\ntelegram_user_id = 42\n",
    )
    .unwrap();
    backup_setup::run_with(&c, cmd, &mut io).await.unwrap();
    let pass = store.get(PASSPHRASE_SECRET).unwrap().unwrap();
    assert_eq!(penelope_ops::backup::kit::words(&pass).len(), 6, "{pass}");
    let confirm = io.asked.iter().filter(|q| q.starts_with("Mot n° ")).count();
    assert_eq!(confirm, 4, "{:?}", io.asked);
    let cfg = config(home.path());
    assert_eq!(cfg.backup.provider, "dir");
    assert_eq!(cfg.backup.dir, nas.display().to_string());
    assert_eq!(
        std::fs::read_dir(&nas).unwrap().count(),
        0,
        "l'essai est effacé"
    );

    // Une seconde fois : la phrase est remplacée par une phrase saisie.
    let c = cli(
        home.path(),
        &["backup", "setup", "--provider", "dir", "--own-passphrase"],
    );
    let Command::Backup { cmd: Some(cmd), .. } = &c.command else {
        panic!()
    };
    let own = "une phrase choisie assez longue";
    let mut io = Scripted::new(
        &store,
        &[
            ("Dossier", nas.to_str().unwrap()),
            ("Une phrase", "n"),
            ("Taper", "noté"),
        ],
        &[own, own],
    );
    backup_setup::run_with(&c, cmd, &mut io).await.unwrap();
    assert_eq!(store.get(PASSPHRASE_SECRET).unwrap().as_deref(), Some(own));
    let mut io = Scripted::new(&store, &[("Une phrase", "n")], &["courte", "courte"]);
    let e = backup_setup::run_with(&c, cmd, &mut io).await.unwrap_err();
    assert!(e.to_string().contains("trop courte"), "{e}");
    assert_eq!(store.get(PASSPHRASE_SECRET).unwrap().as_deref(), Some(own));

    let c = cli(home.path(), &["backup", "kit"]);
    let Command::Backup { cmd: Some(cmd), .. } = &c.command else {
        panic!()
    };
    backup_setup::run_with(
        &c,
        cmd,
        &mut Scripted::new(&store, &[("Taper", "afficher")], &[]),
    )
    .await
    .unwrap();
    backup_setup::run_with(
        &c,
        cmd,
        &mut Scripted::new(&store, &[("Taper", "non")], &[]),
    )
    .await
    .unwrap();

    let c = cli(home.path(), &["backup", "setup", "--provider", "github"]);
    let Command::Backup { cmd: Some(cmd), .. } = &c.command else {
        panic!()
    };
    let e = backup_setup::run_with(&c, cmd, &mut Scripted::new(&store, &[], &[]))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("GitHub n'est plus"), "{e}");
}

/// #329 : `penelope restore <dossier> --no-start` liste, simule puis restaure dans un home
/// vierge : la configuration et les secrets reviennent, rien d'autre n'est demandé.
#[tokio::test]
async fn restore_from_a_folder_brings_the_files_and_the_secrets_back() {
    let origin = tempfile::tempdir().unwrap();
    let nas = origin.path().join("nas");
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::new(1_789_516_800_000));
    let s = penelope_app::services::Services::for_tests(origin.path().join("home"), clock)
        .await
        .unwrap();
    let d = nas.display().to_string();
    s.publish_config("test", move |c| {
        c.backup.provider = "dir".into();
        c.backup.dir = d;
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    s.platform.secrets.set("cle_de_test", "valeur").unwrap();
    penelope_ops::backup::run(&s, true, Some(false))
        .await
        .unwrap();

    let fresh = tempfile::tempdir().unwrap();
    let store = Shared::default();
    let run = |args: &[&str], secrets: &'static [&'static str]| {
        let c = cli(fresh.path(), args);
        let store = store.clone();
        async move {
            let Command::Restore {
                source,
                dry_run,
                list,
                archive,
                no_start,
                ..
            } = &c.command
            else {
                panic!()
            };
            let a = restore::RestoreArgs {
                source: source.clone(),
                dry_run: *dry_run,
                list: *list,
                archive: archive.clone(),
                no_start: *no_start,
                ..Default::default()
            };
            restore::restore_with(&c, a, &mut Scripted::new(&store, &[], secrets)).await
        }
    };
    let nas_s = nas.to_str().unwrap();
    run(&["restore", nas_s, "--list"], &[]).await.unwrap();
    run(&["restore", nas_s, "--dry-run"], &["phrase"])
        .await
        .unwrap();
    assert!(
        store.get("cle_de_test").unwrap().is_none(),
        "--dry-run n'écrit rien"
    );
    let e = run(&["restore", nas_s, "--no-start"], &["autre"])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("phrase de passe incorrecte"), "{e}");
    let e = run(
        &["restore", nas_s, "--archive", "absente", "--no-start"],
        &["phrase"],
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("absente"), "{e}");
    run(&["restore", nas_s, "--no-start"], &["phrase"])
        .await
        .unwrap();
    assert_eq!(store.get("cle_de_test").unwrap().as_deref(), Some("valeur"));
    assert_eq!(
        store.get(PASSPHRASE_SECRET).unwrap().as_deref(),
        Some("phrase")
    );
    assert_eq!(config(fresh.path()).backup.provider, "dir");
}
