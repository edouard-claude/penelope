//! Documentation de la version qui tourne (issue #34) : le dépôt est la source de vérité sur
//! Pénélope, et la documentation embarquée au build correspond exactement au binaire, même
//! hors ligne et en installation release.
//!
//! ```text
//! self_docs list ───────► fichiers, titres et sections
//! self_docs search mots ─► sections qui correspondent, avec lien GitHub à la version
//! self_docs read f [s] ──► contenu borné et paginé d'un fichier ou d'une section
//! known_limits ─────────► « Limites actuelles », « Autres manques », « Encore à brancher »
//! ```

use serde_json::{Value, json};

include!(concat!(env!("OUT_DIR"), "/docs.rs"));

/// Taille d'une page de lecture.
pub const PAGE_CHARS: usize = 8_000;
const REPO_URL: &str = "https://github.com/edouard-claude/penelope/blob";

/// Section d'un fichier Markdown.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub file: &'static str,
    pub level: usize,
    pub heading: String,
    pub anchor: String,
    /// Contenu sous le titre, jusqu'au titre suivant de niveau égal ou supérieur.
    pub body: String,
}

/// Ancre GitHub d'un titre : minuscules, ponctuation retirée, espaces en tirets.
pub fn anchor(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Lien GitHub d'une section, à la version compilée.
pub fn url(file: &str, anchor: Option<&str>) -> String {
    let base = format!("{REPO_URL}/v{}/{file}", crate::VERSION);
    match anchor.filter(|a| !a.is_empty()) {
        Some(a) => format!("{base}#{a}"),
        None => base,
    }
}

/// Sections d'un fichier, blocs de code ignorés pour le repérage des titres.
pub fn sections(file: &'static str, raw: &str) -> Vec<Section> {
    let lines: Vec<&str> = raw.lines().collect();
    let mut heads: Vec<(usize, usize, String)> = Vec::new();
    let mut fenced = false;
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = l.chars().take_while(|c| *c == '#').count();
        if (1..=4).contains(&level) && l[level..].starts_with(' ') {
            heads.push((i, level, l[level..].trim().to_string()));
        }
    }
    heads
        .iter()
        .enumerate()
        .map(|(k, (i, level, heading))| {
            let end = heads[k + 1..]
                .iter()
                .find(|(_, l, _)| l <= level)
                .map(|(j, _, _)| *j)
                .unwrap_or(lines.len());
            Section {
                file,
                level: *level,
                heading: heading.clone(),
                anchor: anchor(heading),
                body: lines[i + 1..end].join("\n").trim().to_string(),
            }
        })
        .collect()
}

fn all_sections() -> Vec<Section> {
    DOCS.iter().flat_map(|(f, raw)| sections(f, raw)).collect()
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// `self_docs list` : fichiers, titre et sections de premier et second niveau.
pub fn list() -> Value {
    json!(
        DOCS.iter()
            .map(|(file, raw)| {
                let secs = sections(file, raw);
                let title = secs
                    .iter()
                    .find(|s| s.level == 1)
                    .map(|s| s.heading.clone())
                    .unwrap_or_else(|| file.to_string());
                json!({
                    "file": file,
                    "title": title,
                    "sections": secs.iter().filter(|s| s.level == 2).map(|s| s.heading.clone()).collect::<Vec<_>>(),
                    "url": url(file, None),
                })
            })
            .collect::<Vec<_>>()
    )
}

/// `self_docs search` : sections dont le titre ou le texte contient les mots, les titres
/// comptant double.
pub fn search(query: &str, limit: usize) -> Value {
    let terms: Vec<String> = fold(query)
        .split_whitespace()
        .filter(|t| t.chars().count() > 1)
        .map(String::from)
        .collect();
    if terms.is_empty() {
        return json!([]);
    }
    let mut scored: Vec<(usize, Section)> = all_sections()
        .into_iter()
        .filter_map(|s| {
            let head = fold(&s.heading);
            let body = fold(&s.body);
            let mut score = 0;
            for t in &terms {
                if head.contains(t.as_str()) {
                    score += 3;
                }
                score += body.matches(t.as_str()).count().min(5);
            }
            let all_present = terms
                .iter()
                .all(|t| head.contains(t.as_str()) || body.contains(t.as_str()));
            (score > 0 && all_present).then_some((score, s))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.level.cmp(&a.1.level).reverse()));
    json!(
        scored
            .into_iter()
            .take(limit.max(1))
            .map(|(score, s)| json!({
                "file": s.file,
                "section": s.heading,
                "score": score,
                "excerpt": s.body.chars().take(400).collect::<String>(),
                "url": url(s.file, Some(&s.anchor)),
            }))
            .collect::<Vec<_>>()
    )
}

/// `self_docs read` : un fichier, ou une section, par pages de [`PAGE_CHARS`] caractères.
pub fn read(file: &str, section: Option<&str>, cursor: usize) -> Result<Value, String> {
    let (path, raw) = DOCS
        .iter()
        .find(|(f, _)| *f == file || f.ends_with(&format!("/{file}")))
        .ok_or_else(|| {
            format!(
                "`{file}` n'est pas dans la documentation embarquée : {}",
                DOCS.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(", ")
            )
        })?;
    let (text, heading, anchor_) = match section.filter(|s| !s.trim().is_empty()) {
        Some(wanted) => {
            let w = fold(wanted);
            let s = sections(path, raw)
                .into_iter()
                .find(|s| fold(&s.heading) == w)
                .or_else(|| {
                    sections(path, raw)
                        .into_iter()
                        .find(|s| fold(&s.heading).contains(&w))
                })
                .ok_or_else(|| format!("section « {wanted} » absente de {path}"))?;
            (
                format!("{} {}\n\n{}", "#".repeat(s.level), s.heading, s.body),
                Some(s.heading.clone()),
                Some(s.anchor),
            )
        }
        None => (raw.to_string(), None, None),
    };
    let chars: Vec<char> = text.chars().collect();
    let start = cursor.min(chars.len());
    let end = (start + PAGE_CHARS).min(chars.len());
    Ok(json!({
        "file": path,
        "section": heading,
        "text": chars[start..end].iter().collect::<String>(),
        "next_cursor": (end < chars.len()).then_some(end),
        "url": url(path, anchor_.as_deref()),
    }))
}

/// Limites connues de la version : sections « Limites actuelles », « Autres manques »,
/// « Encore à brancher » et « Ce qui n'est pas encore branché », avec leurs liens.
pub fn known_limits() -> Value {
    json!(
        all_sections()
            .into_iter()
            .filter(|s| {
                let h = fold(&s.heading);
                h.contains("limites actuelles")
                    || h.contains("autres manques")
                    || h.contains("encore a brancher")
                    || h.contains("pas encore branche")
            })
            .map(|s| json!({
                "file": s.file,
                "section": s.heading,
                "text": s.body.chars().take(2_000).collect::<String>(),
                "url": url(s.file, Some(&s.anchor)),
            }))
            .collect::<Vec<_>>()
    )
}

/// Section de `docs/workflows.md` à citer pour une erreur de validation de workflow.
pub fn workflow_doc_for(error: &str) -> (String, String) {
    let e = fold(error);
    let heading = if e.contains("variant")
        || e.contains("type d'etape")
        || e.contains("type inconnu")
        || e.contains("\"type\"")
        || e.contains("`type`")
    {
        "Les neuf types d'étapes"
    } else if e.contains("transition") || e.contains("goto") {
        "Transitions"
    } else if e.contains("sous-groupe") || e.contains("subgroup") {
        "Sous-groupes"
    } else if e.contains("parametre")
        || e.contains("metadata")
        || e.contains("identifiant")
        || e.contains("variable")
    {
        "Métadonnées"
    } else if e.contains("budget") || e.contains("settings") || e.contains("reglage") {
        "Réglages"
    } else {
        "Fichier"
    };
    (
        heading.to_string(),
        url("docs/workflows.md", Some(&anchor(heading))),
    )
}

/// Réponse de l'outil `self_docs`.
pub fn tool(args: &Value) -> Result<Value, String> {
    match args["action"].as_str().unwrap_or("list") {
        "list" => Ok(list()),
        "search" => {
            let q = args["query"].as_str().ok_or("`query` manquant")?;
            Ok(search(q, args["limit"].as_u64().unwrap_or(8) as usize))
        }
        "read" => read(
            args["file"].as_str().ok_or("`file` manquant")?,
            args["section"].as_str(),
            args["cursor"].as_u64().unwrap_or(0) as usize,
        ),
        "limits" => Ok(known_limits()),
        other => Err(format!(
            "action inconnue `{other}` : list, search, read ou limits"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chaque fichier Markdown de `docs/` (et le README) est embarqué, sans oubli.
    #[test]
    fn every_doc_file_is_embedded() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else if p.extension().is_some_and(|x| x == "md") {
                    out.push(
                        p.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        let mut on_disk = vec!["README.md".to_string()];
        walk(&root.join("docs"), &root, &mut on_disk);
        on_disk.sort();
        let embedded: Vec<String> = DOCS.iter().map(|(f, _)| f.to_string()).collect();
        assert_eq!(embedded, on_disk);
        for (f, raw) in DOCS {
            let disk = std::fs::read_to_string(root.join(f)).unwrap();
            assert_eq!(*raw, disk, "{f} embarqué à jour");
        }
    }

    #[test]
    fn anchors_follow_github() {
        assert_eq!(anchor("Les neuf types d'étapes"), "les-neuf-types-détapes");
        assert_eq!(anchor("10. Mise à jour"), "10-mise-à-jour");
        assert_eq!(anchor("Sous-groupes"), "sous-groupes");
    }

    /// `self_docs search "sous-groupe"` renvoie la section de `docs/workflows.md`, sans
    /// réseau, avec le lien à la version compilée.
    #[test]
    fn search_finds_the_workflow_subgroups_section() {
        let hits = search("sous-groupe", 5);
        let first = &hits[0];
        assert_eq!(first["file"], "docs/workflows.md", "{hits}");
        assert_eq!(first["section"], "Sous-groupes");
        assert_eq!(
            first["url"],
            format!(
                "https://github.com/edouard-claude/penelope/blob/v{}/docs/workflows.md#sous-groupes",
                crate::VERSION
            )
        );
        let page = read("workflows.md", Some("Sous-groupes"), 0).unwrap();
        assert!(
            page["text"]
                .as_str()
                .unwrap()
                .starts_with("## Sous-groupes")
        );
        assert!(read("inexistant.md", None, 0).is_err());
        let limits = known_limits();
        assert!(
            limits
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l["file"] == "docs/workflows.md" && l["section"] == "Limites actuelles"),
            "{limits}"
        );
        let (heading, link) = workflow_doc_for("unknown variant `bidule`, expected one of `agent`");
        assert_eq!(heading, "Les neuf types d'étapes");
        assert!(link.ends_with("docs/workflows.md#les-neuf-types-détapes"));
    }
}
