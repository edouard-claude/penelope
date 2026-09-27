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
