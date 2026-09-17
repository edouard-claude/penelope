//! Backend macOS (§2, seule implémentation livrée).

use crate::dirs::{Dir, Directories, home_dir};
use crate::sandbox::{Coverage, Profile, Sandbox, Wrapped, seatbelt_profile};
use crate::secrets::{EncryptedFileStore, SecretStore};
use crate::service::{SERVICE_LABEL, ServiceManager, ServiceStatus, launchd_plist};
use crate::{PlatformError, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const OS_NAME: &str = "macos";

// ------------------------------------------------------------------ répertoires

/// Colonne macOS du tableau §2.2.
#[derive(Debug, Clone)]
pub struct MacDirs {
    home: PathBuf,
}

impl MacDirs {
    pub fn new() -> Result<Self> {
        Ok(MacDirs {
            home: home_dir().ok_or_else(|| {
                PlatformError::NotFound(
                    "HOME n'est pas défini : impossible de résoudre les répertoires".into(),
                )
            })?,
        })
    }
}

impl Directories for MacDirs {
    fn resolve(&self, dir: Dir) -> PathBuf {
        let app = self.home.join("Library/Application Support/Penelope");
        match dir {
            Dir::Config => app.join("config"),
            Dir::Data => app,
            Dir::State => self
                .home
                .join("Library/Application Support/Penelope")
                .join("state"),
            Dir::Logs => self.home.join("Library/Logs/Penelope"),
            Dir::Cache => self.home.join("Library/Caches/Penelope"),
        }
    }
}

pub fn native_directories() -> Result<Box<dyn Directories>> {
    Ok(Box::new(MacDirs::new()?))
}

// ------------------------------------------------------------------ secrets

/// Trousseau macOS piloté par `/usr/bin/security` (exécutable, jamais un shell).
pub struct KeychainStore {
    account: String,
    /// Index des noms (jamais les valeurs) : `dump-keychain` demanderait un
    /// déverrouillage interactif, impossible en headless.
    index: PathBuf,
}

impl KeychainStore {
    pub fn new(data_dir: &Path) -> Self {
        KeychainStore {
            account: "penelope".into(),
            index: data_dir.join("secret-names.json"),
        }
    }

    fn service(&self, name: &str) -> String {
        format!("penelope.{name}")
    }

    fn read_index(&self) -> Vec<String> {
        std::fs::read_to_string(&self.index)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_index(&self, names: &[String]) -> Result<()> {
        if let Some(p) = self.index.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&self.index, serde_json::to_vec_pretty(names)?)?;
        Ok(())
    }

    /// Vrai si le binaire `security` est disponible et le trousseau accessible.
    pub fn available() -> bool {
        Path::new("/usr/bin/security").is_file()
    }
}

impl SecretStore for KeychainStore {
    fn backend(&self) -> String {
        "Trousseau macOS (security)".into()
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        let out = Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-a",
                &self.account,
                "-s",
                &self.service(name),
                "-w",
            ])
            .stderr(Stdio::null())
            .output()
            .map_err(|e| PlatformError::Secret(format!("security : {e}")))?;
        if !out.status.success() {
            return Ok(None);
        }
        let v = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        Ok(Some(v))
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        let out = Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-a",
                &self.account,
                "-s",
                &self.service(name),
                "-w",
                value,
                "-U", // met à jour si l'entrée existe
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| PlatformError::Secret(format!("security : {e}")))?;
        if !out.status.success() {
            return Err(PlatformError::Secret(format!(
                "écriture dans le Trousseau refusée : {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let mut names = self.read_index();
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
            names.sort();
            self.write_index(&names)?;
        }
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        let _ = Command::new("/usr/bin/security")
            .args([
                "delete-generic-password",
                "-a",
                &self.account,
                "-s",
                &self.service(name),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let mut names = self.read_index();
        names.retain(|n| n != name);
        self.write_index(&names)
    }

    fn list(&self) -> Result<Vec<String>> {
        Ok(self.read_index())
    }
}

/// Choisit le magasin de secrets : Trousseau si disponible, sinon fichier chiffré.
///
/// `PENELOPE_SECRETS=file` force le repli, utile pour un Mac administré uniquement en SSH
/// où le Trousseau reste verrouillé.
pub fn secret_store(dirs: &dyn Directories) -> Result<Box<dyn SecretStore>> {
    let forced_file = std::env::var("PENELOPE_SECRETS")
        .map(|v| v == "file")
        .unwrap_or(false);
    if !forced_file && KeychainStore::available() {
        return Ok(Box::new(KeychainStore::new(&dirs.data())));
    }
    let path = dirs.data().join("secrets.enc");
    if let Ok(kf) = std::env::var("PENELOPE_MASTER_KEY_FILE") {
        return Ok(Box::new(EncryptedFileStore::with_key_file(
            path,
            Path::new(&kf),
        )?));
    }
    let pass = std::env::var("PENELOPE_PASSPHRASE").map_err(|_| {
        PlatformError::Secret(
            "aucun backend de secrets disponible : définir PENELOPE_MASTER_KEY_FILE ou \
             PENELOPE_PASSPHRASE, ou rendre le Trousseau accessible"
                .into(),
        )
    })?;
    Ok(Box::new(EncryptedFileStore::with_passphrase(path, &pass)?))
}

// ------------------------------------------------------------------ bac à sable

pub struct SeatbeltSandbox;

impl Sandbox for SeatbeltSandbox {
    fn coverage(&self) -> Coverage {
        let present = Path::new("/usr/bin/sandbox-exec").is_file();
        Coverage {
            backend: "Seatbelt (sandbox-exec)".into(),
            files: present,
            network: present,
            processes: present,
            notes: if present {
                "profils générés à la volée".into()
            } else {
                "sandbox-exec absent : les commandes sous profil seront refusées".into()
            },
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
        if !Path::new("/usr/bin/sandbox-exec").is_file() {
            return Err(crate::sandbox::unsupported(profile, OS_NAME));
        }
        let sbpl = seatbelt_profile(profile);
        let dir = std::env::temp_dir().join("penelope-sandbox");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!(
            "{}-{}.sb",
            profile.kind.as_str(),
            crate::rand_hex(8)
        ));
        std::fs::write(&file, sbpl)?;

        let mut full = vec![
            "-f".to_string(),
            file.to_string_lossy().to_string(),
            program.to_string_lossy().to_string(),
        ];
        full.extend(args.iter().cloned());
        Ok(Wrapped {
            program: PathBuf::from("/usr/bin/sandbox-exec"),
            args: full,
            cleanup: Some(file),
        })
    }
}

pub fn sandbox() -> Box<dyn Sandbox> {
    Box::new(SeatbeltSandbox)
}

pub fn sandbox_wrapper(profile: &Profile, program: &Path, args: &[String]) -> Result<Wrapped> {
    SeatbeltSandbox.wrap(profile, program, args)
}

// ------------------------------------------------------------------ service

pub struct LaunchdService {
    plist: PathBuf,
    logs: PathBuf,
}

impl LaunchdService {
    pub fn new(dirs: &dyn Directories) -> Result<Self> {
        let home = home_dir().ok_or_else(|| PlatformError::NotFound("HOME non défini".into()))?;
        Ok(LaunchdService {
            plist: home
                .join("Library/LaunchAgents")
                .join(format!("{SERVICE_LABEL}.plist")),
            logs: dirs.logs(),
        })
    }

    fn uid() -> String {
        Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "0".into())
    }

    fn domain() -> String {
        format!("gui/{}", Self::uid())
    }

    fn launchctl(args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("/bin/launchctl").args(args).output()
    }
}

impl ServiceManager for LaunchdService {
    fn mechanism(&self) -> &'static str {
        "launchd (LaunchAgent utilisateur)"
    }

    fn install(&self, exe: &Path, home: Option<&Path>) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.logs)?;
        // Journaux privés : launchd crée stdout et stderr en 0644 s'ils n'existent pas, et
        // ils peuvent contenir des extraits de conversation (issue #26).
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::set_permissions(&self.logs, std::fs::Permissions::from_mode(0o700))?;
            for name in ["daemon.out.log", "daemon.err.log"] {
                let path = self.logs.join(name);
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(&path)?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        if let Some(p) = self.plist.parent() {
            std::fs::create_dir_all(p)?;
        }
        let path = crate::process::search_path();
        std::fs::write(
            &self.plist,
            launchd_plist(exe, home, &self.logs, &path.to_string_lossy()),
        )?;
        // `bootstrap` est l'API moderne ; `load -w` reste le repli sur les anciens macOS.
        let out = Self::launchctl(&["bootstrap", &Self::domain(), &self.plist.to_string_lossy()]);
        if out.map(|o| !o.status.success()).unwrap_or(true) {
            let _ = Self::launchctl(&["load", "-w", &self.plist.to_string_lossy()]);
        }
        Ok(self.plist.clone())
    }

    fn uninstall(&self) -> Result<()> {
        let target = format!("{}/{SERVICE_LABEL}", Self::domain());
        let _ = Self::launchctl(&["bootout", &target]);
        let _ = Self::launchctl(&["unload", "-w", &self.plist.to_string_lossy()]);
        if self.plist.exists() {
            std::fs::remove_file(&self.plist)?;
        }
        Ok(())
    }

    fn start(&self) -> Result<()> {
        let target = format!("{}/{SERVICE_LABEL}", Self::domain());
        // Après un `stop`, le service est déchargé : il faut le recharger avant de le lancer.
        let loaded = Self::launchctl(&["print", &target])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !loaded {
            if !self.plist.exists() {
                return Err(PlatformError::Service(
                    "service non installé : `penelope install`".into(),
                ));
            }
            let out =
                Self::launchctl(&["bootstrap", &Self::domain(), &self.plist.to_string_lossy()])
                    .map_err(|e| PlatformError::Service(e.to_string()))?;
            if !out.status.success() {
                return Err(PlatformError::Service(format!(
                    "launchctl bootstrap : {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            return Ok(());
        }
        let out = Self::launchctl(&["kickstart", "-k", &target])
            .map_err(|e| PlatformError::Service(e.to_string()))?;
        if !out.status.success() {
            return Err(PlatformError::Service(format!(
                "launchctl kickstart : {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        // `kill SIGTERM` ne suffit pas : avec `KeepAlive`, launchd relance aussitôt. On
        // décharge le service ; le plist reste en place, `start` le recharge.
        let target = format!("{}/{SERVICE_LABEL}", Self::domain());
        let out = Self::launchctl(&["bootout", &target])
            .map_err(|e| PlatformError::Service(e.to_string()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
            // Déjà arrêté : ce n'est pas une erreur.
            if !(err.contains("no such process") || err.contains("could not find")) {
                return Err(PlatformError::Service(format!(
                    "launchctl bootout : {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
        Ok(())
    }

    fn status(&self) -> Result<ServiceStatus> {
        let installed = self.plist.exists();
        let target = format!("{}/{SERVICE_LABEL}", Self::domain());
        let out = Self::launchctl(&["print", &target]).ok();
        let text = out
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let running = text.contains("state = running");
        let pid = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("pid = "))
            .and_then(|s| s.trim().parse().ok());
        Ok(ServiceStatus {
            installed,
            running,
            pid,
            mechanism: self.mechanism().into(),
            unit_path: installed.then(|| self.plist.clone()),
            detail: if installed {
                format!("plist : {}", self.plist.display())
            } else {
                "non installé (penelope install)".into()
            },
        })
    }
}

pub fn service_manager(dirs: &dyn Directories) -> Result<Box<dyn ServiceManager>> {
    Ok(Box::new(LaunchdService::new(dirs)?))
}

// ------------------------------------------------------------------ alimentation

pub fn power_manager() -> Box<dyn crate::power::PowerManager> {
    Box::new(crate::power::CountingPower::caffeinate())
}

// ------------------------------------------------------------------ doctor

/// Contrôles propres à macOS (§2.11). Chaque échec porte une commande corrective
/// **proposée**, jamais exécutée.
pub fn doctor_checks() -> Vec<crate::DoctorItem> {
    let mut v = Vec::new();

    // Mise en veille.
    let pmset = Command::new("/usr/bin/pmset")
        .args(["-g", "custom"])
        .output();
    match pmset {
        Ok(o) => {
            let t = String::from_utf8_lossy(&o.stdout);
            let sleep_zero = t
                .lines()
                .filter(|l| l.trim_start().starts_with("sleep"))
                .all(|l| l.split_whitespace().nth(1) == Some("0"));
            v.push(crate::DoctorItem {
                id: "macos.sleep".into(),
                label: "Mise en veille désactivée".into(),
                ok: sleep_zero,
                detail: if sleep_zero {
                    "sleep = 0".into()
                } else {
                    "le Mac peut se mettre en veille et interrompre les runs".into()
                },
                fix: (!sleep_zero).then(|| "sudo pmset -a sleep 0 disablesleep 1".to_string()),
            });
        }
        Err(e) => v.push(crate::DoctorItem {
            id: "macos.sleep".into(),
            label: "Mise en veille".into(),
            ok: false,
            detail: format!("pmset indisponible : {e}"),
            fix: None,
        }),
    }

    // FileVault : un redémarrage sans intervention exige `fdesetup authrestart`.
    let fv = Command::new("/usr/bin/fdesetup").arg("status").output();
    if let Ok(o) = fv {
        let t = String::from_utf8_lossy(&o.stdout);
        let on = t.contains("FileVault is On");
        v.push(crate::DoctorItem {
            id: "macos.filevault".into(),
            label: "Redémarrage sans intervention".into(),
            ok: !on,
            detail: if on {
                "FileVault actif : un redémarrage exige le déverrouillage".into()
            } else {
                "FileVault inactif".into()
            },
            fix: on.then(|| "sudo fdesetup authrestart".to_string()),
        });
    }

    // Accès distant (SSH). `systemsetup -getremotelogin` exige les droits
    // administrateur : on regarde plutôt si `sshd` écoute sur le port 22.
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], 22).into();
    let on =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok();
    v.push(crate::DoctorItem {
        id: "macos.remote_login".into(),
        label: "Accès distant (Remote Login)".into(),
        ok: on,
        detail: if on {
            "sshd écoute sur le port 22".into()
        } else {
            "aucun service SSH sur le port 22".into()
        },
        fix: (!on)
            .then(|| "Réglages Système → Général → Partage → Connexion à distance".to_string()),
    });

    // Bac à sable.
    let cov = SeatbeltSandbox.coverage();
    v.push(crate::DoctorItem {
        id: "macos.sandbox".into(),
        label: "Bac à sable".into(),
        ok: cov.files,
        detail: format!("{} — {}", cov.backend, cov.notes),
        fix: None,
    });

    // Trousseau.
    v.push(crate::DoctorItem {
        id: "macos.keychain".into(),
        label: "Trousseau".into(),
        ok: KeychainStore::available(),
        detail: if KeychainStore::available() {
            "/usr/bin/security disponible".into()
        } else {
            "Trousseau inaccessible : repli sur fichier chiffré".into()
        },
        fix: None,
    });

    // Secteur / batterie.
    if let Ok(o) = Command::new("/usr/bin/pmset").args(["-g", "batt"]).output() {
        let t = String::from_utf8_lossy(&o.stdout);
        let on_ac = t.contains("AC Power");
        v.push(crate::DoctorItem {
            id: "macos.power_source".into(),
            label: "Alimentation secteur".into(),
            ok: on_ac,
            detail: t.lines().next().unwrap_or("").trim().to_string(),
            fix: (!on_ac).then(|| "brancher la machine sur le secteur".to_string()),
        });
    }

    v
}
