//! Surveillance de fichiers (§2.9).
//!
//! Debounce de 300 ms et **resynchronisation complète périodique** (10 min par défaut),
//! pour couvrir les pertes d'événements propres à chaque backend.
//!
//! Décision d'architecture : scrutation des horodatages plutôt que `notify`
//! (`docs/decisions/0005-watcher-par-scrutation.md`). Les volumes surveillés sont petits
//! (vault, skills, workflows, templates, mcp.d), la scrutation est déterministe donc
//! testable avec une horloge fictive, et le PRD exige de toute façon la resynchronisation
//! complète périodique qui en fait le mécanisme de vérité.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
}

impl Change {
    pub fn path(&self) -> &Path {
        match self {
            Change::Created(p) | Change::Modified(p) | Change::Removed(p) => p,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    mtime: SystemTime,
    len: u64,
}

/// État d'un arbre surveillé.
pub struct TreeWatcher {
    roots: Vec<PathBuf>,
    /// Extensions retenues ; vide = toutes.
    extensions: Vec<String>,
    /// Segments de chemin ignorés (`.git`, `.dreams`, `archive`).
    excluded: Vec<String>,
    snapshot: BTreeMap<PathBuf, Stamp>,
}

impl TreeWatcher {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        TreeWatcher {
            roots,
            extensions: Vec::new(),
            excluded: vec![".git".into(), ".dreams".into(), "archive".into()],
            snapshot: BTreeMap::new(),
        }
    }

    pub fn extensions(mut self, exts: &[&str]) -> Self {
        self.extensions = exts.iter().map(|s| s.to_lowercase()).collect();
        self
    }

    pub fn exclude(mut self, segments: &[&str]) -> Self {
        self.excluded = segments.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Premier passage : mémorise l'état sans produire de changement.
    pub fn prime(&mut self) {
        self.snapshot = self.scan();
    }

    /// Recalcule et renvoie les changements depuis le dernier appel.
    pub fn poll(&mut self) -> Vec<Change> {
        let current = self.scan();
        let mut out = Vec::new();
        for (p, st) in &current {
            match self.snapshot.get(p) {
                None => out.push(Change::Created(p.clone())),
                Some(old) if old != st => out.push(Change::Modified(p.clone())),
                _ => {}
            }
        }
        for p in self.snapshot.keys() {
            if !current.contains_key(p) {
                out.push(Change::Removed(p.clone()));
            }
        }
        self.snapshot = current;
        out
    }

    pub fn tracked_count(&self) -> usize {
        self.snapshot.len()
    }

    fn scan(&self) -> BTreeMap<PathBuf, Stamp> {
        let mut out = BTreeMap::new();
        for root in &self.roots {
            self.walk(root, &mut out, 0);
        }
        out
    }

    fn walk(&self, dir: &Path, out: &mut BTreeMap<PathBuf, Stamp>, depth: usize) {
        if depth > 12 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if self.excluded.iter().any(|x| x == &name) {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                self.walk(&path, out, depth + 1);
                continue;
            }
            if !self.extensions.is_empty() {
                let ok = path
                    .extension()
                    .map(|x| {
                        self.extensions
                            .contains(&x.to_string_lossy().to_lowercase())
                    })
                    .unwrap_or(false);
                if !ok {
                    continue;
                }
            }
            if let Ok(meta) = e.metadata() {
                out.insert(
                    path,
                    Stamp {
                        mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        len: meta.len(),
                    },
                );
            }
        }
    }
}

/// Fenêtre de debounce : regroupe les changements d'un même fichier.
pub struct Debouncer {
    window: Duration,
    pending: BTreeMap<PathBuf, (Change, std::time::Instant)>,
}

impl Debouncer {
    pub fn new(window: Duration) -> Self {
        Debouncer {
            window,
            pending: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, c: Change, now: std::time::Instant) {
        self.pending.insert(c.path().to_path_buf(), (c, now));
    }

    /// Renvoie les changements dont la fenêtre est écoulée.
    pub fn drain_ready(&mut self, now: std::time::Instant) -> Vec<Change> {
        let window = self.window;
        let ready: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, (_, t))| now.duration_since(*t) >= window)
            .map(|(p, _)| p.clone())
            .collect();
        ready
            .into_iter()
            .filter_map(|p| self.pending.remove(&p).map(|(c, _)| c))
            .collect()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Intervalles par défaut du PRD.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const FULL_RESYNC: Duration = Duration::from_secs(600);

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, content: &str) {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn detects_create_modify_remove() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = TreeWatcher::new(vec![dir.path().to_path_buf()]).extensions(&["md"]);
        w.prime();
        assert!(w.poll().is_empty());

        let f = dir.path().join("a.md");
        touch(&f, "un");
        let c = w.poll();
        assert_eq!(c, vec![Change::Created(f.clone())]);

        // La taille change : détecté même si la mtime a la même seconde.
        touch(&f, "deux deux");
        assert_eq!(w.poll(), vec![Change::Modified(f.clone())]);

        std::fs::remove_file(&f).unwrap();
        assert_eq!(w.poll(), vec![Change::Removed(f)]);
    }

    #[test]
    fn extension_filter_applies() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = TreeWatcher::new(vec![dir.path().to_path_buf()]).extensions(&["md"]);
        w.prime();
        touch(&dir.path().join("x.txt"), "a");
        touch(&dir.path().join("y.md"), "a");
        let c = w.poll();
        assert_eq!(c.len(), 1);
        assert!(c[0].path().ends_with("y.md"));
    }

    #[test]
    fn excluded_directories_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = TreeWatcher::new(vec![dir.path().to_path_buf()]);
        w.prime();
        touch(&dir.path().join(".git/objects/aa"), "x");
        touch(&dir.path().join(".dreams/rapports/r.md"), "x");
        touch(&dir.path().join("journal/archive/vieux.md"), "x");
        touch(&dir.path().join("journal/2026-09-16.md"), "x");
        let c = w.poll();
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(c[0].path().ends_with("2026-09-16.md"));
    }

    #[test]
    fn nested_directories_are_walked() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = TreeWatcher::new(vec![dir.path().to_path_buf()]).extensions(&["md"]);
        w.prime();
        touch(&dir.path().join("pratiques/sous/x.md"), "a");
        assert_eq!(w.poll().len(), 1);
        assert_eq!(w.tracked_count(), 1);
    }

    #[test]
    fn debouncer_groups_repeated_changes() {
        let mut d = Debouncer::new(Duration::from_millis(300));
        let t0 = std::time::Instant::now();
        let p = PathBuf::from("/x/a.md");
        d.push(Change::Created(p.clone()), t0);
        d.push(Change::Modified(p.clone()), t0 + Duration::from_millis(100));
        assert_eq!(d.pending_count(), 1, "un seul événement par fichier");
        assert!(d.drain_ready(t0 + Duration::from_millis(200)).is_empty());
        let ready = d.drain_ready(t0 + Duration::from_millis(500));
        assert_eq!(ready, vec![Change::Modified(p)]);
        assert_eq!(d.pending_count(), 0);
    }
}
