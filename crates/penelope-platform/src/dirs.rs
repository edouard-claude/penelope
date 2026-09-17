//! Répertoires logiques (§2.2).
//!
//! Le code métier ne manipule **jamais** de chemin littéral : il demande `data()`,
//! `state()`, `logs()`… au backend. `PENELOPE_HOME` (ou `--home`) bascule l'ensemble
//! sous une racine unique, recommandé pour les serveurs administrés en SSH.

use crate::{PlatformError, Result};
use std::path::{Path, PathBuf};

/// Répertoire logique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    Config,
    Data,
    State,
    Logs,
    Cache,
}

impl Dir {
    pub fn as_str(&self) -> &'static str {
        match self {
            Dir::Config => "config",
            Dir::Data => "data",
            Dir::State => "state",
            Dir::Logs => "logs",
            Dir::Cache => "cache",
        }
    }
    pub const ALL: [Dir; 5] = [Dir::Config, Dir::Data, Dir::State, Dir::Logs, Dir::Cache];
}

pub trait Directories: Send + Sync {
    fn resolve(&self, dir: Dir) -> PathBuf;

    fn config(&self) -> PathBuf {
        self.resolve(Dir::Config)
    }
    fn data(&self) -> PathBuf {
        self.resolve(Dir::Data)
    }
    fn state(&self) -> PathBuf {
        self.resolve(Dir::State)
    }
    fn logs(&self) -> PathBuf {
        self.resolve(Dir::Logs)
    }
    fn cache(&self) -> PathBuf {
        self.resolve(Dir::Cache)
    }

    /// Crée tous les répertoires et sous-répertoires attendus par le PRD.
    fn ensure_all(&self) -> Result<()> {
        for d in Dir::ALL {
            std::fs::create_dir_all(self.resolve(d))?;
        }
        let data = self.data();
        for sub in [
            "artifacts",
            "vault",
            "skills",
            "workflows",
            "templates",
            "mcp.d",
            "backups",
        ] {
            std::fs::create_dir_all(data.join(sub))?;
        }
        let state = self.state();
        for sub in ["runs", "locks", "mcp-pids"] {
            std::fs::create_dir_all(state.join(sub))?;
        }
        Ok(())
    }

    fn db_path(&self) -> PathBuf {
        self.data().join("penelope.db")
    }
    fn config_file(&self) -> PathBuf {
        self.config().join("config.toml")
    }
    fn vault(&self) -> PathBuf {
        self.data().join("vault")
    }
    fn skills(&self) -> PathBuf {
        self.data().join("skills")
    }
    /// Skills livrées avec le binaire, réécrites à chaque chargement.
    fn bundled_skills(&self) -> PathBuf {
        self.state().join("skills-bundled")
    }
    fn workflows(&self) -> PathBuf {
        self.data().join("workflows")
    }
    fn templates(&self) -> PathBuf {
        self.data().join("templates")
    }
    fn mcp_d(&self) -> PathBuf {
        self.data().join("mcp.d")
    }
    fn artifacts(&self) -> PathBuf {
        self.data().join("artifacts")
    }
    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.state().join("runs").join(run_id)
    }
    fn socket_path(&self) -> PathBuf {
        self.state().join("rpc.sock")
    }
    fn pid_dir(&self) -> PathBuf {
        self.state().join("mcp-pids")
    }

    /// Développe `{data}`, `{config}`, `{state}`, `{logs}`, `{cache}` et `~` dans une
    /// valeur de configuration.
    fn expand(&self, raw: &str) -> PathBuf {
        let mut s = raw.to_string();
        for d in Dir::ALL {
            let token = format!("{{{}}}", d.as_str());
            if s.contains(&token) {
                s = s.replace(&token, &self.resolve(d).to_string_lossy());
            }
        }
        if let Some(rest) = s.strip_prefix("~/")
            && let Some(home) = home_dir()
        {
            return home.join(rest);
        }
        PathBuf::from(s)
    }
}

/// Répertoire personnel de l'utilisateur, sans dépendance externe.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            // Windows
            let up = std::env::var_os("USERPROFILE").filter(|s| !s.is_empty())?;
            Some(PathBuf::from(up))
        })
}

/// Résolution par racine unique (`PENELOPE_HOME` / `--home`).
#[derive(Debug, Clone)]
pub struct RootedDirs {
    root: PathBuf,
}

impl RootedDirs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        RootedDirs { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Directories for RootedDirs {
    fn resolve(&self, dir: Dir) -> PathBuf {
        self.root.join(dir.as_str())
    }
}

/// Construit la résolution effective : `PENELOPE_HOME` s'il est défini, sinon le
/// backend de l'OS courant.
pub fn resolve_directories(explicit_home: Option<PathBuf>) -> Result<Box<dyn Directories>> {
    if let Some(h) = explicit_home {
        return Ok(Box::new(RootedDirs::new(h)));
    }
    if let Some(h) = std::env::var_os("PENELOPE_HOME").filter(|s| !s.is_empty()) {
        return Ok(Box::new(RootedDirs::new(PathBuf::from(h))));
    }
    crate::backend::native_directories()
}

// ------------------------------------------------------------------ slugs

/// Noms réservés sous Windows : interdits même sur macOS, pour garder les vaults
/// portables (§2.2).
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Valide un slug de fichier (vault, skills, workflows) : `[a-z0-9-]`, ≤ 64, non réservé.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        return Err(PlatformError::InvalidSlug("slug vide".into()));
    }
    if slug.len() > 64 {
        return Err(PlatformError::InvalidSlug(format!(
            "slug trop long ({} > 64) : {slug}",
            slug.len()
        )));
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(PlatformError::InvalidSlug(format!(
            "caractères autorisés : [a-z0-9-] — reçu `{slug}`"
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(PlatformError::InvalidSlug(format!(
            "un slug ne peut pas commencer ni finir par `-` : `{slug}`"
        )));
    }
    if RESERVED.contains(&slug) {
        return Err(PlatformError::InvalidSlug(format!(
            "`{slug}` est un nom réservé sous Windows"
        )));
    }
    Ok(())
}

/// Transforme un texte libre en slug valide.
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in input.chars() {
        let c = deaccent(ch).to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 64 {
        out.truncate(64);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() || RESERVED.contains(&out.as_str()) {
        out.push_str("-x");
    }
    out
}

fn deaccent(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'î' | 'ï' | 'ì' | 'í' => 'i',
        'ô' | 'ö' | 'ò' | 'ó' | 'õ' => 'o',
        'ù' | 'û' | 'ü' | 'ú' => 'u',
        'ÿ' | 'ý' => 'y',
        'ñ' => 'n',
        'œ' => 'o',
        'æ' => 'a',
        other => other,
    }
}

/// Détection de collision **insensible à la casse** : APFS et NTFS le sont par défaut.
pub fn collides_case_insensitive(existing: &[String], candidate: &str) -> Option<String> {
    let c = candidate.to_lowercase();
    existing
        .iter()
        .find(|e| e.to_lowercase() == c && *e != candidate)
        .cloned()
}

/// Écrit un fichier texte en LF, de façon atomique (§2.2).
pub fn write_text_lf(path: &Path, content: &str) -> Result<()> {
    let normalised = content.replace("\r\n", "\n");
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp{}",
        path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, normalised.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_dirs_place_everything_under_one_root() {
        let d = RootedDirs::new("/tmp/pen");
        assert_eq!(d.config(), PathBuf::from("/tmp/pen/config"));
        assert_eq!(d.data(), PathBuf::from("/tmp/pen/data"));
        assert_eq!(d.state(), PathBuf::from("/tmp/pen/state"));
        assert_eq!(d.logs(), PathBuf::from("/tmp/pen/logs"));
        assert_eq!(d.cache(), PathBuf::from("/tmp/pen/cache"));
        assert_eq!(d.db_path(), PathBuf::from("/tmp/pen/data/penelope.db"));
        assert_eq!(d.socket_path(), PathBuf::from("/tmp/pen/state/rpc.sock"));
    }

    #[test]
    fn ensure_all_creates_the_prd_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let d = RootedDirs::new(tmp.path());
        d.ensure_all().unwrap();
        for p in [
            "data/vault",
            "data/skills",
            "data/workflows",
            "data/templates",
            "data/mcp.d",
            "data/artifacts",
            "data/backups",
            "state/runs",
            "state/locks",
            "state/mcp-pids",
            "logs",
            "cache",
            "config",
        ] {
            assert!(tmp.path().join(p).is_dir(), "manquant : {p}");
        }
    }

    #[test]
    fn expand_placeholders() {
        let d = RootedDirs::new("/r");
        assert_eq!(d.expand("{data}/vault"), PathBuf::from("/r/data/vault"));
        assert_eq!(d.expand("{logs}"), PathBuf::from("/r/logs"));
        assert_eq!(d.expand("/absolu/chemin"), PathBuf::from("/absolu/chemin"));
    }

    #[test]
    fn slug_rules() {
        validate_slug("langage-backend").unwrap();
        validate_slug("a1").unwrap();
        assert!(validate_slug("").is_err());
        assert!(validate_slug("Majuscule").is_err());
        assert!(validate_slug("avec espace").is_err());
        assert!(validate_slug("-debut").is_err());
        assert!(validate_slug("fin-").is_err());
        assert!(validate_slug("con").is_err(), "nom réservé Windows");
        assert!(validate_slug(&"a".repeat(65)).is_err());
    }

    #[test]
    fn slugify_handles_french() {
        assert_eq!(slugify("Déploiement critique"), "deploiement-critique");
        assert_eq!(slugify("Client X — 2026"), "client-x-2026");
        assert_eq!(slugify("   "), "-x");
        assert_eq!(slugify("NUL"), "nul-x");
        validate_slug(&slugify("Préférences de l'équipe (backend)")).unwrap();
    }

    #[test]
    fn case_insensitive_collision() {
        let existing = vec!["Client-X".to_string(), "autre".to_string()];
        assert_eq!(
            collides_case_insensitive(&existing, "client-x"),
            Some("Client-X".to_string())
        );
        assert!(collides_case_insensitive(&existing, "nouveau").is_none());
    }

    #[test]
    fn text_files_are_written_in_lf() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.md");
        write_text_lf(&p, "une\r\ndeux\r\n").unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(
            !raw.windows(2).any(|w| w == b"\r\n"),
            "aucun CRLF ne doit rester"
        );
    }

    #[test]
    fn explicit_home_wins() {
        let d = resolve_directories(Some(PathBuf::from("/explicite"))).unwrap();
        assert_eq!(d.data(), PathBuf::from("/explicite/data"));
    }
}
