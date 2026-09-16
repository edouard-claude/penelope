//! Génération de `docs/ca-matrix.md` (§20.2, point 2).
//!
//! L'index n'est pas écrit à la main : il est **extrait des sources**. Un test compare le
//! fichier livré à ce que produit ce module ; ajouter un test `ca_<section>_<n>_<nom>`
//! suffit donc à le mettre à jour (`UPDATE_CA_MATRIX=1 cargo test -p penelope-evals`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Titres des sections du PRD, pour rendre la table lisible.
pub const SECTIONS: &[(&str, &str)] = &[
    ("2", "Plateformes, portabilité et exploitation headless"),
    ("3", "Architecture"),
    ("4", "Noyau : event log, ledger d'effets, générations"),
    ("5", "Sessions et moteur de contexte"),
    ("6", "Mémoire, apprentissage continu et second brain"),
    ("7", "Skills"),
    ("8", "MCP : client complet"),
    ("9", "HITL (Human in the Loop)"),
    ("10", "LLM : providers et routage"),
    ("11", "Outils natifs"),
    ("12", "Workflows, triggers et jobs"),
    ("13", "Sécurité"),
    ("14", "Telegram : implémentation complète"),
    ("15", "CLI et SSH"),
    ("16", "Observabilité"),
    ("17", "Résilience"),
];

/// Un test d'acceptation trouvé dans les sources.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaTest {
    pub section: String,
    pub number: u32,
    pub function: String,
    /// Chemin relatif à la racine du dépôt.
    pub file: String,
}

impl CaTest {
    /// Libellé lisible, dérivé du nom de la fonction.
    pub fn label(&self) -> String {
        let prefix = format!("ca_{}_{}_", self.section, self.number);
        let rest = self
            .function
            .strip_prefix(&prefix)
            .unwrap_or(&self.function);
        let mut s = rest.replace('_', " ");
        if let Some(c) = s.get(..1) {
            s.replace_range(..1, &c.to_uppercase());
        }
        s
    }
}

/// Racine du dépôt, déduite de l'emplacement de ce crate.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("racine du dépôt")
        .to_path_buf()
}

/// Extrait de `raw` les noms de fonctions `ca_<section>_<n>_<nom>`.
pub fn scan_source(raw: &str) -> Vec<(String, u32, String)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let Some(rest) = line.split("fn ").nth(1) else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let Some(tail) = name.strip_prefix("ca_") else {
            continue;
        };
        let mut parts = tail.splitn(3, '_');
        let (Some(section), Some(number), Some(_)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(_), Ok(n)) = (section.parse::<u32>(), number.parse::<u32>()) else {
            continue;
        };
        out.push((section.to_string(), n, name));
    }
    out
}

/// Parcourt les sources du dépôt et collecte tous les tests d'acceptation.
pub fn collect(root: &Path) -> Vec<CaTest> {
    let mut found = Vec::new();
    let crates = root.join("crates");
    let Ok(entries) = std::fs::read_dir(&crates) else {
        return found;
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        walk(&d, root, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<CaTest>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&p, root, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let Ok(raw) = std::fs::read_to_string(&p) else {
                continue;
            };
            let file = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            for (section, number, function) in scan_source(&raw) {
                out.push(CaTest {
                    section,
                    number,
                    function,
                    file: file.clone(),
                });
            }
        }
    }
}

/// Rend la matrice en Markdown.
pub fn render(tests: &[CaTest]) -> String {
    let mut by_section: BTreeMap<u32, Vec<&CaTest>> = BTreeMap::new();
    for t in tests {
        by_section
            .entry(t.section.parse().unwrap_or(0))
            .or_default()
            .push(t);
    }

    let mut s = String::new();
    s.push_str("# Matrice des critères d'acceptation\n\n");
    s.push_str(
        "Index **généré** à partir des sources : chaque test nommé `ca_<section>_<n>_<nom>`\n\
         couvre le critère d'acceptation correspondant du PRD (§20.2, point 2).\n\n\
         Pour le régénérer après avoir ajouté un test :\n\n\
         ```bash\n\
         UPDATE_CA_MATRIX=1 cargo test -p penelope-evals --test ca_matrix\n\
         ```\n\n",
    );
    s.push_str(&format!(
        "{} tests d'acceptation, {} sections couvertes.\n\n",
        tests.len(),
        by_section.len()
    ));

    for (number, list) in &by_section {
        let key = number.to_string();
        let title = SECTIONS
            .iter()
            .find(|(n, _)| *n == key)
            .map(|(_, t)| *t)
            .unwrap_or("(section inconnue)");
        s.push_str(&format!("## §{number}. {title}\n\n"));
        s.push_str("| CA | Test | Fichier |\n|---|---|---|\n");
        for t in list {
            s.push_str(&format!(
                "| CA {}.{} | {} | `{}` |\n",
                t.section,
                t.number,
                t.label(),
                t.file
            ));
        }
        s.push('\n');
    }
    s
}

/// Chemin du fichier livré.
pub fn output_path(root: &Path) -> PathBuf {
    root.join("docs").join("ca-matrix.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_scanning_only_keeps_well_formed_names() {
        let raw = "\
            #[test]\n\
            fn ca_8_3_oauth_discovery_and_pkce() {}\n\
            async fn ca_17_1_a_turn_is_requeued() {}\n\
            fn pas_un_ca() {}\n\
            fn ca_sans_numero() {}\n\
            fn ca_8_3() {}\n";
        let found = scan_source(raw);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "8");
        assert_eq!(found[0].1, 3);
        assert_eq!(found[1].2, "ca_17_1_a_turn_is_requeued");
    }

    #[test]
    fn labels_are_readable() {
        let t = CaTest {
            section: "17".into(),
            number: 2,
            function: "ca_17_2_a_completed_effect_is_replayed".into(),
            file: "crates/x/src/lib.rs".into(),
        };
        assert_eq!(t.label(), "A completed effect is replayed");
    }

    #[test]
    fn rendering_groups_by_section_in_order() {
        let tests = vec![
            CaTest {
                section: "17".into(),
                number: 1,
                function: "ca_17_1_reprise".into(),
                file: "a.rs".into(),
            },
            CaTest {
                section: "4".into(),
                number: 1,
                function: "ca_4_1_chaine".into(),
                file: "b.rs".into(),
            },
        ];
        let md = render(&tests);
        let pos_4 = md.find("## §4.").unwrap();
        let pos_17 = md.find("## §17.").unwrap();
        assert!(pos_4 < pos_17, "les sections doivent être ordonnées");
        assert!(md.contains("| CA 4.1 |"));
        assert!(md.contains("Noyau"));
    }
}
