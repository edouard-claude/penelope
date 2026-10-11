//! Collecte des racines de la sauvegarde : copie d'arbres qui ne suit aucun lien
//! symbolique (#327) et laisse hors de `data/workspace` ce qui se régénère (#341).
//!
//! Sous le workspace, un chemin est écarté par la première règle qui le nomme, de la plus
//! proche à la plus lointaine :
//!
//! ```text
//!  dossier courant, puis ses parents jusqu'au workspace :
//!    .penelopeignore   (partout)
//!    .gitignore        (dans un dépôt Git seulement, comme Git)
//!    .git/info/exclude (à la racine d'un dépôt)
//!  puis, à la racine du workspace :
//!    backup.exclude    (configuration)
//!    caches reconnus   (DEFAULT_EXCLUDES, `target/` à côté d'un Cargo.toml)
//! ```
//!
//! Un `!motif` réinclut, comme dans Git. Rien n'est écarté sous un dossier `.git` :
//! l'historique local ne se régénère pas. Le vault, `mcp-data` et les autres racines sont
//! copiés en entier.

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::{Value, json};
use std::path::Path;

/// Caches et sorties de build reconnus, écartés du workspace avec ou sans dépôt Git :
/// tous se reconstruisent depuis les sources (`npm install`, `go build`, `pip install`…).
/// Sur l'instance du 11/10, les `.gocache` des serveurs MCP pesaient 475 Mo sans qu'aucun
/// `.gitignore` ne les nomme.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules/",
    ".gocache/",
    ".gomodcache/",
    ".venv/",
    "venv/",
    "__pycache__/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".ruff_cache/",
    ".tox/",
    ".next/",
    ".nuxt/",
    ".svelte-kit/",
    ".turbo/",
    ".parcel-cache/",
    ".gradle/",
];

/// Fichier d'exclusions propre à la sauvegarde, lu comme un `.gitignore` là où il est.
pub const IGNORE_FILE: &str = ".penelopeignore";

/// Ce que la collecte a laissé de côté : chemin relatif, volume, règle qui l'a écarté.
#[derive(Debug, Clone)]
pub(crate) struct Excluded {
    pub path: String,
    pub bytes: u64,
    pub rule: String,
}

/// Les règles fixes du workspace : `backup.exclude`, puis les caches reconnus.
pub(crate) struct Rules {
    user: Gitignore,
    defaults: Gitignore,
}

impl Rules {
    /// `root` : le workspace ; `patterns` : `backup.exclude`. Un motif illisible est une
    /// erreur de configuration, dite avant toute copie.
    pub(crate) fn new(root: &Path, patterns: &[String]) -> anyhow::Result<Self> {
        let mut user = GitignoreBuilder::new(root);
        for p in patterns {
            user.add_line(None, p)
                .map_err(|e| anyhow::anyhow!("`backup.exclude` : motif `{p}` illisible : {e}"))?;
        }
        let mut defaults = GitignoreBuilder::new(root);
        for p in DEFAULT_EXCLUDES {
            defaults.add_line(None, p)?;
        }
        Ok(Rules {
            user: user.build()?,
            defaults: defaults.build()?,
        })
    }
}

/// Les règles d'un dossier et de ses parents, de la plus proche à la plus lointaine.
#[derive(Clone, Default)]
struct Scope {
    levels: Vec<Gitignore>,
    in_repo: bool,
    /// Sous un `.git` : rien n'y est écarté.
    raw: bool,
}

impl Scope {
    /// La portée des enfants de `dir` : ses fichiers d'exclusions s'ajoutent en tête.
    fn enter(&self, dir: &Path) -> Scope {
        let mut next = self.clone();
        if self.raw {
            return next;
        }
        if dir.file_name().is_some_and(|n| n == ".git") {
            next.raw = true;
            return next;
        }
        let git = std::fs::symlink_metadata(dir.join(".git")).ok();
        next.in_repo |= git.is_some();
        // Dans un même jeu de règles, la dernière qui nomme un chemin l'emporte : du moins
        // prioritaire au plus prioritaire, `info/exclude`, `.gitignore`, `.penelopeignore`.
        let mut found = Vec::new();
        if git.is_some_and(|m| m.is_dir()) {
            found.push(dir.join(".git/info/exclude"));
        }
        if next.in_repo {
            found.push(dir.join(".gitignore"));
        }
        found.push(dir.join(IGNORE_FILE));
        let mut b = GitignoreBuilder::new(dir);
        let mut any = false;
        // Un fichier de règles en lien n'est pas suivi, comme le reste de la collecte.
        let plain_file =
            |f: &&std::path::PathBuf| std::fs::symlink_metadata(f).is_ok_and(|m| m.is_file());
        for f in found.iter().filter(plain_file) {
            // Une ligne illisible d'un `.gitignore` est ignorée, comme Git le fait ; les
            // autres valent.
            let _ = b.add(f);
            any = true;
        }
        if any && let Ok(g) = b.build() {
            next.levels.insert(0, g);
        }
        next
    }
}

/// Copie d'arbres bornée, qui ne suit aucun lien symbolique.
///
/// Une racine déclarée (le vault, `data/workspace`…) est prise telle que la configuration
/// la désigne ; **sous** elle, un lien symbolique n'est jamais suivi : il est noté
/// (`skipped`), ni copié ni recréé. Rien n'entre ainsi de hors des racines, et une boucle
/// de liens ne peut pas tourner. Les sockets et les tubes (un pont MCP en laisse dans
/// `mcp-data`) sont notés de même, leur serveur les recrée.
///
/// `dry` : rien n'est copié, les volumes sont comptés (l'estimation de l'espace disque).
pub(crate) struct Collect {
    pub bytes: u64,
    pub skipped: Vec<String>,
    pub excluded: Vec<Excluded>,
    patterns: Vec<String>,
    dry: bool,
}

impl Collect {
    /// Une collecte qui copie ; `patterns` : `backup.exclude`.
    pub(crate) fn new(patterns: &[String]) -> Self {
        Collect {
            bytes: 0,
            skipped: Vec::new(),
            excluded: Vec::new(),
            patterns: patterns.to_vec(),
            dry: false,
        }
    }

    /// Une collecte qui compte sans copier.
    pub(crate) fn dry(patterns: &[String]) -> Self {
        Collect {
            dry: true,
            ..Collect::new(patterns)
        }
    }

    /// Copie la racine `src` vers `dst` ; `label` la nomme dans `skipped`. Seules les
    /// racines que la configuration désigne (le vault de `memory.vault_path`,
    /// `config.toml`) peuvent être elles-mêmes un lien : le propriétaire l'a voulu.
    /// `data/workspace` ou `data/mcp-data` en lien ne sont pas suivis. Seul `workspace`
    /// passe par les exclusions.
    pub(crate) fn copy_root(&mut self, src: &Path, dst: &Path, label: &str) -> anyhow::Result<()> {
        let follow = matches!(label, "vault" | "config.toml");
        let meta = if follow {
            std::fs::metadata(src)
        } else {
            std::fs::symlink_metadata(src)
        };
        let meta = match meta {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(anyhow::anyhow!("copie de {} : {e}", src.display())),
        };
        let rules = if label == "workspace" && meta.is_dir() {
            Some(Rules::new(src, &self.patterns)?)
        } else {
            None
        };
        let walk = Walk {
            rules: rules.as_ref(),
            scope: Scope::default(),
        };
        self.copy(src, meta, dst, label, &walk)
            .map_err(|e| anyhow::anyhow!("copie de {} : {e}", src.display()))
    }

    /// `meta` : ce qu'est `src` lui-même, lien compris (`symlink_metadata`) ; un lien,
    /// une socket ou un tube est noté et laissé.
    fn copy(
        &mut self,
        src: &Path,
        meta: std::fs::Metadata,
        dst: &Path,
        rel: &str,
        walk: &Walk,
    ) -> anyhow::Result<()> {
        if meta.is_file() {
            self.bytes += meta.len();
            if !self.dry {
                if let Some(p) = dst.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::copy(src, dst)?;
            }
            return Ok(());
        }
        if !meta.is_dir() {
            self.skipped.push(rel.to_string());
            return Ok(());
        }
        if !self.dry {
            std::fs::create_dir_all(dst)?;
        }
        let inner = Walk {
            rules: walk.rules,
            scope: if walk.rules.is_some() {
                walk.scope.enter(src)
            } else {
                Scope::default()
            },
        };
        for e in std::fs::read_dir(src)?.flatten() {
            let path = e.path();
            let name = e.file_name();
            let child_rel = format!("{rel}/{}", name.to_string_lossy());
            let m = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                // Disparu entre la lecture du dossier et la copie : rien à garder.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if let Some(rule) = inner.excludes(&path, m.is_dir(), src) {
                self.excluded.push(Excluded {
                    bytes: size_of(&path, &m),
                    path: child_rel,
                    rule,
                });
                continue;
            }
            self.copy(&path, m, &dst.join(&name), &child_rel, &inner)?;
        }
        Ok(())
    }

    /// Le volume écarté, et l'entrée du manifeste qui le dit (#341) : les plus gros
    /// d'abord, cinquante au plus.
    pub(crate) fn excluded_entry(&self) -> Option<Value> {
        if self.excluded.is_empty() {
            return None;
        }
        let mut items = self.excluded.clone();
        items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
        let bytes: u64 = items.iter().map(|e| e.bytes).sum();
        Some(json!({
            "name": "workspace",
            "why": "régénérable : ignoré par un `.gitignore` de dépôt, un `.penelopeignore`, \
                    `backup.exclude` ou cache de build reconnu ; à reconstruire depuis les \
                    sources (`cargo build`, `npm install`, `go build`…)",
            "bytes": bytes,
            "count": items.len(),
            "items": items.iter().take(50).map(|e| json!({
                "path": e.path, "bytes": e.bytes, "rule": e.rule,
            })).collect::<Vec<_>>(),
        }))
    }
}

/// L'état du parcours d'un dossier : règles fixes du workspace et portée courante.
struct Walk<'a> {
    rules: Option<&'a Rules>,
    scope: Scope,
}

impl Walk<'_> {
    /// La règle qui écarte `path` (enfant de `parent`), ou `None` s'il est gardé.
    fn excludes(&self, path: &Path, is_dir: bool, parent: &Path) -> Option<String> {
        let rules = self.rules?;
        if self.scope.raw {
            return None;
        }
        let decide = |m: Match<&ignore::gitignore::Glob>| match m {
            Match::None => None,
            Match::Whitelist(_) => Some(None),
            Match::Ignore(g) => Some(Some(describe(g))),
        };
        for level in &self.scope.levels {
            if let Some(d) = decide(level.matched(path, is_dir)) {
                return d;
            }
        }
        if let Some(d) = decide(rules.user.matched(path, is_dir)) {
            return d;
        }
        if let Some(d) = decide(rules.defaults.matched(path, is_dir)) {
            return d.map(|r| format!("cache reconnu : {r}"));
        }
        let is_cargo_target = is_dir
            && path.file_name().is_some_and(|n| n == "target")
            && parent.join("Cargo.toml").is_file();
        is_cargo_target.then(|| "cache reconnu : target/ à côté d'un Cargo.toml".to_string())
    }
}

/// `fichier : motif`, ou le motif seul pour une règle de la configuration.
fn describe(g: &ignore::gitignore::Glob) -> String {
    match g.from() {
        Some(f) => format!(
            "{} : {}",
            f.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            g.original()
        ),
        None => g.original().to_string(),
    }
}

/// Volume d'un chemin écarté, sans suivre de lien.
fn size_of(path: &Path, meta: &std::fs::Metadata) -> u64 {
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            std::fs::symlink_metadata(&p).ok().map(|m| size_of(&p, &m))
        })
        .sum()
}

/// Copie récursive sans exclusion ni lien suivi : la restauration, depuis l'arbre extrait.
pub(crate) fn copy_path(src: &Path, dst: &Path) -> anyhow::Result<()> {
    Collect::new(&[]).copy_root(src, dst, "")
}

/// Ce qu'il faut de libre pour sauvegarder `collected` octets : la copie de travail,
/// le `tar.gz` et l'archive chiffrée coexistent un moment, au pire chacun de la taille
/// du clair, plus une marge de 256 Mo.
pub(crate) fn space_needed(collected: u64) -> u64 {
    collected
        .saturating_mul(3)
        .saturating_add(256 * 1024 * 1024)
}

/// Refuse une sauvegarde que le disque ne tiendrait pas, avant toute copie (#341) ; sans
/// mesure (`free` à `None`), on tente.
pub(crate) fn check_space(collected: u64, free: Option<u64>, dir: &Path) -> anyhow::Result<()> {
    let needed = space_needed(collected);
    match free {
        Some(free) if free < needed => anyhow::bail!(
            "espace disque insuffisant dans {} : {} à sauvegarder demandent {} libres, {} \
             disponibles ; il manque {} (rien n'a été copié)",
            dir.display(),
            mo(collected),
            mo(needed),
            mo(free),
            mo(needed - free)
        ),
        _ => Ok(()),
    }
}

fn mo(bytes: u64) -> String {
    format!("{} Mo", bytes.div_ceil(1024 * 1024))
}
