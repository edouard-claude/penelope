//! Restauration d'une archive sur une machine neuve (#329), daemon arrêté.
//!
//! ```text
//!  archive ─► déchiffrée en mémoire ─► tar (entrée standard) ─► dossier de travail
//!     base, vault (à SON chemin), config, skills, workflows, gabarits, mcp.d,
//!     workspace, mcp-data, médias ─► leur place, l'existant mis de côté
//!     secrets ─► seconde couche ouverte ─► magasin (trousseau), phrase de passe comprise
//!  dossier de travail ─► effacé, succès ou échec
//! ```
//!
//! La CLI (`penelope restore`) va chercher l'archive chez le fournisseur, demande la
//! phrase de passe, appelle [`restore_archive`], puis réinstalle le service et lance
//! `doctor`. Le compte rendu ne garde que ce qui reste vraiment à faire.

use super::*;
use penelope_platform::{Directories, SecretStore};

/// Ce qui se restaure, dans l'ordre : nom dans l'archive, destination. Le vault suit
/// `memory.vault_path` de la configuration **restaurée**.
fn plan(root: &Path, dirs: &dyn Directories) -> Vec<(String, PathBuf)> {
    let data = dirs.data();
    let vault = std::fs::read_to_string(root.join("config.toml"))
        .ok()
        .and_then(|raw| penelope_kernel::config::Config::parse(&raw).ok())
        .map(|(c, _)| dirs.expand(&c.memory.vault_path))
        .unwrap_or_else(|| data.join("vault"));
    [
        ("penelope.db", dirs.db_path()),
        ("config.toml", dirs.config_file()),
        ("vault", vault),
        ("skills", dirs.skills()),
        ("workflows", data.join("workflows")),
        ("templates", data.join("templates")),
        ("mcp.d", data.join("mcp.d")),
        ("workspace", data.join("workspace")),
        ("mcp-data", data.join("mcp-data")),
        ("artifacts", dirs.artifacts()),
        ("media", data.join("media")),
    ]
    .into_iter()
    .filter(|(name, _)| root.join(name).exists())
    .map(|(name, dst)| (name.to_string(), dst))
    .collect()
}

/// Dossier de travail effacé quand il sort de portée : succès, erreur ou panique, rien
/// de déchiffré ne reste sur le disque.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Restaure `archive` dans les répertoires `dirs`. `store` : le magasin où ranger les
/// secrets (`None` : ils sont listés à ressaisir). `dry_run` : le plan seul, rien
/// d'écrit. `which` dit si une commande existe sur cette machine.
pub fn restore_archive(
    dirs: &dyn Directories,
    archive: &Path,
    passphrase: &str,
    store: Option<&dyn SecretStore>,
    dry_run: bool,
    which: &dyn Fn(&str) -> bool,
) -> anyhow::Result<Value> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let scratch = Scratch(
        dirs.data()
            .join("backups")
            .join(format!("restauration-{stamp}")),
    );
    let _ = std::fs::remove_dir_all(&scratch.0);
    std::fs::create_dir_all(&scratch.0)?;
    restrict(&scratch.0);
    let plain = penelope_platform::archive::open_to_vec(archive, passphrase)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let entries = penelope_platform::process::tar_gz_entries(&plain)
        .map_err(|e| anyhow::anyhow!("lecture de l'archive : {e}"))?;
    check_entries(&entries)?;
    penelope_platform::process::extract_tar_gz_bytes(&plain, &scratch.0)
        .map_err(|e| anyhow::anyhow!("extraction : {e}"))?;
    drop(plain);
    let root = scratch.0.join("penelope");
    let manifest: Value = std::fs::read_to_string(root.join("MANIFEST.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);
    let plan = plan(&root, dirs);
    let mut report = json!({
        "created_at": manifest["created_at"],
        "version": manifest["version"],
        "plan": plan.iter().map(|(n, d)| json!({"name": n, "to": d})).collect::<Vec<_>>(),
        "dry_run": dry_run,
    });
    let values = match std::fs::read(root.join(secrets::FILE)) {
        Ok(raw) => secrets::open(&raw, passphrase)?,
        // Archive d'avant #327 : les secrets n'y sont pas.
        Err(_) => Default::default(),
    };
    report["secrets_in_archive"] = json!(values.keys().collect::<Vec<_>>());
    if dry_run {
        report["todo"] = json!(todo(
            &manifest,
            &[],
            store.is_none() && !values.is_empty(),
            which
        ));
        return Ok(report);
    }

    let mut set_aside = Vec::new();
    for (name, dst) in &plan {
        if dst.exists() {
            let aside = PathBuf::from(format!("{}.avant-restauration-{stamp}", dst.display()));
            std::fs::rename(dst, &aside)?;
            set_aside.push(aside);
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        copy_path(&root.join(name), dst)
            .map_err(|e| anyhow::anyhow!("{name} vers {} : {e}", dst.display()))?;
    }
    // Journal WAL d'une base remplacée : retiré, la base restaurée est cohérente.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", dirs.db_path().display()));
    }
    report["set_aside"] = json!(set_aside);

    let failed = match store {
        Some(store) => {
            let (done, failed) = secrets::restore(store, &values, passphrase);
            report["secrets_restored"] = json!(done);
            failed
        }
        None => Vec::new(),
    };
    report["secrets_failed"] = json!(
        failed
            .iter()
            .map(|(n, e)| json!({"name": n, "error": e}))
            .collect::<Vec<_>>()
    );
    report["todo"] = json!(todo(&manifest, &failed, store.is_none(), which));
    report["services"] = manifest["services"].clone();
    Ok(report)
}

/// Ce qui reste à faire après la restauration, une ligne chacun : secrets à ressaisir,
/// modèles à retélécharger, commandes MCP absentes de cette machine.
fn todo(
    manifest: &Value,
    failed: &[(String, String)],
    no_store: bool,
    which: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    let names = |key: &str| -> Vec<String> {
        manifest[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    };
    let mut retype: Vec<String> = names("secrets_expected");
    retype.extend(failed.iter().map(|(n, _)| n.clone()));
    if no_store {
        retype.extend(names("secrets_included"));
    }
    for name in retype {
        out.push(format!(
            "ressaisir le secret : `penelope secret set {name}`"
        ));
    }
    if let Some(models) = manifest["excluded"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["name"] == "models")
        .and_then(|e| e["items"].as_array())
        .filter(|a| !a.is_empty())
    {
        let list: Vec<&str> = models.iter().filter_map(|m| m.as_str()).collect();
        out.push(format!(
            "retélécharger les modèles locaux ({}) : `penelope doctor` dit lesquels manquent",
            list.join(", ")
        ));
    }
    for c in inventory::missing_commands(manifest, which) {
        out.push(format!(
            "installer `{}`, la commande du serveur MCP `{}`",
            c["command"].as_str().unwrap_or_default(),
            c["server"].as_str().unwrap_or_default()
        ));
    }
    out
}

/// Ce qu'une archive peut contenir : des fichiers et des dossiers sous `penelope/`,
/// jamais de chemin absolu, de `..`, ni de lien (une sauvegarde n'en écrit pas : un lien
/// extrait puis suivi écrirait hors de la destination). Une archive qui en porte n'est
/// pas une sauvegarde de Pénélope, ou a été retouchée : rien n'est extrait.
pub(crate) fn check_entries(entries: &[(char, String)]) -> anyhow::Result<()> {
    for (kind, path) in entries {
        let p = Path::new(path);
        let escapes = p.is_absolute()
            || p.components().any(|c| {
                !matches!(
                    c,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            });
        let under_root = p
            .components()
            .find(|c| !matches!(c, std::path::Component::CurDir))
            .is_some_and(|c| c.as_os_str() == "penelope");
        if escapes || !under_root {
            anyhow::bail!("archive refusée : `{path}` sort de la destination");
        }
        if !matches!(kind, '-' | 'd') {
            anyhow::bail!("archive refusée : `{path}` n'est ni un fichier ni un dossier ({kind})");
        }
    }
    Ok(())
}

/// Dossier de travail en 0700 : ce qui y est extrait est en clair le temps de la copie.
fn restrict(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}
