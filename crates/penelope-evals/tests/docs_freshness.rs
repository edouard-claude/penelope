//! Routine de livraison (issue #3) : une section de la documentation qui annonce un manque
//! (« pas encore branché », « à brancher », « non servies », « Limites actuelles ») ne cite
//! jamais une méthode RPC. Le daemon les sert toutes (test `rpc.rs`) : une méthode citée
//! là est forcément une phrase périmée.

use penelope_evals::ca_matrix;
use penelope_kernel::api::method;

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

#[test]
fn no_doc_presents_a_served_rpc_method_as_missing() {
    let root = ca_matrix::repo_root();
    let mut files = vec![root.join("README.md")];
    let mut docs: Vec<_> = std::fs::read_dir(root.join("docs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    docs.sort();
    files.extend(docs);

    let families: std::collections::BTreeSet<&str> = method::ALL
        .iter()
        .filter_map(|m| m.split_once('.').map(|(f, _)| f))
        .collect();
    let mut stale = Vec::new();
    for file in files {
        let raw = std::fs::read_to_string(&file).unwrap();
        for (title, body) in missing_sections(&raw) {
            for m in method::ALL {
                if body.contains(&format!("`{m}`")) {
                    stale.push(format!("{} « {title} » cite `{m}`", file.display()));
                }
            }
            for f in &families {
                if body.contains(&format!("`{f}.*`")) {
                    stale.push(format!("{} « {title} » cite `{f}.*`", file.display()));
                }
            }
        }
    }
    assert!(
        stale.is_empty(),
        "sections périmées (toutes les méthodes RPC sont servies) :\n{}",
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
