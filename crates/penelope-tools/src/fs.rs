//! Outils de fichiers (§11) : lecture, liste, recherche, écriture, édition.
//!
//! Toutes les opérations sont **bornées aux workspaces autorisés** : la vérification a
//! lieu dans le harnais, avant même le bac à sable de l'OS (défense en profondeur).

use crate::error::{ToolError, ToolResult};
use penelope_platform::sandbox::{is_within, normalise};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Verrous par fichier : deux écritures concurrentes sur le même fichier sont sérialisées
/// (§11, « mutex par fichier »).
#[derive(Default)]
pub struct FileLocks {
    locks: Mutex<BTreeMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

impl FileLocks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn for_path(&self, p: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut g = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(normalise(p))
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn tracked(&self) -> usize {
        self.locks.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// Résout un chemin demandé par le modèle contre les workspaces autorisés.
pub fn resolve(path: &str, workspaces: &[PathBuf]) -> ToolResult<PathBuf> {
    if workspaces.is_empty() {
        return Err(ToolError::Denied(
            "aucun workspace autorisé n'est configuré".into(),
        ));
    }
    // Pas d'expansion du tilde : `~/…` joint au workspace créerait un répertoire
    // nommé `~`, en silence, alors que le modèle visait le home de l'utilisateur.
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        return Err(ToolError::Denied(format!(
            "`{path}` : le tilde n'est pas développé, donner un chemin du workspace"
        )));
    }
    let raw = PathBuf::from(path);
    let candidate = if raw.is_absolute() {
        normalise(&raw)
    } else {
        normalise(&workspaces[0].join(&raw))
    };
    if !is_within(&candidate, workspaces) {
        return Err(ToolError::Denied(format!(
            "`{path}` est hors des workspaces autorisés ({})",
            workspaces
                .iter()
                .map(|w| w.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(candidate)
}

/// `fs_read` : lecture paginée par lignes.
pub fn read(path: &Path, offset: usize, limit: usize) -> ToolResult<Value> {
    let raw =
        std::fs::read(path).map_err(|e| ToolError::Io(format!("{} : {e}", path.display())))?;
    if raw.iter().take(8000).any(|b| *b == 0) {
        return Err(ToolError::Invalid(format!(
            "{} est un fichier binaire : utiliser artifact_read après externalisation",
            path.display()
        )));
    }
    let text = String::from_utf8_lossy(&raw);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.min(total);
    let end = (start + limit.max(1)).min(total);
    let body: String = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>6}\t{l}", start + i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(json!({
        "path": path.display().to_string(),
        "lines": total,
        "from": start + 1,
        "to": end,
        "truncated": end < total,
        "content": body,
    }))
}

/// `fs_list`.
pub fn list(path: &Path, recursive: bool, max_entries: usize) -> ToolResult<Value> {
    let mut out = Vec::new();
    walk(path, recursive, max_entries, 0, &mut out)?;
    out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(json!({
        "path": path.display().to_string(),
        "entries": out.len(),
        "items": out,
    }))
}

fn walk(
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    depth: usize,
    out: &mut Vec<Value>,
) -> ToolResult<()> {
    if out.len() >= max_entries || depth > 12 {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(dir).map_err(|e| ToolError::Io(format!("{} : {e}", dir.display())))?;
    for e in entries.flatten() {
        if out.len() >= max_entries {
            return Ok(());
        }
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        let meta = e.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        out.push(json!({
            "path": p.display().to_string(),
            "name": name,
            "dir": is_dir,
            "bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
        }));
        if is_dir && recursive {
            walk(&p, recursive, max_entries, depth + 1, out)?;
        }
    }
    Ok(())
}

/// `fs_search` : expression régulière sur les fichiers texte.
pub fn search(
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
    max_results: usize,
) -> ToolResult<Value> {
    let re = regex::Regex::new(pattern)
        .map_err(|e| ToolError::Invalid(format!("expression régulière invalide : {e}")))?;
    let mut hits = Vec::new();
    let mut files = Vec::new();
    collect_files(root, glob, 0, &mut files)?;
    files.sort();

    for f in files {
        if hits.len() >= max_results {
            break;
        }
        let Ok(raw) = std::fs::read(&f) else { continue };
        if raw.iter().take(4000).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&raw);
        for (i, line) in text.lines().enumerate() {
            if hits.len() >= max_results {
                break;
            }
            if re.is_match(line) {
                hits.push(json!({
                    "path": f.display().to_string(),
                    "line": i + 1,
                    "text": line.chars().take(300).collect::<String>(),
                }));
            }
        }
    }
    Ok(json!({"pattern": pattern, "hits": hits.len(), "results": hits}))
}

fn collect_files(
    dir: &Path,
    glob: Option<&str>,
    depth: usize,
    out: &mut Vec<PathBuf>,
) -> ToolResult<()> {
    if depth > 12 || out.len() > 20_000 {
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            collect_files(&p, glob, depth + 1, out)?;
        } else if matches_glob(&name, glob) {
            out.push(p);
        }
    }
    Ok(())
}

/// Motif simple : `*.rs`, `*test*`, ou rien.
pub fn matches_glob(name: &str, glob: Option<&str>) -> bool {
    let Some(g) = glob else { return true };
    if g.is_empty() || g == "*" {
        return true;
    }
    let parts: Vec<&str> = g.split('*').collect();
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match rest.find(part) {
            Some(pos) => {
                if i == 0 && !g.starts_with('*') && pos != 0 {
                    return false;
                }
                rest = &rest[pos + part.len()..];
            }
            None => return false,
        }
    }
    if !g.ends_with('*') {
        if let Some(last) = parts.last() {
            if !last.is_empty() && !name.ends_with(last) {
                return false;
            }
        }
    }
    true
}

/// `fs_write` : écriture atomique, diff enregistré.
pub fn write(path: &Path, content: &str) -> ToolResult<Value> {
    let before = std::fs::read_to_string(path).ok();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::Io(format!("{} : {e}", parent.display())))?;
    }
    penelope_platform::dirs::write_text_lf(path, content)
        .map_err(|e| ToolError::Io(e.to_string()))?;
    Ok(json!({
        "path": path.display().to_string(),
        "bytes": content.len(),
        "created": before.is_none(),
        "diff": unified_diff(before.as_deref().unwrap_or(""), content, &path.display().to_string()),
    }))
}

/// `fs_edit` : remplacement d'une portion **exacte et unique**.
pub fn edit(path: &Path, old: &str, new: &str, replace_all: bool) -> ToolResult<Value> {
    if old.is_empty() {
        return Err(ToolError::Invalid(
            "`old` ne peut pas être vide : utiliser fs_write pour créer un fichier".into(),
        ));
    }
    let before = std::fs::read_to_string(path)
        .map_err(|e| ToolError::Io(format!("{} : {e}", path.display())))?;
    let occurrences = before.matches(old).count();
    if occurrences == 0 {
        return Err(ToolError::Invalid(format!(
            "la portion à remplacer est absente de {}",
            path.display()
        )));
    }
    if occurrences > 1 && !replace_all {
        return Err(ToolError::Invalid(format!(
            "la portion apparaît {occurrences} fois dans {} : préciser davantage \
             de contexte, ou passer `replace_all`",
            path.display()
        )));
    }
    let after = if replace_all {
        before.replace(old, new)
    } else {
        before.replacen(old, new, 1)
    };
    penelope_platform::dirs::write_text_lf(path, &after)
        .map_err(|e| ToolError::Io(e.to_string()))?;
    Ok(json!({
        "path": path.display().to_string(),
        "replaced": if replace_all { occurrences } else { 1 },
        "diff": unified_diff(&before, &after, &path.display().to_string()),
    }))
}

/// Diff unifié minimal, suffisant pour l'audit et l'affichage Telegram.
pub fn unified_diff(before: &str, after: &str, label: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let mut out = format!("--- a/{label}\n+++ b/{label}\n");
    let mut i = 0;
    let mut j = 0;
    let mut changes = 0;
    while (i < a.len() || j < b.len()) && changes < 400 {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) => {
                // Cherche la resynchronisation la plus proche.
                let resync_b = b[j..].iter().take(40).position(|l| l == x);
                let resync_a = a[i..].iter().take(40).position(|l| l == y);
                match (resync_a, resync_b) {
                    (Some(da), Some(db)) if da <= db => {
                        for l in &a[i..i + da] {
                            out.push_str(&format!("-{l}\n"));
                            changes += 1;
                        }
                        i += da;
                    }
                    (_, Some(db)) => {
                        for l in &b[j..j + db] {
                            out.push_str(&format!("+{l}\n"));
                            changes += 1;
                        }
                        j += db;
                    }
                    (Some(da), None) => {
                        for l in &a[i..i + da] {
                            out.push_str(&format!("-{l}\n"));
                            changes += 1;
                        }
                        i += da;
                    }
                    (None, None) => {
                        out.push_str(&format!("-{x}\n+{y}\n"));
                        changes += 2;
                        i += 1;
                        j += 1;
                    }
                }
            }
            (Some(x), None) => {
                out.push_str(&format!("-{x}\n"));
                changes += 1;
                i += 1;
            }
            (None, Some(y)) => {
                out.push_str(&format!("+{y}\n"));
                changes += 1;
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

/// Statistiques d'un diff, pour le résumé Telegram (§14.2).
pub fn diff_stats(diff: &str) -> (usize, usize) {
    let plus = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let minus = diff
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();
    (plus, minus)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> (tempfile::TempDir, Vec<PathBuf>) {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![normalise(d.path())];
        (d, roots)
    }

    #[test]
    fn resolve_rejects_paths_outside_the_workspace() {
        let (d, roots) = ws();
        assert!(resolve("src/main.rs", &roots).is_ok());
        assert!(resolve(&d.path().join("a.rs").display().to_string(), &roots).is_ok());
        let e = resolve("../../etc/passwd", &roots).unwrap_err();
        assert!(e.to_string().contains("hors des workspaces"));
        assert!(resolve("/etc/passwd", &roots).is_err());
    }

    #[test]
    fn resolve_without_workspace_denies_everything() {
        assert!(resolve("a.rs", &[]).is_err());
    }

    #[test]
    fn read_paginates_and_numbers_lines() {
        let (d, _) = ws();
        let p = d.path().join("a.txt");
        std::fs::write(
            &p,
            (1..=100)
                .map(|i| format!("ligne {i}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let r = read(&p, 10, 5).unwrap();
        assert_eq!(r["lines"], 100);
        assert_eq!(r["from"], 11);
        assert_eq!(r["to"], 15);
        assert_eq!(r["truncated"], true);
        let c = r["content"].as_str().unwrap();
        assert!(c.contains("ligne 11"));
        assert!(!c.contains("ligne 16"));
    }

    #[test]
    fn read_refuses_binary_files() {
        let (d, _) = ws();
        let p = d.path().join("bin");
        std::fs::write(&p, [0u8, 1, 2, 0, 3]).unwrap();
        assert!(read(&p, 0, 10).unwrap_err().to_string().contains("binaire"));
    }

    #[test]
    fn list_is_sorted_and_bounded() {
        let (d, _) = ws();
        for i in 0..30 {
            std::fs::write(d.path().join(format!("f{i:02}.txt")), "x").unwrap();
        }
        let r = list(d.path(), false, 10).unwrap();
        assert_eq!(r["entries"], 10);
        let items = r["items"].as_array().unwrap();
        let first = items[0]["name"].as_str().unwrap();
        let second = items[1]["name"].as_str().unwrap();
        assert!(first < second, "la liste doit être triée");
    }

    #[test]
    fn search_finds_matches_with_line_numbers() {
        let (d, _) = ws();
        std::fs::write(
            d.path().join("a.rs"),
            "fn main() {\n    let tva = 0.20;\n}\n",
        )
        .unwrap();
        std::fs::write(d.path().join("b.md"), "documentation\n").unwrap();
        let r = search(d.path(), r"tva", Some("*.rs"), 10).unwrap();
        assert_eq!(r["hits"], 1);
        assert_eq!(r["results"][0]["line"], 2);
        // Le glob exclut le markdown.
        let r = search(d.path(), "documentation", Some("*.rs"), 10).unwrap();
        assert_eq!(r["hits"], 0);
    }

    #[test]
    fn search_rejects_invalid_regex() {
        let (d, _) = ws();
        assert!(search(d.path(), "[invalide", None, 10).is_err());
    }

    #[test]
    fn glob_matching() {
        assert!(matches_glob("a.rs", Some("*.rs")));
        assert!(!matches_glob("a.md", Some("*.rs")));
        assert!(matches_glob("mod_test.rs", Some("*test*")));
        assert!(matches_glob("quoi que ce soit", None));
        assert!(matches_glob("x", Some("*")));
    }

    #[test]
    fn write_creates_then_reports_a_diff() {
        let (d, _) = ws();
        let p = d.path().join("a.txt");
        let r = write(&p, "une\ndeux\n").unwrap();
        assert_eq!(r["created"], true);
        let r = write(&p, "une\ntrois\n").unwrap();
        assert_eq!(r["created"], false);
        let diff = r["diff"].as_str().unwrap();
        assert!(diff.contains("-deux"));
        assert!(diff.contains("+trois"));
        let (plus, minus) = diff_stats(diff);
        assert_eq!((plus, minus), (1, 1));
    }

    #[test]
    fn edit_requires_a_unique_match() {
        let (d, _) = ws();
        let p = d.path().join("a.rs");
        std::fs::write(&p, "let x = 1;\nlet x = 1;\n").unwrap();

        let e = edit(&p, "let x = 1;", "let x = 2;", false).unwrap_err();
        assert!(e.to_string().contains("2 fois"), "{e}");

        let r = edit(&p, "let x = 1;", "let x = 2;", true).unwrap();
        assert_eq!(r["replaced"], 2);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "let x = 2;\nlet x = 2;\n"
        );
    }

    #[test]
    fn edit_reports_a_missing_portion() {
        let (d, _) = ws();
        let p = d.path().join("a.rs");
        std::fs::write(&p, "contenu\n").unwrap();
        assert!(
            edit(&p, "absent", "x", false)
                .unwrap_err()
                .to_string()
                .contains("absente")
        );
        assert!(edit(&p, "", "x", false).is_err());
    }

    #[tokio::test]
    async fn file_locks_serialise_writes_to_the_same_path() {
        let locks = std::sync::Arc::new(FileLocks::new());
        let p = PathBuf::from("/tmp/ws/a.rs");
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut hs = Vec::new();
        for _ in 0..8 {
            let (l, p, c, m) = (locks.clone(), p.clone(), counter.clone(), max.clone());
            hs.push(tokio::spawn(async move {
                let lock = l.for_path(&p);
                let _g = lock.lock().await;
                let now = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                m.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                c.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        for h in hs {
            h.await.unwrap();
        }
        assert_eq!(
            max.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "une seule écriture à la fois sur un même fichier"
        );
        assert_eq!(locks.tracked(), 1);
    }

    #[test]
    fn diff_handles_insertions_and_deletions() {
        let d = unified_diff("a\nb\nc\n", "a\nc\n", "f");
        assert!(d.contains("-b"));
        assert_eq!(diff_stats(&d), (0, 1), "une suppression, aucun ajout : {d}");

        let d = unified_diff("a\nc\n", "a\nb\nc\n", "f");
        assert!(d.contains("+b"));
        assert_eq!(diff_stats(&d), (1, 0));
    }

    #[test]
    fn files_are_written_in_lf() {
        let (d, _) = ws();
        let p = d.path().join("a.txt");
        write(&p, "une\r\ndeux\r\n").unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(!raw.windows(2).any(|w| w == b"\r\n"));
    }
}
