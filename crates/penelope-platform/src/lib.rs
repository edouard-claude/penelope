//! `penelope-platform` : **le seul crate autorisé à connaître l'OS** (§2.1).
//!
//! Tout ce qui dépend de la plateforme passe par les traits de ce crate, avec un backend
//! par OS sélectionné à la compilation. Le reste du workspace ne contient aucun chemin
//! littéral, aucun appel shell, aucun signal Unix, aucune API Trousseau : le test
//! d'architecture (`penelope-archtest`) le vérifie par recherche de motifs.
//!
//! Cible livrée : macOS ≥ 13. Linux et Windows compilent sur des stubs qui renvoient
//! `PlatformError::Unsupported` avec un message explicite.

#![forbid(unsafe_code)]

pub mod archive;
pub mod audio;
pub mod backend;
pub mod codesign;
pub mod dirs;
pub mod handoff;
pub mod host;
pub mod ipc;
pub mod ocr;
pub mod power;
pub mod process;
pub mod sandbox;
pub mod secrets;
pub mod service;
pub mod terminal;
pub mod watcher;

pub use dirs::{Dir, Directories, RootedDirs, resolve_directories, slugify, validate_slug};
pub use power::{PowerManager, SleepGuard};
pub use process::{ProcessHost, ProcessSpec, UnixProcessHost, which};
pub use sandbox::{Coverage, Profile, ProfileKind, Sandbox};
pub use secrets::{MemorySecretStore, SecretStore, validate_secret_name};
pub use service::{ServiceManager, ServiceStatus};

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("non supporté : {0}")]
    Unsupported(String),

    #[error("entrée-sortie : {0}")]
    Io(#[from] std::io::Error),

    #[error("json : {0}")]
    Json(#[from] serde_json::Error),

    #[error("introuvable : {0}")]
    NotFound(String),

    #[error("secrets : {0}")]
    Secret(String),

    #[error("service : {0}")]
    Service(String),

    #[error("processus : {0}")]
    Process(String),

    #[error("ipc : {0}")]
    Ipc(String),

    #[error("slug invalide : {0}")]
    InvalidSlug(String),
}

pub type Result<T, E = PlatformError> = std::result::Result<T, E>;

/// Élément de rapport `doctor` produit par un backend (§2.11).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DoctorItem {
    pub id: String,
    pub label: String,
    pub ok: bool,
    pub detail: String,
    /// Commande corrective **proposée**, jamais exécutée automatiquement.
    pub fix: Option<String>,
}

/// Faisceau de services de plateforme, construit une fois au démarrage du daemon.
pub struct Platform {
    pub dirs: Box<dyn Directories>,
    pub secrets: Box<dyn SecretStore>,
    pub service: Box<dyn ServiceManager>,
    pub sandbox: Box<dyn Sandbox>,
    pub power: Box<dyn PowerManager>,
    pub processes: UnixProcessHost,
}

impl Platform {
    /// Construit le faisceau pour l'OS courant.
    pub fn bootstrap(home: Option<PathBuf>) -> Result<Platform> {
        let dirs = resolve_directories(home)?;
        dirs.ensure_all()?;
        let secrets = backend::secret_store(dirs.as_ref())?;
        let service = backend::service_manager(dirs.as_ref())?;
        let processes = UnixProcessHost::new(dirs.pid_dir());
        Ok(Platform {
            dirs,
            secrets,
            service,
            sandbox: backend::sandbox(),
            power: backend::power_manager(),
            processes,
        })
    }

    /// Variante de test : racine unique + secrets en mémoire.
    pub fn for_tests(root: PathBuf) -> Result<Platform> {
        let dirs: Box<dyn Directories> = Box::new(RootedDirs::new(root));
        dirs.ensure_all()?;
        let processes = UnixProcessHost::new(dirs.pid_dir());
        Ok(Platform {
            secrets: Box::new(MemorySecretStore::new()),
            service: backend::service_manager(dirs.as_ref())?,
            sandbox: backend::sandbox(),
            power: Box::new(power::CountingPower::noop()),
            processes,
            dirs,
        })
    }

    pub fn os_name(&self) -> &'static str {
        backend::OS_NAME
    }

    /// État de la machine (batterie, disque, mémoire, charge…). Bloquant.
    pub fn host_status(&self, now_unix: i64) -> host::HostStatus {
        host::status(&self.dirs.data(), now_unix)
    }

    /// Contrôles `doctor` communs + propres à l'OS (§2.11).
    pub fn doctor(&self) -> Vec<DoctorItem> {
        let mut v = Vec::new();

        // Service.
        match self.service.status() {
            Ok(s) => v.push(DoctorItem {
                id: "service".into(),
                label: "Service installé et actif".into(),
                ok: s.installed && s.running,
                detail: format!("{} — {}", s.mechanism, s.detail),
                fix: (!s.installed).then(|| "penelope install".to_string()),
            }),
            Err(e) => v.push(DoctorItem {
                id: "service".into(),
                label: "Service".into(),
                ok: false,
                detail: e.to_string(),
                fix: None,
            }),
        }

        // Espace disque.
        let data = self.dirs.data();
        match free_space_gb(&data) {
            Some(gb) => v.push(DoctorItem {
                id: "disk".into(),
                label: "Espace disque ≥ 10 Go".into(),
                ok: gb >= 10.0,
                detail: format!("{gb:.1} Go libres sur {}", data.display()),
                fix: (gb < 10.0).then(|| "libérer de l'espace ou déplacer PENELOPE_HOME".into()),
            }),
            None => v.push(DoctorItem {
                id: "disk".into(),
                label: "Espace disque".into(),
                ok: false,
                detail: "mesure indisponible".into(),
                fix: None,
            }),
        }

        // Dépendances MCP : trouvées dans le PATH effectif, puis interrogées.
        for dep in ["git", "npx", "uvx", "docker"] {
            let item = match which(dep) {
                Some(path) => {
                    match process::probe_version(&path, std::time::Duration::from_secs(3)) {
                        Some(version) => DoctorItem {
                            id: format!("dep.{dep}"),
                            label: format!("Dépendance `{dep}`"),
                            ok: true,
                            detail: format!("{version} ({})", path.display()),
                            fix: None,
                        },
                        None => DoctorItem {
                            id: format!("dep.{dep}"),
                            label: format!("Dépendance `{dep}`"),
                            ok: false,
                            detail: format!("{} ne répond pas à `--version`", path.display()),
                            fix: Some(format!("vérifier l'installation de `{dep}`")),
                        },
                    }
                }
                None => DoctorItem {
                    id: format!("dep.{dep}"),
                    label: format!("Dépendance `{dep}`"),
                    ok: false,
                    detail: format!(
                        "introuvable, y compris dans les emplacements usuels (PATH effectif : {})",
                        process::search_path().to_string_lossy()
                    ),
                    fix: Some(format!(
                        "installer `{dep}`, ou réinstaller le service depuis un terminal où \
                         `{dep}` marche : `penelope uninstall && penelope install`"
                    )),
                },
            };
            v.push(item);
        }

        // Secrets.
        v.push(DoctorItem {
            id: "secrets".into(),
            label: "Magasin de secrets".into(),
            ok: self.secrets.list().is_ok(),
            detail: self.secrets.backend(),
            fix: None,
        });

        v.extend(backend::doctor_checks());
        v
    }
}

/// Espace libre en gigaoctets, via `df -k` (exécutable, jamais un shell).
fn free_space_gb(path: &std::path::Path) -> Option<f64> {
    if cfg!(windows) {
        return None;
    }
    let out = std::process::Command::new("/bin/df")
        .args(["-k", &path.to_string_lossy()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let avail_kb: f64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail_kb / 1024.0 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_for_tests_creates_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let p = Platform::for_tests(dir.path().to_path_buf()).unwrap();
        assert!(p.dirs.vault().is_dir());
        assert!(p.dirs.pid_dir().is_dir());
        assert_eq!(p.secrets.backend(), "mémoire (tests)");
    }

    #[test]
    fn doctor_reports_dependencies_and_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = Platform::for_tests(dir.path().to_path_buf()).unwrap();
        let checks = p.doctor();
        assert!(checks.iter().any(|c| c.id == "dep.git"));
        assert!(checks.iter().any(|c| c.id == "disk"));
        assert!(checks.iter().any(|c| c.id == "secrets"));
        // Chaque échec doit être exploitable : soit un détail, soit une correction.
        for c in &checks {
            assert!(!c.detail.is_empty(), "contrôle sans détail : {}", c.id);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_directories_follow_the_prd_table() {
        let d = backend::MacDirs::new().unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            d.resolve(Dir::Data),
            home.join("Library/Application Support/Penelope")
        );
        assert_eq!(d.resolve(Dir::Logs), home.join("Library/Logs/Penelope"));
        assert_eq!(d.resolve(Dir::Cache), home.join("Library/Caches/Penelope"));
    }

    /// CA 2 : `PENELOPE_HOME` bascule tout sous une racine unique.
    #[test]
    fn ca_2_2_penelope_home_reroots_everything() {
        let d = resolve_directories(Some(PathBuf::from("/srv/pen"))).unwrap();
        for (got, want) in [
            (d.config(), "/srv/pen/config"),
            (d.data(), "/srv/pen/data"),
            (d.state(), "/srv/pen/state"),
            (d.logs(), "/srv/pen/logs"),
            (d.cache(), "/srv/pen/cache"),
        ] {
            assert_eq!(got, PathBuf::from(want));
        }
    }
}
