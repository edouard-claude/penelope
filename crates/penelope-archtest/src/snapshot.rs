//! Lecture du workspace pour les règles de gel (#209 à #214) : tous les fichiers Rust
//! (`src/`, `tests/`, `examples/`) avec leur contenu, et la coupe « code / tests »
//! commune aux règles qui ne regardent que le code.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::{Crate, collect, crates, workspace_root};

/// Un fichier Rust du workspace, lu une fois pour toutes les règles.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub crate_name: String,
    /// Chemin relatif à la racine du dépôt, séparateur `/`
    /// (`crates/penelope-daemon/src/telegram.rs`).
    pub rel: String,
    pub raw: String,
}

impl SourceFile {
    pub fn new(crate_name: &str, rel: &str, raw: impl Into<String>) -> Self {
        Self {
            crate_name: crate_name.to_string(),
            rel: rel.to_string(),
            raw: raw.into(),
        }
    }

    /// Nombre de lignes, tel que `raw.lines().count()` (R1).
    pub fn line_count(&self) -> usize {
        self.raw.lines().count()
    }

    /// Nom du fichier sans son répertoire.
    pub fn name(&self) -> &str {
        self.rel.rsplit('/').next().unwrap_or(&self.rel)
    }

    /// Le fichier est sous `crates/<crate>/src/` (les plafonds de crate ne comptent que
    /// `src/`, R4).
    pub fn in_src(&self) -> bool {
        let mut parts = self.rel.splitn(4, '/');
        parts.next() == Some("crates") && parts.nth(1) == Some("src")
    }

    /// Chemin relatif à `crates/penelope-daemon/src/`, si le fichier en fait partie.
    pub fn daemon_rel(&self) -> Option<&str> {
        self.rel.strip_prefix(DAEMON_SRC)
    }
}

/// Préfixe des sources du daemon ; les tables `[daemon.*]` de `budget.toml` sont
/// relatives à ce répertoire.
pub const DAEMON_SRC: &str = "crates/penelope-daemon/src/";

/// Photographie du workspace, ou d'un jeu de fichiers fictifs dans les tests.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub files: Vec<SourceFile>,
}

impl Snapshot {
    pub fn of(files: Vec<SourceFile>) -> Self {
        let mut files = files;
        files.sort_by(|a, b| a.rel.cmp(&b.rel));
        Self { files }
    }

    /// Lit `src/`, `tests/` et `examples/` de chaque crate.
    pub fn workspace() -> Self {
        let root = workspace_root();
        let mut files = Vec::new();
        for c in crates() {
            for path in all_sources(&c) {
                let Ok(raw) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(SourceFile::new(&c.name, &rel, raw));
            }
        }
        Self::of(files)
    }

    pub fn find(&self, rel: &str) -> Option<&SourceFile> {
        self.files.iter().find(|f| f.rel == rel)
    }
}

/// La photographie du workspace, lue une seule fois par processus de test.
pub fn workspace_snapshot() -> &'static Snapshot {
    static SNAPSHOT: OnceLock<Snapshot> = OnceLock::new();
    SNAPSHOT.get_or_init(Snapshot::workspace)
}

/// Tous les fichiers Rust d'un crate : `src/`, `tests/` et `examples/`.
///
/// `sources()` ne voit que `src/` ; les règles de gel doivent aussi plafonner les fichiers
/// de tests d'intégration (R2), d'où ce parcours plus large.
pub fn all_sources(c: &Crate) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["src", "tests", "examples"] {
        collect(&c.dir.join(dir), &mut out);
    }
    out.sort();
    out
}

/// Index (base 0) de la ligne `#[cfg(test)]` qui ouvre le module de tests, s'il y en a
/// un dans ce fichier.
///
/// Repère : `#[cfg(test)]` en colonne 0, éventuellement suivi d'autres attributs, puis
/// d'une ligne `mod x {` (ou `pub mod`, `pub(crate) mod`). Une déclaration `mod x;` sous
/// `#[cfg(test)]` ne coupe pas (le module vit dans un autre fichier, comme
/// `ticket_to_deploy_e2e` dans `lib.rs` du daemon), ni un `#[cfg(test)] fn` isolé.
pub fn test_module_start(raw: &str) -> Option<usize> {
    let lines: Vec<&str> = raw.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with("#[cfg(test)]") {
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && lines[j].starts_with("#[") {
            j += 1;
        }
        if lines.get(j).is_some_and(|next| opens_inline_module(next)) {
            return Some(i);
        }
    }
    None
}

fn opens_inline_module(line: &str) -> bool {
    let rest = line
        .strip_prefix("pub(crate) ")
        .or_else(|| line.strip_prefix("pub(super) "))
        .or_else(|| line.strip_prefix("pub "))
        .unwrap_or(line);
    rest.starts_with("mod ") && line.trim_end().ends_with('{')
}

/// Les lignes de code d'un fichier : avant le module de tests, hors lignes de
/// commentaire (`//`, `///`, `//!`). Numéros de ligne en base 1.
pub fn code_lines(raw: &str) -> Vec<(usize, &str)> {
    let stop = test_module_start(raw).unwrap_or(usize::MAX);
    raw.lines()
        .enumerate()
        .take_while(|(i, _)| *i < stop)
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .map(|(i, line)| (i + 1, line))
        .collect()
}

/// Le chemin `rel` d'un fichier du workspace, à partir de son chemin absolu.
pub fn relative_to_root(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_module_cut_ignores_declarations_and_lone_functions() {
        let raw = "fn a() {}\n#[cfg(test)]\nmod e2e;\nfn b() {}\n#[cfg(test)]\nfn helper() {}\n#[cfg(test)]\n#[allow(dead_code)]\nmod tests {\n    fn c() {}\n}\n";
        assert_eq!(test_module_start(raw), Some(6));
        let code: Vec<usize> = code_lines(raw).into_iter().map(|(n, _)| n).collect();
        assert_eq!(code, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn an_indented_cfg_test_does_not_cut() {
        let raw = "fn a() {}\n    #[cfg(test)]\n    mod inner {\n    }\nfn b() {}\n";
        assert_eq!(test_module_start(raw), None);
        assert_eq!(code_lines(raw).len(), 5);
    }

    #[test]
    fn comment_lines_are_not_code() {
        let raw = "//! doc\n/// item\nfn a() {} // fin\n// note\n";
        let code: Vec<&str> = code_lines(raw).into_iter().map(|(_, l)| l).collect();
        assert_eq!(code, vec!["fn a() {} // fin"]);
    }

    #[test]
    fn a_pub_crate_test_module_cuts_too() {
        let raw = "fn a() {}\n#[cfg(test)]\npub(crate) mod testing {\n}\n";
        assert_eq!(test_module_start(raw), Some(1));
    }

    #[test]
    fn source_files_know_their_place() {
        let f = SourceFile::new(
            "penelope-daemon",
            "crates/penelope-daemon/src/a/b.rs",
            "x\ny\n",
        );
        assert!(f.in_src());
        assert_eq!(f.daemon_rel(), Some("a/b.rs"));
        assert_eq!(f.name(), "b.rs");
        assert_eq!(f.line_count(), 2);
        let t = SourceFile::new("penelope-evals", "crates/penelope-evals/tests/docs.rs", "");
        assert!(!t.in_src());
        assert_eq!(t.daemon_rel(), None);
    }

    #[test]
    fn the_workspace_snapshot_sees_integration_tests() {
        let snap = workspace_snapshot();
        assert!(snap.files.len() > 200, "fichiers : {}", snap.files.len());
        assert!(
            snap.files
                .iter()
                .any(|f| f.rel.starts_with("crates/penelope-evals/tests/")),
            "les tests d'intégration ne sont pas parcourus"
        );
        assert!(snap.files.iter().all(|f| f.rel.starts_with("crates/")));
    }
}
