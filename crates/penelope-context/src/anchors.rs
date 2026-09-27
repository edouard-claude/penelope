//! Index d'ancres, extrait **mécaniquement** (§5.4).
//!
//! « Jamais paraphrasé » : ce sont des expressions régulières, pas un appel au modèle.
//! L'index accompagne chaque résumé pour que les identifiants exacts (chemins, SHA,
//! numéros de ticket, URLs, messages d'erreur) survivent à la compaction.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    Path,
    Sha,
    Ticket,
    PullRequest,
    Url,
    Error,
    Identifier,
}

impl AnchorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnchorKind::Path => "chemin",
            AnchorKind::Sha => "sha",
            AnchorKind::Ticket => "ticket",
            AnchorKind::PullRequest => "pr",
            AnchorKind::Url => "url",
            AnchorKind::Error => "erreur",
            AnchorKind::Identifier => "identifiant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Anchor {
    pub kind: AnchorKind,
    pub value: String,
}

struct Rules {
    rules: Vec<(AnchorKind, Regex)>,
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| {
        let defs: &[(AnchorKind, &str)] = &[
            (AnchorKind::Url, r"https?://[^\s<>()\[\]{}\\^`'\x22]+"),
            (
                AnchorKind::Sha,
                r"\b[0-9a-f]{7,40}\b",
            ),
            (
                AnchorKind::PullRequest,
                r"(?i)\b(?:pull|merge)[ _-]?request\s*#?\d+|\b(?:PR|MR)[ _-]?#?\d+\b",
            ),
            // `#` n'est pas un caractère de mot : pas de `\b` devant, sinon `#4312`
            // précédé d'une espace n'est jamais reconnu.
            (
                AnchorKind::Ticket,
                r"\b[A-Z][A-Z0-9]{1,9}-\d+\b|#\d{2,7}\b",
            ),
            (
                AnchorKind::Path,
                r"\b(?:[A-Za-z0-9_.-]+/){1,}[A-Za-z0-9_.-]+\.[A-Za-z0-9]{1,8}\b|\b[A-Za-z0-9_.-]+\.(?:rs|go|ts|tsx|js|py|toml|json|yaml|yml|md|sql|sh|c|h|cpp|java|kt|swift)\b",
            ),
            (
                AnchorKind::Error,
                r"(?m)^\s*(?:error(?:\[[A-Z]\d+\])?|erreur|panicked at|thread '[^']+' panicked|FAILED|Traceback)[^\n]{0,160}",
            ),
            (
                AnchorKind::Identifier,
                r"\b(?:[0-9A-HJKMNP-TV-Z]{26}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\b",
            ),
        ];
        Rules {
            rules: defs
                .iter()
                .filter_map(|(k, re)| Regex::new(re).ok().map(|r| (*k, r)))
                .collect(),
        }
    })
}

/// Extrait les ancres d'un texte, dédupliquées et triées (ordre déterministe).
pub fn extract(text: &str) -> Vec<Anchor> {
    let mut set: BTreeSet<Anchor> = BTreeSet::new();
    for (kind, re) in &rules().rules {
        for m in re.find_iter(text).take(200) {
            let value = m.as_str().trim().to_string();
            if value.is_empty() || value.len() > 300 {
                continue;
            }
            // Un SHA n'est retenu que s'il contient au moins un chiffre ET une lettre :
            // évite de capturer « deadbeef » dans une phrase ou un nombre décimal.
            if *kind == AnchorKind::Sha {
                let has_digit = value.chars().any(|c| c.is_ascii_digit());
                let has_alpha = value.chars().any(|c| c.is_ascii_alphabetic());
                if !has_digit || !has_alpha || value.len() < 7 {
                    continue;
                }
            }
            set.insert(Anchor { kind: *kind, value });
        }
    }
    set.into_iter().collect()
}

/// Extrait les ancres de plusieurs textes, sous plafond global.
pub fn extract_many(texts: &[&str], max: usize) -> Vec<Anchor> {
    let mut set: BTreeSet<Anchor> = BTreeSet::new();
    for t in texts {
        for a in extract(t) {
            set.insert(a);
            if set.len() >= max {
                return set.into_iter().collect();
            }
        }
    }
    set.into_iter().collect()
}

/// Fusionne l'index d'un résumé mis à jour : les ancres récentes d'abord, puis les
/// anciennes tant que le plafond le permet. Résultat trié, sans doublon.
pub fn merge(recent: &[Anchor], older: &[Anchor], max: usize) -> Vec<Anchor> {
    let mut set: BTreeSet<Anchor> = BTreeSet::new();
    for a in recent.iter().chain(older) {
        if set.len() >= max {
            break;
        }
        set.insert(a.clone());
    }
    set.into_iter().collect()
}

/// Rendu compact de l'index, injecté avec le résumé.
pub fn render(anchors: &[Anchor]) -> String {
    if anchors.is_empty() {
        return String::new();
    }
    let mut by_kind: std::collections::BTreeMap<&'static str, Vec<&str>> = Default::default();
    for a in anchors {
        by_kind.entry(a.kind.as_str()).or_default().push(&a.value);
    }
    let mut out = String::from("Ancres :\n");
    for (k, vs) in by_kind {
        out.push_str(&format!("- {k} : {}\n", vs.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(text: &str, kind: AnchorKind) -> Vec<String> {
        extract(text)
            .into_iter()
            .filter(|a| a.kind == kind)
            .map(|a| a.value)
            .collect()
    }

    #[test]
    fn extracts_paths() {
        let v = values(
            "modifie src/main.rs et crates/penelope-llm/src/sse.rs",
            AnchorKind::Path,
        );
        assert!(v.contains(&"src/main.rs".to_string()), "{v:?}");
        assert!(
            v.contains(&"crates/penelope-llm/src/sse.rs".to_string()),
            "{v:?}"
        );
    }

    #[test]
    fn extracts_sha_but_not_plain_numbers() {
        let v = values("commit 4f9a2b1 puis 1234567 et 0123456789", AnchorKind::Sha);
        assert!(v.contains(&"4f9a2b1".to_string()));
        assert!(
            !v.iter().any(|x| x == "1234567"),
            "un nombre pur n'est pas un SHA : {v:?}"
        );
    }

    #[test]
    fn extracts_tickets_and_prs() {
        let a = extract("voir PROJ-1423 et #4312, la PR #77 est ouverte");
        let tickets: Vec<_> = a
            .iter()
            .filter(|x| x.kind == AnchorKind::Ticket)
            .map(|x| x.value.as_str())
            .collect();
        assert!(tickets.contains(&"PROJ-1423"), "{tickets:?}");
        assert!(tickets.contains(&"#4312"), "{tickets:?}");
        assert!(a.iter().any(|x| x.kind == AnchorKind::PullRequest));
    }

    #[test]
    fn extracts_urls_without_trailing_punctuation() {
        let v = values(
            "voir https://example.com/a/b?x=1 pour la suite",
            AnchorKind::Url,
        );
        assert_eq!(v, vec!["https://example.com/a/b?x=1"]);
    }

    #[test]
    fn extracts_error_lines() {
        let text = "sortie :\nerror[E0308]: mismatched types\n  --> src/a.rs:3\nfini";
        let v = values(text, AnchorKind::Error);
        assert!(v.iter().any(|e| e.contains("E0308")), "{v:?}");
    }

    #[test]
    fn extracts_ulids_and_uuids() {
        let v = values(
            "uid 01J8AABBCCDDEEFFGGHHJJKKMM et 3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            AnchorKind::Identifier,
        );
        assert_eq!(v.len(), 2, "{v:?}");
    }

    #[test]
    fn extraction_is_deterministic_and_deduplicated() {
        let t = "src/a.rs src/a.rs src/b.rs";
        let a = extract(t);
        let b = extract(t);
        assert_eq!(a, b);
        assert_eq!(
            a.iter().filter(|x| x.value == "src/a.rs").count(),
            1,
            "pas de doublon"
        );
    }

    #[test]
    fn nothing_is_paraphrased() {
        // Le texte de l'ancre est exactement celui du document source.
        let t = "chemin: crates/penelope-context/src/anchors.rs";
        let v = values(t, AnchorKind::Path);
        assert!(t.contains(&v[0]));
    }

    #[test]
    fn render_groups_by_kind() {
        let a = extract("src/a.rs et PROJ-12 et https://x.example/y");
        let r = render(&a);
        assert!(r.contains("chemin :"));
        assert!(r.contains("ticket :"));
        assert!(r.contains("url :"));
    }

    #[test]
    fn merge_prefers_recent_anchors_under_the_cap() {
        let recent = extract("src/nouveau.rs et PROJ-2");
        let older = extract("src/ancien.rs, src/vieux.rs et PROJ-2");
        let all = merge(&recent, &older, 10);
        assert_eq!(all.len(), 4, "PROJ-2 n'apparaît qu'une fois : {all:?}");
        let capped = merge(&recent, &older, 3);
        assert_eq!(capped.len(), 3);
        assert!(capped.iter().any(|a| a.value == "src/nouveau.rs"));
        assert!(capped.iter().any(|a| a.value == "PROJ-2"));
    }

    #[test]
    fn extract_many_respects_the_cap() {
        let texts: Vec<String> = (0..100).map(|i| format!("src/f{i}.rs")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        assert!(extract_many(&refs, 10).len() <= 10);
    }

    #[test]
    fn anchor_kinds_have_french_names() {
        let names: Vec<&str> = [
            AnchorKind::Path,
            AnchorKind::Sha,
            AnchorKind::Ticket,
            AnchorKind::PullRequest,
            AnchorKind::Url,
            AnchorKind::Error,
            AnchorKind::Identifier,
        ]
        .iter()
        .map(|k| k.as_str())
        .collect();
        assert_eq!(
            names,
            [
                "chemin",
                "sha",
                "ticket",
                "pr",
                "url",
                "erreur",
                "identifiant"
            ]
        );
    }
}
