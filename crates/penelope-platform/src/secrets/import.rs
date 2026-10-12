//! Un bloc de secrets collé tel quel (#350) : découpé sans modèle, sans réseau.
//!
//! ```text
//!  DEV :                       ──► environnement « dev »
//!    Access Key  : 0W9E…       ──► mentaleco.dev.access_key
//!  [qa] / ## QA / qa:          ──► environnement « qa »
//!  export API_SECRET="…"       ──► mentaleco.qa.api_secret
//!  CERT=-----BEGIN …           ──► le bloc PEM entier, jusqu'à -----END
//! ```
//!
//! Le nom rangé est `ci.<projet>.<env>.<libellé>` en slug, que `validate_secret_name`
//! accepte. Le préfixe [`PREFIX`] les sépare des secrets internes (`codex.oauth`,
//! `openrouter_api_key`…) : seuls ceux-là s'injectent dans une commande. Une ligne illisible n'est rendue que par son numéro : aucune erreur, aucun
//! journal ne cite une valeur.

use super::validate_secret_name;

/// Préfixe des secrets rangés par un collage : le seul espace qu'une commande reçoit
/// (`shell_exec` avec `secrets`, #352).
pub const PREFIX: &str = "ci.";
use crate::dirs::slugify;

/// Ce que le découpage a tiré d'un bloc.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    /// Nom dans le magasin, valeur ; un même nom repris garde la dernière valeur.
    pub entries: Vec<(String, String)>,
    /// Lignes non reconnues, numérotées depuis 1.
    pub unread: Vec<usize>,
}

impl Parsed {
    /// Les noms rangés, dans l'ordre du bloc.
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|(n, _)| n.as_str()).collect()
    }
}

/// `Access Key` → `access_key` ; vide si rien de lisible (`slugify` y met `-x`).
fn slug(s: &str) -> String {
    let out = slugify(s);
    if out.starts_with('-') {
        return String::new();
    }
    out.replace('-', "_")
}

/// Sépare `clé` et `valeur` au premier `=` ou `:`. `None` : pas de séparateur.
fn split(line: &str) -> Option<(&str, &str)> {
    let at = line.find(['=', ':'])?;
    Some((line[..at].trim(), line[at + 1..].trim()))
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// Une ligne qui ouvre un environnement : `DEV :`, `[qa]`, `## PROD`, `# dev`, ou un mot
/// seul (`staging`). Une phrase sans marqueur n'en est pas une.
fn header(line: &str) -> Option<String> {
    let line = line.trim();
    let marked = line.starts_with('#') || line.starts_with('[') || line.ends_with(':');
    let t = line.trim_start_matches('#').trim();
    let t = t
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(t);
    let t = t.strip_suffix(':').unwrap_or(t).trim();
    let s = slug(t);
    let short = s.len() <= 40 && !t.contains(['=', ':']);
    (!s.is_empty() && short && (marked || !t.contains(char::is_whitespace))).then_some(s)
}

/// Découpe `text` pour le projet `project`. `Err` : le projet n'a pas de nom utilisable.
pub fn parse_block(project: &str, text: &str) -> Result<Parsed, String> {
    let project = slug(project);
    if project.is_empty() {
        return Err("nom de projet vide".into());
    }
    let mut out = Parsed::default();
    let mut env: Option<String> = None;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let n = i + 1;
        let raw = lines[i].trim();
        i += 1;
        if raw.is_empty() {
            continue;
        }
        let line = raw.strip_prefix("export ").unwrap_or(raw).trim();
        let pair = split(line).filter(|(k, _)| !k.is_empty() && !k.starts_with('#'));
        let (key, value) = match pair {
            Some((k, v)) if !v.is_empty() => (k, v.to_string()),
            // `DEV :` ou `[qa]` : une section, pas une valeur.
            _ => {
                match header(raw) {
                    Some(h) if pair.is_none_or(|(_, v)| v.is_empty()) => env = Some(h),
                    _ => out.unread.push(n),
                }
                continue;
            }
        };
        let mut value = unquote(&value).to_string();
        // Un certificat ou une clé PEM court sur plusieurs lignes.
        if value.starts_with("-----BEGIN") && !value.contains("-----END") {
            while i < lines.len() {
                value.push('\n');
                value.push_str(lines[i].trim_end());
                i += 1;
                if lines[i - 1].contains("-----END") {
                    break;
                }
            }
        }
        let name = match &env {
            Some(e) => format!("{PREFIX}{project}.{e}.{}", slug(key)),
            None => format!("{PREFIX}{project}.{}", slug(key)),
        };
        if slug(key).is_empty() || validate_secret_name(&name).is_err() {
            out.unread.push(n);
            continue;
        }
        out.entries.retain(|(k, _)| *k != name);
        out.entries.push((name, value));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le collage du 12/10, en-têtes et libellés tels quels (valeurs factices).
    const BLOCK: &str = "DEV :
  Bucket      : app-mentaleco-dev
  Access Key  : AKDEVXXXXXXXXXXXXXXX
  Secret Key  : sk/dev+value=with:colons

QA :
  Access Key  : AKQAXXXXXXXXXXXXXXXX
  Secret Key  : sk-qa

PROD :
  Access Key  : AKPRODXXXXXXXXXXXXXX
";

    #[test]
    fn a_pasted_block_is_split_by_section_and_label() {
        let p = parse_block("MentalEco", BLOCK).unwrap();
        assert_eq!(
            p.names(),
            [
                "ci.mentaleco.dev.bucket",
                "ci.mentaleco.dev.access_key",
                "ci.mentaleco.dev.secret_key",
                "ci.mentaleco.qa.access_key",
                "ci.mentaleco.qa.secret_key",
                "ci.mentaleco.prod.access_key",
            ]
        );
        // La valeur garde ses `=` et `:` : seul le premier séparateur coupe.
        assert_eq!(p.entries[2].1, "sk/dev+value=with:colons");
        assert!(p.unread.is_empty(), "{:?}", p.unread);
    }

    #[test]
    fn env_files_exports_quotes_and_bracket_headers_are_read() {
        let p = parse_block(
            "app",
            "[dev]\nexport API_SECRET=\"a b c\"\nPOSTGRES_PASSWORD='x'\n## QA\nTOKEN = t\n",
        )
        .unwrap();
        assert_eq!(
            p.entries,
            [
                ("ci.app.dev.api_secret".to_string(), "a b c".to_string()),
                ("ci.app.dev.postgres_password".to_string(), "x".to_string()),
                ("ci.app.qa.token".to_string(), "t".to_string()),
            ]
        );
    }

    #[test]
    fn a_pem_block_is_kept_whole() {
        let p = parse_block(
            "app",
            "CERT=-----BEGIN KEY-----\nAAAA\nBBBB\n-----END KEY-----\nNEXT=1\n",
        )
        .unwrap();
        assert_eq!(
            p.entries[0].1,
            "-----BEGIN KEY-----\nAAAA\nBBBB\n-----END KEY-----"
        );
        assert_eq!(p.entries[1].0, "ci.app.next");
    }

    /// Une ligne illisible est rendue par son numéro, jamais citée ; un nom repris garde
    /// la dernière valeur.
    #[test]
    fn unreadable_lines_are_numbered_and_the_last_value_wins() {
        let p = parse_block(
            "app",
            "dev:\nune phrase sans séparateur mais longue de plus de quarante caractères\nK=1\nK=2\nVIDE=\n",
        )
        .unwrap();
        assert_eq!(p.unread, [2, 5]);
        assert_eq!(p.entries, [("ci.app.dev.k".to_string(), "2".to_string())]);
        assert!(parse_block("  ", "K=1").is_err());
    }
}
