//! Le fournisseur unique des sauvegardes (#327) : un bucket S3, un dossier (disque, NAS,
//! volume monté) ou iCloud Drive, qui n'est qu'un dossier synchronisé par macOS.
//!
//! ```text
//!  backup.provider ─┬─ s3 ─────► bucket/prefixe/penelope-….tar.gz.enc (+ .manifest.json)
//!                   ├─ dir ────► backup.dir/penelope-….tar.gz.enc     (+ .manifest.json)
//!                   └─ icloud ─► ~/Library/Mobile Documents/com~apple~CloudDocs/Penelope/…
//! ```
//!
//! La même rotation (`keep_daily`, `keep_weekly`, `keep_monthly`) s'applique partout,
//! manifestes compris.

use super::*;
use penelope_kernel::config::{Backup, BackupS3};

/// Racine d'iCloud Drive dans le répertoire personnel.
pub const ICLOUD_DRIVE: &str = "Library/Mobile Documents/com~apple~CloudDocs";
/// Sous-dossier d'iCloud Drive quand `backup.dir` est vide.
pub const ICLOUD_DEFAULT_DIR: &str = "Penelope";

/// Le stockage objet documenté par défaut (Scaleway, Paris) : `penelope backup setup` et
/// la restauration sur machine neuve le proposent.
pub const SCALEWAY_ENDPOINT: &str = "https://s3.fr-par.scw.cloud";
pub const SCALEWAY_REGION: &str = "fr-par";

/// Où vont les sauvegardes.
#[derive(Debug, Clone)]
pub enum Target {
    S3(BackupS3),
    Dir { path: PathBuf, icloud: bool },
}

impl Target {
    /// Le fournisseur de la configuration. `home` : le répertoire personnel, pour `~/` et
    /// iCloud Drive. Sans fournisseur, l'erreur donne la commande qui le règle.
    pub fn resolve(
        cfg: &Backup,
        dirs: &dyn penelope_platform::Directories,
        home: Option<&Path>,
    ) -> anyhow::Result<Target> {
        let no_home = || anyhow::anyhow!("répertoire personnel inconnu (HOME absent)");
        match cfg.effective_provider() {
            None => anyhow::bail!(
                "aucun fournisseur de sauvegarde : `penelope backup setup` (S3, dossier ou \
                 iCloud Drive)"
            ),
            Some("s3") => Ok(Target::S3(cfg.s3.clone())),
            Some("dir") => {
                let raw = cfg.dir.trim();
                let path = match raw.strip_prefix("~/") {
                    Some(rest) => home.ok_or_else(no_home)?.join(rest),
                    None => dirs.expand(raw),
                };
                if !path.is_absolute() {
                    anyhow::bail!("backup.dir `{raw}` : un chemin absolu est attendu");
                }
                Ok(Target::Dir {
                    path,
                    icloud: false,
                })
            }
            Some("icloud") => {
                let drive = home.ok_or_else(no_home)?.join(ICLOUD_DRIVE);
                let sub = match cfg.dir.trim() {
                    "" => ICLOUD_DEFAULT_DIR,
                    d => d.trim_matches('/'),
                };
                Ok(Target::Dir {
                    path: drive.join(sub),
                    icloud: true,
                })
            }
            Some(other) => anyhow::bail!("backup.provider `{other}` inconnu"),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Target::S3(_) => "s3",
            Target::Dir { icloud: true, .. } => "icloud",
            Target::Dir { .. } => "dir",
        }
    }

    /// Libellé lisible de l'emplacement, pour `doctor`, le digest et le kit de secours.
    pub fn location(&self) -> String {
        match self {
            Target::S3(c) => format!(
                "S3 `{}/{}` ({})",
                c.bucket,
                s3::normalized_prefix(&c.prefix),
                c.endpoint
            ),
            Target::Dir { path, icloud: true } => format!("iCloud Drive ({})", path.display()),
            Target::Dir { path, .. } => path.display().to_string(),
        }
    }

    /// Le dossier d'un fournisseur `dir` ou `icloud`, prêt à recevoir : iCloud Drive doit
    /// être activé, et un volume (`/Volumes/<nom>`) monté, faute de quoi le dossier serait
    /// créé sur le disque interne, à l'insu du propriétaire.
    pub fn ready_dir(&self) -> anyhow::Result<&Path> {
        let Target::Dir { path, icloud } = self else {
            anyhow::bail!("fournisseur S3 : pas de dossier");
        };
        if *icloud {
            let drive = path
                .ancestors()
                .find(|a| a.ends_with(ICLOUD_DRIVE))
                .unwrap_or(path);
            if !drive.is_dir() {
                anyhow::bail!(
                    "iCloud Drive n'est pas activé sur cette machine ({} absent) : Réglages \
                     Système, identifiant Apple, iCloud, iCloud Drive",
                    drive.display()
                );
            }
        }
        let mut parts = path.components();
        if let (Some(_), Some(first), Some(volume)) = (parts.next(), parts.next(), parts.next())
            && first.as_os_str() == "Volumes"
        {
            let mount = Path::new("/Volumes").join(volume.as_os_str());
            if !mount.is_dir() {
                anyhow::bail!(
                    "le volume {} n'est pas monté : la sauvegarde n'est pas écrite sur le \
                     disque interne à sa place",
                    mount.display()
                );
            }
        }
        std::fs::create_dir_all(path)
            .map_err(|e| anyhow::anyhow!("dossier de sauvegarde {} : {e}", path.display()))?;
        Ok(path)
    }
}

/// Envoie l'archive et son manifeste chez le fournisseur, puis applique la rotation.
pub async fn push(
    s: &Services,
    target: &Target,
    archive: &Path,
    report: &Value,
) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let mut out = match target {
        Target::S3(c) => push_s3(s, c, &cfg.backup, archive, report).await?,
        Target::Dir { .. } => {
            let dir = target.ready_dir()?.to_path_buf();
            let (archive, report, retention) =
                (archive.to_path_buf(), report.clone(), cfg.backup.clone());
            tokio::task::spawn_blocking(move || push_dir(&dir, &archive, &report, &retention))
                .await
                .map_err(|e| anyhow::anyhow!("envoi interrompu : {e}"))??
        }
    };
    out["provider"] = json!(target.label());
    out["location"] = json!(target.location());
    Ok(out)
}

/// Copie l'archive dans le dossier (nom provisoire, puis renommage : une copie coupée ne
/// passe jamais pour une archive), vérifie sa somme, pose le manifeste, fait la rotation.
pub fn push_dir(
    dir: &Path,
    archive: &Path,
    report: &Value,
    retention: &Backup,
) -> anyhow::Result<Value> {
    let name = archive
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "penelope.tar.gz.enc".into());
    let dest = dir.join(&name);
    let partial = dir.join(format!(".{name}.partiel"));
    std::fs::copy(archive, &partial)
        .map_err(|e| anyhow::anyhow!("copie vers {} : {e}", dir.display()))?;
    let expected = report["sha256"].as_str().unwrap_or_default();
    let copied = sha256_of(&partial)?;
    if !expected.is_empty() && copied != expected {
        let _ = std::fs::remove_file(&partial);
        anyhow::bail!(
            "copie vers {} altérée (somme différente) : rien n'a été remplacé",
            dir.display()
        );
    }
    std::fs::rename(&partial, &dest)?;
    std::fs::write(
        dir.join(s3::manifest_key(&name)),
        serde_json::to_vec_pretty(report)?,
    )?;
    let names: Vec<String> = archive_names(dir);
    let removed = rotation_plan(&names, retention);
    for n in &removed {
        std::fs::remove_file(dir.join(n))?;
        let _ = std::fs::remove_file(dir.join(s3::manifest_key(n)));
    }
    Ok(json!({
        "path": dest,
        "key": name,
        "bytes": std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
        "rotated": removed,
    }))
}

/// Les archives d'un dossier, par nom.
pub fn archive_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            (n.starts_with("penelope-") && n.ends_with(s3::ARCHIVE_SUFFIX)).then_some(n)
        })
        .collect()
}

/// Envoie l'archive et son manifeste dans le bucket, puis applique la rotation aux
/// objets du préfixe (#289).
async fn push_s3(
    s: &Services,
    c: &BackupS3,
    retention: &Backup,
    archive: &Path,
    report: &Value,
) -> anyhow::Result<Value> {
    let client = s3::S3Client::from_config(c, s.platform.secrets.as_ref())?;
    let prefix = s3::normalized_prefix(&c.prefix);
    let name = archive
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "penelope.tar.gz.enc".into());
    let key = format!("{prefix}{name}");
    let up = client.put_file(&key, archive).await?;
    client
        .put_bytes(&s3::manifest_key(&key), serde_json::to_vec_pretty(report)?)
        .await?;
    let names: Vec<String> = s3::list_archives(&client, &prefix)
        .await?
        .into_iter()
        .map(|o| o.key.strip_prefix(&prefix).unwrap_or(&o.key).to_string())
        .collect();
    let removed = rotation_plan(&names, retention);
    for n in &removed {
        let old = format!("{prefix}{n}");
        client.delete(&old).await?;
        let _ = client.delete(&s3::manifest_key(&old)).await;
    }
    Ok(json!({
        "endpoint": client.endpoint(),
        "bucket": client.bucket(),
        "key": key,
        "bytes": up.bytes,
        "etag": up.etag,
        "parts": up.parts,
        "rotated": removed,
    }))
}

/// Les archives d'un dossier de fournisseur, la plus récente d'abord, avec leur taille.
pub fn list_dir(dir: &Path) -> Vec<(String, u64)> {
    let mut names = archive_names(dir);
    names.sort();
    names.reverse();
    names
        .into_iter()
        .map(|n| {
            let size = std::fs::metadata(dir.join(&n))
                .map(|m| m.len())
                .unwrap_or(0);
            (n, size)
        })
        .collect()
}

/// L'archive à restaurer dans un dossier : celle qui porte `wanted` (nom entier ou fin
/// du nom), sinon la plus récente.
pub fn pick_in_dir(dir: &Path, wanted: Option<&str>) -> anyhow::Result<PathBuf> {
    let all = list_dir(dir);
    let found = match wanted {
        Some(w) => all.iter().find(|(n, _)| n == w || n.ends_with(w)),
        None => all.first(),
    };
    match (found, wanted) {
        (Some((n, _)), _) => Ok(dir.join(n)),
        (None, Some(w)) => anyhow::bail!(
            "archive `{w}` absente de {} ; `--list` donne celles qui existent",
            dir.display()
        ),
        (None, None) => anyhow::bail!("aucune sauvegarde chiffrée dans {}", dir.display()),
    }
}
