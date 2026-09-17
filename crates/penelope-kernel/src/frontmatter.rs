//! Frontmatter YAML (sous-ensemble) des fichiers Markdown du vault et des skills.
//!
//! Le PRD impose un frontmatter YAML obligatoire (§6.4) et le format `agentskills.io`
//! (§7). Seul un sous-ensemble est utilisé : scalaires, listes en ligne `[a, b]`, listes
//! à tirets, booléens, nombres. Implémentation locale plutôt qu'une dépendance YAML
//! complète : le format est figé, les fichiers sont édités à la main en SSH, et les
//! messages d'erreur doivent désigner la **ligne** fautive.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum FmValue {
    Str(String),
    List(Vec<String>),
    Bool(bool),
    Num(f64),
}

impl FmValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FmValue::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Vec<String> {
        match self {
            FmValue::List(v) => v.clone(),
            FmValue::Str(s) if s.is_empty() => Vec::new(),
            FmValue::Str(s) => vec![s.clone()],
            _ => Vec::new(),
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            FmValue::Bool(b) => Some(*b),
            FmValue::Str(s) => match s.as_str() {
                "true" | "yes" | "oui" => Some(true),
                "false" | "no" | "non" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            FmValue::Num(n) => Some(*n),
            FmValue::Str(s) => s.parse().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frontmatter {
    pub fields: BTreeMap<String, FmValue>,
    /// Corps du document, sans le frontmatter.
    pub body: String,
    /// Nombre de lignes occupées par le frontmatter, délimiteurs compris.
    pub header_lines: usize,
}

impl Frontmatter {
    pub fn get(&self, key: &str) -> Option<&FmValue> {
        self.fields.get(key)
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }
    pub fn string(&self, key: &str) -> String {
        self.str(key).unwrap_or_default().to_string()
    }
    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key).map(|v| v.as_list()).unwrap_or_default()
    }
    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }
    pub fn f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(|v| v.as_f64())
    }
    pub fn has(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ligne {} : {}", self.line, self.message)
    }
}

impl std::error::Error for FrontmatterError {}

/// Analyse un document. Un document **sans** délimiteur `---` renvoie un frontmatter vide
/// et le document entier comme corps : c'est valide pour un fichier de notes libre.
pub fn parse(raw: &str) -> Result<Frontmatter, FrontmatterError> {
    let normalised = raw.replace("\r\n", "\n");
    let lines: Vec<&str> = normalised.split('\n').collect();

    // Un BOM éventuel ne doit pas empêcher la détection du délimiteur.
    let first = lines
        .first()
        .map(|l| l.trim_start_matches('\u{feff}').trim());
    if first != Some("---") {
        return Ok(Frontmatter {
            fields: BTreeMap::new(),
            body: normalised,
            header_lines: 0,
        });
    }

    let mut fields: BTreeMap<String, FmValue> = BTreeMap::new();
    let mut end: Option<usize> = None;
    let mut pending_list: Option<(String, Vec<String>)> = None;

    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end = Some(i);
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }

        // Élément de liste à tirets.
        if let Some(item) = trimmed.trim_start().strip_prefix("- ") {
            match &mut pending_list {
                Some((_, items)) => {
                    items.push(unquote(item.trim()));
                    continue;
                }
                None => {
                    return Err(FrontmatterError {
                        line: i + 1,
                        message: "élément de liste sans clé".into(),
                    });
                }
            }
        }
        if let Some((key, items)) = pending_list.take() {
            fields.insert(key, FmValue::List(items));
        }

        let Some((k, v)) = trimmed.split_once(':') else {
            return Err(FrontmatterError {
                line: i + 1,
                message: format!("ligne sans `:` : `{}`", trimmed.trim()),
            });
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            return Err(FrontmatterError {
                line: i + 1,
                message: "clé vide".into(),
            });
        }
        let value = v.trim();
        if value.is_empty() {
            // Début d'une liste à tirets.
            pending_list = Some((key, Vec::new()));
            continue;
        }
        fields.insert(key, parse_scalar(value));
    }

    if let Some((key, items)) = pending_list.take() {
        fields.insert(key, FmValue::List(items));
    }

    let Some(end) = end else {
        return Err(FrontmatterError {
            line: lines.len(),
            message: "frontmatter non refermé (`---` manquant)".into(),
        });
    };

    Ok(Frontmatter {
        fields,
        body: lines[end + 1..].join("\n"),
        header_lines: end + 1,
    })
}

fn parse_scalar(v: &str) -> FmValue {
    if v.starts_with('[') && v.ends_with(']') {
        return FmValue::List(split_inline_list(&v[1..v.len() - 1]));
    }
    match v {
        "true" | "yes" => return FmValue::Bool(true),
        "false" | "no" => return FmValue::Bool(false),
        _ => {}
    }
    if let Ok(n) = v.parse::<f64>() {
        // Une date `2026-09-17` parse en nombre sur certains locales : on la garde en
        // chaîne si elle contient un tiret.
        if !v.contains('-') || v.starts_with('-') {
            return FmValue::Num(n);
        }
    }
    FmValue::Str(unquote(v))
}

/// Découpe une liste en ligne `a, "b, c", 'd'' e'` sur les virgules **hors guillemets** :
/// une valeur citée qui contient une virgule reste un seul élément (issue #48).
fn split_inline_list(inner: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            // Dans des guillemets doubles, `\"` est un guillemet, pas la fin.
            Some('"') if c == '\\' => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(q) if c == q => {
                // `''` dans des apostrophes est une apostrophe, pas la fin.
                if q == '\'' && chars.peek() == Some(&'\'') {
                    current.push(c);
                    current.push(chars.next().unwrap_or(q));
                } else {
                    quote = None;
                    current.push(c);
                }
            }
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                current.push(c);
            }
            None if c == ',' => {
                items.push(std::mem::take(&mut current));
                continue;
            }
            None => current.push(c),
        }
    }
    items.push(current);
    items
        .into_iter()
        .map(|s| unquote(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        return s[1..s.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        return s[1..s.len() - 1].replace("''", "'");
    }
    s.to_string()
}

/// Chaîne YAML valide : entre guillemets dès qu'elle serait lue autrement
/// (lien `[[x]]`, `: `, ` #`, indicateur en tête, booléen ou nombre), telle quelle sinon.
/// Une date `AAAA-MM-JJ` reste nue : elle se lit comme une date.
pub fn yaml_string(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let needs = s.is_empty()
        || s != s.trim()
        || s.contains(": ")
        || s.ends_with(':')
        || s.contains(" #")
        || s.contains("[[")
        || s.contains(['\n', '\r', '\t'])
        || s.starts_with([
            '[', ']', '{', '}', '>', '|', '*', '&', '!', '%', '@', '`', '"', '\'', ',', '?', '#',
            '-',
        ])
        || matches!(
            lower.as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
        )
        || (s.parse::<f64>().is_ok() && !is_date(s));
    if needs {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace(['\n', '\r', '\t'], " ")
        )
    } else {
        s.to_string()
    }
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Sérialise un frontmatter, en préservant l'ordre alphabétique des clés. Les listes sont
/// écrites en blocs (`- élément`), la forme usuelle des propriétés YAML (issue #29).
pub fn render(fields: &BTreeMap<String, FmValue>, body: &str) -> String {
    let mut out = String::from("---\n");
    for (k, v) in fields {
        match v {
            FmValue::Str(s) => out.push_str(&format!("{k}: {}\n", yaml_string(s))),
            FmValue::Bool(b) => out.push_str(&format!("{k}: {b}\n")),
            FmValue::Num(n) => {
                if n.fract() == 0.0 {
                    out.push_str(&format!("{k}: {}\n", *n as i64));
                } else {
                    out.push_str(&format!("{k}: {n}\n"));
                }
            }
            FmValue::List(items) if items.is_empty() => out.push_str(&format!("{k}: []\n")),
            FmValue::List(items) => {
                out.push_str(&format!("{k}:\n"));
                for item in items {
                    out.push_str(&format!("  - {}\n", yaml_string(item)));
                }
            }
        }
    }
    out.push_str("---\n");
    out.push_str(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #29 : ce qui est écrit reste du YAML qui se relit comme prévu.
    #[test]
    fn rendered_properties_are_valid_yaml() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "aliases".into(),
            FmValue::List(vec!["Factur-X".into(), "ZUGFeRD 2".into()]),
        );
        fields.insert("tags".into(), FmValue::List(vec![]));
        fields.insert("titre".into(), FmValue::Str("Contrat: v2 #final".into()));
        fields.insert("source".into(), FmValue::Str("[[contrat-v2]]".into()));
        fields.insert("created".into(), FmValue::Str("2026-09-17".into()));
        fields.insert("version".into(), FmValue::Str("12".into()));
        fields.insert("citation".into(), FmValue::Str("il a dit \"oui\"".into()));
        let raw = render(&fields, "# x\n");
        assert!(
            raw.contains("aliases:\n  - Factur-X\n  - ZUGFeRD 2\n"),
            "{raw}"
        );
        assert!(raw.contains("tags: []\n"));
        assert!(raw.contains("titre: \"Contrat: v2 #final\"\n"));
        assert!(raw.contains("source: \"[[contrat-v2]]\"\n"));
        assert!(raw.contains("created: 2026-09-17\n"));
        assert!(raw.contains("version: \"12\"\n"));
        let back = parse(&raw).unwrap();
        assert_eq!(back.string("titre"), "Contrat: v2 #final");
        assert_eq!(back.string("source"), "[[contrat-v2]]");
        assert_eq!(back.string("citation"), "il a dit \"oui\"");
        assert_eq!(back.list("aliases"), vec!["Factur-X", "ZUGFeRD 2"]);
        assert!(back.list("tags").is_empty());
    }

    /// #48 : une liste en ligne dont une valeur citée contient une virgule n'est pas
    /// découpée dans la valeur. Cas venu des fichiers édités à la main.
    #[test]
    fn an_inline_list_keeps_a_comma_inside_a_quoted_value() {
        let fm = parse("---\naliases: [\"Le Crew, coworking\", Crew]\n---\n").unwrap();
        assert_eq!(fm.list("aliases"), vec!["Le Crew, coworking", "Crew"]);

        let fm =
            parse("---\ntags: ['Aujourd''hui, demain', \"un \\\"vrai\\\" titre\", \"\", b]\n---\n")
                .unwrap();
        assert_eq!(
            fm.list("tags"),
            vec!["Aujourd'hui, demain", "un \"vrai\" titre", "b"],
            "guillemets échappés, apostrophe doublée, élément vide ignoré"
        );
        assert!(
            parse("---\naliases: [ ]\n---\n")
                .unwrap()
                .list("aliases")
                .is_empty()
        );
        assert!(
            parse("---\naliases: []\n---\n")
                .unwrap()
                .list("aliases")
                .is_empty()
        );
    }

    #[test]
    fn parses_scalars_lists_and_body() {
        let raw = "---\n\
                   type: pratique\n\
                   id: langage-backend\n\
                   confiance: 0.8\n\
                   preuves: 9\n\
                   statut: active\n\
                   maj: 2026-09-17\n\
                   declencheurs: [langage, stack, nouveau projet]\n\
                   ---\n\
                   # Langage backend\n\n## Défaut\nGo.\n";
        let f = parse(raw).unwrap();
        assert_eq!(f.str("type"), Some("pratique"));
        assert_eq!(f.f64("confiance"), Some(0.8));
        assert_eq!(f.f64("preuves"), Some(9.0));
        assert_eq!(
            f.str("maj"),
            Some("2026-09-17"),
            "une date reste une chaîne"
        );
        assert_eq!(
            f.list("declencheurs"),
            vec!["langage", "stack", "nouveau projet"]
        );
        assert!(f.body.starts_with("# Langage backend"));
        assert_eq!(f.header_lines, 9);
    }

    #[test]
    fn parses_dash_lists() {
        let raw = "---\nallowed_tools:\n  - fs_read\n  - shell_exec\nname: x\n---\ncorps\n";
        let f = parse(raw).unwrap();
        assert_eq!(f.list("allowed_tools"), vec!["fs_read", "shell_exec"]);
        assert_eq!(f.str("name"), Some("x"));
    }

    #[test]
    fn booleans_and_quotes() {
        let f = parse("---\nsub_agent: true\ndesc: \"une : description\"\n---\n").unwrap();
        assert!(f.bool("sub_agent", false));
        assert_eq!(f.str("desc"), Some("une : description"));
    }

    #[test]
    fn document_without_frontmatter_is_all_body() {
        let f = parse("# Notes libres\ncontenu").unwrap();
        assert!(f.fields.is_empty());
        assert_eq!(f.body, "# Notes libres\ncontenu");
        assert_eq!(f.header_lines, 0);
    }

    #[test]
    fn unterminated_frontmatter_reports_the_line() {
        let e = parse("---\na: 1\nb: 2\n").unwrap_err();
        assert!(e.message.contains("non refermé"), "{e}");
    }

    #[test]
    fn malformed_line_reports_its_number() {
        let e = parse("---\nbonne: valeur\nligne sans deux-points\n---\n").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.to_string().contains("ligne 3"));
    }

    #[test]
    fn crlf_is_tolerated() {
        let f = parse("---\r\nname: x\r\n---\r\ncorps\r\n").unwrap();
        assert_eq!(f.str("name"), Some("x"));
        assert!(!f.body.contains('\r'));
    }

    #[test]
    fn comments_are_ignored() {
        let f = parse("---\n# un commentaire\nname: x\n---\n").unwrap();
        assert_eq!(f.fields.len(), 1);
    }

    #[test]
    fn render_roundtrip() {
        let raw =
            "---\nname: revue\nversion: 1.2\nallowed_tools: [fs_read, git_diff]\n---\ncorps\n";
        let f = parse(raw).unwrap();
        let out = render(&f.fields, &f.body);
        let back = parse(&out).unwrap();
        assert_eq!(back.fields, f.fields);
        assert_eq!(back.body, f.body);
    }

    #[test]
    fn list_helper_tolerates_a_single_string() {
        let f = parse("---\ndeclencheurs: deploiement\n---\n").unwrap();
        assert_eq!(f.list("declencheurs"), vec!["deploiement"]);
    }
}
