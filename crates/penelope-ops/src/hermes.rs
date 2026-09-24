//! Import d'une instance Hermes (§7, §8.7, §20.2 point 3).
//!
//! ```text
//!  ~/.hermes/                                   Pénélope
//!    skills/**/SKILL.md ─────────────────────►  {data}/skills/<slug>/    (existante : gardée)
//!    SOUL.md, AGENTS.md ─────────────────────►  vault/                   (différent : mis de côté)
//!    memories/MEMORY.md ─────────────────────►  vault/memoire.md         (uid, origine owner)
//!    memories/USER.md   ─────────────────────►  vault/profil.md
//!    config.yaml mcp_servers ────────────────►  mcp.d/<nom>.toml ─► essai : ok | auth_required | failed
//!    secrets (env, en-têtes, .env) ──────────►  SecretStore, `${SECRET:…}` dans la déclaration
//! ```
//!
//! L'importeur est tolérant : un élément qui ne passe pas est noté dans le rapport, le reste
//! continue. Sans `apply`, rien n'est écrit et le rapport décrit ce qui serait fait. Rien
//! d'existant n'est écrasé : un second import ne duplique rien.

use crate::ports::McpAdmin;
use penelope_app::ports::Messenger;
use penelope_app::services::Services;
use penelope_kernel::event::EventDraft;
use penelope_mcp::config::ServerConfig;
use penelope_memory::{Level, Provenance};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const SKILL_MAX_FILES: usize = 500;
const SKILL_MAX_BYTES: u64 = 50 * 1024 * 1024;
const MCP_TEST_TIMEOUT: Duration = Duration::from_secs(120);
const MCP_TEST_PARALLEL: usize = 4;

/// Un élément du rapport.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    /// `skill`, `fichier`, `mémoire` ou `mcp`.
    pub kind: &'static str,
    pub name: String,
    /// `imported`, `planned`, `exists`, `identical`, `set_aside`, `invalid`, `refused`,
    /// `ok`, `auth_required`, `failed`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Rapport d'import.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub root: PathBuf,
    pub applied: bool,
    pub items: Vec<Item>,
    /// Noms des secrets rangés (jamais leurs valeurs).
    pub secrets: Vec<String>,
    pub warnings: Vec<String>,
}

impl Report {
    fn push(&mut self, kind: &'static str, name: &str, status: &'static str, detail: String) {
        self.items.push(Item {
            kind,
            name: name.to_string(),
            status,
            detail,
        });
    }

    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    pub fn count(&self, kind: &str, status: &str) -> usize {
        self.items
            .iter()
            .filter(|i| i.kind == kind && i.status == status)
            .count()
    }
}

/// Options d'un import.
#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub apply: bool,
    /// Essayer les serveurs MCP importés.
    pub test: bool,
}

/// `$HERMES_HOME`, sinon `~/.hermes`.
pub fn default_root() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("HERMES_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    penelope_platform::dirs::home_dir().map(|h| h.join(".hermes"))
}

/// Importe tout ce qui peut l'être.
pub async fn import(
    s: &Services,
    mcp: Option<Arc<dyn McpAdmin>>,
    opts: &Options,
) -> Result<Report, String> {
    if !opts.root.is_dir() {
        return Err(format!(
            "instance Hermes introuvable : {} (`--path` pour une autre racine)",
            opts.root.display()
        ));
    }
    let mut r = Report {
        root: opts.root.clone(),
        applied: opts.apply,
        ..Default::default()
    };
    import_skills(s, opts, &mut r).await;
    let files = import_files(s, opts, &mut r);
    let memories = import_memories(s, opts, &mut r).await;
    import_mcp(s, mcp, opts, &mut r).await;

    if opts.apply {
        if (files || memories)
            && let Err(e) = penelope_vault::vault_git::vault_sync(s, "import: hermes").await
        {
            r.warn(format!("commit du vault : {e}"));
        }
        let counts: BTreeMap<String, usize> = r.items.iter().fold(BTreeMap::new(), |mut m, i| {
            *m.entry(format!("{}.{}", i.kind, i.status)).or_default() += 1;
            m
        });
        let _ = s
            .events
            .append(EventDraft::new(
                "import.hermes",
                json!({"root": r.root, "counts": counts, "secrets": r.secrets.len()}),
            ))
            .await;
    }
    Ok(r)
}

// ------------------------------------------------------------------ skills

/// `[a-z0-9-]`, 64 caractères au plus.
pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let cut: String = out.chars().take(64).collect();
    cut.trim_matches('-').to_string()
}

fn find_skill_dirs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for d in dirs {
        if d.join("SKILL.md").is_file() {
            out.push(d);
        } else {
            find_skill_dirs(&d, depth + 1, out);
        }
    }
}

/// Valeur d'une clé de premier niveau du frontmatter, blocs `>` et `|` repliés.
fn header_field(lines: &[&str], key: &str) -> String {
    let prefix = format!("{key}:");
    let Some(i) = lines.iter().position(|l| l.starts_with(&prefix)) else {
        return String::new();
    };
    let first = lines[i][prefix.len()..].trim();
    let block = matches!(first, ">" | ">-" | ">+" | "|" | "|-" | "|+");
    let mut parts: Vec<String> = Vec::new();
    if !block && !first.is_empty() {
        parts.push(unquote(first));
    }
    for l in &lines[i + 1..] {
        if l.trim().is_empty() {
            if block {
                continue;
            }
            break;
        }
        if !l.starts_with(' ') && !l.starts_with('\t') {
            break;
        }
        if !block && l.trim_start().starts_with("- ") {
            break;
        }
        parts.push(l.trim().to_string());
    }
    parts.join(" ")
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// Valeur de frontmatter écrite pour le parseur de Pénélope (sans échappement).
fn header_value(v: &str) -> String {
    let v = v.replace(['\n', '\r'], " ");
    let ambiguous = (v.starts_with('[') && v.ends_with(']'))
        || matches!(v.as_str(), "true" | "false" | "yes" | "no")
        || v.parse::<f64>().is_ok();
    if ambiguous { format!("\"{v}\"") } else { v }
}

/// Rend un `SKILL.md` Hermes lisible par Pénélope : nom en slug, description sur une
/// ligne. Un frontmatter déjà conforme est gardé tel quel.
pub fn normalize_skill(raw: &str, dir_name: &str) -> Result<(String, String), String> {
    let raw = raw.replace("\r\n", "\n");
    let lines: Vec<&str> = raw.split('\n').collect();
    let has_header = lines.first().map(|l| l.trim()) == Some("---");
    let close = has_header
        .then(|| {
            lines
                .iter()
                .skip(1)
                .position(|l| l.trim() == "---")
                .map(|i| i + 1)
        })
        .flatten();

    let (header, body): (Vec<&str>, String) = match close {
        Some(end) => (lines[1..end].to_vec(), lines[end + 1..].join("\n")),
        None => (Vec::new(), raw.clone()),
    };
    let name_field = header_field(&header, "name");
    let slug = slugify(if name_field.trim().is_empty() {
        dir_name
    } else {
        &name_field
    });
    if slug.is_empty() {
        return Err("nom de skill vide".into());
    }
    let description = match header_field(&header, "description") {
        d if !d.trim().is_empty() => d,
        _ => body
            .lines()
            .map(|l| l.trim().trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_string(),
    };
    if description.trim().is_empty() {
        return Err("`description` absente et corps vide".into());
    }
    if body.trim().is_empty() {
        return Err("le corps de la skill est vide".into());
    }

    let conforming = close.is_some()
        && name_field == slug
        && penelope_kernel::frontmatter::parse(&raw)
            .is_ok_and(|fm| fm.string("description") == description);
    if conforming {
        return Ok((slug, raw));
    }
    let mut out = format!(
        "---\nname: {slug}\ndescription: {}\n",
        header_value(&description)
    );
    let version = header_field(&header, "version");
    if !version.is_empty() {
        out.push_str(&format!("version: {}\n", header_value(&version)));
    }
    out.push_str("---\n");
    out.push_str(body.trim_start_matches('\n'));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok((slug, out))
}

fn copy_tree(from: &Path, to: &Path, files: &mut usize, bytes: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{} : {e}", to.display()))?;
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .map_err(|e| format!("{} : {e}", from.display()))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            copy_tree(&from.join(&name), &to.join(&name), files, bytes)?;
        } else if ft.is_file() {
            *files += 1;
            *bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            if *files > SKILL_MAX_FILES || *bytes > SKILL_MAX_BYTES {
                return Err("skill trop volumineuse".into());
            }
            std::fs::copy(from.join(&name), to.join(&name)).map_err(|e| e.to_string())?;
        }
        // Liens symboliques ignorés : ils pourraient sortir de la skill.
    }
    Ok(())
}

async fn import_skills(s: &Services, opts: &Options, r: &mut Report) {
    let mut dirs = Vec::new();
    find_skill_dirs(&opts.root.join("skills"), 0, &mut dirs);
    if dirs.is_empty() {
        return;
    }
    let root = s.platform.dirs.skills();
    let staging = s.platform.dirs.state().join("import-hermes").join("skills");
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut imported = 0;

    for dir in dirs {
        let dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let raw = match std::fs::read_to_string(dir.join("SKILL.md")) {
            Ok(raw) => raw,
            Err(e) => {
                r.push("skill", &dir_name, "invalid", e.to_string());
                continue;
            }
        };
        let (slug, fixed) = match normalize_skill(&raw, &dir_name) {
            Ok(v) => v,
            Err(e) => {
                r.push("skill", &dir_name, "invalid", e);
                continue;
            }
        };
        if let Err(e) = penelope_skills::parse_skill(
            &dir.join("SKILL.md"),
            &fixed,
            penelope_skills::Scope::User,
        ) {
            r.push("skill", &slug, "invalid", e.message);
            continue;
        }
        if root.join(&slug).exists() || !taken.insert(slug.clone()) {
            r.push("skill", &slug, "exists", String::new());
            continue;
        }
        let detail = if slug == dir_name {
            String::new()
        } else {
            format!("depuis `{dir_name}`")
        };
        if !opts.apply {
            r.push("skill", &slug, "planned", detail);
            continue;
        }
        // Copie à l'écart, puis renommage : le watcher ne voit jamais une skill à moitié.
        let stage = staging.join(&slug);
        let _ = std::fs::remove_dir_all(&stage);
        let (mut files, mut bytes) = (0, 0);
        let staged = copy_tree(&dir, &stage, &mut files, &mut bytes).and_then(|_| {
            penelope_platform::dirs::write_text_lf(&stage.join("SKILL.md"), &fixed)
                .map_err(|e| e.to_string())
        });
        let placed = staged.and_then(|_| {
            std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            match std::fs::rename(&stage, root.join(&slug)) {
                Ok(()) => Ok(()),
                Err(_) => copy_tree(&stage, &root.join(&slug), &mut 0, &mut 0),
            }
        });
        let _ = std::fs::remove_dir_all(&stage);
        match placed {
            Ok(()) => {
                imported += 1;
                r.push("skill", &slug, "imported", detail);
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(root.join(&slug));
                r.push("skill", &slug, "failed", e);
            }
        }
    }
    if imported > 0
        && let Err(e) = penelope_app::services::reload_skills(s).await
    {
        r.warn(format!("rechargement des skills : {e}"));
    }
}

// ------------------------------------------------------------------ SOUL.md, AGENTS.md

fn import_files(s: &Services, opts: &Options, r: &mut Report) -> bool {
    let vault = crate::helpers::vault_dir(s);
    let aside_dir = s.platform.dirs.state().join("import-hermes");
    let mut changed = false;
    for name in ["SOUL.md", "AGENTS.md"] {
        let Ok(content) = std::fs::read_to_string(opts.root.join(name)) else {
            continue;
        };
        if let Some(kind) = penelope_observe::redact::secret_kind(&content) {
            r.push(
                "fichier",
                name,
                "refused",
                format!("contient un {kind} : à nettoyer avant import"),
            );
            continue;
        }
        let dest = vault.join(name);
        match std::fs::read_to_string(&dest) {
            Ok(existing) if existing.trim() == content.trim() => {
                r.push("fichier", name, "identical", String::new());
            }
            Ok(_) => {
                let aside = aside_dir.join(name);
                if opts.apply
                    && let Err(e) = std::fs::create_dir_all(&aside_dir)
                        .and_then(|_| std::fs::write(&aside, &content))
                {
                    r.push("fichier", name, "failed", e.to_string());
                    continue;
                }
                r.push(
                    "fichier",
                    name,
                    "set_aside",
                    format!(
                        "le vault a déjà le sien : version Hermes dans {} pour fusion à la main",
                        aside.display()
                    ),
                );
            }
            Err(_) if !opts.apply => r.push("fichier", name, "planned", String::new()),
            Err(_) => {
                let written = std::fs::create_dir_all(&vault)
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        penelope_kernel::config::atomic_write(&dest, content.as_bytes())
                            .map_err(|e| e.to_string())
                    });
                match written {
                    Ok(()) => {
                        changed = true;
                        r.push("fichier", name, "imported", String::new());
                    }
                    Err(e) => r.push("fichier", name, "failed", e),
                }
            }
        }
    }
    changed
}

// ------------------------------------------------------------------ mémoire

fn strip_bullet(line: &str) -> Option<&str> {
    for p in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(p) {
            return Some(rest);
        }
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        return line[digits..].strip_prefix(". ");
    }
    None
}

/// Entrées d'un `MEMORY.md` ou `USER.md` Hermes : séparées par une ligne `§`, sinon une
/// par puce ou par paragraphe. Chaque entrée tient sur une ligne.
pub fn memory_entries(raw: &str) -> Vec<String> {
    let raw = raw.replace("\r\n", "\n");
    let body = match penelope_kernel::frontmatter::parse(&raw) {
        Ok(fm) => fm.body,
        Err(_) => raw,
    };
    // Hermes écrit `\n§\n` entre les entrées ; un `§` en ligne sépare aussi.
    let body = body.replace('§', "\n§\n");
    let sectioned = body.lines().any(|l| l.trim() == "§");
    let mut out = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let flush = |current: &mut Vec<String>, out: &mut Vec<String>| {
        let text = current
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() {
            out.push(text);
        }
        current.clear();
    };
    for line in body.lines() {
        let t = line.trim();
        if sectioned {
            if t == "§" {
                flush(&mut current, &mut out);
            } else if !t.is_empty() && !t.starts_with('#') {
                current.push(strip_bullet(t).unwrap_or(t).to_string());
            }
            continue;
        }
        if t.is_empty() || t.starts_with('#') {
            flush(&mut current, &mut out);
        } else if let Some(rest) = strip_bullet(t) {
            flush(&mut current, &mut out);
            current.push(rest.to_string());
        } else {
            current.push(t.to_string());
        }
    }
    flush(&mut current, &mut out);
    out
}

fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches('.')
        .to_lowercase()
}

async fn import_memories(s: &Services, opts: &Options, r: &mut Report) -> bool {
    let vault = crate::helpers::vault_dir(s);
    let mut changed = false;
    for (source, level, dest) in [
        ("MEMORY.md", Level::Coeur, "memoire.md"),
        ("USER.md", Level::Profil, "profil.md"),
    ] {
        let Some(path) = [
            opts.root.join("memories").join(source),
            opts.root.join(source),
        ]
        .into_iter()
        .find(|p| p.is_file()) else {
            continue;
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) => {
                r.push("mémoire", source, "failed", e.to_string());
                continue;
            }
        };
        let mut known: BTreeSet<String> = std::fs::read_to_string(vault.join(dest))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let text = penelope_memory::vault::strip_annotations(l);
                strip_bullet(text.trim()).map(normalized)
            })
            .collect();
        let (mut added, mut duplicates, mut refused) = (0, 0, 0);
        let at = s.clock.now_rfc3339();
        for entry in memory_entries(&raw) {
            if !known.insert(normalized(&entry)) {
                duplicates += 1;
                continue;
            }
            if let Err(why) = crate::vault_ops::write_filter(&entry) {
                refused += 1;
                r.warn(format!("{source} : entrée ignorée ({why})"));
                continue;
            }
            if opts.apply {
                let prov = Provenance::owner("import-hermes", "import", &at)
                    .with_source(format!("hermes:{}", source));
                if let Err(e) =
                    crate::vault_ops::remember_with(s, &vault, level, &entry, prov).await
                {
                    refused += 1;
                    r.warn(format!("{source} : entrée ignorée ({e})"));
                    continue;
                }
                changed = true;
            }
            added += 1;
        }
        r.push(
            "mémoire",
            &format!("{source} → {dest}"),
            if opts.apply { "imported" } else { "planned" },
            format!("{added} entrée(s), {duplicates} déjà présente(s), {refused} refusée(s)"),
        );
    }
    changed
}

// ------------------------------------------------------------------ serveurs MCP

/// `KEY=valeur` d'un `.env`, guillemets et `export` retirés.
pub fn parse_dotenv(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let k = k.trim();
            if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return None;
            }
            Some((k.to_string(), unquote(v.trim())))
        })
        .collect()
}

fn looks_secret(key: &str, value: &str) -> bool {
    if value.trim().is_empty() || value.contains("${") {
        return false;
    }
    let k = key.to_ascii_uppercase().replace('-', "_");
    let by_key = [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "AUTHORIZATION",
        "COOKIE",
    ]
    .iter()
    .any(|h| k.contains(h))
        || k == "KEY"
        || k.ends_with("_KEY")
        || k == "PAT"
        || k.ends_with("_PAT");
    by_key || penelope_observe::redact::secret_kind(value).is_some()
}

fn secret_slug(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// Serveur Hermes converti.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    pub config: ServerConfig,
    /// `(nom, valeur)` à ranger dans le magasin avant d'écrire la déclaration.
    pub secrets: Vec<(String, String)>,
    pub notes: Vec<String>,
}

struct Secretizer<'a> {
    server: String,
    dotenv: &'a BTreeMap<String, String>,
    workspace: &'a Path,
    secrets: Vec<(String, String)>,
    notes: Vec<String>,
}

impl Secretizer<'_> {
    fn keep(&mut self, name: String, value: String) -> String {
        let name: String = name.chars().take(128).collect();
        if !self.secrets.iter().any(|(n, _)| *n == name) {
            self.secrets.push((name.clone(), value));
        }
        format!("${{SECRET:{name}}}")
    }

    /// `${VAR}` et `$VAR` : valeur du `.env` d'Hermes (rangée si c'est un secret), sinon
    /// variable d'environnement du daemon.
    fn placeholders(&mut self, value: &str) -> String {
        let whole = value
            .strip_prefix('$')
            .filter(|v| !v.starts_with('{') && !v.is_empty())
            .filter(|v| v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        let value = match whole {
            Some(var) => format!("${{{var}}}"),
            None => value.to_string(),
        };
        let mut out = String::new();
        let mut rest = value.as_str();
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let inner = &after[..end];
            let var = inner
                .strip_prefix("env:")
                .or_else(|| inner.strip_prefix("ENV:"))
                .unwrap_or(inner);
            let plain = var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if let Some(v) = self.builtin(inner) {
                out.push_str(&v);
            } else if !plain || var.is_empty() {
                out.push_str(&rest[start..start + 2 + end + 1]);
            } else if let Some(v) = self.dotenv.get(var) {
                if looks_secret(var, v) {
                    let name = format!("hermes_{}", secret_slug(var));
                    out.push_str(&self.keep(name, v.clone()));
                } else {
                    out.push_str(v);
                }
            } else {
                self.notes.push(format!(
                    "`{var}` absente du .env : lue dans l'environnement du daemon"
                ));
                out.push_str(&format!("${{ENV:{var}}}"));
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// Variables propres à Hermes (`${userHome}`, `${workspaceFolder}`, `${/}`…).
    fn builtin(&mut self, inner: &str) -> Option<String> {
        let workspace = || self.workspace.to_string_lossy().to_string();
        Some(match inner {
            "userHome" => penelope_platform::dirs::home_dir()?
                .to_string_lossy()
                .to_string(),
            "workspaceFolder" => {
                let w = workspace();
                self.notes.push(format!(
                    "`${{workspaceFolder}}` remplacé par l'espace de travail {w}"
                ));
                w
            }
            "workspaceFolderBasename" => "workspace".to_string(),
            "pathSeparator" | "/" => std::path::MAIN_SEPARATOR.to_string(),
            _ => return None,
        })
    }

    fn value(&mut self, kind: &str, key: &str, raw: &str) -> String {
        let value = self.placeholders(raw);
        if !looks_secret(key, &value) {
            return value;
        }
        let name = format!(
            "mcp_{}_{}{}",
            secret_slug(&self.server),
            if kind == "header" { "header_" } else { "" },
            secret_slug(key)
        );
        for scheme in ["Bearer ", "Basic ", "Token "] {
            if let Some(token) = value.strip_prefix(scheme) {
                return format!("{scheme}{}", self.keep(name, token.trim().to_string()));
            }
        }
        self.keep(name, value)
    }
}

fn duration(node: &yaml::Node) -> Option<String> {
    let v = node.as_str()?.trim();
    if v.is_empty() {
        return None;
    }
    match v.parse::<f64>() {
        Ok(secs) => Some(format!("{}s", secs.ceil().max(1.0) as u64)),
        Err(_) => Some(v.to_string()),
    }
}

/// Convertit une entrée `mcp_servers.<nom>` d'Hermes.
pub fn convert_server(
    raw_name: &str,
    node: &yaml::Node,
    dotenv: &BTreeMap<String, String>,
    workspace: &Path,
) -> Result<Converted, String> {
    let name: String = raw_name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.trim_matches('-').is_empty() {
        return Err("nom de serveur vide".into());
    }
    let yaml::Node::Map(fields) = node else {
        return Err("déclaration illisible (table attendue)".into());
    };
    let mut cfg = ServerConfig {
        name: name.clone(),
        ..Default::default()
    };
    let mut sec = Secretizer {
        server: name,
        dotenv,
        workspace,
        secrets: Vec::new(),
        notes: Vec::new(),
    };
    let mut ignored = Vec::new();
    for (k, v) in fields {
        match k.as_str() {
            "command" => {
                cfg.command = sec.placeholders(v.as_str().unwrap_or_default());
            }
            "args" => {
                cfg.args = match v {
                    yaml::Node::List(items) => items
                        .iter()
                        .filter_map(|i| i.as_str())
                        .map(|a| sec.placeholders(a))
                        .collect(),
                    other => other
                        .as_str()
                        .unwrap_or_default()
                        .split_whitespace()
                        .map(|a| sec.placeholders(a))
                        .collect(),
                };
            }
            "env" | "environment" => {
                for (ek, ev) in v.entries() {
                    let value = sec.value("env", ek, ev.as_str().unwrap_or_default());
                    cfg.env.insert(ek.clone(), value);
                }
            }
            "headers" => {
                for (hk, hv) in v.entries() {
                    let value = sec.value("header", hk, hv.as_str().unwrap_or_default());
                    cfg.headers.insert(hk.clone(), value);
                }
            }
            "url" => cfg.url = sec.placeholders(v.as_str().unwrap_or_default()),
            "cwd" => cfg.cwd = v.as_str().unwrap_or_default().to_string(),
            "timeout" => {
                if let Some(t) = duration(v) {
                    cfg.timeout = t;
                }
            }
            "idle_timeout_seconds" => {
                if let Some(t) = duration(v) {
                    cfg.idle_timeout = t;
                }
            }
            "lazy" => cfg.lazy_start = v.as_bool().unwrap_or(true),
            "auth" if v.as_str() == Some("oauth") => sec
                .notes
                .push("OAuth : autorisation à refaire côté Pénélope (`penelope mcp auth`)".into()),
            "oauth" => {
                for (ok, ov) in v.entries() {
                    match ok.as_str() {
                        "client_id" => {
                            cfg.client_id = ov.as_str().unwrap_or_default().to_string();
                        }
                        "scope" | "scopes" => {
                            cfg.scopes = match ov {
                                yaml::Node::List(items) => items
                                    .iter()
                                    .filter_map(|i| i.as_str())
                                    .map(String::from)
                                    .collect(),
                                other => other
                                    .as_str()
                                    .unwrap_or_default()
                                    .split_whitespace()
                                    .map(String::from)
                                    .collect(),
                            };
                        }
                        other => ignored.push(format!("`oauth.{other}`")),
                    }
                }
            }
            "tools" => sec.notes.push(
                "filtre `tools` d'Hermes sans équivalent : tous les outils sont exposés".into(),
            ),
            "enabled" => cfg.enabled = v.as_bool().unwrap_or(true),
            "disabled" => cfg.enabled = !v.as_bool().unwrap_or(false),
            "transport" | "type" => {
                cfg.transport = match v.as_str().unwrap_or_default().to_lowercase().as_str() {
                    "stdio" => "stdio",
                    "sse" => "sse",
                    "http" | "streamable-http" | "streamable_http" | "streamablehttp" => "http",
                    _ => "auto",
                }
                .to_string();
            }
            other => ignored.push(format!("`{other}`")),
        }
    }
    if !ignored.is_empty() {
        sec.notes
            .push(format!("sans équivalent, ignoré : {}", ignored.join(", ")));
    }
    Ok(Converted {
        config: cfg,
        secrets: sec.secrets,
        notes: sec.notes,
    })
}

async fn import_mcp(s: &Services, sup: Option<Arc<dyn McpAdmin>>, opts: &Options, r: &mut Report) {
    let Some(raw) = ["config.yaml", "config.yml"]
        .iter()
        .find_map(|f| std::fs::read_to_string(opts.root.join(f)).ok())
    else {
        return;
    };
    let doc = yaml::parse(&raw);
    let servers = doc
        .get("mcp_servers")
        .or_else(|| doc.get("mcpServers"))
        .or_else(|| doc.get("mcp").and_then(|m| m.get("servers")));
    let Some(servers) = servers else {
        r.warn("config.yaml sans section `mcp_servers`".into());
        return;
    };
    let dotenv = std::fs::read_to_string(opts.root.join(".env"))
        .map(|raw| parse_dotenv(&raw))
        .unwrap_or_default();
    let dir = sup
        .as_ref()
        .map(|s| s.dir().to_path_buf())
        .unwrap_or_else(|| s.platform.dirs.mcp_d());

    let workspace = s.platform.dirs.data().join("workspace");
    let mut written: Vec<(ServerConfig, String)> = Vec::new();
    for (raw_name, node) in servers.entries() {
        let c = match convert_server(raw_name, node, &dotenv, &workspace) {
            Ok(c) => c,
            Err(e) => {
                r.push("mcp", raw_name, "invalid", e);
                continue;
            }
        };
        let name = c.config.name.clone();
        let known = match &sup {
            Some(sup) => sup.config_of(&name).await.is_some(),
            None => false,
        };
        if known || dir.join(format!("{name}.toml")).exists() {
            r.push("mcp", &name, "exists", String::new());
            continue;
        }
        if let Err(e) = c.config.validate() {
            r.push("mcp", &name, "invalid", e.to_string());
            continue;
        }
        let names: Vec<String> = c.secrets.iter().map(|(n, _)| n.clone()).collect();
        let mut detail = c.notes.join(" ; ");
        if !names.is_empty() {
            let moved = format!("{} secret(s) vers le magasin", names.len());
            detail = if detail.is_empty() {
                moved
            } else {
                format!("{moved} ; {detail}")
            };
        }
        if !opts.apply {
            r.secrets.extend(names);
            r.push("mcp", &name, "planned", detail);
            continue;
        }
        let stored = c
            .secrets
            .iter()
            .try_for_each(|(n, v)| s.platform.secrets.set(n, v).map_err(|e| e.to_string()));
        if let Err(e) = stored {
            r.push("mcp", &name, "failed", format!("secret non rangé : {e}"));
            continue;
        }
        r.secrets.extend(names);
        if let Err(e) = penelope_mcp::config::write_server(&dir, &c.config) {
            r.push("mcp", &name, "failed", e.to_string());
            continue;
        }
        written.push((c.config, detail));
    }
    if written.is_empty() {
        return;
    }
    let Some(sup) = sup else {
        for (cfg, detail) in written {
            r.push("mcp", &cfg.name, "imported", detail);
        }
        return;
    };
    sup.reload().await;
    let (to_test, untested): (Vec<_>, Vec<_>) = written
        .into_iter()
        .partition(|(cfg, _)| opts.test && cfg.enabled);
    for (cfg, detail) in untested {
        let detail = if cfg.enabled {
            detail
        } else {
            ["désactivé, comme dans Hermes".to_string(), detail]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ; ")
        };
        r.push("mcp", &cfg.name, "imported", detail);
    }
    for chunk in to_test.chunks(MCP_TEST_PARALLEL) {
        let tests = chunk
            .iter()
            .map(|(cfg, _)| tokio::time::timeout(MCP_TEST_TIMEOUT, sup.test(cfg)));
        let results = futures::future::join_all(tests).await;
        for ((cfg, detail), result) in chunk.iter().zip(results) {
            let (status, why) = match result {
                Ok(v) if v["ok"].as_bool() == Some(true) => (
                    "ok",
                    format!("{} outil(s)", v["tools"].as_u64().unwrap_or(0)),
                ),
                Ok(v) if v["auth_required"].as_bool() == Some(true) => (
                    "auth_required",
                    format!(
                        "`penelope mcp auth {}` ou `/mcp auth {}`",
                        cfg.name, cfg.name
                    ),
                ),
                Ok(v) => ("failed", v["error"].as_str().unwrap_or("échec").to_string()),
                Err(_) => ("failed", "pas de réponse dans le délai".to_string()),
            };
            let detail = [why, detail.clone()]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ; ");
            r.push("mcp", &cfg.name, status, detail);
        }
    }
}

// ------------------------------------------------------------------ rapport

fn status_label(status: &str) -> &'static str {
    match status {
        "imported" => "importé(s)",
        "planned" => "à importer",
        "exists" => "déjà présent(s)",
        "identical" => "identique(s)",
        "set_aside" => "mis de côté",
        "invalid" => "invalide(s)",
        "refused" => "refusé(s)",
        "ok" => "ok",
        "auth_required" => "à autoriser",
        "failed" => "en échec",
        _ => "?",
    }
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "imported" | "ok" | "identical" => "✅",
        "planned" => "•",
        "exists" => "↩️",
        "set_aside" => "📂",
        "auth_required" => "🔐",
        _ => "❌",
    }
}

/// Rapport lisible (CLI et Telegram).
pub fn render(r: &Report) -> String {
    let mut out = if r.applied {
        format!("📥 Import Hermes depuis {}", r.root.display())
    } else {
        format!("🔎 Import Hermes, simulation sur {}", r.root.display())
    };
    for (kind, label) in [
        ("skill", "Skills"),
        ("fichier", "Fichiers"),
        ("mémoire", "Mémoire"),
        ("mcp", "Serveurs MCP"),
    ] {
        let items: Vec<&Item> = r.items.iter().filter(|i| i.kind == kind).collect();
        if items.is_empty() {
            continue;
        }
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for i in &items {
            match counts.iter_mut().find(|(s, _)| *s == i.status) {
                Some((_, n)) => *n += 1,
                None => counts.push((i.status, 1)),
            }
        }
        let summary: Vec<String> = counts
            .iter()
            .map(|(s, n)| format!("{n} {}", status_label(s)))
            .collect();
        out.push_str(&format!("\n\n{label} : {}", summary.join(", ")));
        for i in items {
            let notable = kind == "mémoire"
                || kind == "fichier"
                || !matches!(i.status, "imported" | "planned" | "exists" | "ok");
            if notable {
                let detail = if i.detail.is_empty() {
                    String::new()
                } else {
                    format!(" : {}", i.detail)
                };
                out.push_str(&format!("\n{} {}{detail}", status_icon(i.status), i.name));
            }
        }
    }
    if r.items.is_empty() {
        out.push_str("\n\nRien à importer.");
    }
    if !r.secrets.is_empty() {
        out.push_str(&format!(
            "\n\n🔐 {} secret(s) {} : {}",
            r.secrets.len(),
            if r.applied {
                "rangé(s) dans le magasin"
            } else {
                "à ranger dans le magasin"
            },
            r.secrets.join(", ")
        ));
    }
    for w in r.warnings.iter().take(10) {
        out.push_str(&format!("\n⚠️ {w}"));
    }
    if r.warnings.len() > 10 {
        out.push_str(&format!("\n⚠️ … et {} autre(s)", r.warnings.len() - 10));
    }
    if !r.applied {
        out.push_str("\n\nRien n'a été écrit : relancer sans `--dry-run` pour importer.");
    }
    out
}

/// Méthode RPC `import.hermes` : `path`, `apply` (faux par défaut), `test` (vrai par
/// défaut). Le rapport d'un import appliqué part aussi sur Telegram.
pub async fn rpc(
    s: &Services,
    mcp: Option<Arc<dyn McpAdmin>>,
    messenger: Option<Arc<dyn Messenger>>,
    p: &Value,
) -> anyhow::Result<Value> {
    let root = match p["path"].as_str().filter(|x| !x.trim().is_empty()) {
        Some(path) => s.platform.dirs.expand(path),
        None => default_root().ok_or_else(|| anyhow::anyhow!("répertoire personnel inconnu"))?,
    };
    let opts = Options {
        root,
        apply: p["apply"].as_bool().unwrap_or(false),
        test: p["test"].as_bool().unwrap_or(true),
    };
    let report = import(s, mcp, &opts).await.map_err(anyhow::Error::msg)?;
    let text = render(&report);
    if opts.apply
        && let Some(m) = messenger
    {
        let origin = crate::helpers::owner_origin_of(s);
        if !matches!(origin, crate::bus::Origin::Internal { .. }) {
            let _ = m.send_text(&origin, &text).await;
        }
    }
    let mut v = serde_json::to_value(&report)?;
    v["text"] = json!(text);
    Ok(v)
}

// ------------------------------------------------------------------ YAML

/// Sous-ensemble de YAML suffisant pour `config.yaml` d'Hermes : tables et listes par
/// indentation, listes et tables en ligne, chaînes entre guillemets, blocs `|` et `>`,
/// commentaires. Tolérant : une ligne incomprise est sautée, jamais fatale.
pub mod yaml;

#[cfg(test)]
mod tests;
