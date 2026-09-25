//! Fraîcheur de la documentation (issues #3 et #38). La documentation est aussi ce que
//! Pénélope sait d'elle-même (`self_docs`) : une page périmée lui fait dire des choses
//! fausses.
//!
//! ```text
//!  index docs/README.md ─► cite chaque guide et chaque décision
//!  liens ────────────────► aucun lien relatif mort, ancres comprises
//!  commandes ────────────► catalogue Telegram = docs/telegram.md
//!  configuration ────────► chaque clé dans la référence générée d'install-headless.md
//!  outils ───────────────► chaque outil natif dans la référence générée
//!  version ──────────────► sa section dans progress.md
//!  limites ──────────────► aucune méthode, commande ou outil livré présenté comme manquant
//! ```
//!
//! Les deux références se régénèrent :
//!
//! ```bash
//! UPDATE_DOCS=1 cargo test -p penelope-evals --test docs
//! ```

use penelope_evals::ca_matrix;
use penelope_kernel::api::method;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    ca_matrix::repo_root()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{} : {e}", path.display()))
}

/// `README.md` et tout `docs/`, décisions comprises.
fn markdown_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    let mut out = vec![root().join("README.md")];
    walk(&root().join("docs"), &mut out);
    out.sort();
    out
}

/// Lignes hors blocs de code.
fn prose(markdown: &str) -> Vec<&str> {
    let mut in_code = false;
    markdown
        .lines()
        .filter(|l| {
            if l.trim_start().starts_with("```") {
                in_code = !in_code;
                return false;
            }
            !in_code
        })
        .collect()
}

/// Ancres GitHub des titres d'un fichier, doublons suffixés.
fn anchors(markdown: &str) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = BTreeSet::new();
    for line in prose(markdown) {
        let level = line.chars().take_while(|c| *c == '#').count();
        if level == 0 || !line[level..].starts_with(' ') {
            continue;
        }
        let base = penelope_executor::selfdocs::anchor(line[level..].trim());
        let n = seen.entry(base.clone()).or_insert(0);
        out.insert(if *n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        });
        *n += 1;
    }
    out
}

/// Liens Markdown `[texte](cible)` hors blocs de code.
fn links(markdown: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\]\(([^)\s]+)(?:\s+\x22[^\x22]*\x22)?\)").unwrap();
    prose(markdown)
        .iter()
        .flat_map(|l| {
            // Le code en ligne peut contenir des crochets : on l'écarte.
            let without_code = regex::Regex::new(r"`[^`]*`").unwrap().replace_all(l, "");
            re.captures_iter(&without_code)
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

// ------------------------------------------------------------------ index et liens

#[test]
fn the_index_cites_every_guide_and_decision() {
    let docs = root().join("docs");
    let index = read(&docs.join("README.md"));
    let targets: BTreeSet<String> = links(&index)
        .into_iter()
        .map(|l| l.split('#').next().unwrap_or_default().to_string())
        .collect();
    let mut missing = Vec::new();
    for f in markdown_files() {
        let Ok(rel) = f.strip_prefix(&docs) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel == "README.md" {
            continue;
        }
        if !targets.contains(&rel) {
            missing.push(rel);
        }
    }
    assert!(
        missing.is_empty(),
        "docs/README.md ne cite pas : {}",
        missing.join(", ")
    );
    assert!(
        read(&root().join("README.md")).contains("](docs/README.md)"),
        "le README racine doit renvoyer à l'index docs/README.md"
    );
}

#[test]
fn the_readme_does_not_freeze_changing_coverage_counts() {
    let readme = read(&root().join("README.md"));
    let counts =
        regex::Regex::new(r"\b[0-9][0-9 ]* (?:tests verts|outils|clés de configuration)\b")
            .unwrap();
    let stale: Vec<&str> = counts.find_iter(&readme).map(|m| m.as_str()).collect();
    assert!(
        stale.is_empty(),
        "compteurs du README à remplacer par une référence générée : {stale:?}"
    );
}

#[test]
fn no_relative_link_is_dead_anchors_included() {
    let mut dead = Vec::new();
    let mut cache: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    for file in markdown_files() {
        let raw = read(&file);
        let dir = file.parent().unwrap().to_path_buf();
        for target in links(&raw) {
            if target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            let (path, anchor) = match target.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (target.as_str(), None),
            };
            let resolved = if path.is_empty() {
                file.clone()
            } else {
                dir.join(path)
            };
            if !resolved.exists() {
                dead.push(format!("{} → {target}", file.display()));
                continue;
            }
            if let Some(a) = anchor
                && resolved.extension().is_some_and(|x| x == "md")
            {
                let set = cache
                    .entry(resolved.clone())
                    .or_insert_with(|| anchors(&read(&resolved)));
                let wanted = a.to_lowercase();
                if !set.contains(&wanted) {
                    dead.push(format!("{} → {target} (ancre inconnue)", file.display()));
                }
            }
        }
    }
    assert!(dead.is_empty(), "liens morts :\n{}", dead.join("\n"));
}

// ------------------------------------------------------------------ commandes

#[test]
fn telegram_commands_and_their_documentation_match() {
    let catalog: BTreeSet<&str> = penelope_telegram::commands::all()
        .iter()
        .map(|c| c.name)
        .collect();
    let doc = read(&root().join("docs/telegram.md"));
    let missing: Vec<&&str> = catalog
        .iter()
        .filter(|c| !doc.contains(&format!("/{c}")))
        .collect();
    assert!(
        missing.is_empty(),
        "commandes absentes de docs/telegram.md : {missing:?}"
    );
    // Et inversement : la section « Commandes » ne cite que des commandes du catalogue.
    let start = doc.find("\n## Commandes").expect("section Commandes");
    let end = doc[start + 1..]
        .find("\n## ")
        .map(|i| start + 1 + i)
        .unwrap_or(doc.len());
    let re = regex::Regex::new(r"(?:^|[\s`(,])/([a-z][a-z_]*)\b").unwrap();
    let unknown: BTreeSet<String> = re
        .captures_iter(&doc[start..end])
        .map(|c| c[1].to_string())
        .filter(|c| !catalog.contains(c.as_str()) && !["start", "help"].contains(&c.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "docs/telegram.md cite des commandes hors catalogue : {unknown:?}"
    );
}

// ------------------------------------------------------------------ références générées

/// Remplace (ou vérifie) le bloc généré `name` d'un fichier.
fn generated_block(file: &Path, name: &str, content: &str) {
    let raw = read(file);
    let begin = format!("<!-- reference:{name}:debut");
    let end = format!("<!-- reference:{name}:fin -->");
    let b = raw
        .find(&begin)
        .unwrap_or_else(|| panic!("{} : bloc `{name}` absent", file.display()));
    let b_end = raw[b..].find("-->").map(|i| b + i + 3).unwrap();
    let e = raw
        .find(&end)
        .unwrap_or_else(|| panic!("{} : fin du bloc `{name}` absente", file.display()));
    let current = &raw[b_end..e];
    let wanted = format!("\n{content}");
    if current == wanted {
        return;
    }
    if std::env::var("UPDATE_DOCS").as_deref() == Ok("1") {
        let updated = format!("{}{wanted}{}", &raw[..b_end], &raw[e..]);
        std::fs::write(file, updated).unwrap();
        return;
    }
    panic!(
        "{} : référence `{name}` périmée. Régénérer :\n\
         UPDATE_DOCS=1 cargo test -p penelope-evals --test docs",
        file.display()
    );
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Champs documentés de chaque structure de `config.rs` : (champ, type, doc).
fn config_structs(source: &str) -> BTreeMap<String, Vec<(String, String, String)>> {
    let struct_re = regex::Regex::new(r"^pub struct (\w+) \{").unwrap();
    let field_re = regex::Regex::new(r"^    pub (\w+): (.+),$").unwrap();
    let mut out: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut doc: Vec<String> = Vec::new();
    for line in source.lines() {
        if let Some(c) = struct_re.captures(line) {
            current = Some(c[1].to_string());
            doc.clear();
            continue;
        }
        if line.starts_with('}') {
            current = None;
            continue;
        }
        let Some(name) = &current else { continue };
        let t = line.trim();
        if let Some(d) = t.strip_prefix("///") {
            doc.push(d.trim().to_string());
        } else if t.starts_with("#[") {
        } else if let Some(c) = field_re.captures(line) {
            out.entry(name.clone()).or_default().push((
                c[1].to_string(),
                c[2].to_string(),
                doc.join(" "),
            ));
            doc.clear();
        } else {
            doc.clear();
        }
    }
    out
}

fn value_at<'a>(v: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(v, |acc, k| acc.get(k))
}

/// Lignes de la référence : (clé, défaut, rôle).
fn config_rows(
    structs: &BTreeMap<String, Vec<(String, String, String)>>,
    defaults: &Value,
    name: &str,
    prefix: Vec<String>,
    rows: &mut Vec<(String, String, String)>,
) {
    for (field, ty, doc) in structs.get(name).cloned().unwrap_or_default() {
        let mut path = prefix.clone();
        path.push(field.clone());
        if structs.contains_key(ty.as_str()) {
            config_rows(structs, defaults, &ty, path, rows);
            continue;
        }
        if let Some(inner) = ty.strip_prefix("BTreeMap<String, ") {
            let value_ty = inner.trim_end_matches('>').trim();
            let keys: Vec<String> = value_at(defaults, &path)
                .and_then(|v| v.as_object())
                .map(|m| m.keys().cloned().collect())
                .filter(|k: &Vec<String>| !k.is_empty())
                .unwrap_or_else(|| vec!["<nom>".to_string()]);
            for k in keys {
                let mut p = path.clone();
                p.push(k);
                if structs.contains_key(value_ty) {
                    config_rows(structs, defaults, value_ty, p, rows);
                } else {
                    rows.push((p.join("."), default_of(defaults, &p), doc.clone()));
                }
            }
            continue;
        }
        rows.push((path.join("."), default_of(defaults, &path), doc));
    }
}

fn default_of(defaults: &Value, path: &[String]) -> String {
    match value_at(defaults, path) {
        Some(v) => format!("`{v}`"),
        None => "–".into(),
    }
}

/// Toutes les clés de configuration : (clé, défaut rendu, rôle).
fn config_reference() -> Vec<(String, String, String)> {
    // config.rs et ses sections, sorties dans config/*.rs (épopée #208, lot L).
    let mut source = read(&root().join("crates/penelope-kernel/src/config.rs"));
    let mut parts: Vec<PathBuf> =
        std::fs::read_dir(root().join("crates/penelope-kernel/src/config"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs") && !p.ends_with("tests.rs"))
            .collect();
    parts.sort();
    for p in parts {
        source.push_str(&read(&p));
    }
    let structs = config_structs(&source);
    let defaults = serde_json::to_value(penelope_kernel::config::Config::default()).unwrap();
    let mut rows = Vec::new();
    config_rows(&structs, &defaults, "Config", Vec::new(), &mut rows);
    rows
}

#[test]
fn every_configuration_key_is_documented() {
    let rows = config_reference();
    let undocumented: Vec<&String> = rows
        .iter()
        .filter(|(_, _, doc)| doc.is_empty())
        .map(|(k, _, _)| k)
        .collect();
    assert!(
        undocumented.is_empty(),
        "clés sans commentaire `///` dans config.rs : {undocumented:?}"
    );
    let mut table = String::new();
    let mut section = String::new();
    for (key, default, doc) in &rows {
        let top = key.split('.').next().unwrap_or_default().to_string();
        if top != section {
            if !section.is_empty() {
                table.push('\n');
            }
            table.push_str(&format!(
                "**[{top}]**\n\n| Clé | Défaut | Rôle |\n|---|---|---|\n"
            ));
            section = top;
        }
        table.push_str(&format!(
            "| `{}` | {} | {} |\n",
            key,
            cell(default),
            cell(doc)
        ));
    }
    generated_block(&root().join("docs/install-headless.md"), "config", &table);
    // Toutes les clés figurent donc dans install-headless.md.
    let doc = read(&root().join("docs/install-headless.md"));
    for (key, _, _) in &rows {
        assert!(doc.contains(&format!("`{key}`")), "{key}");
    }
}

/// Nombre avec espaces de milliers : `131 072`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// #107 : la page sur le contexte suit le code. Ses chiffres par fenêtre sont calculés
/// par `CompactionParams`, chaque clé citée existe, et une valeur citée sous la forme
/// `` `clé` = `valeur` `` est le défaut du code.
#[test]
fn the_context_page_follows_the_code() {
    use penelope_context::compaction::{CompactionParams, reserved_output};
    let cfg = penelope_kernel::config::Config::default();
    let mut table = String::from(
        "| Fenêtre | Seuil | Compaction de fond dès | Réserve de réponse | Queue verbatim | \
         Groupe d'outils gardé entier |\n|---|---|---|---|---|---|\n",
    );
    for window in [8_192u64, 32_768, 131_072, 200_000, 1_000_000] {
        let p = CompactionParams::from_config(&cfg, window, "openrouter:exemple/modele");
        table.push_str(&format!(
            "| {} | {} % | {} | {} | {} | {} |\n",
            grouped(window),
            (p.threshold * 100.0).round(),
            grouped(p.background_threshold_tokens(p.background_margin)),
            grouped(reserved_output(window)),
            grouped(p.tail_budget()),
            grouped(p.tool_group_budget()),
        ));
    }
    let page = root().join("docs/context.md");
    generated_block(&page, "fenetres", &table);

    let rows = config_reference();
    let defaults: BTreeMap<&str, &str> = rows
        .iter()
        .map(|(k, d, _)| (k.as_str(), d.as_str()))
        .collect();
    let key_re =
        regex::Regex::new(r"`((?:context|budget|models|memory|sandbox|tools)\.[a-z_.]+)`").unwrap();
    let value_re = regex::Regex::new(r"`([a-z_]+\.[a-z_.]+)` = (`[^`]+`)").unwrap();
    // Un nom d'événement a la forme d'une clé : il existe s'il est émis par le daemon,
    // sous-modules compris (les tests sortis dans `<module>/tests.rs` aussi), ou par la
    // conversation qui en est sortie (compaction, épopée #208, T23).
    fn walk(dir: &Path, out: &mut String) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push_str(&read(&p));
            }
        }
    }
    let mut daemon_source = String::new();
    for dir in [
        "crates/penelope-daemon/src",
        "crates/penelope-conversation/src",
    ] {
        walk(&root().join(dir), &mut daemon_source);
    }
    let raw = read(&page);
    let mut wrong = Vec::new();
    for line in prose(&raw) {
        for c in key_re.captures_iter(line) {
            let key = &c[1];
            let known = defaults.contains_key(key)
                || daemon_source.contains(&format!("\"{key}\""))
                || defaults.keys().any(|k| {
                    k.strip_suffix(".<nom>")
                        .is_some_and(|base| key == base || key.starts_with(&format!("{base}.")))
                });
            if !known {
                wrong.push(format!("clé inconnue : {key}"));
            }
        }
        for c in value_re.captures_iter(line) {
            match defaults.get(&c[1]) {
                Some(d) if *d == &c[2] => {}
                Some(d) => wrong.push(format!("{} = {} au lieu de {d}", &c[1], &c[2])),
                None => wrong.push(format!("valeur d'une clé inconnue : {}", &c[1])),
            }
        }
    }
    assert!(wrong.is_empty(), "docs/context.md :\n{}", wrong.join("\n"));
}

#[test]
fn every_native_tool_is_documented() {
    let mut table = String::from("| Outil | Risque | Rôle |\n|---|---|---|\n");
    for t in penelope_tools::all_tools() {
        let first = t
            .description
            .split_inclusive(". ")
            .next()
            .unwrap_or(t.description)
            .trim();
        let scope = if t.workflow_only {
            " (dans un workflow)"
        } else if penelope_tools::is_on_demand(t.name) {
            " (à la demande)"
        } else {
            ""
        };
        table.push_str(&format!(
            "| `{}` | {} | {}{scope} |\n",
            t.name,
            t.risk.as_str(),
            cell(first)
        ));
    }
    generated_block(&root().join("docs/install-headless.md"), "outils", &table);
    let all: String = markdown_files().iter().map(|f| read(f)).collect();
    for t in penelope_tools::all_tools() {
        assert!(
            all.contains(&format!("`{}`", t.name)),
            "outil non documenté : {}",
            t.name
        );
    }
}

// ------------------------------------------------------------------ version

#[test]
fn the_workspace_version_has_its_progress_section() {
    let progress = read(&root().join("docs/progress.md"));
    let v = penelope_daemon::VERSION;
    assert!(
        progress.lines().any(|l| l.trim() == format!("### {v}")),
        "docs/progress.md n'a pas de section « ### {v} »"
    );
}

/// Identifiant d'un suffixe de pré-release, dans l'ordre semver : un nombre passe sous
/// un mot (`1 < alpha`), les nombres se comparent entre eux (`alpha.2 < alpha.10`), les
/// mots en ASCII (`alpha < beta < rc`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Number(u64),
    Text(String),
}

/// Ce qui suit les trois nombres : une pré-release, ou rien. Les variantes sont dans cet
/// ordre pour que la version pleine passe au-dessus de toutes ses pré-releases
/// (`1.0.0-rc.1 < 1.0.0`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Pre(Vec<Ident>),
    Full,
}

/// Version d'une section `### x.y.z` ou `### x.y.z-<pré>` de `docs/progress.md`, ou du
/// workspace (`1.0.0-alpha.N` sur la branche v1, #212). L'ordre dérivé est celui de
/// semver : `0.17.59 < 1.0.0-alpha.2 < 1.0.0-alpha.10 < 1.0.0-rc.1 < 1.0.0`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    numbers: (u64, u64, u64),
    stage: Stage,
}

fn parse_version(s: &str) -> Option<Version> {
    let (core, pre) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };
    let mut it = core.split('.');
    let mut next = || it.next()?.parse::<u64>().ok();
    let numbers = (next()?, next()?, next()?);
    if it.next().is_some() {
        return None;
    }
    let ident = |id: &str| {
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| {
            id.parse::<u64>()
                .map_or_else(|_| Ident::Text(id.to_string()), Ident::Number)
        })
    };
    let stage = match pre {
        None => Stage::Full,
        Some(pre) => Stage::Pre(pre.split('.').map(ident).collect::<Option<Vec<_>>>()?),
    };
    Some(Version { numbers, stage })
}

/// La plus haute section `### x.y.z` de `progress` est la version `workspace`, sinon
/// l'erreur qui dit quoi faire.
fn highest_section_matches(progress: &str, workspace: &str) -> Result<(), String> {
    let (highest, title) = progress
        .lines()
        .filter_map(|l| {
            let title = l.trim().strip_prefix("### ")?;
            Some((parse_version(title)?, title))
        })
        .max()
        .ok_or("aucune section de version dans docs/progress.md")?;
    let current = parse_version(workspace)
        .ok_or_else(|| format!("version du workspace illisible : {workspace}"))?;
    // #212 : le bloc « Version 1 (branche v1) » ne vit que sur v1. S'il arrive sur main
    // par un rétroportage, sa plus haute section dépasse la version 0.17 du workspace.
    if highest.numbers.0 > current.numbers.0 {
        return Err(format!(
            "section V1 sur main : docs/progress.md décrit la version {title} alors que le \
             workspace est en {workspace} ; un rétroportage depuis v1 a emporté le bloc \
             « Version 1 (branche v1) », qui n'a rien à faire sur cette branche : le retirer \
             du lot"
        ));
    }
    if highest > current {
        return Err(format!(
            "docs/progress.md décrit la version {title}, le workspace est en {workspace} : \
             poser la version avant de fusionner (`make bump V={title}`), sinon le lot part \
             sans release"
        ));
    }
    Ok(())
}

/// #147 : et l'inverse. Trois sections `0.17.24`, `0.17.25`, `0.17.26` ont existé le
/// 20/09 pendant que le workspace restait en `0.17.23` : des lots fermés, documentés,
/// et aucune release — `penelope upgrade` disait « à jour » à une instance qui avait
/// trois versions de retard. Une section écrite sans bump fait donc échouer la CI.
#[test]
fn the_highest_progress_section_is_the_workspace_version() {
    let progress = read(&root().join("docs/progress.md"));
    if let Err(why) = highest_section_matches(&progress, penelope_daemon::VERSION) {
        panic!("{why}");
    }
}

/// #212 : le parseur lit le suffixe et suit l'ordre semver, pour que la branche v1
/// (`1.0.0-alpha.N`) passe le test et qu'un bloc V1 sur main le fasse échouer.
#[test]
fn progress_versions_follow_semver() {
    let v = |s: &str| parse_version(s).unwrap_or_else(|| panic!("{s} illisible"));
    assert_eq!(v("0.17.59").numbers, (0, 17, 59));
    assert_eq!(v("0.17.59").stage, Stage::Full);
    assert_eq!(
        v("1.0.0-alpha.2").stage,
        Stage::Pre(vec![Ident::Text("alpha".into()), Ident::Number(2)])
    );
    for (low, high) in [
        ("0.17.59", "0.17.60"),
        ("0.17.60", "1.0.0-alpha.1"),
        ("1.0.0-alpha.2", "1.0.0-alpha.10"),
        ("1.0.0-alpha.10", "1.0.0-beta.1"),
        ("1.0.0-beta.1", "1.0.0-rc.1"),
        ("1.0.0-rc.1", "1.0.0"),
        ("1.0.0-alpha", "1.0.0-alpha.1"),
        ("1.0.0-1", "1.0.0-alpha"),
        ("1.0.0", "1.0.1-alpha.1"),
    ] {
        assert!(v(low) < v(high), "{low} < {high}");
    }
    for bad in [
        "0.17",
        "0.17.59.1",
        "1.0.0-",
        "1.0.0-alpha..1",
        "1.0.0-rc 1",
        "x.y.z",
        "Branché depuis la 0.1.0",
    ] {
        assert_eq!(parse_version(bad), None, "{bad}");
    }
}

/// #212 : sections `0.17.59` et `1.0.0-alpha.1` : vert avec un workspace en
/// `1.0.0-alpha.1`, rouge en `0.17.59` (bloc V1 sur main) ; et #147 reste attrapé sur
/// les deux branches.
#[test]
fn a_v1_block_passes_on_v1_and_fails_on_main() {
    let both = "## Version 1 (branche v1)\n\n### 1.0.0-alpha.1\n\nx\n\n## Résumé\n\n\
                ### 0.17.59\n\ny\n\n### 0.17.58\n";
    assert_eq!(highest_section_matches(both, "1.0.0-alpha.1"), Ok(()));
    let why = highest_section_matches(both, "0.17.59").unwrap_err();
    assert!(why.starts_with("section V1 sur main"), "{why}");

    let why = highest_section_matches("### 0.17.60\n### 0.17.59\n", "0.17.59").unwrap_err();
    assert!(why.contains("`make bump V=0.17.60`"), "{why}");
    let v1 = "### 1.0.0-alpha.2\n### 1.0.0-alpha.1\n### 0.17.59\n";
    let why = highest_section_matches(v1, "1.0.0-alpha.1").unwrap_err();
    assert!(why.contains("`make bump V=1.0.0-alpha.2`"), "{why}");
    assert_eq!(highest_section_matches(v1, "1.0.0-alpha.2"), Ok(()));
    assert_eq!(
        highest_section_matches("### 1.0.0-rc.1\n### 0.17.59\n", "1.0.0"),
        Ok(()),
        "la version pleine passe au-dessus de ses pré-releases"
    );
    assert!(
        highest_section_matches(both, "1.0.0-").is_err(),
        "workspace illisible"
    );
}

// ------------------------------------------------------------------ limites

/// Sections dont le titre annonce un manque : (titre, corps).
fn missing_sections(markdown: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(usize, String, String)> = None;
    let mut in_code = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        let heading = !in_code && level > 0 && line[level..].starts_with(' ');
        if heading {
            if let Some((l, title, body)) = current.take() {
                if level > l {
                    current = Some((l, title, body + line + "\n"));
                    continue;
                }
                out.push((title, body));
            }
            let title = line[level..].trim().to_string();
            let lower = title.to_lowercase();
            if [
                "pas encore branché",
                "à brancher",
                "non servies",
                "limites actuelles",
                "autres manques",
            ]
            .iter()
            .any(|k| lower.contains(k))
            {
                current = Some((level, title, String::new()));
            }
            continue;
        }
        if let Some((_, _, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some((_, title, body)) = current {
        out.push((title, body));
    }
    out
}

/// Une section qui annonce un manque ne cite jamais une méthode RPC servie, une commande
/// du catalogue ni un outil livré : ce serait une phrase périmée.
#[test]
fn no_doc_presents_something_shipped_as_missing() {
    let families: BTreeSet<&str> = method::ALL
        .iter()
        .filter_map(|m| m.split_once('.').map(|(f, _)| f))
        .collect();
    let commands: Vec<&str> = penelope_telegram::commands::all()
        .iter()
        .map(|c| c.name)
        .collect();
    let tools: Vec<&str> = penelope_tools::all_tools().iter().map(|t| t.name).collect();
    let mut stale = Vec::new();
    for file in markdown_files() {
        let raw = read(&file);
        for (title, body) in missing_sections(&raw) {
            let mut cite = |what: String| {
                if body.contains(&what) {
                    stale.push(format!("{} « {title} » cite {what}", file.display()));
                }
            };
            for m in method::ALL {
                cite(format!("`{m}`"));
            }
            for f in &families {
                cite(format!("`{f}.*`"));
            }
            for c in &commands {
                cite(format!("`/{c}`"));
            }
            for t in &tools {
                cite(format!("`{t}`"));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "sections périmées (méthodes, commandes et outils livrés) :\n{}",
        stale.join("\n")
    );
}

#[test]
fn sections_are_cut_at_the_next_heading_of_the_same_level() {
    let md = "# A\n## Ce qui n'est pas encore branché\n`wf.run` manque\n### détail\nx\n## Suite\n`wf.run` servi\n";
    let s = missing_sections(md);
    assert_eq!(s.len(), 1);
    assert!(s[0].1.contains("détail"));
    assert!(!s[0].1.contains("servi"));
}

#[test]
fn anchors_follow_github() {
    let a = anchors(
        "# Titre\n## 2. Compiler et installer\n## Limites actuelles\n## Limites actuelles\n```\n# pas un titre\n```\n",
    );
    assert!(a.contains("2-compiler-et-installer"), "{a:?}");
    assert!(a.contains("limites-actuelles") && a.contains("limites-actuelles-1"));
    assert!(!a.contains("pas-un-titre"));
}
