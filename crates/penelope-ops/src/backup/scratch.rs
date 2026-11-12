//! Dossiers de travail de `backups/` (#465).
//!
//! Une sauvegarde copie la base et les dossiers dans `backups/work-<date>` avant de les
//! archiver et de les chiffrer ; une restauration extrait l'archive dans
//! `backups/restauration-<date>`, après l'avoir parfois tirée dans
//! `backups/telechargement-<date>`. Chacun était retiré par le processus qui l'avait créé,
//! à la fin de son travail : un arrêt du daemon, un `kill`, une panne ou un disque plein
//! le laissait là, en clair et lisible de tous. Le 13/10, une sauvegarde interrompue a
//! laissé 3,1 Gio, dont une copie de la base, pendant un mois.
//!
//! ```text
//!  Scratch::create ── dossier 0700, `.owner` (pid) ── travail ── Drop : retiré
//!
//!  sweep, avant chaque sauvegarde et à chaque passe de maintenance du daemon :
//!    un dossier de travail est retiré quand son propriétaire n'est plus là
//!      ├─ `.owner` de ce processus, et plus aucun `Scratch` vivant pour lui
//!      ├─ `.owner` d'un processus mort, ou marque de plus d'un jour (pid réattribué)
//!      └─ pas de `.owner` depuis dix minutes (version d'avant, créateur coupé net)
//! ```
//!
//! Tout le reste de `backups/` que le code ne range pas lui-même (l'ancienne destination
//! git `repo`, par exemple) n'est jamais retiré : [`unexpected`] le montre à `doctor`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

/// Préfixes des dossiers de travail : sauvegarde, restauration, téléchargement.
pub const PREFIXES: &[&str] = &["work-", "restauration-", "telechargement-"];

/// La marque du propriétaire, dans le dossier : son pid.
const OWNER: &str = ".owner";

/// Au-delà, la marque ne vaut plus : aucun travail ne dure un jour, et un pid finit par
/// être réattribué à un autre programme.
const OWNER_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Délai laissé à un dossier sans marque : le temps, pour son créateur, de l'écrire.
const UNMARKED_GRACE: Duration = Duration::from_secs(600);

/// Les dossiers de travail de ce processus, tant que leur [`Scratch`] vit.
static LIVE: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn live() -> MutexGuard<'static, BTreeSet<PathBuf>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Le nom sous lequel un dossier est tenu : son chemin réel, pour qu'un lien dans le
/// chemin des données ne fasse pas prendre un dossier vivant pour un reste.
fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Un dossier de travail : créé en 0700, marqué du pid de son créateur, retiré quand la
/// valeur tombe (fin normale, erreur ou panique).
pub struct Scratch {
    path: PathBuf,
    key: PathBuf,
}

impl Scratch {
    /// Crée `path`, vide. Un dossier du même nom est d'abord retiré.
    pub fn create(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_dir_all(&path);
        private_dir(&path)?;
        // Tenu avant d'être marqué : sans marque, le délai le protège ; marqué de ce
        // processus, c'est d'être tenu qui le protège. Un échec retire ce qui a été créé.
        let scratch = Scratch {
            key: key(&path),
            path,
        };
        live().insert(scratch.key.clone());
        write_private(&scratch.path.join(OWNER), std::process::id().to_string())?;
        Ok(scratch)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        live().remove(&self.key);
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Écrit `bytes` dans `path`, créé en 0600 : ce que la sauvegarde pose en clair dans son
/// dossier de travail n'est lisible que du propriétaire, même sorti de ce dossier.
pub fn write_private(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes.as_ref())?;
    restrict_file(path)
}

/// Ramène à 0600 un fichier écrit par un autre que nous (SQLite, `tar`).
pub fn restrict_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Depuis combien de temps `path` n'a pas été modifié ; `None` si on ne peut pas le dire
/// (introuvable, ou daté du futur).
fn age(path: &Path) -> Option<Duration> {
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Les entrées de `backups` : nom et chemin.
fn entries(backups: &Path) -> Vec<(String, PathBuf)> {
    let mut all: Vec<(String, PathBuf)> = std::fs::read_dir(backups)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
        .collect();
    all.sort();
    all
}

fn is_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Un vrai dossier (pas un lien) qui porte un préfixe de dossier de travail.
fn is_scratch(name: &str, path: &Path) -> bool {
    PREFIXES.iter().any(|p| name.starts_with(p)) && is_dir(path)
}

/// Plus personne ne travaille dans ce dossier.
fn abandoned(path: &Path) -> bool {
    if live().contains(&key(path)) {
        return false;
    }
    let marker = path.join(OWNER);
    let owner = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok());
    let Some(pid) = owner else {
        // Pas de marque lisible : une version d'avant #465, ou un créateur en train de
        // l'écrire. Le délai tranche.
        return age(path).is_some_and(|a| a >= UNMARKED_GRACE);
    };
    if pid == std::process::id() {
        // À nous, et plus aucun `Scratch` ne le tient : son retrait a échoué.
        return true;
    }
    let fresh = age(&marker).is_none_or(|a| a < OWNER_MAX_AGE);
    !(fresh && penelope_platform::process::process_exists(pid))
}

/// Poids d'un fichier ou d'un dossier, sans suivre les liens.
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| tree_bytes(&e.path()))
        .sum()
}

/// Retire de `backups` les dossiers de travail abandonnés. Renvoie leur nom et leur poids.
pub fn sweep(backups: &Path) -> Vec<(String, u64)> {
    let mut swept = Vec::new();
    for (name, path) in entries(backups) {
        if !is_scratch(&name, &path) || !abandoned(&path) {
            continue;
        }
        let bytes = tree_bytes(&path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                tracing::warn!(dir = %name, bytes, "dossier de travail abandonné : retiré");
                swept.push((name, bytes));
            }
            Err(e) => {
                tracing::warn!(dir = %name, error = %e, "dossier de travail abandonné : non retiré")
            }
        }
    }
    swept
}

/// Ce que `backups` contient et que le code n'y range pas : nom et poids. Les archives,
/// leurs manifestes et les instantanés (`penelope-*`), une copie en cours vers ce dossier
/// (`.penelope-*`), la base mise de côté par une restauration (`avant-restauration-*`) et
/// un dossier de travail dont le propriétaire est là sont attendus.
pub fn unexpected(backups: &Path) -> Vec<(String, u64)> {
    entries(backups)
        .into_iter()
        .filter(|(name, path)| {
            let known = ["penelope-", ".penelope-", "avant-restauration-"]
                .iter()
                .any(|p| name.starts_with(p))
                && !is_dir(path);
            let working = is_scratch(name, path) && !abandoned(path);
            !(known || working || name == ".DS_Store")
        })
        .map(|(name, path)| {
            let bytes = tree_bytes(&path);
            (name, bytes)
        })
        .collect()
}

/// « 3,1 Gio », « 724 Mo », « 12 Ko » : une taille pour un message.
pub fn human(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KIB * KIB * KIB {
        format!("{:.1} Gio", b / (KIB * KIB * KIB)).replace('.', ",")
    } else if b >= KIB * KIB {
        format!("{:.0} Mo", b / (KIB * KIB))
    } else if b >= KIB {
        format!("{:.0} Ko", b / KIB)
    } else {
        format!("{bytes} o")
    }
}

#[cfg(test)]
mod tests;
