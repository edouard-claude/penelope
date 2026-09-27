//! Backend macOS (§2, seule implémentation livrée).

use crate::dirs::{Dir, Directories, home_dir};
use crate::sandbox::{Coverage, Profile, Sandbox, Wrapped, seatbelt_profile};
use crate::secrets::chunks::{self, MAX_CHUNKS, MAX_LINE_BYTES};
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

/// Chemin du binaire du Trousseau.
const SECURITY: &str = "/usr/bin/security";

/// Trousseau macOS piloté par `/usr/bin/security` (exécutable, jamais un shell).
pub struct KeychainStore {
    account: String,
    /// Index des noms (jamais les valeurs) : `dump-keychain` demanderait un
    /// déverrouillage interactif, impossible en headless.
    index: PathBuf,
    /// `/usr/bin/security`, sauf en test : de quoi vérifier que l'échec d'écriture ne
    /// recopie rien de la valeur (issue #148).
    program: PathBuf,
}

impl KeychainStore {
    pub fn new(data_dir: &Path) -> Self {
        KeychainStore {
            account: "penelope".into(),
            index: data_dir.join("secret-names.json"),
            program: PathBuf::from(SECURITY),
        }
    }

    #[cfg(test)]
    fn with_program(data_dir: &Path, program: &Path) -> Self {
        KeychainStore {
            program: program.to_path_buf(),
            ..Self::new(data_dir)
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
        Path::new(SECURITY).is_file()
    }
}

impl KeychainStore {
    /// Lit un item du trousseau, par son service exact.
    fn read_item(&self, service: &str) -> Option<String> {
        let out = Command::new(&self.program)
            .args([
                "find-generic-password",
                "-a",
                &self.account,
                "-s",
                service,
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let raw = out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())?;
        // `security -w` rend en hexadécimal tout mot de passe non imprimable (#157). Une
        // tête écrite par une version antérieure revient ainsi en 42 caractères ; elle
        // est décodée ici, et seulement si elle redonne bien une tête de morceaux.
        Some(chunks::decode_hex_head(&raw).unwrap_or(raw))
    }

    /// Écrit un item. **La sortie de `security` n'est jamais recopiée** : elle contient
    /// des morceaux de la ligne envoyée, donc du secret lui-même (issue #148).
    fn write_item(&self, service: &str, value: &str) -> Result<()> {
        use std::io::Write;
        let line = add_command(&self.account, service, value);
        if line.len() > MAX_LINE_BYTES {
            return Err(PlatformError::Secret(format!(
                "valeur trop longue pour le Trousseau ({} octets une fois en hexadécimal, \
                 maximum {MAX_LINE_BYTES} par commande)",
                line.len()
            )));
        }
        let mut child = Command::new(&self.program)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| PlatformError::Secret(format!("security : {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(line.as_bytes())
                .map_err(|e| PlatformError::Secret(format!("security : {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| PlatformError::Secret(format!("security : {e}")))?;
        if !out.status.success() {
            return Err(PlatformError::Secret(format!(
                "écriture dans le Trousseau refusée (security, code {}) ; trousseau \
                 verrouillé, ou item protégé par une autre application",
                out.status.code().unwrap_or(-1)
            )));
        }
        Ok(())
    }

    fn remove_item(&self, service: &str) -> bool {
        Command::new(&self.program)
            .args([
                "delete-generic-password",
                "-a",
                &self.account,
                "-s",
                service,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false)
    }

    fn chunk_service(&self, name: &str, i: usize) -> String {
        format!("{}#{i}", self.service(name))
    }
}

impl SecretStore for KeychainStore {
    fn backend(&self) -> String {
        "Trousseau macOS (security)".into()
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        let Some(head) = self.read_item(&self.service(name)) else {
            return Ok(None);
        };
        let Some(count) = chunks::count(&head) else {
            // Des morceaux existent : cette tête devrait en annoncer le nombre. Illisible,
            // elle ne doit surtout pas passer pour la valeur du secret — c'est ce que
            // faisait la version précédente, qui rendait 42 caractères d'hexadécimal à la
            // place d'un `Grant` de 8 Ko (issue #157).
            if self.read_item(&self.chunk_service(name, 0)).is_some() {
                return Err(PlatformError::Secret(format!(
                    "secret `{name}` : en-tête de morceaux illisible ({} caractères) alors \
                     que des morceaux existent ; la valeur n'est pas rendue plutôt que \
                     rendue fausse",
                    head.chars().count()
                )));
            }
            // Item unique écrit par une version antérieure : rendu tel quel.
            return Ok(Some(head));
        };
        let mut out = String::new();
        for i in 0..count {
            let service = self.chunk_service(name, i);
            let part = self.read_item(&service).ok_or_else(|| {
                PlatformError::Secret(format!(
                    "secret `{name}` incomplet : morceau {i} sur {count} introuvable"
                ))
            })?;
            out.push_str(&part);
        }
        Ok(Some(out))
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        crate::secrets::validate_secret_name(name)?;
        // `security -i` n'accepte que 4 096 octets par ligne : au-delà, le reste de la
        // ligne est relu comme des commandes, et `security` recopie ces morceaux — donc
        // le secret — dans sa sortie d'erreur (issue #148). Une valeur trop longue est
        // donc découpée, et chaque morceau tient dans sa propre commande.
        let head = self.service(name);
        let fits = add_command(&self.account, &head, value).len() <= MAX_LINE_BYTES
            && chunks::count(value).is_none();
        // Les morceaux d'une écriture précédente ne doivent pas survivre à celle-ci.
        self.drop_chunks(name);
        if fits {
            self.write_item(&head, value)?;
        } else {
            let parts = chunks::split(value, self.chunk_budget(name));
            for (i, part) in parts.iter().enumerate() {
                self.write_item(&self.chunk_service(name, i), part)?;
            }
            self.write_item(&head, &chunks::header(parts.len()))?;
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
        self.drop_chunks(name);
        self.remove_item(&self.service(name));
        let mut names = self.read_index();
        names.retain(|n| n != name);
        self.write_index(&names)
    }

    fn list(&self) -> Result<Vec<String>> {
        Ok(self.read_index())
    }
}

impl KeychainStore {
    /// Octets de valeur par morceau, d'après la longueur de la commande une fois le nom
    /// posé : l'hexadécimal double la taille.
    fn chunk_budget(&self, name: &str) -> usize {
        let overhead = add_command(&self.account, &self.chunk_service(name, MAX_CHUNKS), "").len();
        (MAX_LINE_BYTES - overhead) / 2
    }

    /// Retire les morceaux d'un secret, s'il en avait.
    fn drop_chunks(&self, name: &str) {
        let known = self
            .read_item(&self.service(name))
            .as_deref()
            .and_then(chunks::count);
        // Sans en-tête lisible, on balaie jusqu'au premier manquant : un secret écrit
        // puis interrompu ne doit pas laisser de morceaux derrière lui.
        let upper = known.unwrap_or(MAX_CHUNKS);
        for i in 0..upper {
            if !self.remove_item(&self.chunk_service(name, i)) && known.is_none() {
                break;
            }
        }
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
            });
        }
        if !Path::new("/usr/bin/sandbox-exec").is_file() {
            return Err(crate::sandbox::unsupported(profile, OS_NAME));
        }
        // Le profil passe en argument (`-p`), jamais par un fichier : un fichier dans le
        // dossier temporaire, que les profils autorisent en écriture, pouvait être réécrit
        // par un processus déjà confiné avant que `sandbox-exec` ne le lise (issue #90).
        // Quelques Kio, loin d'`ARG_MAX` (1 Mio), sans shell entre les deux.
        let mut full = vec![
            "-p".to_string(),
            seatbelt_profile(profile),
            program.to_string_lossy().to_string(),
        ];
        full.extend(args.iter().cloned());
        Ok(Wrapped {
            program: PathBuf::from("/usr/bin/sandbox-exec"),
            args: full,
        })
    }
}

/// Commande `add-generic-password` pour `security -i`, valeur en hexadécimal. Compte et
/// service sont des noms validés (`[A-Za-z0-9_.-]`, plus `#` pour les morceaux) : aucun
/// guillemet à poser.
fn add_command(account: &str, service: &str, value: &str) -> String {
    let hex: String = value
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("add-generic-password -a {account} -s {service} -U -X {hex}\n")
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

#[cfg(test)]
mod tests;
