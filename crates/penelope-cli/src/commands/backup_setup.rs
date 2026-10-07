//! `penelope backup setup` et `penelope backup kit` (#328) : la mise en place de la
//! sauvegarde et son kit de secours. Hors daemon quand il ne répond pas ; par lui sinon,
//! parce qu'en SSH le trousseau n'est ouvert qu'à lui (#252).

use super::console::{Console, Terminal};
use super::offline::store_secret;
use super::*;
use penelope_ops::backup::{PASSPHRASE_SECRET, kit, provider};

pub(super) async fn run(cli: &Cli, cmd: &BackupCmd) -> CliResult<()> {
    run_with(cli, cmd, &mut Terminal).await
}

/// [`run`] sur une invite donnée : le terminal, ou un double scripté dans les tests.
pub(super) async fn run_with(cli: &Cli, cmd: &BackupCmd, io: &mut dyn Console) -> CliResult<()> {
    match cmd {
        BackupCmd::Setup {
            provider,
            own_passphrase,
        } => setup(cli, io, provider.clone(), *own_passphrase).await,
        BackupCmd::Kit => {
            eprintln!(
                "Le kit contient la phrase de passe des sauvegardes et les clés du fournisseur."
            );
            if io.ask("Taper « afficher » pour l'afficher", "")? != "afficher" {
                println!("Kit non affiché.");
                return Ok(());
            }
            println!("{}", kit_text(cli, io).await?);
            Ok(())
        }
    }
}

fn dirs_of(cli: &Cli) -> CliResult<Box<dyn penelope_platform::Directories>> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    dirs.ensure_all().map_err(|e| CliError::Io(e.to_string()))?;
    Ok(dirs)
}

fn local_config(dirs: &dyn penelope_platform::Directories) -> penelope_kernel::config::Config {
    std::fs::read_to_string(dirs.config_file())
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(c, _)| c)
        .unwrap_or_default()
}

/// Le kit : rendu par le daemon s'il répond, sinon depuis la configuration et le magasin
/// de cette machine.
async fn kit_text(cli: &Cli, io: &dyn Console) -> CliResult<String> {
    let dirs = dirs_of(cli)?;
    match call(&dirs.socket_path(), m::BACKUP, json!({"kit": true})).await {
        Ok(v) => return Ok(v["text"].as_str().unwrap_or_default().to_string()),
        Err(CliError::DaemonUnreachable(_)) => {}
        Err(e) => return Err(e),
    }
    let cfg = local_config(dirs.as_ref());
    let home = penelope_platform::dirs::home_dir();
    let target = provider::Target::resolve(&cfg.backup, dirs.as_ref(), home.as_deref())
        .map_err(|e| CliError::Validation(e.to_string()))?;
    let store = io.store(dirs.as_ref())?;
    let pass = store
        .get(PASSPHRASE_SECRET)
        .map_err(|e| CliError::Io(e.to_string()))?
        .ok_or_else(|| {
            CliError::Validation("aucune phrase de passe : `penelope backup setup`".into())
        })?;
    let keys = match &target {
        provider::Target::S3(c) => store
            .expand(&c.access_key_id)
            .ok()
            .zip(store.expand(&c.secret_access_key).ok()),
        _ => None,
    };
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    Ok(kit::render(
        &cfg.backup,
        &target,
        &pass,
        keys.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
        &day,
    ))
}

/// Le nom du secret derrière `${SECRET:nom}`, sinon `default`.
pub(super) fn secret_name(raw: &str, default: &str) -> String {
    penelope_platform::secrets::placeholders(raw)
        .into_iter()
        .find_map(|p| {
            p.strip_prefix("SECRET:")
                .or_else(|| p.strip_prefix("KEYCHAIN:"))
                .map(String::from)
        })
        .unwrap_or_else(|| default.to_string())
}

/// `penelope backup setup` : fournisseur, essai, phrase de passe, configuration, kit.
async fn setup(
    cli: &Cli,
    io: &mut dyn Console,
    provider_arg: Option<String>,
    own: bool,
) -> CliResult<()> {
    let dirs = dirs_of(cli)?;
    let socket = dirs.socket_path();
    if !dirs.config_file().exists() {
        return Err(CliError::Usage(format!(
            "aucune configuration dans {} : installer Pénélope d'abord (`penelope onboard`), \
             ou restaurer une sauvegarde (`penelope restore`)",
            dirs.config_file().display()
        )));
    }
    let cfg = local_config(dirs.as_ref());
    let home = penelope_platform::dirs::home_dir();
    let mut backup = cfg.backup.clone();
    backup.provider = match provider_arg {
        Some(p) => p,
        None => io.ask(
            "Fournisseur (s3, dir, icloud)",
            cfg.backup.effective_provider().unwrap_or("s3"),
        )?,
    };
    let mut s3_keys: Option<(String, String)> = None;
    match backup.provider.as_str() {
        "s3" => {
            let s3 = &mut backup.s3;
            let or = |v: &str, d: &str| {
                if v.trim().is_empty() {
                    d.to_string()
                } else {
                    v.to_string()
                }
            };
            s3.endpoint = io.ask("Adresse S3", &or(&s3.endpoint, provider::SCALEWAY_ENDPOINT))?;
            let region = if s3.region == "us-east-1" && s3.endpoint.contains("scw.cloud") {
                provider::SCALEWAY_REGION.to_string()
            } else {
                s3.region.clone()
            };
            s3.region = io.ask("Région", &region)?;
            s3.bucket = io.ask(
                "Bucket, créé d'avance et réservé aux sauvegardes",
                &s3.bucket,
            )?;
            s3.prefix = io.ask("Préfixe", &s3.prefix)?;
            let id = io.secret("Identifiant de la clé d'accès S3 (rien ne s'affiche) : ")?;
            let key = io.secret("Clé secrète S3 (rien ne s'affiche) : ")?;
            s3_keys = Some((id, key));
        }
        "dir" => {
            backup.dir = io.ask(
                "Dossier des sauvegardes (chemin absolu, `~/` admis)",
                &backup.dir,
            )?
        }
        "icloud" => {
            let sub = io.ask(
                "Sous-dossier d'iCloud Drive",
                match backup.dir.trim() {
                    "" => provider::ICLOUD_DEFAULT_DIR,
                    d => d,
                },
            )?;
            backup.dir = if sub == provider::ICLOUD_DEFAULT_DIR {
                String::new()
            } else {
                sub
            };
        }
        other => {
            return Err(CliError::Usage(format!(
                "fournisseur `{other}` inconnu : s3, dir ou icloud (GitHub n'est plus une \
                 destination de sauvegarde)"
            )));
        }
    }
    let mut next = cfg.clone();
    next.backup = backup.clone();
    next.validate()
        .map_err(|e| CliError::Validation(e.to_string()))?;
    let target = provider::Target::resolve(&backup, dirs.as_ref(), home.as_deref())
        .map_err(|e| CliError::Validation(e.to_string()))?;
    let creds = s3_keys
        .as_ref()
        .map(|(a, k)| penelope_ops::backup::sigv4::Credentials {
            access_key: a.clone(),
            secret_key: k.clone(),
        });
    eprintln!("Essai du fournisseur…");
    let at = provider::probe(&target, creds)
        .await
        .map_err(|e| CliError::Validation(format!("fournisseur injoignable : {e}")))?;
    eprintln!("✅ {at} : écriture et effacement réussis.");

    if let Some((id, key)) = &s3_keys {
        for (raw, default, value) in [
            (&backup.s3.access_key_id, "s3_access_key_id", id),
            (&backup.s3.secret_access_key, "s3_secret_access_key", key),
        ] {
            store_secret(&socket, &secret_name(raw, default), value, || {
                io.store(dirs.as_ref())
            })
            .await?;
        }
    }

    // La phrase de passe : gardée si elle existe et que le propriétaire le veut.
    let names: Vec<String> = match call(&socket, m::SECRET_LIST, json!({})).await {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => io
            .store(dirs.as_ref())
            .and_then(|s| s.list().map_err(|e| CliError::Io(e.to_string())))
            .unwrap_or_default(),
    };
    let exists = names.iter().any(|n| n == PASSPHRASE_SECRET);
    let keep = exists && io.ask("Une phrase de passe existe : la garder ? (o/n)", "o")? == "o";
    let generated = !keep && !own;
    if !keep {
        let pass = if own {
            let p = io.secret("Phrase de passe (16 caractères au moins, rien ne s'affiche) : ")?;
            if p.chars().count() < 16 {
                return Err(CliError::Usage(
                    "phrase de passe trop courte : 16 caractères au moins".into(),
                ));
            }
            if io.secret("La même, encore : ")? != p {
                return Err(CliError::Usage(
                    "les deux saisies diffèrent : rien n'a été enregistré".into(),
                ));
            }
            p
        } else {
            kit::generate_passphrase().map_err(|e| CliError::Io(e.to_string()))?
        };
        if exists {
            eprintln!(
                "⚠️ Les archives déjà faites restent lisibles avec l'ancienne phrase seulement."
            );
        }
        store_secret(&socket, PASSPHRASE_SECRET, &pass, || {
            io.store(dirs.as_ref())
        })
        .await?;
    }

    // La configuration : écrite d'un bloc (les clés S3 ne se valident qu'ensemble), puis
    // relue par le daemon s'il tourne.
    let clock: penelope_kernel::clock::SharedClock =
        std::sync::Arc::new(penelope_kernel::clock::SystemClock);
    let owner = cfg.owner.telegram_user_id;
    penelope_kernel::config::ConfigStore::load_or_create(dirs.config_file(), None, clock, owner)
        .and_then(|cs| {
            cs.mutate("backup setup", move |c| {
                c.backup = backup;
                Ok(vec!["backup".into()])
            })
        })
        .map_err(|e| CliError::Validation(e.to_string()))?;
    let _ = call(&socket, m::CONFIG_RELOAD, json!({})).await;

    let text = kit_text(cli, io).await?;
    println!("\nVoici le kit de secours. Il ne sera plus affiché sans `penelope backup kit`.\n");
    println!("{text}");
    confirm_kit(io, &text, generated)?;
    println!(
        "✅ Sauvegarde en place, vers {at}{}. Première sauvegarde : `penelope backup`.",
        match cfg.backup.cron.trim() {
            "" => String::new(),
            cron => format!(", chaque nuit (`{cron}`)"),
        }
    );
    Ok(())
}

/// La preuve que le kit est rangé : quatre mots de la phrase générée, ou « noté ».
fn confirm_kit(io: &mut dyn Console, text: &str, generated: bool) -> CliResult<()> {
    let pass = text
        .lines()
        .find_map(|l| l.strip_prefix("Phrase de passe : "))
        .unwrap_or_default()
        .to_string();
    if !generated {
        while io.ask("Taper « noté » une fois le kit rangé", "")? != "noté" {}
        return Ok(());
    }
    for _ in 0..3 {
        let positions = kit::confirm_positions(kit::words(&pass).len());
        let mut typed = Vec::new();
        for p in &positions {
            typed.push(io.ask(&format!("Mot n° {p} de la phrase de passe"), "")?);
        }
        if kit::confirmed(&pass, &positions, &typed) {
            println!("Kit confirmé.");
            return Ok(());
        }
        eprintln!("Ce ne sont pas les bons mots : relire le kit.");
    }
    Err(CliError::Validation(
        "kit non confirmé : la phrase de passe est enregistrée, `penelope backup kit` la \
         réaffiche"
            .into(),
    ))
}
