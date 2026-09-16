//! Profils de bac à sable logiques (§2.5, §13.2).
//!
//! Trois profils identiques sur tous les OS : `readonly`, `workspace-write`, `mcp-stdio`.
//! `full` n'est atteignable que par configuration explicite, serveur par serveur.
//!
//! Un profil **non applicable échoue fermé** : la commande est refusée, sauf dérogation
//! explicite par outil dans `policies.toml`, tracée dans l'audit.

use crate::{PlatformError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileKind {
    ReadOnly,
    WorkspaceWrite,
    McpStdio,
    Full,
}

impl ProfileKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProfileKind::ReadOnly => "readonly",
            ProfileKind::WorkspaceWrite => "workspace-write",
            ProfileKind::McpStdio => "mcp-stdio",
            ProfileKind::Full => "full",
        }
    }
    pub fn parse(s: &str) -> Option<ProfileKind> {
        Some(match s {
            "readonly" => ProfileKind::ReadOnly,
            "workspace-write" => ProfileKind::WorkspaceWrite,
            "mcp-stdio" => ProfileKind::McpStdio,
            "full" => ProfileKind::Full,
            _ => return None,
        })
    }
}

/// Profil concret : le type logique, plus les chemins effectivement concernés.
#[derive(Debug, Clone)]
pub struct Profile {
    pub kind: ProfileKind,
    /// Répertoires en écriture (workspace, tmp, répertoire de données du serveur).
    pub writable: Vec<PathBuf>,
    /// Répertoires en lecture au-delà des chemins système.
    pub readable: Vec<PathBuf>,
    pub allow_network: bool,
    /// Dérogation explicite : le profil n'est pas appliqué, mais l'audit le sait.
    pub waived: bool,
}

impl Profile {
    pub fn read_only() -> Self {
        Profile {
            kind: ProfileKind::ReadOnly,
            writable: Vec::new(),
            readable: Vec::new(),
            allow_network: false,
            waived: false,
        }
    }

    pub fn workspace_write(workspace: impl Into<PathBuf>) -> Self {
        Profile {
            kind: ProfileKind::WorkspaceWrite,
            writable: vec![workspace.into(), std::env::temp_dir()],
            readable: Vec::new(),
            allow_network: false,
            waived: false,
        }
    }

    pub fn mcp_stdio(data_dir: impl Into<PathBuf>, readable: Vec<PathBuf>) -> Self {
        Profile {
            kind: ProfileKind::McpStdio,
            writable: vec![data_dir.into(), std::env::temp_dir()],
            readable,
            allow_network: true,
            waived: false,
        }
    }

    pub fn full() -> Self {
        Profile {
            kind: ProfileKind::Full,
            writable: Vec::new(),
            readable: Vec::new(),
            allow_network: true,
            waived: false,
        }
    }

    pub fn with_network(mut self, yes: bool) -> Self {
        self.allow_network = yes;
        self
    }

    pub fn waive(mut self) -> Self {
        self.waived = true;
        self
    }

    /// Vrai si ce profil doit être imposé par le backend.
    pub fn enforced(&self) -> bool {
        !self.waived && self.kind != ProfileKind::Full
    }
}

/// Commande réécrite pour s'exécuter sous le bac à sable.
#[derive(Debug, Clone)]
pub struct Wrapped {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Fichier temporaire de profil à supprimer après exécution, s'il y en a un.
    pub cleanup: Option<PathBuf>,
}

/// Couverture réellement obtenue sur cet OS (`penelope doctor` l'affiche, §2.5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Coverage {
    pub backend: String,
    pub files: bool,
    pub network: bool,
    pub processes: bool,
    pub notes: String,
}

pub trait Sandbox: Send + Sync {
    fn coverage(&self) -> Coverage;
    /// Réécrit une commande pour l'exécuter sous le profil. Échoue si le profil ne peut
    /// pas être appliqué (échec fermé).
    fn wrap(
        &self,
        profile: &Profile,
        program: &std::path::Path,
        args: &[String],
    ) -> Result<Wrapped>;
}

/// Le chemin tel que donné, plus sa forme résolue quand elle diffère.
///
/// Seatbelt compare les chemins réels : `/var/folders/…` (dossier temporaire) ou `/tmp`
/// sont des liens vers `/private/…`, et une règle écrite sur le lien n'autorise rien.
/// Pour un chemin qui n'existe pas encore, on résout son plus proche ancêtre existant.
fn with_real_path(p: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_path_buf()];
    let mut existing = p;
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut full = real;
            for part in rest.iter().rev() {
                full.push(part);
            }
            if full != p {
                out.push(full);
            }
            return out;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return out,
        }
    }
}

/// Génère un profil Seatbelt (SBPL) pour macOS.
///
/// Le profil part d'un refus global, puis autorise le strict nécessaire. Les chemins sont
/// insérés sous forme de littéraux `subpath`, avec échappement des guillemets.
pub fn seatbelt_profile(p: &Profile) -> String {
    let mut s = String::from("(version 1)\n(deny default)\n");
    s.push_str("(allow process-exec)\n(allow process-fork)\n(allow sysctl-read)\n");
    s.push_str("(allow mach-lookup)\n(allow signal (target self))\n");
    // Lecture : nécessaire pour charger les bibliothèques et lire le code.
    s.push_str("(allow file-read* (subpath \"/usr\") (subpath \"/bin\") (subpath \"/sbin\")\n");
    s.push_str("  (subpath \"/System\") (subpath \"/Library\") (subpath \"/opt\")\n");
    s.push_str("  (subpath \"/private/var/db\") (subpath \"/dev\") (literal \"/\"))\n");

    for r in p.readable.iter().flat_map(|r| with_real_path(r)) {
        s.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", esc(&r)));
    }
    if p.kind == ProfileKind::ReadOnly {
        // Lecture du répertoire courant autorisée, aucune écriture.
        s.push_str("(allow file-read*)\n");
    } else {
        s.push_str("(allow file-read*)\n");
        for w in p.writable.iter().flat_map(|w| with_real_path(w)) {
            s.push_str(&format!(
                "(allow file-write* file-read* (subpath \"{}\"))\n",
                esc(&w)
            ));
        }
        // /dev/null et consorts, indispensables à tout processus.
        s.push_str(
            "(allow file-write-data (literal \"/dev/null\") (literal \"/dev/dtracehelper\"))\n",
        );
    }

    if p.allow_network {
        s.push_str("(allow network*)\n");
    } else {
        s.push_str("(deny network*)\n");
    }
    s
}

fn esc(p: &std::path::Path) -> String {
    p.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Vérifie qu'un chemin est bien sous l'un des répertoires autorisés en écriture.
/// Utilisé en défense en profondeur par les outils `fs_write` avant même le bac à sable.
pub fn is_within(path: &std::path::Path, roots: &[PathBuf]) -> bool {
    let canon = normalise(path);
    roots.iter().any(|r| canon.starts_with(normalise(r)))
}

/// Normalise un chemin sans toucher au disque : résout `.` et `..` textuellement.
///
/// Ne suit pas les liens symboliques (le bac à sable de l'OS s'en charge) ; sert à
/// rejeter tôt les `../../etc/passwd` évidents.
pub fn normalise(p: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn unsupported(profile: &Profile, os: &str) -> PlatformError {
    PlatformError::Unsupported(format!(
        "le profil de bac à sable `{}` n'est pas implémenté sur {os} : commande refusée \
         (échec fermé). Ajouter une dérogation explicite dans policies.toml pour passer outre.",
        profile.kind.as_str()
    ))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn symlinked_workspaces_are_allowed_by_their_real_path() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("reel");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("lien");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let profile = seatbelt_profile(&Profile::workspace_write(link.join("run-1")));
        let resolved = real.canonicalize().unwrap().join("run-1");
        assert!(
            profile.contains(&format!("(subpath \"{}\")", resolved.display())),
            "le chemin réel d'un dossier encore inexistant est autorisé :\n{profile}"
        );
        assert!(profile.contains(&format!("(subpath \"{}\")", link.join("run-1").display())));
    }

    use super::*;
    use std::path::Path;

    #[test]
    fn profile_kinds_roundtrip() {
        for k in [
            ProfileKind::ReadOnly,
            ProfileKind::WorkspaceWrite,
            ProfileKind::McpStdio,
            ProfileKind::Full,
        ] {
            assert_eq!(ProfileKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn full_profile_is_not_enforced() {
        assert!(!Profile::full().enforced());
        assert!(Profile::workspace_write("/w").enforced());
        assert!(!Profile::workspace_write("/w").waive().enforced());
    }

    #[test]
    fn seatbelt_denies_by_default_and_allows_workspace() {
        let p = Profile::workspace_write("/tmp/ws");
        let s = seatbelt_profile(&p);
        assert!(s.starts_with("(version 1)\n(deny default)"));
        assert!(s.contains("(allow file-write* file-read* (subpath \"/tmp/ws\"))"));
        assert!(s.contains("(deny network*)"));
    }

    #[test]
    fn seatbelt_readonly_has_no_write_rule() {
        let s = seatbelt_profile(&Profile::read_only());
        assert!(!s.contains("file-write*"), "{s}");
    }

    #[test]
    fn seatbelt_escapes_quotes_in_paths() {
        let p = Profile::workspace_write(PathBuf::from("/tmp/a\"b"));
        assert!(seatbelt_profile(&p).contains("/tmp/a\\\"b"));
    }

    #[test]
    fn network_toggle() {
        assert!(seatbelt_profile(&Profile::mcp_stdio("/d", vec![])).contains("(allow network*)"));
        assert!(
            seatbelt_profile(&Profile::workspace_write("/w").with_network(true))
                .contains("(allow network*)")
        );
    }

    #[test]
    fn path_containment_rejects_traversal() {
        let roots = vec![PathBuf::from("/tmp/ws")];
        assert!(is_within(Path::new("/tmp/ws/a/b.txt"), &roots));
        assert!(!is_within(Path::new("/tmp/autre/b.txt"), &roots));
        assert!(
            !is_within(Path::new("/tmp/ws/../etc/passwd"), &roots),
            "la traversée doit être rejetée avant même le bac à sable"
        );
    }

    #[test]
    fn normalise_resolves_dots() {
        assert_eq!(
            normalise(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }
}
