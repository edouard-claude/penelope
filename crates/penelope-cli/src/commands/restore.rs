//! Restaurations, daemon arrêté : une base depuis une sauvegarde (`restore`), une instance
//! entière depuis une sauvegarde chiffrée (`restore-all`, issue #42).

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

/// Les arguments de `penelope restore-all`.
#[derive(Debug, Clone, Default)]
pub(super) struct RestoreAllArgs {
    pub source: Option<String>,
    pub dry_run: bool,
    pub list: bool,
    pub archive: Option<String>,
    pub endpoint: Option<String>,
    pub region: Option<String>,
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

/// La section `[backup.s3]` du config.toml local, ou ses défauts s'il n'existe pas encore.
fn local_s3(dirs: &dyn penelope_platform::Directories) -> penelope_kernel::config::BackupS3 {
    std::fs::read_to_string(dirs.config_file())
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(c, _)| c.backup.s3)
        .unwrap_or_default()
}

/// Télécharge l'archive choisie depuis le bucket (ou les liste), et rend son chemin.
async fn fetch_from_s3(
    dirs: &dyn penelope_platform::Directories,
    args: &RestoreAllArgs,
    source: &str,
    work: &std::path::Path,
) -> CliResult<Option<PathBuf>> {
    use penelope_ops::backup::s3;
    let s3cfg = resolve_s3(
        source,
        &local_s3(dirs),
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

/// `penelope restore-all` : remonte une instance entière depuis une sauvegarde chiffrée
/// (issue #42). Se fait daemon arrêté, sur une machine où il n'y a encore rien.
pub(super) async fn restore_all(cli: &Cli, args: RestoreAllArgs) -> CliResult<()> {
    let dry_run = args.dry_run;
    let socket = socket_path(cli.home.clone())?;
    if !dry_run && !args.list && call(&socket, m::STATUS, json!({})).await.is_ok() {
        return Err(CliError::Usage(
            "le daemon tourne : `penelope stop` d'abord, puis relancer la restauration".into(),
        ));
    }
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let work = dirs.data().join("backups").join("restore");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| CliError::Io(e.to_string()))?;

    // Source : une archive locale, un bucket S3, ou un dépôt à cloner.
    let source = args.source.clone().ok_or_else(|| {
        CliError::Usage(
            "donner l'archive `.tar.gz.enc`, le dépôt privé des sauvegardes ou `s3` : \
             `penelope restore-all git@github.com:moi/penelope-backups.git`, \
             `penelope restore-all s3 --list`"
                .into(),
        )
    })?;
    let is_s3 = source == "s3" || source.starts_with("s3://");
    if args.list && !is_s3 {
        return Err(CliError::Usage(
            "`--list` ne vaut que pour une source S3 (`penelope restore-all s3 --list`)".into(),
        ));
    }
    let archive = if source.ends_with(".enc") {
        PathBuf::from(&source)
    } else if is_s3 {
        match fetch_from_s3(dirs.as_ref(), &args, &source, &work).await? {
            Some(p) => p,
            None => return Ok(()),
        }
    } else {
        return Err(CliError::Usage(format!(
            "`{source}` : une archive `.tar.gz.enc` ou `s3` est attendue. GitHub n'est plus une \
             destination de sauvegarde (#327) : `git clone` l'ancien dépôt, puis donner \
             l'archive voulue"
        )));
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
    if manifest["derived_excluded"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        println!(
            "Les index de recherche (plein texte, vecteurs) ne sont pas dans l'archive : le \
             daemon les reconstruit à son premier passage de maintenance, dans la minute qui \
             suit `penelope start` ; les vecteurs reviennent ensuite par le rattrapage \
             d'embeddings."
        );
    }
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
