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

mod mcp;
pub use mcp::*;

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
