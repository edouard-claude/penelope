//! Backends Linux et Windows : **stubs non livrés** (§2.1).
//!
//! Le code métier compile sur ces cibles (`cargo check --workspace` en CI, §2.13) mais
//! toute opération propre à l'OS renvoie `PlatformError::Unsupported` avec un message
//! explicite. Les tableaux §2.2 à §2.11 sont une conception de référence : écrire un
//! backend revient à remplacer ce fichier, sans toucher au reste du workspace.

use crate::dirs::{Dir, Directories, home_dir};
use crate::sandbox::{Coverage, Profile, Sandbox, Wrapped};
use crate::secrets::{EncryptedFileStore, SecretStore};
use crate::service::{ServiceManager, ServiceStatus};
use crate::{PlatformError, Result};
use std::path::{Path, PathBuf};

pub const OS_NAME: &str = if cfg!(target_os = "linux") {
    "linux"
} else if cfg!(target_os = "windows") {
    "windows"
} else {
    "inconnu"
};

fn unsupported(what: &str) -> PlatformError {
    PlatformError::Unsupported(format!(
        "{what} n'est pas implémenté sur {OS_NAME} : seul macOS est livré (PRD §2.1). \
         La conception de référence est décrite dans les tableaux §2.2 à §2.11."
    ))
}

/// Résolution XDG (Linux) ou `%APPDATA%` (Windows), conforme au tableau §2.2.
#[derive(Debug, Clone)]
pub struct StubDirs {
    home: PathBuf,
}

impl Directories for StubDirs {
    fn resolve(&self, dir: Dir) -> PathBuf {
        if cfg!(target_os = "windows") {
            let base = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join("AppData/Local"));
            let app = base.join("Penelope");
            return match dir {
                Dir::Config => std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.home.join("AppData/Roaming"))
                    .join("Penelope/config"),
                Dir::Data => app,
                Dir::State => app.join("state"),
                Dir::Logs => app.join("logs"),
                Dir::Cache => app.join("cache"),
            };
        }
        let xdg = |var: &str, default: &str| -> PathBuf {
            std::env::var_os(var)
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(default))
        };
        match dir {
            Dir::Config => xdg("XDG_CONFIG_HOME", ".config").join("penelope"),
            Dir::Data => xdg("XDG_DATA_HOME", ".local/share").join("penelope"),
            Dir::State => xdg("XDG_STATE_HOME", ".local/state").join("penelope"),
            Dir::Logs => xdg("XDG_STATE_HOME", ".local/state").join("penelope/logs"),
            Dir::Cache => xdg("XDG_CACHE_HOME", ".cache").join("penelope"),
        }
    }
}

pub fn native_directories() -> Result<Box<dyn Directories>> {
    Ok(Box::new(StubDirs {
        home: home_dir().ok_or_else(|| PlatformError::NotFound("HOME non défini".into()))?,
    }))
}

/// Sur ces cibles, seul le fichier chiffré est disponible : c'est de toute façon le cas
/// habituel d'un serveur headless (§2.4).
pub fn secret_store(dirs: &dyn Directories) -> Result<Box<dyn SecretStore>> {
    let path = dirs.data().join("secrets.enc");
    if let Ok(kf) = std::env::var("PENELOPE_MASTER_KEY_FILE") {
        return Ok(Box::new(EncryptedFileStore::with_key_file(
            path,
            Path::new(&kf),
        )?));
    }
    let pass = std::env::var("PENELOPE_PASSPHRASE").map_err(|_| {
        PlatformError::Secret(
            "définir PENELOPE_MASTER_KEY_FILE ou PENELOPE_PASSPHRASE pour le magasin de secrets"
                .into(),
        )
    })?;
    Ok(Box::new(EncryptedFileStore::with_passphrase(path, &pass)?))
}

pub struct StubSandbox;

impl Sandbox for StubSandbox {
    fn coverage(&self) -> Coverage {
        Coverage {
            backend: format!("aucun ({OS_NAME})"),
            files: false,
            network: false,
            processes: false,
            notes: "backend non livré : tout profil imposé échoue fermé".into(),
        }
    }
    fn wrap(&self, profile: &Profile, program: &Path, args: &[String]) -> Result<Wrapped> {
        if !profile.enforced() {
            return Ok(Wrapped {
                program: program.to_path_buf(),
                args: args.to_vec(),
                cleanup: None,
            });
        }
        Err(crate::sandbox::unsupported(profile, OS_NAME))
    }
}

pub fn sandbox() -> Box<dyn Sandbox> {
    Box::new(StubSandbox)
}

pub fn sandbox_wrapper(profile: &Profile, program: &Path, args: &[String]) -> Result<Wrapped> {
    StubSandbox.wrap(profile, program, args)
}

pub struct StubService;

impl ServiceManager for StubService {
    fn mechanism(&self) -> &'static str {
        if cfg!(target_os = "linux") {
            "systemd utilisateur (non livré)"
        } else {
            "Service Windows (non livré)"
        }
    }
    fn install(&self, _exe: &Path, _home: Option<&Path>) -> Result<PathBuf> {
        Err(unsupported("l'installation en service"))
    }
    fn uninstall(&self) -> Result<()> {
        Err(unsupported("la désinstallation du service"))
    }
    fn start(&self) -> Result<()> {
        Err(unsupported("le démarrage du service"))
    }
    fn stop(&self) -> Result<()> {
        Err(unsupported("l'arrêt du service"))
    }
    fn status(&self) -> Result<ServiceStatus> {
        Ok(ServiceStatus {
            installed: false,
            running: false,
            pid: None,
            mechanism: self.mechanism().into(),
            unit_path: None,
            detail: format!("backend {OS_NAME} non livré"),
        })
    }
}

pub fn service_manager(_dirs: &dyn Directories) -> Result<Box<dyn ServiceManager>> {
    Ok(Box::new(StubService))
}

pub fn power_manager() -> Box<dyn crate::power::PowerManager> {
    Box::new(crate::power::CountingPower::noop())
}

pub fn doctor_checks() -> Vec<crate::DoctorItem> {
    vec![crate::DoctorItem {
        id: "platform.backend".into(),
        label: "Backend de plateforme".into(),
        ok: false,
        detail: format!("{OS_NAME} : backend non livré, seul macOS est supporté"),
        fix: None,
    }]
}
