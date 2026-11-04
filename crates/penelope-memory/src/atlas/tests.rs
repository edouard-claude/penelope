use super::*;

fn write(vault: &Path, rel: &str, body: &str) {
    let p = vault.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn edges(a: &Atlas) -> Vec<(String, String)> {
    a.links
        .iter()
        .map(|l| {
            let (x, y) = (a.nodes[l.a].title.clone(), a.nodes[l.b].title.clone());
            if x <= y { (x, y) } else { (y, x) }
        })
        .collect()
}

/// #428 : une note ajoutée, un lien modifié, une note renommée puis supprimée se voient
/// au calcul suivant ; une cible introuvable est une référence ; les journaux en ajout
/// seul et les blocs de code ne font pas de liens.
#[test]
fn the_atlas_follows_the_vault_on_each_build() {
    let dir = tempfile::tempdir().unwrap();
    let v = dir.path();
    write(
        v,
        "concepts/rust.md",
        "---\naliases: [Ferris]\n---\nVoir [[cargo]].\n",
    );
    write(v, "concepts/cargo.md", "Outil.\n");
    write(v, "log.md", "## op\n[[rust]] et [[fantome]]\n");
    write(v, "notes/code.md", "```\n[[pas-un-lien]]\n```\n");
    write(v, "notes/secrets-bancaires.md", "[[rust]]\n");
    let a = build(v).unwrap();
    assert_eq!(a.stats.notes, 3);
    assert_eq!(edges(&a), vec![("cargo".into(), "rust".into())]);
    assert_eq!(a.stats.references, 0, "ni log.md ni le bloc de code");
    let fp = fingerprint(v);
    assert_eq!(fingerprint(v), fp, "sans changement, même empreinte");
    assert_eq!(build(v).unwrap(), a, "déterministe, placement compris");

    // Ajout d'une note liée par alias, et d'un lien vers une cible absente.
    write(
        v,
        "notes/journee.md",
        "J'ai vu [[Ferris]] et [[outil-absent]].\n",
    );
    assert_ne!(fingerprint(v), fp);
    let a = build(v).unwrap();
    assert!(edges(&a).contains(&("journee".into(), "rust".into())));
    assert_eq!(a.stats.references, 1);
    let r = a.nodes.iter().find(|n| n.kind == "ref").unwrap();
    assert_eq!(
        (r.title.as_str(), r.cat.as_str()),
        ("outil-absent", "références")
    );

    // Renommage puis suppression : l'ancien nom disparaît.
    std::fs::rename(v.join("concepts/cargo.md"), v.join("concepts/build.md")).unwrap();
    let a = build(v).unwrap();
    assert!(
        a.nodes
            .iter()
            .all(|n| n.title != "cargo" || n.kind == "ref")
    );
    std::fs::remove_file(v.join("notes/journee.md")).unwrap();
    let a = build(v).unwrap();
    assert!(a.nodes.iter().all(|n| n.title != "journee"));
    assert_eq!(a.stats.par_dossier.get("concepts"), Some(&2));
}

#[test]
fn a_missing_vault_is_an_error_not_an_empty_atlas() {
    let dir = tempfile::tempdir().unwrap();
    assert!(build(&dir.path().join("absent")).is_err());
}
