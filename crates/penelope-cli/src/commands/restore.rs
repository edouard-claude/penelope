//! `penelope restore` (#329), daemon arrêté : une instance entière depuis l'archive
//! chiffrée du fournisseur (S3, dossier, iCloud Drive ou fichier), puis le service
//! réinstallé, démarré, et `doctor`. `restore-all` en est l'alias ; un fichier `.db` donne
//! l'ancienne restauration de la seule base.

use super::*;

/// Restauration hors ligne : refusée daemon en marche, base actuelle mise de côté.
pub(super) async fn restore_offline(cli: &Cli, file: &std::path::Path) -> CliResult<()> {
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

/// Les arguments de `penelope restore`.
#[derive(Debug, Clone, Default)]
pub(super) struct RestoreArgs {
    pub source: Option<String>,
    pub dry_run: bool,
    pub list: bool,
    pub archive: Option<String>,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub no_start: bool,
}

/// Variables d'environnement qui portent les clés S3 d'une restauration sur une machine
/// neuve, avant tout magasin de secrets.
pub const S3_ACCESS_KEY_ENV: &str = "PENELOPE_S3_ACCESS_KEY_ID";
pub const S3_SECRET_KEY_ENV: &str = "PENELOPE_S3_SECRET_ACCESS_KEY";

/// Ce que `s3` ou `s3://bucket/prefixe` désigne, complété par `[backup.s3]` de la
/// configuration locale et par les options de la ligne de commande (#289).
pub(super) fn resolve_s3(
    source: &str,
    local: &penelope_kernel::config::BackupS3,
    endpoint: Option<&str>,
    region: Option<&str>,
) -> Result<penelope_kernel::config::BackupS3, String> {
    let mut s3 = local.clone();
    if let Some(rest) = source.strip_prefix("s3://") {
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        s3.bucket = bucket.to_string();
        s3.prefix = prefix.to_string();
    } else if source != "s3" {
        return Err(format!("`{source}` n'est pas une source S3"));
    }
    if let Some(e) = endpoint {
        s3.endpoint = e.to_string();
    }
    if let Some(r) = region {
        s3.region = r.to_string();
    }
    if s3.endpoint.trim().is_empty() {
        return Err(
            "adresse S3 inconnue : `--endpoint https://…`, ou `backup.s3.endpoint` \
                    dans config.toml"
                .into(),
        );
    }
    if s3.bucket.trim().is_empty() {
        return Err(
            "bucket S3 inconnu : `s3://bucket/prefixe`, ou `backup.s3.bucket` dans config.toml"
                .into(),
        );
    }
    Ok(s3)
}

/// Les clés S3 : l'environnement d'abord, puis le magasin de secrets de la machine, sinon
/// l'invite (jamais un argument).
fn s3_credentials(
    dirs: &dyn penelope_platform::Directories,
    s3: &penelope_kernel::config::BackupS3,
) -> CliResult<penelope_ops::backup::sigv4::Credentials> {
    use penelope_ops::backup::sigv4::Credentials;
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    if let (Some(a), Some(k)) = (env(S3_ACCESS_KEY_ENV), env(S3_SECRET_KEY_ENV)) {
        return Ok(Credentials {
            access_key: a.trim().to_string(),
            secret_key: k.trim().to_string(),
        });
    }
    if let Ok(store) = penelope_platform::backend::secret_store(dirs)
        && let (Ok(a), Ok(k)) = (
            store.expand(&s3.access_key_id),
            store.expand(&s3.secret_access_key),
        )
        && !a.trim().is_empty()
        && !k.trim().is_empty()
    {
        return Ok(Credentials {
            access_key: a.trim().to_string(),
            secret_key: k.trim().to_string(),
        });
    }
    eprintln!(
        "Clés S3 absentes de l'environnement ({S3_ACCESS_KEY_ENV}, {S3_SECRET_KEY_ENV}) et du \
         magasin de secrets."
    );
    let access_key = penelope_platform::terminal::read_secret("Identifiant de la clé S3 : ")
        .map_err(|e| CliError::Io(e.to_string()))?;
    let secret_key = penelope_platform::terminal::read_secret("Clé secrète S3 : ")
        .map_err(|e| CliError::Io(e.to_string()))?;
    if access_key.trim().is_empty() || secret_key.trim().is_empty() {
        return Err(CliError::Usage("clés S3 vides".into()));
    }
    Ok(Credentials {
        access_key: access_key.trim().to_string(),
        secret_key: secret_key.trim().to_string(),
    })
}

/// Le config.toml local, ou les défauts s'il n'existe pas encore (machine neuve).
fn local_config(dirs: &dyn penelope_platform::Directories) -> penelope_kernel::config::Config {
    std::fs::read_to_string(dirs.config_file())
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(c, _)| c)
        .unwrap_or_default()
}

/// Télécharge l'archive choisie depuis le bucket (ou les liste), et rend son chemin.
async fn fetch_from_s3(
    dirs: &dyn penelope_platform::Directories,
    args: &RestoreArgs,
    source: &str,
    work: &std::path::Path,
) -> CliResult<Option<PathBuf>> {
    use penelope_ops::backup::s3;
    let s3cfg = resolve_s3(
        source,
        &local_config(dirs).backup.s3,
        args.endpoint.as_deref(),
        args.region.as_deref(),
    )
    .map_err(CliError::Usage)?;
    let creds = s3_credentials(dirs, &s3cfg)?;
    let client = s3::S3Client::new(
        &s3cfg.endpoint,
        &s3cfg.bucket,
        &s3cfg.region,
        s3cfg.path_style,
        creds,
    )
    .map_err(|e| CliError::Validation(e.to_string()))?;
    let prefix = s3::normalized_prefix(&s3cfg.prefix);
    let archives = s3::list_archives(&client, &prefix)
        .await
        .map_err(|e| CliError::Io(e.to_string()))?;
    if args.list {
        if archives.is_empty() {
            println!("Aucune sauvegarde sous {}/{prefix}", s3cfg.bucket);
        }
        for o in &archives {
            println!(
                "{}  {:>6} Mo  {}",
                o.last_modified,
                o.size / (1024 * 1024),
                o.key
            );
        }
        return Ok(None);
    }
    let chosen = match &args.archive {
        Some(k) => archives
            .iter()
            .find(|o| o.key == *k || o.key.ends_with(k.as_str()))
            .ok_or_else(|| {
                CliError::Validation(format!(
                    "archive `{k}` absente de {}/{prefix} ; `--list` donne celles qui existent",
                    s3cfg.bucket
                ))
            })?,
        None => archives.first().ok_or_else(|| {
            CliError::Validation(format!(
                "aucune sauvegarde chiffrée sous {}/{prefix}",
                s3cfg.bucket
            ))
        })?,
    };
    let name = chosen
        .key
        .rsplit('/')
        .next()
        .unwrap_or("sauvegarde.tar.gz.enc");
    let dest = work.join(name);
    println!(
        "Téléchargement de {} ({} Mo)…",
        chosen.key,
        chosen.size / (1024 * 1024)
    );
    client
        .get_to_file(&chosen.key, &dest)
        .await
        .map_err(|e| CliError::Io(e.to_string()))?;
    Ok(Some(dest))
}

/// Où lire l'archive.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Source {
    /// Une archive `.tar.gz.enc` locale.
    File(PathBuf),
    /// `s3`, ou `s3://bucket/prefixe`.
    S3(String),
    /// Un dossier de fournisseur (`dir`, ou iCloud Drive).
    Dir(PathBuf),
}

/// La source désignée par l'argument, sinon par `backup.provider` de la configuration
/// locale ; `None` : rien de configuré, la question se pose à l'invite.
pub(super) fn source_of(
    arg: Option<&str>,
    local: &penelope_kernel::config::Backup,
    dirs: &dyn penelope_platform::Directories,
    home: Option<&std::path::Path>,
) -> Result<Option<Source>, String> {
    use penelope_ops::backup::provider::Target;
    let from_target = |t: Target| match t {
        Target::S3(_) => Source::S3("s3".into()),
        Target::Dir { path, .. } => Source::Dir(path),
    };
    match arg {
        None => match local.effective_provider() {
            None => Ok(None),
            Some(_) => Target::resolve(local, dirs, home)
                .map(|t| Some(from_target(t)))
                .map_err(|e| e.to_string()),
        },
        Some(s) if s == "s3" || s.starts_with("s3://") => Ok(Some(Source::S3(s.into()))),
        Some(s) if s.ends_with(".enc") => Ok(Some(Source::File(PathBuf::from(s)))),
        Some(s) if s == "icloud" || s == "dir" => {
            let mut cfg = local.clone();
            cfg.provider = s.into();
            Target::resolve(&cfg, dirs, home)
                .map(|t| Some(from_target(t)))
                .map_err(|e| e.to_string())
        }
        Some(s) if std::path::Path::new(s).is_dir() => Ok(Some(Source::Dir(PathBuf::from(s)))),
        Some(s) => Err(format!(
            "`{s}` : une archive `.tar.gz.enc`, un dossier de sauvegardes, `s3`, \
             `s3://bucket/prefixe` ou `icloud` est attendu. GitHub n'est plus une destination \
             de sauvegarde (#327) : `git clone` l'ancien dépôt, puis donner l'archive voulue"
        )),
    }
}

/// Une ligne lue à l'invite, avec sa valeur par défaut.
fn ask(question: &str, default: &str) -> CliResult<String> {
    if default.is_empty() {
        eprint!("{question} : ");
    } else {
        eprint!("{question} [{default}] : ");
    }
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
        .map_err(|e| CliError::Io(e.to_string()))?;
    let line = line.trim();
    Ok(if line.is_empty() { default } else { line }.to_string())
}

/// Machine neuve, rien de configuré : le fournisseur se demande. Pour S3, l'adresse et
/// le bucket complètent l'argument (`s3://bucket/prefixe`) et `--endpoint`.
fn ask_source(args: &mut RestoreArgs) -> CliResult<Source> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(CliError::Usage(
            "aucune configuration sur cette machine : donner la source, `penelope restore \
             s3://bucket/prefixe --endpoint https://s3.fr-par.scw.cloud`, `penelope restore \
             icloud` ou `penelope restore /Volumes/NAS/penelope` (le kit de secours la donne)"
                .into(),
        ));
    }
    eprintln!(
        "Aucune configuration sur cette machine : où est la sauvegarde ? (le kit de secours le dit)"
    );
    match ask("Fournisseur (s3, dir, icloud)", "s3")?.as_str() {
        "s3" => {
            let endpoint = ask(
                "Adresse S3",
                penelope_ops::backup::provider::SCALEWAY_ENDPOINT,
            )?;
            let region = ask("Région", penelope_ops::backup::provider::SCALEWAY_REGION)?;
            let bucket = ask("Bucket", "")?;
            let prefix = ask("Préfixe", "penelope/")?;
            args.endpoint = Some(endpoint);
            args.region = Some(region);
            Ok(Source::S3(format!("s3://{bucket}/{prefix}")))
        }
        "icloud" => {
            let home = penelope_platform::dirs::home_dir();
            let drive = home
                .map(|h| h.join(penelope_ops::backup::provider::ICLOUD_DRIVE))
                .unwrap_or_default();
            let sub = ask(
                "Sous-dossier d'iCloud Drive",
                penelope_ops::backup::provider::ICLOUD_DEFAULT_DIR,
            )?;
            Ok(Source::Dir(drive.join(sub)))
        }
        "dir" => Ok(Source::Dir(PathBuf::from(ask(
            "Dossier des sauvegardes",
            "",
        )?))),
        other => Err(CliError::Usage(format!(
            "fournisseur `{other}` inconnu : s3, dir ou icloud"
        ))),
    }
}

/// Dossier de téléchargement effacé en fin de commande, succès ou échec.
struct Download(PathBuf);

impl Drop for Download {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `penelope restore` : remonte une instance entière depuis une sauvegarde chiffrée
/// (#42, #329). Se fait daemon arrêté ; finit par le service démarré et `doctor`.
pub(super) async fn restore(cli: &Cli, mut args: RestoreArgs) -> CliResult<()> {
    if let Some(db) = args.source.as_deref().filter(|s| s.ends_with(".db")) {
        return restore_offline(cli, std::path::Path::new(db)).await;
    }
    let socket = socket_path(cli.home.clone())?;
    if !args.dry_run && !args.list && call(&socket, m::STATUS, json!({})).await.is_ok() {
        return Err(CliError::Usage(
            "le daemon tourne : `penelope stop` d'abord, puis relancer la restauration".into(),
        ));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    dirs.ensure_all().map_err(|e| CliError::Io(e.to_string()))?;
    let local = local_config(dirs.as_ref());
    let home = penelope_platform::dirs::home_dir();
    let source = match source_of(
        args.source.as_deref(),
        &local.backup,
        dirs.as_ref(),
        home.as_deref(),
    )
    .map_err(CliError::Usage)?
    {
        Some(s) => s,
        None => ask_source(&mut args)?,
    };
    let download = Download(dirs.data().join("backups").join(format!(
        "telechargement-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S")
    )));
    let archive = match &source {
        Source::File(p) if args.list => {
            return Err(CliError::Usage(format!(
                "`--list` vaut pour un fournisseur, pas pour l'archive {}",
                p.display()
            )));
        }
        Source::File(p) => p.clone(),
        Source::S3(s) => {
            std::fs::create_dir_all(&download.0).map_err(|e| CliError::Io(e.to_string()))?;
            match fetch_from_s3(dirs.as_ref(), &args, s, &download.0).await? {
                Some(p) => p,
                None => return Ok(()),
            }
        }
        Source::Dir(dir) if args.list => {
            let all = penelope_ops::backup::provider::list_dir(dir);
            if all.is_empty() {
                println!("Aucune sauvegarde dans {}", dir.display());
            }
            for (name, size) in all {
                println!("{:>6} Mo  {name}", size / (1024 * 1024));
            }
            return Ok(());
        }
        Source::Dir(dir) => {
            penelope_ops::backup::provider::pick_in_dir(dir, args.archive.as_deref())
                .map_err(|e| CliError::Validation(e.to_string()))?
        }
    };
    if !archive.is_file() {
        return Err(CliError::Validation(format!(
            "{} introuvable",
            archive.display()
        )));
    }

    // Phrase de passe : à l'invite, masquée, jamais en argument (#328).
    let pass = penelope_platform::terminal::read_secret(
        "Phrase de passe de la sauvegarde (rien ne s'affiche) : ",
    )
    .map_err(|e| CliError::Io(e.to_string()))?;
    let pass = pass.trim().to_string();
    let store = match penelope_platform::backend::secret_store(dirs.as_ref()) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("⚠️ magasin de secrets indisponible ({e}) : les secrets seront à ressaisir.");
            None
        }
    };
    println!("Restauration de {}…", archive.display());
    let report = penelope_ops::backup::restore::restore_archive(
        dirs.as_ref(),
        &archive,
        &pass,
        store.as_deref(),
        args.dry_run,
        &|c| penelope_platform::process::which(c).is_some(),
    )
    .map_err(|e| CliError::Validation(e.to_string()))?;
    drop(download);
    print_report(&report);
    if args.dry_run {
        println!("\n(--dry-run : rien n'a été écrit)");
        return Ok(());
    }
    let mut todo: Vec<String> = report["todo"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    if args.no_start {
        todo.insert(
            0,
            "`penelope install` puis `penelope start` (service)".into(),
        );
    } else {
        todo.splice(0..0, start_everything(cli, dirs.as_ref(), &report).await);
    }
    if todo.is_empty() {
        println!("\n✅ Tout est en place : Pénélope repart comme hier.");
    } else {
        println!("\n✅ Restauré. Il reste à faire :");
        for t in &todo {
            println!("  - {t}");
        }
    }
    println!(
        "Les index de recherche reviennent seuls : le daemon les reconstruit à son premier \
         passage de maintenance, puis le rattrapage d'embeddings recalcule les vecteurs."
    );
    Ok(())
}

/// Ce qui a été remis en place.
fn print_report(report: &Value) {
    println!(
        "Sauvegarde du {} (version {}) :",
        report["created_at"].as_str().unwrap_or("?"),
        report["version"].as_str().unwrap_or("?")
    );
    for p in report["plan"].as_array().into_iter().flatten() {
        println!(
            "  {} → {}",
            p["name"].as_str().unwrap_or_default(),
            p["to"].as_str().unwrap_or_default()
        );
    }
    for a in report["set_aside"].as_array().into_iter().flatten() {
        println!(
            "  existant mis de côté : {}",
            a.as_str().unwrap_or_default()
        );
    }
    let restored = report["secrets_restored"].as_array().map(|a| a.len());
    let known = report["secrets_in_archive"].as_array().map(|a| a.len());
    match (restored, known) {
        (Some(n), _) => println!("  secrets rangés dans le magasin : {n}"),
        (None, Some(n)) if n > 0 => println!("  secrets dans l'archive : {n}"),
        _ => {}
    }
}

/// Réinstalle le service du daemon et les serveurs d'inférence de la sauvegarde, attend
/// que le daemon réponde, puis lance `doctor`. Renvoie ce qui n'a pas pu se faire.
async fn start_everything(
    cli: &Cli,
    dirs: &dyn penelope_platform::Directories,
    report: &Value,
) -> Vec<String> {
    let mut todo = Vec::new();
    for svc in report["services"].as_array().into_iter().flatten() {
        let label = svc["label"].as_str().unwrap_or_default();
        let args: Vec<String> = svc["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(String::from))
            .collect();
        let Some(program) = args.first().map(PathBuf::from).filter(|p| p.is_file()) else {
            todo.push(format!(
                "serveur d'inférence `{label}` : programme absent ({}), `penelope local \
                 install` une fois `mlx-lm` installé",
                args.first().map(String::as_str).unwrap_or("?")
            ));
            continue;
        };
        let installed = penelope_platform::backend::agent_manager(dirs, label, args.clone())
            .and_then(|m| m.install(&program, None));
        if let Err(e) = installed {
            todo.push(format!("serveur d'inférence `{label}` : {e}"));
        }
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            todo.push(format!("`penelope install` puis `penelope start` ({e})"));
            return todo;
        }
    };
    let installed = penelope_platform::backend::service_manager(dirs)
        .and_then(|m| m.install(&exe, cli.home.as_deref()).map(|_| m));
    let mgr = match installed {
        Ok(m) => m,
        Err(e) => {
            todo.push(format!("`penelope install` puis `penelope start` ({e})"));
            return todo;
        }
    };
    let _ = mgr.start();
    println!("\nService installé ; démarrage du daemon…");
    let socket = dirs.socket_path();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while call(&socket, m::STATUS, json!({})).await.is_err() {
        if std::time::Instant::now() > deadline {
            todo.push("le daemon ne répond pas après 30 s : `penelope logs`".into());
            return todo;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    println!();
    if let Err(e) = doctor(cli).await {
        todo.push(format!("`penelope doctor` : {e}"));
    }
    todo
}
