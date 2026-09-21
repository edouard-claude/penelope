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
mod tests {
    use super::*;

    /// #95 : la valeur ne figure qu'en hexadécimal, sur l'entrée standard ; guillemets,
    /// barres obliques, espaces et `$` n'ont rien à échapper.
    #[test]
    fn a_secret_goes_through_stdin_in_hex() {
        let value = "a \"b\" \\c $HOME é";
        let line = add_command("penelope", "penelope.essai", value);
        assert!(!line.contains(value) && !line.contains("HOME"), "{line}");
        let hex = line
            .trim_end()
            .rsplit(' ')
            .next()
            .expect("hexadécimal en fin de commande")
            .to_string();
        assert_eq!(
            line,
            format!("add-generic-password -a penelope -s penelope.essai -U -X {hex}\n")
        );
        let back: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(String::from_utf8(back).unwrap(), value);
    }

    /// #95, sur la machine : écrit puis relit un secret d'essai dans le Trousseau de
    /// session, et vérifie qu'aucun processus ne l'a eu en argument. Touche au Trousseau
    /// du propriétaire : lancé à la main seulement.
    #[test]
    #[ignore]
    fn a_secret_round_trips_through_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let k = KeychainStore::new(dir.path());
        let value = "a \"b\" \\c $HOME fin";
        k.set("essai-issue-95", value).unwrap();
        assert_eq!(k.get("essai-issue-95").unwrap().as_deref(), Some(value));
        k.delete("essai-issue-95").unwrap();
    }

    /// #90 : le profil passe en argument ; aucun fichier n'est écrit, même après cent
    /// enveloppes.
    #[test]
    fn the_profile_goes_inline_and_no_file_is_written() {
        let dir = std::env::temp_dir().join("penelope-sandbox");
        let count = || std::fs::read_dir(&dir).map(|r| r.count()).unwrap_or(0);
        let before = count();
        let profile = Profile::workspace_write("/tmp/ws");
        let mut last = None;
        for _ in 0..100 {
            last = Some(sandbox_wrapper(&profile, Path::new("/bin/echo"), &["x".into()]).unwrap());
        }
        let w = last.unwrap();
        if Path::new("/usr/bin/sandbox-exec").is_file() {
            assert_eq!(w.program, PathBuf::from("/usr/bin/sandbox-exec"));
            assert_eq!(w.args[0], "-p");
            assert!(w.args[1].starts_with("(version 1)"), "{}", w.args[1]);
            assert_eq!(&w.args[2..], ["/bin/echo", "x"]);
        }
        assert_eq!(count(), before, "aucun fichier de profil");
    }

    /// #89 et #90, sur la machine : un processus confiné ne lit pas un chemin refusé,
    /// lit le reste, et le profil ne dépend d'aucun fichier qu'un voisin pourrait
    /// réécrire. Lancé à la main : `cargo test -p penelope-platform seatbelt -- --ignored`.
    #[test]
    #[ignore]
    fn seatbelt_enforces_denied_reads_on_this_mac() {
        let dir = tempfile::tempdir().unwrap();
        let secret_dir = dir.path().join("cles");
        std::fs::create_dir_all(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("id_ed25519"), "CLE PRIVEE").unwrap();
        std::fs::write(dir.path().join("public.txt"), "lisible").unwrap();
        let mut profile = Profile::mcp_stdio(dir.path().join("data"), Vec::new());
        profile.deny_read = vec![secret_dir.clone()];

        let run = |file: &Path| {
            let w = sandbox_wrapper(
                &profile,
                Path::new("/bin/cat"),
                &[file.to_string_lossy().to_string()],
            )
            .unwrap();
            std::process::Command::new(&w.program)
                .args(&w.args)
                .output()
                .unwrap()
        };
        let denied = run(&secret_dir.join("id_ed25519"));
        assert!(!denied.status.success());
        assert!(
            String::from_utf8_lossy(&denied.stderr).contains("Operation not permitted"),
            "{}",
            String::from_utf8_lossy(&denied.stderr)
        );
        let allowed = run(&dir.path().join("public.txt"));
        assert_eq!(String::from_utf8_lossy(&allowed.stdout), "lisible");
    }

    /// #122, sur la machine : sous un profil qui ferme le trousseau, même un certificat
    /// public du système est « introuvable » (c'est ce qui trompait le propriétaire) ; le
    /// même profil déclaré avec le trousseau le trouve. Seul le trousseau des racines du
    /// système est lu, jamais celui de l'utilisateur. Lancé à la main.
    #[test]
    #[ignore]
    fn seatbelt_opens_the_keychain_only_when_declared_on_this_mac() {
        let dir = tempfile::tempdir().unwrap();
        let find = |profile: &Profile| {
            let w = sandbox_wrapper(
                profile,
                Path::new("/usr/bin/security"),
                &[
                    "find-certificate".into(),
                    "-c".into(),
                    "Apple Root CA".into(),
                    "/System/Library/Keychains/SystemRootCertificates.keychain".into(),
                ],
            )
            .unwrap();
            std::process::Command::new(&w.program)
                .args(&w.args)
                .output()
                .unwrap()
        };
        let closed = Profile::mcp_stdio(dir.path().join("data"), Vec::new());
        let refused = find(&closed);
        assert!(!refused.status.success());
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("could not be found"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
        let found = find(&closed.clone().with_keychain(true));
        assert!(
            found.status.success(),
            "{}",
            String::from_utf8_lossy(&found.stderr)
        );
    }

    /// #91, sur la machine : sous `mcp-stdio` (réseau ouvert), une socket Unix locale
    /// n'est pas joignable ; la résolution de nom l'est. Lancé à la main.
    #[test]
    #[ignore]
    fn seatbelt_closes_unix_sockets_on_this_mac() {
        let dir = tempfile::Builder::new()
            .prefix("pnl")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("s.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let profile = Profile::mcp_stdio(dir.path().join("data"), Vec::new());
        let python = |code: String| {
            let w = sandbox_wrapper(
                &profile,
                Path::new("/usr/bin/python3"),
                &["-c".into(), code],
            )
            .unwrap();
            std::process::Command::new(&w.program)
                .args(&w.args)
                .output()
                .unwrap()
        };
        let connect = python(format!(
            "import socket;s=socket.socket(socket.AF_UNIX);s.connect({:?})",
            sock.to_string_lossy()
        ));
        assert!(!connect.status.success());
        assert!(
            String::from_utf8_lossy(&connect.stderr).contains("Operation not permitted"),
            "{}",
            String::from_utf8_lossy(&connect.stderr)
        );
        let dns = python("import socket;socket.getaddrinfo('localhost', 80)".into());
        assert!(
            dns.status.success(),
            "{}",
            String::from_utf8_lossy(&dns.stderr)
        );
    }

    /// #148 : le 20/09, `security` a recopié des morceaux de la ligne reçue — donc du
    /// secret — dans sa sortie d'erreur, et cette sortie est partie sur Telegram. Ici un
    /// faux `security` fait exactement cela : le message rendu ne doit rien en garder.
    #[test]
    fn a_failing_security_never_leaks_the_value_into_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("faux-security");
        std::fs::write(
            &fake,
            "#!/bin/sh\ncat >&2\necho 'security: unknown command' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let store = KeychainStore::with_program(dir.path(), &fake);
        let value = "jeton-tres-secret-0123456789";
        let err = store.set("essai", value).unwrap_err().to_string();

        assert!(!err.contains(value), "valeur en clair : {err}");
        let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
        for n in (8..=hex.len()).step_by(8) {
            assert!(
                !err.contains(&hex[..n]),
                "fragment hexadécimal de {n} caractères : {err}"
            );
        }
        assert!(!err.contains("unknown command"), "sortie recopiée : {err}");
        assert!(err.contains("Trousseau"), "{err}");
    }

    /// Un faux `security` **fidèle** : il range les items dans un dossier, et surtout il
    /// rend `-w` comme le vrai — en clair si le mot de passe est imprimable, **en
    /// hexadécimal sinon** (issue #157).
    ///
    /// C'est toute la leçon de ce lot : le faux `security` de #148 rendait la valeur telle
    /// quelle, donc la suite était verte pendant qu'un `Grant` de 8 Ko était relu faux sur
    /// la machine. Un double de test qui ment sur le point qui compte ne prouve rien.
    #[cfg(unix)]
    fn faithful_security(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let store = dir.join("items");
        std::fs::create_dir_all(&store).unwrap();
        let fake = dir.join("faux-security");
        // `-i` lit la ligne `add-generic-password … -X <hex>` sur l'entrée standard.
        let script = format!(
            r#"#!/usr/bin/env python3
import os, re, sys
STORE = {store:?}
def path(service):
    return os.path.join(STORE, service.replace("/", "_"))
args = sys.argv[1:]
if args[:1] == ["-i"]:
    args = sys.stdin.readline().split()
cmd = args[0] if args else ""
def opt(flag):
    return args[args.index(flag) + 1] if flag in args else ""
service = opt("-s")
if cmd == "add-generic-password":
    open(path(service), "wb").write(bytes.fromhex(opt("-X")))
    sys.exit(0)
if cmd == "find-generic-password":
    try:
        raw = open(path(service), "rb").read()
    except FileNotFoundError:
        sys.stderr.write("could not be found\n"); sys.exit(44)
    # Le vrai `security` : en clair si imprimable, sinon en hexadécimal.
    try:
        text = raw.decode("utf-8")
        printable = all(c == "\n" or c == "\t" or ord(c) >= 32 for c in text)
    except UnicodeDecodeError:
        printable = False
    sys.stdout.write(text if printable else raw.hex())
    sys.exit(0)
if cmd == "delete-generic-password":
    try:
        os.remove(path(service)); sys.exit(0)
    except FileNotFoundError:
        sys.exit(44)
sys.exit(1)
"#,
            store = store.to_string_lossy()
        );
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    /// #157 : un secret assez long pour être découpé était écrit juste et relu faux. La
    /// tête portait un caractère de contrôle, `security -w` la rendait en hexadécimal, et
    /// `get` rendait ces 42 caractères comme valeur du secret.
    #[cfg(unix)]
    #[test]
    fn a_chunked_secret_survives_a_security_that_prints_hex() {
        let dir = tempfile::tempdir().unwrap();
        let fake = faithful_security(dir.path());
        let store = KeychainStore::with_program(dir.path(), &fake);

        let value = format!("{{\"access_token\":\"{}\"}}", "e".repeat(8 * 1024));
        store.set("grant", &value).expect("écriture");
        let back = store.get("grant").expect("relecture");
        let relus = back.as_deref().map(str::len).unwrap_or(0);
        assert_eq!(
            back.as_deref(),
            Some(value.as_str()),
            "{} octets écrits, {relus} relus",
            value.len()
        );

        // La tête écrite est bien imprimable : c'est ce qui la fait revenir intacte.
        let head = store.read_item(&store.service("grant")).unwrap();
        assert_eq!(head, chunks::header(chunks::count(&head).unwrap()));
        assert!(
            head.chars().all(|c| !c.is_control()),
            "aucun caractère de contrôle dans la tête : {head:?}"
        );

        // Un secret court garde sa forme d'avant.
        store.set("court", "sk-abc").unwrap();
        assert_eq!(store.get("court").unwrap().as_deref(), Some("sk-abc"));
        store.delete("grant").unwrap();
        assert_eq!(store.get("grant").unwrap(), None);
    }

    /// #157 : un secret posé par une version antérieure porte l'ancienne marque, avec son
    /// caractère de contrôle. Il doit rester lisible après la mise à jour, sans migration.
    #[cfg(unix)]
    #[test]
    fn a_secret_written_with_the_old_mark_is_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let fake = faithful_security(dir.path());
        let store = KeychainStore::with_program(dir.path(), &fake);

        // Écrit à la main comme le faisait 0.17.35 : morceaux, puis tête marquée `\x01`.
        store
            .write_item(&store.chunk_service("ancien", 0), "début-")
            .unwrap();
        store
            .write_item(&store.chunk_service("ancien", 1), "fin")
            .unwrap();
        let old_head = format!("{}{}", chunks::LEGACY_MARK, 2);
        store
            .write_item(&store.service("ancien"), &old_head)
            .unwrap();

        // Le faux `security` la rend en hexadécimal, comme le vrai.
        assert_eq!(
            store.get("ancien").unwrap().as_deref(),
            Some("début-fin"),
            "l'ancienne marque reste lisible"
        );
    }

    /// #157 : une tête illisible alors que des morceaux existent ne doit **jamais** passer
    /// pour la valeur. Mieux vaut une erreur qu'un jeton faux qui rendra 401 plus tard.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_head_is_an_error_not_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let fake = faithful_security(dir.path());
        let store = KeychainStore::with_program(dir.path(), &fake);

        store
            .write_item(&store.chunk_service("casse", 0), "morceau")
            .unwrap();
        store
            .write_item(&store.service("casse"), "tête-abîmée")
            .unwrap();
        let err = store.get("casse").unwrap_err().to_string();
        assert!(err.contains("en-tête de morceaux illisible"), "{err}");

        // Sans morceau, la même tête est un secret ordinaire d'une version antérieure.
        store
            .write_item(&store.service("simple"), "tête-abîmée")
            .unwrap();
        assert_eq!(store.get("simple").unwrap().as_deref(), Some("tête-abîmée"));
    }

    /// #148 : le vrai Trousseau, avec un secret de 16 Ko. Écrit un item réel, donc
    /// `#[ignore]` : `cargo test -p penelope-platform -- --ignored keychain`.
    #[test]
    #[ignore = "écrit dans le Trousseau de l'utilisateur"]
    fn keychain_holds_a_long_secret_and_forgets_it() {
        if !KeychainStore::available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = KeychainStore::new(dir.path());
        let name = "penelope.test.gros-secret";
        let value = format!(
            "{{\"access_token\":\"{}\",\"refresh_token\":\"{}\"}}",
            "e".repeat(8 * 1024),
            "r".repeat(8 * 1024)
        );

        store.set(name, &value).expect("écriture");
        assert_eq!(store.get(name).unwrap().as_deref(), Some(value.as_str()));
        assert_eq!(store.list().unwrap(), [name], "un seul nom logique");

        // Réécriture plus courte : les morceaux de la version longue ne survivent pas.
        store.set(name, "court").expect("réécriture");
        assert_eq!(store.get(name).unwrap().as_deref(), Some("court"));
        assert!(
            store.read_item(&store.chunk_service(name, 0)).is_none(),
            "morceau resté derrière"
        );

        store.delete(name).expect("suppression");
        assert_eq!(store.get(name).unwrap(), None);
        assert!(store.list().unwrap().is_empty());
    }
}
