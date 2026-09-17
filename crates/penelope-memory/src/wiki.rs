//! Vault tenu en wiki Markdown valide à tout moment (issue #29).
//!
//! ```text
//! écriture ─► touch : type, created, updated     ─► propriétés YAML interrogeables
//! lien ─────► Resolver : wikilink par nom de fichier ou chemin, racine d'abord en cas de doublon
//! rêve ─────► lint : liens non résolus, orphelines, impasses, alias et noms en double,
//!             identifiants de bloc invalides ou dupliqués, propriétés mal typées
//! opération ► log.md : `## [AAAA-MM-JJ] op | titre`, en ajout seul
//! renommage ► rename_note : fichier déplacé, wikilinks réécrits dans tout le vault
//! ```
//!
//! Méthode « LLM Wiki » : sources brutes immuables (`attachments/`), wiki possédé par
//! l'agent, opérations ingest / query / lint tracées dans `log.md`.

use penelope_kernel::frontmatter::{self, FmValue};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

/// Dossier des pièces jointes (originaux immuables).
pub const ATTACHMENTS_DIR: &str = "attachments";
/// Journal des opérations, en ajout seul.
pub const LOG_FILE: &str = "log.md";

/// Type (`type:`) d'une note selon son emplacement ; `None` pour une note libre.
pub fn note_type(rel: &str) -> Option<&'static str> {
    let rel = rel.trim_start_matches("./");
    let top = rel.split('/').next().unwrap_or_default();
    Some(match (top, rel) {
        (_, "profil.md") => "profil",
        (_, "memoire.md") => "memoire",
        (_, "projets.md") => "projets",
        (_, "notes.md") => "notes",
        (_, "DREAMS.md") => "revue",
        (_, "index.md") => "index",
        (_, LOG_FILE) => "log",
        (_, "concepts/_a-definir.md") => "index",
        ("journal", _) => "journal",
        ("sources", _) => "source",
        ("concepts", _) => "concept",
        ("accueil", _) => "accueil",
        ("audits", _) => "audit",
        ("entites", _) => "entite",
        ("pratiques", _) => "pratique",
        ("notes", _) => "session",
        _ => return None,
    })
}

/// Pose les propriétés d'une note sans toucher au reste : `type` et `created` s'ils
/// manquent, `updated` à la date du jour si `bump` (ou s'il manque). Les lignes existantes
/// du frontmatter sont conservées telles quelles ; un frontmatter invalide n'est pas
/// réécrit (le lint le signale).
pub fn touch(raw: &str, kind: &str, today: &str, bump: bool, extra: &[(&str, &str)]) -> String {
    let normalised = raw.replace("\r\n", "\n");
    let Ok(fm) = frontmatter::parse(&normalised) else {
        return raw.to_string();
    };
    let mut wanted: Vec<(String, String)> = Vec::new();
    if !fm.has("created") {
        wanted.push(("created".into(), today.into()));
    }
    for (k, v) in extra {
        if !fm.has(k) {
            wanted.push(((*k).into(), frontmatter::yaml_string(v)));
        }
    }
    if !fm.has("type") {
        wanted.push(("type".into(), kind.into()));
    }
    let set_updated = bump || !fm.has("updated");
    if fm.header_lines == 0 {
        let mut head = String::from("---\n");
        wanted.push(("updated".into(), today.into()));
        wanted.sort();
        for (k, v) in wanted {
            head.push_str(&format!("{k}: {v}\n"));
        }
        head.push_str("---\n");
        return head + &normalised;
    }
    let mut lines: Vec<String> = normalised.split('\n').map(String::from).collect();
    let close = fm.header_lines - 1;
    if set_updated {
        match lines[1..close]
            .iter()
            .position(|l| l.starts_with("updated:"))
        {
            Some(i) => lines[i + 1] = format!("updated: {today}"),
            None => wanted.push(("updated".into(), today.into())),
        }
    }
    if wanted.is_empty() && !set_updated {
        return raw.to_string();
    }
    for (offset, (k, v)) in wanted.into_iter().enumerate() {
        lines.insert(close + offset, format!("{k}: {v}"));
    }
    lines.join("\n")
}

/// Remplace le corps d'une note en gardant son frontmatter tel quel.
pub fn replace_body(raw: &str, body: &str) -> String {
    let normalised = raw.replace("\r\n", "\n");
    match frontmatter::parse(&normalised) {
        Ok(fm) if fm.header_lines > 0 => {
            let head: Vec<&str> = normalised.split('\n').take(fm.header_lines).collect();
            format!("{}\n{body}", head.join("\n"))
        }
        _ => body.to_string(),
    }
}

/// Corps d'une note, sans frontmatter.
pub fn body_of(raw: &str) -> String {
    match frontmatter::parse(raw) {
        Ok(fm) => fm.body,
        Err(_) => raw.to_string(),
    }
}

/// Ligne de `log.md` : préfixe constant, lisible avec `grep "^## \[" log.md`.
pub fn log_line(date: &str, op: &str, title: &str) -> String {
    let title = title.replace(['\n', '\r'], " ");
    format!("## [{date}] {op} | {}", title.trim())
}

/// Ajoute une opération à `log.md` (créé avec ses propriétés au besoin).
pub fn append_log(raw: &str, date: &str, op: &str, title: &str, details: &[String]) -> String {
    let mut body = if raw.trim().is_empty() {
        touch("# Journal des opérations\n", "log", date, true, &[])
    } else {
        touch(raw, "log", date, true, &[])
    };
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(&log_line(date, op, title));
    body.push('\n');
    for d in details {
        body.push_str(&format!("- {}\n", d.replace(['\n', '\r'], " ")));
    }
    body
}

fn link_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(!?)\[\[([^\[\]]+?)\]\]").expect("regex valide"))
}

/// Lien wiki décomposé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    pub embed: bool,
    /// Cible sans sous-chemin ni texte affiché (`note`, `dossier/note`, `doc.pdf`).
    pub target: String,
    /// `#Titre` ou `#^bloc`, sans le `#`.
    pub subpath: Option<String>,
    pub raw: String,
}

/// Liens wiki d'un texte, blocs de code exclus.
pub fn wiki_links(raw: &str) -> Vec<WikiLink> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in raw.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        for c in link_re().captures_iter(line) {
            let inner = c[2].split('|').next().unwrap_or_default();
            let (target, subpath) = match inner.split_once('#') {
                Some((t, s)) => (t.trim().to_string(), Some(s.trim().to_string())),
                None => (inner.trim().to_string(), None),
            };
            out.push(WikiLink {
                embed: !c[1].is_empty(),
                target,
                subpath,
                raw: c[0].to_string(),
            });
        }
    }
    out
}

/// Fichiers du vault, relatifs, hors dossiers cachés (dont les dossiers cachés de
/// configuration d'éditeur, jamais touchés) et hors `archive/`.
pub fn vault_files(vault: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "archive" {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, out);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(vault, vault, &mut out);
    out.sort();
    out
}

fn stem(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.strip_suffix(".md").unwrap_or(name).to_string()
}

fn depth(rel: &str) -> usize {
    rel.matches('/').count()
}

/// Résolution des wikilinks : par nom de fichier (sans chemin ni `.md`), la note la moins
/// profonde d'abord en cas de doublon (puis l'ordre alphabétique), ou par chemin depuis la
/// racine ; à défaut, par alias déclaré dans une seule note.
#[derive(Debug, Clone, Default)]
pub struct Resolver {
    /// Nom en minuscules (sans `.md` pour une note, avec extension sinon) → chemins.
    names: BTreeMap<String, Vec<String>>,
    /// Alias en minuscules → notes qui le déclarent.
    aliases: BTreeMap<String, Vec<String>>,
    files: BTreeSet<String>,
}

impl Resolver {
    pub fn scan(vault: &Path) -> Resolver {
        let mut r = Resolver::default();
        for rel in vault_files(vault) {
            if rel.ends_with(".md")
                && let Ok(raw) = std::fs::read_to_string(vault.join(&rel))
                && let Ok(fm) = frontmatter::parse(&raw)
            {
                for alias in aliases_of(&fm) {
                    r.aliases
                        .entry(alias.to_lowercase())
                        .or_default()
                        .push(rel.clone());
                }
            }
            r.add(&rel);
        }
        r
    }

    fn add(&mut self, rel: &str) {
        let key = if rel.ends_with(".md") {
            stem(rel)
        } else {
            rel.rsplit('/').next().unwrap_or(rel).to_string()
        };
        let paths = self.names.entry(key.to_lowercase()).or_default();
        paths.push(rel.to_string());
        paths.sort_by(|a, b| depth(a).cmp(&depth(b)).then(a.cmp(b)));
        self.files.insert(rel.to_string());
    }

    /// Chemin désigné par la cible d'un lien (`note`, `dossier/note`, `doc.pdf`).
    pub fn resolve(&self, target: &str) -> Option<String> {
        let target = target.trim().trim_start_matches('/');
        if target.is_empty() {
            return None;
        }
        if target.contains('/') {
            let wanted = if Path::new(target).extension().is_some() {
                target.to_lowercase()
            } else {
                format!("{}.md", target.to_lowercase())
            };
            return self
                .files
                .iter()
                .filter(|f| {
                    let f = f.to_lowercase();
                    f == wanted || f.ends_with(&format!("/{wanted}"))
                })
                .min_by(|a, b| depth(a).cmp(&depth(b)).then(a.cmp(b)))
                .cloned();
        }
        let key = target.strip_suffix(".md").unwrap_or(target).to_lowercase();
        self.names.get(&key).and_then(|p| p.first()).cloned()
    }

    /// Note qui déclare cet alias, quand un seul fichier le fait.
    pub fn resolve_alias(&self, alias: &str) -> Option<String> {
        match self.aliases.get(&alias.trim().to_lowercase()) {
            Some(p) if p.len() == 1 => Some(p[0].clone()),
            _ => None,
        }
    }

    /// Nom déjà porté par un fichier du vault.
    pub fn is_taken(&self, name: &str) -> bool {
        self.names.contains_key(&name.to_lowercase())
    }

    /// Fichiers qui portent ce nom, les moins profonds d'abord.
    pub fn paths_named(&self, name: &str) -> Vec<String> {
        self.names
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Cible de lien la plus courte qui désigne `rel` sans ambiguïté : le nom si unique,
    /// sinon le chemin sans `.md`.
    pub fn link_target(&self, rel: &str) -> String {
        let name = stem(rel);
        match self.names.get(&name.to_lowercase()) {
            Some(p) if p.len() > 1 => rel.strip_suffix(".md").unwrap_or(rel).to_string(),
            _ => name,
        }
    }

    /// Nom libre dérivé de `base` : `base`, puis `base-<suffixe>`, puis numérotés.
    pub fn unique_name(&self, base: &str, suffix: &str) -> String {
        if !self.is_taken(base) {
            return base.to_string();
        }
        let first = format!("{base}-{suffix}");
        if !self.is_taken(&first) {
            return first;
        }
        (2..)
            .map(|n| format!("{first}-{n}"))
            .find(|c| !self.is_taken(c))
            .unwrap_or(first)
    }
}

/// Alias d'une note : `aliases` (liste), et l'ancien `alias` (chaîne séparée par virgules).
pub fn aliases_of(fm: &frontmatter::Frontmatter) -> Vec<String> {
    let mut out = fm.list("aliases");
    if let Some(legacy) = fm.str("alias") {
        out.extend(
            legacy
                .split(',')
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty()),
        );
    }
    out.retain(|a| !a.trim().is_empty());
    out
}

/// Réécrit les liens qui visent `old` (nom ou chemin sans `.md`) vers `new`, sous-chemin et
/// texte affiché conservés.
pub fn rewrite_links(raw: &str, old: &[String], new: &str) -> String {
    let old: Vec<String> = old.iter().map(|o| o.to_lowercase()).collect();
    link_re()
        .replace_all(raw, |c: &regex::Captures| {
            let inner = &c[2];
            let (head, display) = match inner.split_once('|') {
                Some((h, d)) => (h, Some(d)),
                None => (inner, None),
            };
            let (target, sub) = match head.split_once('#') {
                Some((t, s)) => (t, Some(s)),
                None => (head, None),
            };
            if !old.contains(&target.trim().to_lowercase()) {
                return c[0].to_string();
            }
            let mut out = format!("{}[[{new}", &c[1]);
            if let Some(s) = sub {
                out.push('#');
                out.push_str(s);
            }
            if let Some(d) = display {
                out.push('|');
                out.push_str(d);
            }
            out.push_str("]]");
            out
        })
        .into_owned()
}

/// Déplace une note et réécrit tous les wikilinks qui la visaient : un déplacement sans
/// réécriture laisserait des liens morts. Renvoie le nombre de fichiers réécrits.
pub fn rename_note(vault: &Path, from: &str, to: &str) -> std::io::Result<usize> {
    let src = vault.join(from);
    let dst = vault.join(to);
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::rename(&src, &dst)?;
    let old = vec![
        stem(from),
        from.strip_suffix(".md").unwrap_or(from).to_string(),
    ];
    let resolver = Resolver::scan(vault);
    let new = resolver.link_target(to);
    let mut rewritten = 0;
    for rel in vault_files(vault)
        .into_iter()
        .filter(|f| f.ends_with(".md"))
    {
        let path = vault.join(&rel);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let out = rewrite_links(&raw, &old, &new);
        if out != raw {
            penelope_kernel::config::atomic_write(&path, out.as_bytes())
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

// ------------------------------------------------------------------ lint

/// Constat du lint, par catégorie.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct LintReport {
    pub notes: usize,
    /// (note, lien) : cible introuvable.
    pub unresolved: Vec<(String, String)>,
    /// (note, lien) : `#^bloc` absent de la note visée.
    pub broken_blocks: Vec<(String, String)>,
    /// Notes sans lien entrant (sources, concepts, entités, pratiques).
    pub orphans: Vec<String>,
    /// Notes sans lien sortant (sources, concepts, entités).
    pub deadends: Vec<String>,
    /// (alias, notes) : alias porté par plusieurs notes, ou égal au nom d'une autre note.
    pub duplicate_aliases: Vec<(String, Vec<String>)>,
    /// (nom, chemins) : même nom de fichier dans plusieurs dossiers.
    pub name_collisions: Vec<(String, Vec<String>)>,
    /// (note, ligne, identifiant) : caractères hors lettres latines, chiffres et tirets, ou
    /// ancien commentaire `<!-- uid -->`.
    pub invalid_block_ids: Vec<(String, usize, String)>,
    /// (note, identifiant) : identifiant de bloc répété dans la note.
    pub duplicate_block_ids: Vec<(String, String)>,
    /// (note, problème) : propriétés mal typées ou absentes.
    pub bad_properties: Vec<(String, String)>,
}

impl LintReport {
    pub fn problems(&self) -> usize {
        self.unresolved.len()
            + self.broken_blocks.len()
            + self.duplicate_aliases.len()
            + self.name_collisions.len()
            + self.invalid_block_ids.len()
            + self.duplicate_block_ids.len()
            + self.bad_properties.len()
    }

    pub fn is_clean(&self) -> bool {
        self.problems() == 0
    }

    /// Une ligne par catégorie non vide.
    pub fn summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |n: usize, label: &str, sample: Option<String>| {
            if n > 0 {
                out.push(match sample {
                    Some(s) => format!("{n} {label} (ex. {s})"),
                    None => format!("{n} {label}"),
                });
            }
        };
        push(
            self.unresolved.len(),
            "lien(s) non résolu(s)",
            self.unresolved
                .first()
                .map(|(f, l)| format!("{l} dans {f}")),
        );
        push(
            self.broken_blocks.len(),
            "renvoi(s) vers un bloc absent",
            self.broken_blocks
                .first()
                .map(|(f, l)| format!("{l} dans {f}")),
        );
        push(
            self.name_collisions.len(),
            "nom(s) de fichier en double",
            self.name_collisions
                .first()
                .map(|(n, p)| format!("{n} : {}", p.join(", "))),
        );
        push(
            self.duplicate_aliases.len(),
            "alias en double",
            self.duplicate_aliases
                .first()
                .map(|(a, p)| format!("{a} : {}", p.join(", "))),
        );
        push(
            self.invalid_block_ids.len(),
            "identifiant(s) de bloc invalide(s)",
            self.invalid_block_ids
                .first()
                .map(|(f, l, id)| format!("{id} dans {f}:{l}")),
        );
        push(
            self.duplicate_block_ids.len(),
            "identifiant(s) de bloc dupliqué(s)",
            self.duplicate_block_ids
                .first()
                .map(|(f, id)| format!("{id} dans {f}")),
        );
        push(
            self.bad_properties.len(),
            "propriété(s) à corriger",
            self.bad_properties
                .first()
                .map(|(f, m)| format!("{f} : {m}")),
        );
        push(
            self.orphans.len(),
            "note(s) orpheline(s)",
            self.orphans.first().cloned(),
        );
        push(
            self.deadends.len(),
            "note(s) sans lien sortant",
            self.deadends.first().cloned(),
        );
        out
    }
}

/// Notes dont le lien entrant ou sortant est attendu.
fn graph_note(rel: &str) -> bool {
    ["sources/", "concepts/", "entites/", "pratiques/"]
        .iter()
        .any(|p| rel.starts_with(p))
        && !stem(rel).starts_with('_')
}

fn is_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn loose_block_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?:^|\s)\^(\S+)\s*$").expect("regex valide"))
}

/// Identifiants de bloc d'une note : (ligne, identifiant).
fn block_ids(raw: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, line) in raw.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(id) =
            crate::vault::standalone_block_id(line).or_else(|| crate::vault::block_id(line))
        {
            out.push((i + 1, id));
        }
    }
    out
}

/// Lint du graphe et des propriétés, sans rien modifier.
pub fn lint(vault: &Path) -> LintReport {
    let resolver = Resolver::scan(vault);
    let mut report = LintReport::default();
    let notes: Vec<String> = vault_files(vault)
        .into_iter()
        .filter(|f| f.ends_with(".md"))
        .collect();
    report.notes = notes.len();
    let mut inbound: BTreeMap<String, usize> = BTreeMap::new();
    let mut outbound: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocks: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut contents: BTreeMap<String, String> = BTreeMap::new();

    for rel in &notes {
        let Ok(raw) = std::fs::read_to_string(vault.join(rel)) else {
            continue;
        };
        // Propriétés.
        match frontmatter::parse(&raw) {
            Err(e) => report
                .bad_properties
                .push((rel.clone(), format!("frontmatter invalide ({e})"))),
            Ok(fm) => {
                if fm.has("alias") {
                    report
                        .bad_properties
                        .push((rel.clone(), "`alias` déprécié : `aliases` en liste".into()));
                }
                for key in ["aliases", "tags", "cssclasses"] {
                    if let Some(FmValue::Str(_)) = fm.get(key) {
                        report
                            .bad_properties
                            .push((rel.clone(), format!("`{key}` doit être une liste")));
                    }
                }
                for tag in fm.list("tags") {
                    let t = tag.trim_start_matches('#');
                    if t.chars().all(|c| c.is_ascii_digit())
                        || t.chars()
                            .any(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
                    {
                        report
                            .bad_properties
                            .push((rel.clone(), format!("tag invalide : `{tag}`")));
                    }
                }
                for key in ["created", "updated", "date", "expire"] {
                    if let Some(v) = fm.get(key) {
                        let ok = v.as_str().is_some_and(is_date);
                        if !ok {
                            report.bad_properties.push((
                                rel.clone(),
                                format!("`{key}` n'est pas une date AAAA-MM-JJ"),
                            ));
                        }
                    }
                }
                if let Some(kind) = note_type(rel)
                    && !fm.has("type")
                {
                    report
                        .bad_properties
                        .push((rel.clone(), format!("`type: {kind}` manquant")));
                }
            }
        }
        // Identifiants de bloc.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (i, line) in raw.lines().enumerate() {
            if crate::vault::Annotations::has_legacy_uid(line) {
                let id = crate::vault::line_uid(line).unwrap_or_default();
                report
                    .invalid_block_ids
                    .push((rel.clone(), i + 1, format!("<!-- uid: {id} -->")));
            } else if let Some(c) = loose_block_re().captures(line.trim_end())
                && !crate::vault::is_valid_block_id(&c[1])
                && !line.contains("[[")
                && c[1].chars().all(|ch| !ch.is_whitespace())
                && c[1].len() > 1
            {
                report
                    .invalid_block_ids
                    .push((rel.clone(), i + 1, c[1].to_string()));
            }
        }
        for (_, id) in block_ids(&raw) {
            if !seen.insert(id.clone()) {
                report.duplicate_block_ids.push((rel.clone(), id));
            }
        }
        blocks.insert(rel.clone(), seen);
        contents.insert(rel.clone(), raw);
    }

    for (rel, raw) in &contents {
        for link in wiki_links(raw) {
            if link.target.is_empty() {
                // `[[#titre]]` ou `[[#^bloc]]` : la note elle-même.
                if let Some(id) = link.subpath.as_deref().and_then(|s| s.strip_prefix('^'))
                    && !blocks.get(rel).is_some_and(|b| b.contains(id))
                {
                    report.broken_blocks.push((rel.clone(), link.raw.clone()));
                }
                continue;
            }
            // Nom de fichier ou chemin, puis alias déclaré par une seule note.
            let Some(target) = resolver
                .resolve(&link.target)
                .or_else(|| resolver.resolve_alias(&link.target))
            else {
                report.unresolved.push((rel.clone(), link.raw.clone()));
                continue;
            };
            *outbound.entry(rel.clone()).or_default() += 1;
            if &target != rel {
                *inbound.entry(target.clone()).or_default() += 1;
            }
            if let Some(id) = link.subpath.as_deref().and_then(|s| s.strip_prefix('^'))
                && target.ends_with(".md")
                && !blocks.get(&target).is_some_and(|b| b.contains(id))
            {
                report.broken_blocks.push((rel.clone(), link.raw.clone()));
            }
        }
    }

    for rel in &notes {
        if graph_note(rel) {
            if inbound.get(rel).copied().unwrap_or(0) == 0 {
                report.orphans.push(rel.clone());
            }
            if outbound.get(rel).copied().unwrap_or(0) == 0 && !rel.starts_with("pratiques/") {
                report.deadends.push(rel.clone());
            }
        }
    }
    for (name, paths) in &resolver.names {
        let md: Vec<String> = paths
            .iter()
            .filter(|p| p.ends_with(".md"))
            .cloned()
            .collect();
        if md.len() > 1 {
            report.name_collisions.push((name.clone(), md));
        }
    }
    for (alias, paths) in &resolver.aliases {
        let mut owners: Vec<String> = paths.clone();
        if let Some(named) = resolver.names.get(alias) {
            owners.extend(named.iter().filter(|p| !paths.contains(p)).cloned());
        }
        owners.sort();
        owners.dedup();
        if owners.len() > 1 {
            report.duplicate_aliases.push((alias.clone(), owners));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(vault: &Path, rel: &str, body: &str) {
        let p = vault.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn touch_sets_properties_without_rewriting_the_rest() {
        let fresh = touch(
            "# Mémoire de fond\n- x ^A\n",
            "memoire",
            "2026-09-17",
            true,
            &[],
        );
        assert_eq!(
            fresh,
            "---\ncreated: 2026-09-17\ntype: memoire\nupdated: 2026-09-17\n---\n# Mémoire de fond\n- x ^A\n"
        );
        let later = touch(&fresh, "memoire", "2026-09-20", true, &[]);
        assert!(later.contains("created: 2026-09-17\n"));
        assert!(later.contains("updated: 2026-09-20\n"));
        assert_eq!(later.matches("updated:").count(), 1);
        let human = "---\ntitre: Mes notes # à moi\naliases:\n  - n\n---\ncorps\n";
        let touched = touch(
            human,
            "notes",
            "2026-09-17",
            false,
            &[("date", "2026-09-17")],
        );
        assert!(touched.starts_with("---\ntitre: Mes notes # à moi\naliases:\n  - n\n"));
        assert!(touched.contains("date: 2026-09-17\n") && touched.contains("type: notes\n"));
        assert_eq!(touch(&touched, "notes", "2026-09-18", false, &[]), touched);
        let broken = "---\nsans deux-points\n---\n";
        assert_eq!(touch(broken, "notes", "2026-09-17", true, &[]), broken);
    }

    #[test]
    fn wikilinks_resolve_by_name_then_path() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path();
        write(v, "yobbu.md", "# racine\n");
        write(v, "sources/yobbu.md", "---\ntype: source\n---\n");
        write(
            v,
            "concepts/factur-x.md",
            "---\ntype: concept\naliases:\n  - ZUGFeRD\n---\n",
        );
        write(v, "attachments/contrat.pdf", "%PDF");
        write(v, ".editeur/notes-cachees.md", "[[nulle-part]]");
        let r = Resolver::scan(v);
        assert_eq!(
            r.resolve("yobbu").as_deref(),
            Some("yobbu.md"),
            "racine d'abord"
        );
        assert_eq!(
            r.resolve("sources/yobbu").as_deref(),
            Some("sources/yobbu.md")
        );
        assert_eq!(
            r.resolve("Factur-X").as_deref(),
            Some("concepts/factur-x.md")
        );
        assert_eq!(
            r.resolve("contrat.pdf").as_deref(),
            Some("attachments/contrat.pdf")
        );
        assert_eq!(r.resolve("zugferd"), None, "un alias n'est pas un nom");
        assert_eq!(
            r.resolve_alias("zugferd").as_deref(),
            Some("concepts/factur-x.md")
        );
        assert_eq!(r.link_target("sources/yobbu.md"), "sources/yobbu");
        assert_eq!(r.link_target("concepts/factur-x.md"), "factur-x");
        assert_eq!(r.unique_name("yobbu", "concept"), "yobbu-concept");
        assert!(!vault_files(v).iter().any(|f| f.starts_with('.')));
    }

    #[test]
    fn renaming_a_note_rewrites_every_link() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path();
        write(v, "accueil/2026-09-17.md", "# Accueil\n");
        write(
            v,
            "profil.md",
            "- Tutoiement [[2026-09-17#^q5|séance]] ^A\n- Voir ![[accueil/2026-09-17]] ^B\n- [[autre]] ^C\n",
        );
        let n = rename_note(v, "accueil/2026-09-17.md", "accueil/accueil-2026-09-17.md").unwrap();
        assert_eq!(n, 1);
        let profil = std::fs::read_to_string(v.join("profil.md")).unwrap();
        assert!(
            profil.contains("[[accueil-2026-09-17#^q5|séance]]"),
            "{profil}"
        );
        assert!(profil.contains("![[accueil-2026-09-17]]"));
        assert!(profil.contains("[[autre]]"));
        assert!(v.join("accueil/accueil-2026-09-17.md").exists());
    }

    /// #48 : un alias écrit en liste en ligne, avec une virgule dans la valeur citée,
    /// résout le lien qui le vise et ne crée pas d'alias fantôme.
    #[test]
    fn an_inline_alias_list_with_a_comma_resolves_its_links() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path();
        write(
            v,
            "index.md",
            "---\ntype: index\n---\n- [[Le Crew, coworking]] · [[Crew]]\n",
        );
        write(
            v,
            "entites/le-crew.md",
            "---\ntype: entite\naliases: [\"Le Crew, coworking\", Crew]\n---\n- [[index]]\n",
        );
        let r = lint(v);
        assert!(r.unresolved.is_empty(), "alias fantôme : {r:?}");
        assert!(r.duplicate_aliases.is_empty(), "{r:?}");
    }

    #[test]
    fn lint_reports_the_graph_problems() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path();
        write(
            v,
            "index.md",
            "---\ntype: index\n---\n- [[contrat]] · [[factur-x]]\n",
        );
        write(
            v,
            "sources/contrat.md",
            "---\ntype: source\ncreated: 2026-09-17\n---\n- Concepts : [[factur-x]] [[ZUGFeRD]] [[fantome]] ^concepts-contrat\n- [[memoire#^ABSENT]]\n",
        );
        write(
            v,
            "concepts/factur-x.md",
            "---\ntype: concept\nalias: ZUGFeRD, FX\ntags: facture\n---\n- Norme ^D1\n- Doublon ^D1\n- Ancien <!-- uid: X9 -->\n- Invalide ^bad_id\n",
        );
        write(
            v,
            "concepts/isole.md",
            "---\ntype: concept\ncreated: hier\n---\n- rien\n",
        );
        write(v, "memoire.md", "- fait ^M1\n");
        write(v, "journal/2026-09-17.md", "- note\n");
        write(
            v,
            "entites/contrat.md",
            "---\ntype: entite\n---\n[[contrat]]\n",
        );
        let r = lint(v);
        assert!(
            r.unresolved.iter().any(|(_, l)| l == "[[fantome]]"),
            "{r:?}"
        );
        assert!(
            !r.unresolved.iter().any(|(_, l)| l == "[[ZUGFeRD]]"),
            "résolu par alias"
        );
        assert!(
            r.broken_blocks
                .iter()
                .any(|(_, l)| l == "[[memoire#^ABSENT]]")
        );
        assert!(r.orphans.contains(&"concepts/isole.md".to_string()));
        assert!(r.deadends.contains(&"concepts/factur-x.md".to_string()));
        assert!(r.name_collisions.iter().any(|(n, _)| n == "contrat"));
        assert!(
            r.duplicate_block_ids
                .contains(&("concepts/factur-x.md".into(), "D1".into()))
        );
        assert!(r.invalid_block_ids.iter().any(|(_, _, id)| id == "bad_id"));
        assert!(
            r.invalid_block_ids
                .iter()
                .any(|(_, _, id)| id.contains("X9"))
        );
        let props: Vec<&String> = r.bad_properties.iter().map(|(_, m)| m).collect();
        assert!(
            props.iter().any(|m| m.contains("`alias` déprécié")),
            "{props:?}"
        );
        assert!(
            props
                .iter()
                .any(|m| m.contains("`tags` doit être une liste"))
        );
        assert!(props.iter().any(|m| m.contains("`created`")));
        assert!(props.iter().any(|m| m.contains("`type: memoire` manquant")));
        assert!(props.iter().any(|m| m.contains("`type: journal` manquant")));
        assert!(!r.is_clean());
        assert!(
            r.summary()
                .iter()
                .any(|l| l.starts_with("1 lien(s) non résolu(s)"))
        );
    }

    #[test]
    fn log_lines_are_greppable_and_appended() {
        let first = append_log("", "2026-09-17", "ingest", "Contrat v2", &[]);
        let second = append_log(
            &first,
            "2026-09-18",
            "dream",
            "3 promues",
            &["memoire.md".into()],
        );
        let lines: Vec<&str> = second.lines().filter(|l| l.starts_with("## [")).collect();
        assert_eq!(
            lines,
            vec![
                "## [2026-09-17] ingest | Contrat v2",
                "## [2026-09-18] dream | 3 promues"
            ]
        );
        assert!(second.starts_with(&first[..first.find("updated").unwrap()]));
        assert!(second.contains("type: log") && second.contains("updated: 2026-09-18"));
    }
}
