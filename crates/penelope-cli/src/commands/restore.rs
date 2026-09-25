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

/// `penelope restore-all` : remonte une instance entière depuis une sauvegarde chiffrée
/// (issue #42). Se fait daemon arrêté, sur une machine où il n'y a encore rien.
pub(super) async fn restore_all(cli: &Cli, source: Option<String>, dry_run: bool) -> CliResult<()> {
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
